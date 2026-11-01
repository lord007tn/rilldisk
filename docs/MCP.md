# MCP integration

The command `rilldisk mcp --root C:\Data` starts a stdio server. Repeat `--root`
for more allowed directories. Configure the executable as a local MCP server in
your client; use an absolute executable path. There is no shell wrapper and no
network server. Stdout contains only protocol messages.

1. Call `allowed_roots` to learn the configured scope.
2. Call `start_scan` with `{"path":"C:\\Data"}`. Keep the returned `scan_id`.
3. Poll `scan_status` with that ID. Status is `running`, `complete`, `partial`,
   `cancelled` or `failed`. Completed results contain a summary; failure contains
   an error. A partial or cancelled result remains queryable.
4. Call `query_entries` with the ID, optionally `parent_id`, `query`, `files_only`,
   `offset` and `limit`. Root entry ID is 0. Results are ordered by logical size
   descending and entry ID ascending. `next_offset: null` ends pagination. Default
   page size is 50, maximum 500.
5. Call `cancel_scan` to request cooperative cancellation. Poll until it stops.

Only one scan/snapshot is retained. A new scan expires the previous completed ID;
queries with that ID fail rather than returning another scan's data. Queries run
against immutable in-memory results, not additional live reads. MCP currently
does not export full snapshots or expose destructive operations.
Two queries may run concurrently; a capacity error means the client should retry
after an outstanding query finishes.

Scan roots are canonicalized, and paths outside them are rejected, including a
symlink root pointing outside the allowed directory. This is not a security
sandbox for adversarial filesystem mutation. The scanner skips discovered
reparse entries; it does not intentionally cross junctions or hydrate cloud files.

Local tool results include full filenames and paths. Your AI client controls
whether these results go to a model provider. Filenames may contain instructions,
control text, or sensitive names; treat them strictly as data. A tool response is
not authorization to delete anything.

For machine validation, `python scripts/test_mcp.py <executable>` launches a real
subprocess and exercises protocol negotiation, discovery, root confinement,
background scans, progress, pagination, Unicode, cancellation and input validation.
