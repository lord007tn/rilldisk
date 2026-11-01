# Security

Do not include sensitive paths or scan reports in public issues. Report a
vulnerability through [GitHub private vulnerability reporting](https://github.com/lord007tn/rilldisk/security/advisories/new).

Phase one is a local metadata reader. It has no file mutation tools, network
listeners, telemetry, update service or embedded AI. MCP clients control any
downstream transmission of paths. Allowed roots are a convenience boundary, not
an operating-system sandbox. Do not expose the stdio server through an untrusted
bridge or run it elevated by default. Filenames are untrusted data.
