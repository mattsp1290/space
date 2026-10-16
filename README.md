# Space

A local-only desktop disk explorer, being built in Rust with GPUI.

## Current status: native feasibility spike (G0)

This is not yet a disk scanner. The first implementation tests a native window,
folder picker, clipboard/reveal, keyboard navigation, a virtualized 100,000-row
synthetic list, and cancelable background progress. It never enumerates the
selected folder or changes its contents. Native acceptance remains pending on
Windows, macOS, Linux X11 and Linux Wayland. Build/CLI smoke results do not count
as native GUI acceptance.

- [Build and development commands](docs/development.md)
- [Platform acceptance and blockers](docs/platform-support.md)
- [Dependencies and license review](docs/dependencies.md)
- [Approved implementation plan](.agents/plans/desktop-disk-explorer/00-overview.md)

The scanner, SQLite cache and browsing views wait for G0 acceptance. Space's own
code remains MIT licensed; third-party dependency notices must accompany packages.
