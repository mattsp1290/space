# Space

A local-only desktop disk explorer, being built in Rust with GPUI.

## Current status: scan-and-treemap redesign

Space opens on **Home**. Choose **Scan all disks** once to scan discovered local
sources serially, with progress and a nested mini map on each disk card. **Scan a
folder…** handles a chosen local folder. Saved scans remain available offline.

**Explore** puts a nested, squarified treemap below two thin bars. Click a tile to
select it, click a folder header (or double-click its body) to zoom, and press
Backspace or Alt+Up to go up. Breadcrumbs jump to an ancestor. The optional side
list shows children or the largest files under the current scope. Name search
matches **file names**, retaining their ancestor folders; it does not match folder
names. Logical and allocated sizes, copy path, validated reveal, and rescan remain.
A finished rescan offers **Show** without replacing the displayed snapshot.

Changes, cache management/diagnostics, hardlink aliases, and the seven-field
filter panel have been removed from the app. Comparison remains available only
in the store library. To reset saved scans, quit Space and remove its cache
directory (`~/Library/Application Support/Space` on macOS, `$XDG_DATA_HOME/Space`
or `~/.local/share/Space` on Linux, `%LOCALAPPDATA%/Space` on Windows). Source files
are never deleted by Space. The SQLite schema is unchanged.

This is an early implementation, not a disk-explorer release. Root relinking and
complete release validation remain unfinished. The CI matrix covers Linux, Windows and both macOS architectures. See [slice scope and limitations](docs/scan-slice.md).

The G0 bootstrap passed all four native CI targets. Current acceptance uses native
CI as approved on 2026-10-16; hardware checks remain deferred. Build/CLI smoke
results are not native GUI evidence. Every new source revision needs its own CI.

- [Build and development commands](docs/development.md)
- [Platform acceptance and blockers](docs/platform-support.md)
- [Dependencies and license review](docs/dependencies.md)
- [Scan-and-treemap design and acceptance](.agents/plans/scan-and-treemap-redesign/00-overview.md)
- [Historical dashboard design and acceptance](.agents/plans/disk-dashboard-redesign/00-overview.md)
- [Approved implementation plan](.agents/plans/desktop-disk-explorer/00-overview.md)

Space's own code remains MIT licensed. Executable distribution is gated on
complete, reviewed third-party license/NOTICE texts; CI uploads text evidence only.
