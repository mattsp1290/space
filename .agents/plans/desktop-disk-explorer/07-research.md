# Research and evidence limits
Researched 2026-10-16. Repository initial commit inspected locally and through GitHub. No application compiled or native drive scanned during planning. External documentation is current research, not validation of the future pinned dependency. Record resolved immutable dependency references at G0.

- GPUI current README: https://github.com/zed-industries/zed/tree/main/crates/gpui — current Windows/macOS/Linux platform routing, pre-1.0 warning and platform features.
- GPUI Apache manifest: https://github.com/zed-industries/zed/blob/main/crates/gpui/Cargo.toml
- Zed UI GPL manifest: https://github.com/zed-industries/zed/blob/main/crates/ui/Cargo.toml
- Zed installation requirements (not automatically Space's minima): https://zed.dev/docs/installation
- Windows development documentation has renderer guidance that differs from end-user requirements; verify pinned source: https://zed.dev/docs/development/windows
- Apple distribution: https://developer.apple.com/developer-id/
- GNU du apparent/allocated/hardlink semantics: https://www.gnu.org/software/coreutils/manual/html_node/du-invocation.html
- Windows identity/hardlinks: https://learn.microsoft.com/en-us/windows/win32/fileio/hard-links-and-junctions
- Windows allocation: https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_standard_info
- Windows symlink-following behavior: https://learn.microsoft.com/en-us/windows/win32/fileio/symbolic-link-effects-on-file-systems-functions
- Apple allocated metadata: https://developer.apple.com/documentation/foundation/urlresourcekey/totalfileallocatedsizekey
- Apple filesystem/shared storage: https://developer.apple.com/documentation/foundation/about-apple-file-system
- Windows cloud-file behavior: https://learn.microsoft.com/en-us/windows/win32/cfApi/build-a-cloud-file-sync-engine
- Watcher overflow requires reconciliation: https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-readdirectorychangesw
- Simple category precedent: https://apps.gnome.org/Baobab/
- Fast scanning/treemap precedent (vendor performance claims, not Space benchmarks): https://diskanalyzer.com/
- QDirStat broad feature precedent: https://github.com/shundhammer/qdirstat
- Snapshot comparison precedent: https://manuals.jam-software.de/treesize/EN/disk_usage_comparison.html and https://www.foldersizes.com/support/help/snapshot-comparison

Three independent feature advocates argued minimalist explorer, historical analysis, and platform correctness. Their conclusions informed scope; this is separate from the required implementation-plan reviews. No competitor's undocumented implementation or performance claims are assumed as Space requirements.
