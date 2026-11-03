# Scanner qualification

This checklist maps issues [3](https://github.com/lord007tn/rilldisk/issues/3)
and [4](https://github.com/lord007tn/rilldisk/issues/4) to reproducible checks.
Windows remains the primary platform. These checks do not establish an OS sandbox.

| Requirement | Implementation and check | Evidence boundary |
| --- | --- | --- |
| Explicit MFT rights and fallback | `scan`, `mft_source`; `tests/backends.rs` | Explicit MFT errors without existing rights; Auto can fall back; no automatic elevation |
| NTFS names, identities and accounting | `platform/windows/mft.rs` unit tests; `tests/ntfs_image.rs` | Synthetic resident, nonresident, sparse, hard-link, extension, WOF and malformed records; independently authored image counts |
| Independent fixture qualification | `scripts/benchmark.py`, `scripts/test_benchmark.py` | Counts must match the generator manifest even when every backend agrees; only the known locked-folder omission is accepted |
| Allocation truth | `benchmark.py --verify-allocation --require-identical` | MFT must match handle-derived allocation exactly; directory-listing tolerance is printed and bounded, never applied to MFT |
| Whole-volume time, memory and coverage | Scan benchmark workflow, Windows whole-system-drive step | Public runner image; aggregate counts only; changing files and unreadable folders prevent treating this as an equality gate |
| Warm/cold conditions | Rotating backend order; optional `--cold-first`; PERFORMANCE.md | First observed run is not controlled cold storage; no cache purge or workstation-wide performance claim |
| Queued directory replacement | `queued_directory_replaced_with_symlink_is_not_followed` | Deterministic replacement after discovery and before child open |
| Ancestor replacement | `relative_child_keeps_its_parent_after_ancestor_replacement`, `full_path_reopen_rejects_a_replaced_ancestor` | Retained parent handles and descriptor-pressure full-path reopens tested separately |
| Unix default | `auto_status_describes_the_unix_backend_that_actually_runs` | Auto and Directory use no-follow opens; Legacy remains an unqualified reference path |

Local follow-up on November 3, 2026: six benchmark-gate regressions pass. A
Windows synthetic fixture (500 requested files, existing v0.1.3 example binaries)
passes the strengthened manifest and allocation gate with 544 readable files,
387 directories and one expected locked-folder issue. Both directory backends
agree on counts; allocation deviations are -0.040% and -0.043% against measured
truth. This unelevated check does not exercise raw MFT access. Timings from this
small, single-run fixture are not a performance claim.

Issue 3 still needs fresh elevated Windows benchmark results for this revision
before closing its qualification gate. Cross-platform Rust CI runs the Unix
replacement tests on Linux and macOS; local Linux evidence alone does not qualify
macOS. Existing benchmark history is recorded in PERFORMANCE.md, separately from
new runs. Private inventories must never be attached to either issue.

No-follow opens prevent traversing symlink components. Directory relocation,
mount replacement and mutations between MCP validation and scan startup remain
outside the allowed-root guarantee. Legacy explicitly lacks race protection.
