// SPDX-License-Identifier: AGPL-3.0-only
//! Windows: embed the icon, version information and application manifest
//! (asInvoker, per-monitor DPI, long paths) so Explorer, Task Manager, the
//! taskbar and installers show RillDisk properly. Other targets: nothing.

fn main() {
    println!("cargo:rerun-if-changed=../../packaging/icons/rilldisk.ico");
    println!("cargo:rerun-if-changed=../../packaging/windows/rilldisk.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // The resource compiler runs elsewhere, so hand it absolute paths.
    let packaging = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging");
    let icon = packaging.join("icons/rilldisk.ico");
    let manifest = packaging.join("windows/rilldisk.manifest");
    let mut res = winresource::WindowsResource::new();
    res.set_icon(icon.to_str().expect("UTF-8 path"))
        .set_manifest_file(manifest.to_str().expect("UTF-8 path"))
        .set("ProductName", "RillDisk")
        .set("FileDescription", "RillDisk command line and MCP server")
        .set("CompanyName", "RillDisk contributors")
        .set(
            "LegalCopyright",
            "AGPL-3.0-only. Copyright (C) RillDisk contributors",
        );
    if let Err(error) = res.compile() {
        // A missing resource compiler must not hide the real build error.
        panic!(
            "embedding Windows resources failed: {error}. Install the Windows SDK, or set RC_PATH to its rc.exe"
        );
    }
}
