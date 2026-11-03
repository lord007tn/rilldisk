<details>
<summary><b>First launch, verifying downloads, updating</b></summary>

- **Windows:** the builds are not code-signed yet. If SmartScreen appears, choose *More info*, then *Run anyway*.
- **macOS:** the app is not notarized yet. Open *System Settings > Privacy & Security* and choose *Open Anyway*, or run `xattr -dr com.apple.quarantine /Applications/RillDisk.app`.
- **Linux:** builds need glibc 2.28 or newer (Debian 10+, Ubuntu 20.04+, RHEL 8+). The static musl build runs anywhere.
- **Verify:** `sha256sum --check --ignore-missing SHA256SUMS`, and `gh attestation verify <file> --repo lord007tn/rilldisk` for the signed build provenance.
- **Update:** the app offers new versions (with your consent). From a terminal, run `rilldisk update`. Windows MSI copies can choose **Update and restart** on the update card; Windows asks for administrator consent. Copies installed with `.deb`, `.rpm`, AppImage or `.dmg` link the matching new package instead.

Full guide: [docs/INSTALL.md](https://github.com/lord007tn/rilldisk/blob/main/docs/INSTALL.md)
</details>

## What's changed
