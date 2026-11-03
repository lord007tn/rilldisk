# Proposal: showing a folder or file in the file manager

Status: implemented, November 3, 2026, in the desktop app (`reveal.rs`, the
context menus, the Explore panel footer and the cleanup cards).

AGENTS.md asks for a scoped proposal before RillDisk starts another program. This
document is that scope. Anything not described here is out of scope.

## Problem

RillDisk finds the folder or file that takes the space, but you then have to type
or paste its path into Explorer to do anything with it. Every disk analyzer offers
a direct "Show in Explorer".

## Scope

1. **Where.** The desktop app only. The right-click menu of every folder or file
   (map, rings, Explore panel, Largest files, search results, Clean up), of the
   sidebar's drives and places, of the home screen's drives and of the scan root
   in the header. Also a folder button in the Explore panel footer and on the hover
   actions of a cleanup card. The CLI and the MCP server never start a file manager.
2. **What starts.** A fixed program, never a shell and never a path from the scan:
   - Windows: `%SystemRoot%\explorer.exe`. A folder is passed as `"C:\path"` and
     opens in a window; a file is passed as `/select,"C:\path"` and is selected in
     its folder. Windows names cannot contain `"`, so the quoting cannot be broken.
     A drive root is passed unquoted (`D:\`), because Explorer reads `"D:\"` as an
     escaped quote.
   - macOS: `/usr/bin/open -R <path>` for files and folders alike. Finder reveals
     the item. `-R` is used for folders too, because opening a bundle folder
     (`.app`) would launch it.
   - Linux and others: `xdg-open <folder>`, where the folder is the item itself
     if it is a real directory, otherwise its parent. `xdg-open` therefore only
     ever receives a directory.
3. **Never a file.** "Folder" means `symlink_metadata(path).is_dir()` at the
   moment of the click, so a link to a folder is treated as a file and is selected,
   not followed. Only absolute paths are accepted.
4. **Nothing read, nothing changed.** RillDisk passes one path. It does not wait
   for, read from or write to the program. It reaps the process on a background
   thread so no zombie is left. Explorer's exit code is meaningless and ignored.
5. **Failures.** A missing path or a program that fails to start shows a short
   toast ("It is no longer there. Rescan to refresh"). Nothing is retried.
6. **Scripted captures** (`--capture`) never start a file manager. They only open
   and screenshot the menus.

## Out of scope

- Opening, previewing or running files ("Open" or "Open with").
- Terminals, shells or any command other than the three above.
- Any MCP tool or CLI flag that starts a program.

## Tests

`reveal::tests` checks the exact arguments for folders, files, drive roots,
`\\?\` paths and commas on Windows, `-R` for bundles on macOS, the folder-only
rule on Linux, and the refusal of relative and missing paths.
