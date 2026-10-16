# Space

A local-only desktop disk explorer, being built in Rust with GPUI.

## Current status: disk dashboard redesign

Start on **All disks** and choose **Scan all disks** to scan the currently discovered
local targets, one at a time. Each disk has its own cached treemap, capacity, scan
status, and explicit Explore action. Scan folder handles a chosen local folder.
The focused explorer keeps navigation, filters, aliases, and cached diagnostics
available without stacking every control above the map. Space stores
immutable results in SQLite and reopens them without rescanning, including when
the source is disconnected. Browse size-sorted folders, subtree or root largest files,
bounded treemaps, and explicit name/path/extension/size/time filters. Mounted-target
selection, hardlink alias selection, and confirmed cache-only removal are included.
Inspect cached diagnostics,
switch logical/allocated metrics, cancel scans, copy paths, and reveal validated
live paths. A previous snapshot stays available while a new scan runs.

Changes compares compatible cached observations with before/after values,
coverage-aware changes, and growth/decrease rankings. The preceding Changes
revision `75b76ab` passed both complete five-job native CI runs; the dashboard
redesign requires its own exact-revision validation.

This is an early implementation, not a disk-explorer release. Root relinking and
complete release validation remain unfinished. The CI matrix covers Linux, Windows and both macOS architectures. See [slice scope and limitations](docs/scan-slice.md).

The G0 bootstrap passed all four native CI targets. Current acceptance uses native
CI as approved on 2026-10-16; hardware checks remain deferred. Build/CLI smoke
results are not native GUI evidence. Every new source revision needs its own CI.

- [Build and development commands](docs/development.md)
- [Platform acceptance and blockers](docs/platform-support.md)
- [Dependencies and license review](docs/dependencies.md)
- [Dashboard design and acceptance](.agents/plans/disk-dashboard-redesign/00-overview.md)
- [Approved implementation plan](.agents/plans/desktop-disk-explorer/00-overview.md)

Space's own code remains MIT licensed. Executable distribution is gated on
complete, reviewed third-party license/NOTICE texts; CI uploads text evidence only.
