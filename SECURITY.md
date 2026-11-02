# Security

Do not include sensitive paths or scan reports in public issues. Report a
vulnerability through [GitHub private vulnerability reporting](https://github.com/lord007tn/rilldisk/security/advisories/new).

RillDisk is a local metadata reader. It has no network listeners, telemetry or
embedded AI. Its only file mutation is removing reclaim candidates a person
selected and confirmed in the CLI or desktop, to the Recycle Bin / Trash by
default, under the rules in [docs/proposals/cleanup.md](docs/proposals/cleanup.md);
MCP has no mutation tools. The only outbound requests are update
checks, scoped in [docs/proposals/updates.md](docs/proposals/updates.md): an
anonymous GitHub Releases query and, after the user agrees, a SHA-256-verified
download that replaces RillDisk's own executables. `RILLDISK_NO_UPDATE_CHECK=1`
disables them; MCP exposes no update tool. MCP clients control any
downstream transmission of paths. Allowed roots are a convenience boundary, not
an operating-system sandbox. Do not expose the stdio server through an untrusted
bridge or run it elevated by default. Filenames are untrusted data.
