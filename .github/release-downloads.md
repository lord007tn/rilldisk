## Download

| System | Recommended | Also available |
| --- | --- | --- |
| **Windows 10/11** x64 | [RillDisk-{version}-x64.msi](https://github.com/lord007tn/rilldisk/releases/download/{tag}/RillDisk-{version}-x64.msi) | [portable .zip](https://github.com/lord007tn/rilldisk/releases/download/{tag}/rilldisk-{tag}-x86_64-pc-windows-msvc.zip) |
| **Windows 11** ARM64 | [RillDisk-{version}-arm64.msi](https://github.com/lord007tn/rilldisk/releases/download/{tag}/RillDisk-{version}-arm64.msi) | [portable .zip](https://github.com/lord007tn/rilldisk/releases/download/{tag}/rilldisk-{tag}-aarch64-pc-windows-msvc.zip) |
| **macOS** Apple silicon | [RillDisk-{version}-arm64.dmg](https://github.com/lord007tn/rilldisk/releases/download/{tag}/RillDisk-{version}-arm64.dmg) | [.tar.gz](https://github.com/lord007tn/rilldisk/releases/download/{tag}/rilldisk-{tag}-aarch64-apple-darwin.tar.gz) |
| **macOS** Intel | [RillDisk-{version}-x64.dmg](https://github.com/lord007tn/rilldisk/releases/download/{tag}/RillDisk-{version}-x64.dmg) | [.tar.gz](https://github.com/lord007tn/rilldisk/releases/download/{tag}/rilldisk-{tag}-x86_64-apple-darwin.tar.gz) |
| **Debian, Ubuntu, Mint, Pop!_OS** | `.deb` [x86_64](https://github.com/lord007tn/rilldisk/releases/download/{tag}/rilldisk_{version}_amd64.deb) · [arm64](https://github.com/lord007tn/rilldisk/releases/download/{tag}/rilldisk_{version}_arm64.deb) | AppImage, .tar.gz |
| **Fedora, RHEL, Rocky, Alma, openSUSE** | `.rpm` [x86_64](https://github.com/lord007tn/rilldisk/releases/download/{tag}/rilldisk-{version}-1.x86_64.rpm) · [aarch64](https://github.com/lord007tn/rilldisk/releases/download/{tag}/rilldisk-{version}-1.aarch64.rpm) | AppImage, .tar.gz |
| **Any Linux desktop** (Arch, Manjaro, Gentoo...) | AppImage [x86_64](https://github.com/lord007tn/rilldisk/releases/download/{tag}/RillDisk-{version}-x86_64.AppImage) · [aarch64](https://github.com/lord007tn/rilldisk/releases/download/{tag}/RillDisk-{version}-aarch64.AppImage) | [.tar.gz x86_64](https://github.com/lord007tn/rilldisk/releases/download/{tag}/rilldisk-{tag}-x86_64-unknown-linux-gnu.tar.gz) · [aarch64](https://github.com/lord007tn/rilldisk/releases/download/{tag}/rilldisk-{tag}-aarch64-unknown-linux-gnu.tar.gz) |
| **Servers, containers, Alpine** (CLI + MCP only) | static musl [x86_64](https://github.com/lord007tn/rilldisk/releases/download/{tag}/rilldisk-{tag}-x86_64-unknown-linux-musl.tar.gz) · [aarch64](https://github.com/lord007tn/rilldisk/releases/download/{tag}/rilldisk-{tag}-aarch64-unknown-linux-musl.tar.gz) | |

<details>
<summary><b>First launch, verifying downloads, updating</b></summary>

- **Windows:** the builds are not code-signed yet. If SmartScreen appears, choose *More info*, then *Run anyway*.
- **macOS:** the app is not notarized yet. Open *System Settings > Privacy & Security* and choose *Open Anyway*, or run `xattr -dr com.apple.quarantine /Applications/RillDisk.app`.
- **Linux:** builds need glibc 2.28 or newer (Debian 10+, Ubuntu 20.04+, RHEL 8+). The static musl build runs anywhere.
- **Verify:** `sha256sum --check --ignore-missing SHA256SUMS`, and `gh attestation verify <file> --repo lord007tn/rilldisk` for the signed build provenance.
- **Update:** the app offers new versions (with your consent). From a terminal, run `rilldisk update`. Copies installed with the MSI, `.deb`, `.rpm`, AppImage or `.dmg` are pointed at the matching new package instead.

Full guide: [docs/INSTALL.md](https://github.com/lord007tn/rilldisk/blob/main/docs/INSTALL.md)
</details>

## What's changed
