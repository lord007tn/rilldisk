// SPDX-License-Identifier: AGPL-3.0-only
//! Issue classification and privilege detection. Its own test binary, because the
//! elevated Windows case enables a process-wide privilege.
use rilldisk_core::{IssueKind, ScanIssue, elevation};
use std::io;

#[test]
fn raw_os_errors_are_classified_per_platform() {
    let kind = |code| IssueKind::of(&io::Error::from_raw_os_error(code));
    if cfg!(windows) {
        assert_eq!(kind(5), IssueKind::AccessDenied); // ERROR_ACCESS_DENIED
        assert_eq!(kind(65), IssueKind::AccessDenied); // ERROR_NETWORK_ACCESS_DENIED
        assert_eq!(kind(1314), IssueKind::AccessDenied); // ERROR_PRIVILEGE_NOT_HELD
        assert_eq!(kind(2), IssueKind::NotFound);
        assert_eq!(kind(3), IssueKind::NotFound);
        assert_eq!(kind(32), IssueKind::InUse); // ERROR_SHARING_VIOLATION
        assert_eq!(kind(33), IssueKind::InUse);
        // ERROR_INVALID_FUNCTION, not EPERM.
        assert_eq!(kind(1), IssueKind::Other);
        assert_eq!(kind(13), IssueKind::Other); // ERROR_INVALID_DATA
    } else {
        assert_eq!(kind(13), IssueKind::AccessDenied); // EACCES
        assert_eq!(kind(1), IssueKind::AccessDenied); // EPERM (macOS privacy)
        assert_eq!(kind(2), IssueKind::NotFound);
        assert_eq!(kind(20), IssueKind::NotFound); // ENOTDIR
        assert_eq!(kind(16), IssueKind::InUse); // EBUSY
        assert_eq!(kind(5), IssueKind::Other); // EIO
    }
}

#[test]
fn portable_error_kinds_are_classified() {
    let kind = |k| IssueKind::of(&io::Error::from(k));
    assert_eq!(
        kind(io::ErrorKind::PermissionDenied),
        IssueKind::AccessDenied
    );
    assert_eq!(kind(io::ErrorKind::NotFound), IssueKind::NotFound);
    assert_eq!(kind(io::ErrorKind::ResourceBusy), IssueKind::InUse);
    assert_eq!(kind(io::ErrorKind::InvalidData), IssueKind::Other);
    assert_eq!(
        IssueKind::of(&io::Error::other("directory became a reparse point")),
        IssueKind::Other
    );
}

#[test]
fn removing_a_subtree_drops_its_listed_issues_from_the_counts() {
    use rilldisk_core::{ScanOptions, scan};
    use std::{fs, sync::Arc};
    let dir = tempfile::tempdir().unwrap();
    for folder in ["cache/deep", "keep"] {
        fs::create_dir_all(dir.path().join(folder)).unwrap();
    }
    let mut snapshot = scan(dir.path(), ScanOptions::default(), Arc::default()).unwrap();
    let root = snapshot.summary.root.clone();
    let id = |path: &std::path::Path| {
        (0..snapshot.entries.len())
            .find(|&id| snapshot.path(id) == path)
            .unwrap()
    };
    let cache = id(&root.join("cache"));
    // As if the scan had been refused inside `cache` and in `keep`, with one more
    // denial past the listing cap (counted, not listed).
    let mut privacy = ScanIssue::new(root.join("cache/deep"), IssueKind::AccessDenied, "x");
    privacy.os_error = Some(1);
    let summary = &mut snapshot.summary;
    summary.issues = vec![
        privacy,
        ScanIssue::new(root.join("cache"), IssueKind::InUse, "busy"),
        ScanIssue::new(root.join("keep"), IssueKind::AccessDenied, "x"),
        // A sibling whose name only starts like the removed folder stays.
        ScanIssue::new(root.join("cache-old"), IssueKind::AccessDenied, "x"),
    ];
    summary.issue_count = 5;
    summary.access_denied_count = 4;
    let macos = u64::from(cfg!(target_os = "macos"));
    summary.privacy_denied_count = macos;
    let after = snapshot.without(&[cache]).summary;
    let paths: Vec<_> = after.issues.iter().map(|i| i.path.clone()).collect();
    assert_eq!(paths, [root.join("keep"), root.join("cache-old")]);
    assert_eq!(after.issue_count, 3);
    assert_eq!(after.access_denied_count, 3);
    assert_eq!(after.privacy_denied_count, 0);
}

#[test]
fn issues_serialize_their_kind() {
    let issue = ScanIssue::from_error("x", &io::Error::from(io::ErrorKind::PermissionDenied));
    let json = serde_json::to_value(&issue).unwrap();
    assert_eq!(json["kind"], "access_denied");
    assert!(json["message"].is_string());
    assert!(json.get("os_error").is_none());
    let raw = ScanIssue::from_error("x", &io::Error::from_raw_os_error(2));
    let json = serde_json::to_value(&raw).unwrap();
    assert_eq!(json["kind"], "not_found");
    assert_eq!(json["os_error"], 2);
}

#[test]
fn elevation_state_is_consistent() {
    let state = elevation();
    assert!(!(state.is_elevated && state.can_elevate));
    if cfg!(unix) {
        assert!(!state.backup_privilege_enabled);
        assert!(!rilldisk_core::enable_backup_privilege());
    }
}

/// A folder whose ACL denies the current user listing is reported as
/// `access_denied`. When the test runs elevated (CI runners), enabling the backup
/// privilege must then read it without touching the ACL.
#[cfg(windows)]
#[test]
fn deny_ace_is_access_denied_and_backup_privilege_reads_it() {
    use rilldisk_core::{ScanOptions, scan};
    use std::{fs, process::Command, sync::Arc};
    let root = tempfile::tempdir().unwrap();
    let locked = root.path().join("locked");
    fs::create_dir(&locked).unwrap();
    fs::write(locked.join("inside.bin"), [7_u8; 4096]).unwrap();
    let user = match (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) {
        (Ok(domain), Ok(name)) => format!("{domain}\\{name}"),
        _ => panic!("USERDOMAIN and USERNAME must be set"),
    };
    let icacls = |args: &[&str]| {
        Command::new("icacls")
            .arg(&locked)
            .args(args)
            .output()
            .is_ok_and(|o| o.status.success())
    };
    // RD: list folder / read data.
    // A setup that cannot deny access fails loudly instead of passing unchecked.
    assert!(
        icacls(&["/deny", &format!("{user}:(RD)")]),
        "could not add a deny ACE"
    );
    if fs::read_dir(&locked).is_ok() {
        icacls(&["/remove:d", &user]);
        panic!("the deny ACE did not take effect");
    }
    let first = scan(root.path(), ScanOptions::default(), Arc::default());
    let state = elevation();
    // Elevated (CI runners): collect the privileged outcomes without asserting.
    let second = state.is_elevated.then(|| {
        let enabled = rilldisk_core::enable_backup_privilege();
        let privileged = elevation().backup_privilege_enabled;
        let scanned = scan(root.path(), ScanOptions::default(), Arc::default());
        (enabled, privileged, scanned)
    });
    // Restore before any assertion, so the temporary folder can always be removed.
    assert!(icacls(&["/remove:d", &user]));
    let first = first.unwrap();
    assert!(!first.summary.complete);
    assert_eq!(first.summary.issue_count, 1, "{:?}", first.summary.issues);
    assert_eq!(first.summary.access_denied_count, 1);
    let issue = &first.summary.issues[0];
    assert!(issue.path.ends_with("locked"), "{issue:?}");
    assert_eq!(issue.kind, IssueKind::AccessDenied);
    assert_eq!(issue.os_error, Some(5));
    assert_eq!(first.summary.elevated, state.is_elevated);
    if let Some((enabled, privileged, second)) = second {
        assert!(enabled, "enable_backup_privilege failed while elevated");
        assert!(privileged);
        let second = second.unwrap();
        assert!(second.summary.complete, "{:?}", second.summary.issues);
        assert_eq!(second.summary.access_denied_count, 0);
        assert_eq!(second.summary.logical_bytes, 4096);
    }
}

/// A root behind a parent that cannot be searched has no summary to count the
/// denial in, so the error itself names the route.
#[cfg(unix)]
#[test]
fn an_unopenable_root_says_access_was_denied() {
    use rilldisk_core::{ScanOptions, is_elevated, scan};
    use std::{fs, os::unix::fs::PermissionsExt, sync::Arc};
    if is_elevated() {
        return; // root searches every folder
    }
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("closed");
    fs::create_dir_all(parent.join("inside")).unwrap();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o000)).unwrap();
    let result = scan(
        &parent.join("inside"),
        ScanOptions::default(),
        Arc::default(),
    );
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
    let message = format!("{:#}", result.expect_err("the root cannot be opened"));
    assert!(message.contains("access denied"), "{message}");
    assert!(message.contains("administrator rights"), "{message}");
}
