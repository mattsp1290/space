# Space

A local-only desktop disk explorer, being built in Rust with GPUI.

## Current status: first scan/cache browsing slice

Choose a local folder and explicitly start a metadata-only scan. Space stores
immutable results in SQLite and reopens them without rescanning, including when
the source is disconnected. Browse paged folders or whole-scan largest files,
switch logical/allocated metrics, cancel scans, copy paths, and reveal validated
live paths. A previous snapshot stays available while a new scan runs.

This is an early implementation, not a disk-explorer release. Treemap, filters,
size-sorted folder views, subtree-largest, Changes, root management and complete
diagnostics remain unfinished. See [slice scope and limitations](docs/scan-slice.md).

The G0 bootstrap passed all four native CI targets. Current acceptance uses native
CI as approved on 2026-10-16; hardware checks remain deferred. Build/CLI smoke
results are not native GUI evidence. Every new source revision needs its own CI.

- [Build and development commands](docs/development.md)
- [Platform acceptance and blockers](docs/platform-support.md)
- [Dependencies and license review](docs/dependencies.md)
- [Approved implementation plan](.agents/plans/desktop-disk-explorer/00-overview.md)

Space's own code remains MIT licensed. Executable distribution is gated on
complete, reviewed third-party license/NOTICE texts; CI uploads text evidence only.
