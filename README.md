# Space

A local-only desktop disk explorer, being built in Rust with GPUI.

## Current status: native feasibility spike (G0)

This is not yet a disk scanner. The first implementation tests a native window,
folder picker, clipboard/reveal, keyboard navigation, a virtualized 100,000-row
synthetic list, and cancelable background progress. It never enumerates the
selected folder or changes its contents. The current implementation gate uses the full four-target native CI matrix,
as approved on 2026-10-16. Real-hardware verification on Windows, macOS, Linux
X11 and Wayland is deferred. Build/CLI smoke results are not native GUI evidence.

- [Build and development commands](docs/development.md)
- [Platform acceptance and blockers](docs/platform-support.md)
- [Dependencies and license review](docs/dependencies.md)
- [Approved implementation plan](.agents/plans/desktop-disk-explorer/00-overview.md)

The scanner, SQLite cache and browsing views follow successful G0 CI acceptance. Space's own
code remains MIT licensed; third-party dependency notices must accompany packages.
