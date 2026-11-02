# RillDisk 0.1.2

- Find updates in a persistent bottom bar, without opening Settings. Download
  progress uses received bytes; verification and Windows permission have named
  phases. Retry and error details stay accessible.
- Windows MSI installations can download and verify their matching installer,
  request Windows consent, install, and reopen RillDisk. Only the installer is
  elevated. Consent denial keeps the window open; installer results and reboot
  requirements are reported explicitly. Active scans/cleanup block restart.
- Improve parent/child navigation and the folder sidebar: stable child lists,
  clickable hierarchy, width-aware breadcrumbs, visible Parent, empty/unreadable
  states, long names, and returning to the folder you left. Rescans retain the
  current folder when it still exists.
- Remove the download table from generated GitHub release descriptions.

[Navigation evidence and limits](NAVIGATION.md) includes screenshots, a recording,
and reproduction steps. The live navigation recording uses scripted views; it
is not evidence of whole-drive performance. Scan speed is unchanged.

## Updater verification

The native `--capture OUTPUT --tour updates --theme light` tour uses fictional
release metadata and controlled progress, including unknown size, verification,
permission, declined consent and checksum failure. Clicking the visible bottom
button is asserted to start progress. It does not download or install software.
The screenshots/recording below are cropped to the bottom control.

![Update available](media/v0.1.2-update-available.png)
![Download progress](media/v0.1.2-update-downloading.png)
![Updater interaction recording](media/v0.1.2-updater.gif)

The separate native helper probe uses the published, checksum-verified 0.1.1 MSI
for a same-version repair. The first probe exposed a renamed-MSI repair failure (1603); staging now keeps
the original filename. The follow-up probe is awaiting Windows consent at PR
preparation. A forward upgrade to 0.1.2 and the original window's
close/relaunch cycle require the published 0.1.2 artifact and remain release
acceptance steps. A staged installer retains its original release filename,
which Windows Installer needs for repair/source registration.

Keyboard Tab traversal remains limited; arrow keys, Enter and Backspace provide
folder navigation. See NAVIGATION.md for the exact coverage and known gaps.
