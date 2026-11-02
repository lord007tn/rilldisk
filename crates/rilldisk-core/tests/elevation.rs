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
        _ => return eprintln!("skipped: no USERNAME"),
    };
    let icacls = |args: &[&str]| {
        Command::new("icacls")
            .arg(&locked)
            .args(args)
            .output()
            .is_ok_and(|o| o.status.success())
    };
    // RD: list folder / read data.
    if !icacls(&["/deny", &format!("{user}:(RD)")]) {
        return eprintln!("skipped: could not add a deny ACE");
    }
    if fs::read_dir(&locked).is_ok() {
        icacls(&["/remove:d", &user]);
        return eprintln!("skipped: the deny ACE did not take effect");
    }
    let first = scan(root.path(), ScanOptions::default(), Arc::default());
    let state = elevation();
    let second = state
        .is_elevated
        .then(|| {
            assert!(rilldisk_core::enable_backup_privilege());
            assert!(elevation().backup_privilege_enabled);
            scan(root.path(), ScanOptions::default(), Arc::default())
        })
        .transpose();
    // Restore before asserting, so the temporary folder can always be removed.
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
    if let Some(second) = second.unwrap() {
        assert!(second.summary.complete, "{:?}", second.summary.issues);
        assert_eq!(second.summary.access_denied_count, 0);
        assert_eq!(second.summary.logical_bytes, 4096);
    }
}
