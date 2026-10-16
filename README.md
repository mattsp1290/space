# Space

A local-only desktop disk explorer, being built in Rust with GPUI.

## Current status: cached browsing and visualization slice

Choose a local folder and explicitly start a metadata-only scan. Space stores
immutable results in SQLite and reopens them without rescanning, including when
the source is disconnected. Browse size-sorted folders, subtree or root largest files,
bounded treemaps, and explicit name/path/extension/size/time filters. Mounted-target
selection, hardlink alias selection, and confirmed cache-only removal are included.
Inspect cached diagnostics,
switch logical/allocated metrics, cancel scans, copy paths, and reveal validated
live paths. A previous snapshot stays available while a new scan runs.

Changes compares compatible cached observations with before/after values,
coverage-aware changes, and growth/decrease rankings. This follow-on is awaiting
its own native CI validation.

This is an early implementation, not a disk-explorer release. Root relinking and
complete release validation remain unfinished. The preceding milestone revision
`2793d99` passed all five CI jobs across Linux, Windows and both macOS architectures. See [slice scope and limitations](docs/scan-slice.md).

The G0 bootstrap passed all four native CI targets. Current acceptance uses native
CI as approved on 2026-10-16; hardware checks remain deferred. Build/CLI smoke
results are not native GUI evidence. Every new source revision needs its own CI.

- [Build and development commands](docs/development.md)
- [Platform acceptance and blockers](docs/platform-support.md)
- [Dependencies and license review](docs/dependencies.md)
- [Approved implementation plan](.agents/plans/desktop-disk-explorer/00-overview.md)

Space's own code remains MIT licensed. Executable distribution is gated on
complete, reviewed third-party license/NOTICE texts; CI uploads text evidence only.
