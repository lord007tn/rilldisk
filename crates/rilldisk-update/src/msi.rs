// SPDX-License-Identifier: AGPL-3.0-only
//! Explicit, verified updates for our own Windows MSI. See proposals/updates.md.
use crate::{
    InstallSource, Package, Release, detect_install, net, state_path, target, verify_sha256,
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    fs,
    io::Read,
    os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0},
    System::{
        Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize},
        SystemInformation::GetSystemDirectoryW,
        Threading::{
            GetExitCodeProcess, INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
        },
    },
    UI::Shell::{SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW},
};

pub const HELPER_ARG: &str = "--rilldisk-install-helper";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Status {
    AwaitingConsent,
    InstallerStarted,
    Finished {
        version: String,
        reboot_required: bool,
    },
    Failed(String),
}

#[derive(Serialize, Deserialize)]
struct Request {
    desktop: PathBuf,
    args: Vec<OsString>,
    version: String,
    sha256: String,
    parent_pid: u32,
}

pub struct Prepared {
    request: PathBuf,
    helper: PathBuf,
}

pub struct Handoff {
    pub child: Child,
    request: PathBuf,
}

fn pending_path() -> Result<PathBuf> {
    Ok(state_path()
        .context("No per-user update folder is available")?
        .with_file_name("pending-installer.json"))
}

fn write_status(request: &Path, status: &Status) -> Result<()> {
    // Atomic publication: readers must never consume a half-written receipt.
    let path = request.with_file_name("status.json");
    let temp = path.with_extension("tmp");
    fs::write(&temp, serde_json::to_vec(status)?)?;
    fs::rename(temp, path)?;
    Ok(())
}

fn read_status(request: &Path) -> Result<Status> {
    let bytes = fs::read(request.with_file_name("status.json"))?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// The caller has already obtained an explicit Update and restart click.
pub fn prepare(release: &Release, progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<Prepared> {
    let desktop = std::env::current_exe()?.canonicalize()?;
    if !matches!(
        detect_install(&desktop),
        InstallSource::Managed {
            package: Some(Package::Msi),
            ..
        }
    ) {
        bail!("Installer updates are only supported for RillDisk's own MSI installation");
    }
    let asset = release
        .package_asset("msi", target().context("Unsupported target")?)
        .context("This release has no matching MSI package")?;
    if release.version <= crate::current_version() {
        bail!("The installer is not newer than this RillDisk version");
    }
    let name = installer_name(&release.version.to_string())?;
    let url = format!(
        "https://github.com/{}/releases/download/v{}/{name}",
        crate::REPOSITORY,
        release.version
    );
    if release.tag != format!("v{}", release.version) || asset.name != name || asset.url != url {
        bail!("The MSI is not the expected RillDisk release asset");
    }
    let expected = release
        .checksums
        .get(&asset.name)
        .context("The MSI has no release checksum")?;
    let bytes = net::download(&asset.url, 1 << 30, progress)?;
    verify_sha256(&bytes, expected)?;
    let root = state_path()
        .context("No update storage is available")?
        .with_file_name("updates");
    fs::create_dir_all(&root)?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let dir = root.join(format!(
        "{}-{}-{nonce}",
        release.version,
        std::process::id()
    ));
    fs::create_dir(&dir)?;
    fs::write(dir.join(&name), bytes)?;
    let helper = dir.join("rilldisk-update-helper.exe");
    // A distinct file identity lets Windows Installer replace the installed app
    // after it closes without also closing this helper.
    fs::copy(&desktop, &helper)?;
    let request = dir.join("request.json");
    let data = Request {
        desktop,
        args: std::env::args_os().skip(1).collect(),
        version: release.version.to_string(),
        sha256: expected.clone(),
        parent_pid: std::process::id(),
    };
    fs::write(&request, serde_json::to_vec(&data)?)?;
    write_status(&request, &Status::AwaitingConsent)?;
    Ok(Prepared { request, helper })
}

impl Prepared {
    pub fn start(self) -> Result<Handoff> {
        fs::write(pending_path()?, serde_json::to_vec(&self.request)?)?;
        let child = Command::new(&self.helper)
            .arg(HELPER_ARG)
            .arg(&self.request)
            .creation_flags(0x0800_0000)
            .spawn()
            .context("Could not start the update helper");
        let child = match child {
            Ok(child) => child,
            Err(error) => {
                write_status(&self.request, &Status::Failed(format!("{error:#}")))?;
                return Err(error);
            }
        };
        Ok(Handoff {
            child,
            request: self.request,
        })
    }
}

impl Handoff {
    pub fn status(&mut self) -> Result<Status> {
        let status = read_status(&self.request)?;
        if status == Status::AwaitingConsent && self.child.try_wait()?.is_some() {
            // Re-read after exit: the helper may have published its failure
            // between our first read and try_wait.
            let final_status = read_status(&self.request)?;
            if final_status == Status::AwaitingConsent {
                bail!("The installer helper exited before Windows started installation");
            }
            return Ok(final_status);
        }
        Ok(status)
    }
}

/// Show a real installer result once on the next desktop launch.
pub fn take_result() -> Result<Option<Status>> {
    let pointer = pending_path()?;
    let request: PathBuf = match fs::read(&pointer) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let status = read_status(&request)?;
    if matches!(status, Status::Finished { .. } | Status::Failed(_)) {
        fs::remove_file(pointer)?;
        return Ok(Some(status));
    }
    Ok(None)
}

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: each Handle owns exactly one non-null Windows process handle.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

fn installer_name(version: &str) -> Result<String> {
    let arch = match target() {
        Some("x86_64-pc-windows-msvc") => "x64",
        Some("aarch64-pc-windows-msvc") => "arm64",
        _ => bail!("Unsupported MSI target"),
    };
    // Windows Installer's registered source must retain the release filename.
    Ok(format!("RillDisk-{version}-{arch}.msi"))
}

fn system_installer() -> Result<PathBuf> {
    let mut buffer = vec![0_u16; 32768];
    // SAFETY: the buffer is writable and its length is accurately supplied.
    let len = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    if len == 0 || len >= buffer.len() {
        bail!("Cannot locate the Windows system directory");
    }
    use std::os::windows::ffi::OsStringExt;
    Ok(PathBuf::from(OsString::from_wide(&buffer[..len])).join("msiexec.exe"))
}

fn launch(msi: &Path) -> Result<Handle> {
    let exe = system_installer()?;
    let parameters = format!(
        "/i \"{}\" /passive /norestart /l*v \"{}\"",
        msi.display(),
        msi.with_file_name("install.log").display()
    );
    if msi
        .as_os_str()
        .encode_wide()
        .any(|c| c == 0 || c == b'"' as u16)
    {
        bail!("Invalid installer path");
    }
    let wide = |text: &std::ffi::OsStr| text.encode_wide().chain(Some(0)).collect::<Vec<_>>();
    let exe = wide(exe.as_os_str());
    let parameters = wide(std::ffi::OsStr::new(&parameters));
    let verb = wide(std::ffi::OsStr::new("runas"));
    // SAFETY: optional fields in this FFI structure may be null.
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
    info.lpVerb = verb.as_ptr();
    info.lpFile = exe.as_ptr();
    info.lpParameters = parameters.as_ptr();
    info.nShow = 1;
    // SAFETY: every string remains alive through this synchronous API call.
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(1223) {
            bail!("Administrator consent was declined. Nothing was installed.");
        }
        return Err(error).context("Windows could not start the installer");
    }
    if info.hProcess.is_null() {
        bail!("Windows did not return an installer process handle");
    }
    Ok(Handle(info.hProcess))
}

fn verified_install(request: &Path, data: &Request) -> Result<bool> {
    if !matches!(
        detect_install(&data.desktop),
        InstallSource::Managed {
            package: Some(Package::Msi),
            ..
        }
    ) {
        bail!("The original installation is no longer an MSI installation");
    }
    let msi = request.with_file_name(installer_name(&data.version)?);
    // Keep the verified bytes immutable until Windows Installer exits.
    let mut file = fs::OpenOptions::new().read(true).share_mode(1).open(&msi)?;
    let size = file.metadata()?.len();
    if size > 1 << 30 {
        bail!("Installer exceeds the size limit");
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    verify_sha256(&bytes, &data.sha256)?;
    drop(bytes);
    // Hold the original process identity before the UAC prompt can delay us.
    // SAFETY: requesting synchronization access does not mutate the process.
    let parent = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, data.parent_pid) };
    let parent = if parent.is_null() {
        None
    } else {
        Some(Handle(parent))
    };
    let installer = launch(&msi)?;
    fs::write(request.with_file_name("installer-started"), [])?;
    write_status(request, &Status::InstallerStarted)?;
    if let Some(parent) = parent {
        // SAFETY: parent owns the valid synchronization handle.
        unsafe {
            WaitForSingleObject(parent.0, Duration::from_secs(30).as_millis() as u32);
        }
    }
    // SAFETY: the installer handle is owned and valid until this function ends.
    if unsafe { WaitForSingleObject(installer.0, INFINITE) } != WAIT_OBJECT_0 {
        return Err(std::io::Error::last_os_error())
            .context("Could not wait for Windows Installer");
    }
    let mut code = 0;
    // SAFETY: code is writable, and the completed process handle is valid.
    if unsafe { GetExitCodeProcess(installer.0, &mut code) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let reboot_required = installer_result(code)?;
    if !reboot_required {
        verify_installed(data)?;
    }
    Ok(reboot_required)
}

fn installer_result(code: u32) -> Result<bool> {
    match code {
        0 => Ok(false),
        3010 => Ok(true),
        1602 => bail!("Windows Installer was cancelled. Start the update again when ready."),
        _ => bail!(
            "Windows Installer failed (code {code}). The previous version may still be installed."
        ),
    }
}

#[derive(Deserialize)]
struct InstalledManifest {
    version: String,
    executables_sha256: std::collections::HashMap<String, String>,
}

fn verify_installed(data: &Request) -> Result<()> {
    let dir = data
        .desktop
        .parent()
        .context("Installation has no parent folder")?;
    let bytes = fs::read(dir.join("BUILD-MANIFEST.json"))?;
    if bytes.len() > 1 << 20 {
        bail!("Installed build manifest is too large");
    }
    let manifest: InstalledManifest = serde_json::from_slice(&bytes)?;
    if manifest.version != data.version {
        bail!(
            "Installer completed but the installed version does not match {}",
            data.version
        );
    }
    for name in ["rilldisk.exe", "rilldisk-desktop.exe"] {
        let expected = manifest
            .executables_sha256
            .get(name)
            .context("Installed manifest has no binary checksum")?;
        verify_sha256(&fs::read(dir.join(name))?, expected)
            .with_context(|| format!("Installed {name} does not match its release manifest"))?;
    }
    Ok(())
}

/// Private desktop entry point: no GPU/UI initialization is needed here.
pub fn run_helper(request: &Path) -> Result<()> {
    if fs::metadata(request)?.len() > 65536 {
        bail!("Installer request is too large");
    }
    let bytes = fs::read(request)?;
    if bytes.len() > 65536 {
        bail!("Installer request is too large");
    }
    let data: Request = serde_json::from_slice(&bytes)?;
    semver::Version::parse(&data.version).context("Invalid installer target version")?;
    if data.desktop.file_name() != Some(std::ffi::OsStr::new("rilldisk-desktop.exe")) {
        bail!("Unexpected desktop executable name");
    }
    // SAFETY: COM is initialized for this helper thread; uninitialized below
    // only when this call succeeded (including S_FALSE).
    let com = unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) };
    let result = verified_install(request, &data);
    let log = request.with_file_name("install.log");
    let result = if log.exists() {
        result.with_context(|| format!("Installer log: {}", log.display()))
    } else {
        result
    };
    if com >= 0 {
        unsafe {
            CoUninitialize();
        }
    }
    let receipt = match &result {
        Ok(reboot_required) => write_status(
            request,
            &Status::Finished {
                version: data.version.clone(),
                reboot_required: *reboot_required,
            },
        ),
        Err(error) => write_status(request, &Status::Failed(format!("{error:#}"))),
    };
    // Consent/start failure is observed by the still-open original window.
    // Once an installer started, that window closed and needs to be relaunched.
    if result.is_ok() || request.with_file_name("installer-started").exists() {
        // A rolled-back older desktop may not understand our receipt format.
        // Surface failures/reboot requirements even in that case.
        let notice = match &result {
            Err(error) => Some((
                format!(
                    "RillDisk could not complete its update.\n\n{error:#}\n\nRillDisk will reopen."
                ),
                true,
            )),
            Ok(true) => Some((
                format!(
                    "Restart Windows to finish updating RillDisk to {}. Your computer will not restart automatically.",
                    data.version
                ),
                false,
            )),
            _ => receipt.as_ref().err().map(|error| (
                format!("Installation finished, but its result could not be saved.\n\n{error:#}\n\nRillDisk will reopen."),
                true,
            )),
        };
        if let Some((text, failed)) = notice {
            show_notice(&text, failed);
        }
        if let Err(error) = Command::new(&data.desktop).args(data.args).spawn() {
            let message = format!(
                "Installer finished, but RillDisk could not reopen: {error}. Start RillDisk from the Start menu."
            );
            let _ = write_status(request, &Status::Failed(message.clone()));
            show_notice(&message, true);
            bail!("{message}");
        }
    }
    result?;
    receipt.context("Could not save the installer result")
}

fn show_notice(text: &str, failed: bool) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MessageBoxW,
    };
    let text: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    let title: Vec<u16> = "RillDisk update".encode_utf16().chain(Some(0)).collect();
    // SAFETY: both terminated strings remain alive for the modal call.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK
                | if failed {
                    MB_ICONERROR
                } else {
                    MB_ICONINFORMATION
                },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installer_results_distinguish_reboot_and_cancel() {
        assert!(!installer_result(0).unwrap());
        assert!(installer_result(3010).unwrap());
        assert!(
            installer_result(1602)
                .unwrap_err()
                .to_string()
                .contains("cancelled")
        );
        assert!(
            installer_result(1618)
                .unwrap_err()
                .to_string()
                .contains("1618")
        );
        assert!(installer_result(1641).is_err());
    }

    #[test]
    fn corrupt_package_is_rejected_before_any_installer_launch() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("installed-by"), "msi").unwrap();
        let desktop = dir.path().join("rilldisk-desktop.exe");
        fs::write(&desktop, b"original").unwrap();
        fs::write(
            dir.path().join(installer_name("0.1.3").unwrap()),
            b"corrupt",
        )
        .unwrap();
        let request = dir.path().join("request.json");
        let data = Request {
            desktop: desktop.clone(),
            args: Vec::new(),
            version: "0.1.3".into(),
            sha256: "0".repeat(64),
            parent_pid: std::process::id(),
        };
        let error = verified_install(&request, &data).unwrap_err().to_string();
        assert!(
            error.contains("checksum") || error.contains("SHA"),
            "{error}"
        );
        assert!(!dir.path().join("installer-started").exists());
        assert_eq!(fs::read(desktop).unwrap(), b"original");
    }

    #[test]
    fn receipt_tracks_launch_separately_from_completion() {
        let dir = tempfile::tempdir().unwrap();
        let request = dir.path().join("request.json");
        for state in [
            Status::AwaitingConsent,
            Status::InstallerStarted,
            Status::Failed("declined".into()),
            Status::Finished {
                version: "0.1.3".into(),
                reboot_required: true,
            },
        ] {
            write_status(&request, &state).unwrap();
            assert_eq!(read_status(&request).unwrap(), state);
        }
    }

    #[test]
    fn completion_requires_the_installed_release_and_matching_executables() {
        let dir = tempfile::tempdir().unwrap();
        let data = Request {
            desktop: dir.path().join("rilldisk-desktop.exe"),
            args: Vec::new(),
            version: "0.1.3".into(),
            sha256: "0".repeat(64),
            parent_pid: 0,
        };
        fs::write(
            dir.path().join("BUILD-MANIFEST.json"),
            br#"{"version":"0.1.2","executables_sha256":{}}"#,
        )
        .unwrap();
        assert!(
            verify_installed(&data)
                .unwrap_err()
                .to_string()
                .contains("version")
        );
        fs::write(dir.path().join("BUILD-MANIFEST.json"), br#"{"version":"0.1.3","executables_sha256":{"rilldisk.exe":"0000000000000000000000000000000000000000000000000000000000000000"}}"#).unwrap();
        fs::write(dir.path().join("rilldisk.exe"), b"modified").unwrap();
        assert!(verify_installed(&data).is_err());
    }
}
