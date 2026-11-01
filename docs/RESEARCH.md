# Category research

Checked November 1, 2026. This is product direction, not a general speed ranking.

* [WizTree](https://diskanalyzer.com/) documents direct MFT scanning as its NTFS
  speed advantage. Its [command-line guide](https://diskanalyzer.com/guide)
  distinguishes ordinary scans from administrator/MFT mode and supports CSV export.
  Folder export timing in ordinary mode cannot validate an MFT performance claim.
* [WinDirStat](https://windirstat.net/) is an established open-source Windows disk
  analyzer. Its current site documents accelerated elevated NTFS scans, logical
  versus physical sizes, saved snapshots and JSON/CSV reporting.
* [SquirrelDisk 2](https://github.com/adileo/squirreldisk) already uses native Rust
  and provides a CLI with fresh scans and JSON output. Rust and a CLI alone are
  therefore not a sufficient product distinction. Its repository is AGPL-licensed.

RillDisk starts with one shared Rust data engine, explicit accounting semantics,
local MCP tools and a headless CLI. The next interface is native Rust with GPU
rendering. The separate NTFS fast path must earn speed claims through correctness,
matched scan scope and recorded privilege/cache conditions.

The first implementation is independent code. No code or assets were copied from
the referenced analyzers. AGPL-3.0-only is the project's chosen license from day one.

Next work: [native desktop](https://github.com/lord007tn/rilldisk/issues/2),
[NTFS qualification](https://github.com/lord007tn/rilldisk/issues/3), and
[future portable traversal](https://github.com/lord007tn/rilldisk/issues/4).
