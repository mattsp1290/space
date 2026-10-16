# Dependency and asset record

## Bootstrap pin

- Rust compiler/toolchain: `1.98.1`, recorded in `rust-toolchain.toml`.
- `gpui` and `gpui_platform`: common immutable Zed revision
  `9f63850f7affbd57772a4f99554d4fd333b50a6f`.
- Platform features: `font-kit`, `wayland`, `x11`, with the platform crate's default
  features disabled. The web multithreading default is not needed for this native
  application. Target-specific transitive defaults still require inspection.
- `Cargo.lock` is the reproducibility record. CI uses `--locked`; never regenerate
  the lock in CI to conceal an incompatible dependency graph.

The initial lock resolves 643 packages. Full metadata inventory has not yet been
generated: network dependency fetching failed and the offline cache lacks
accesskit_macos 0.26.3. Do not confuse lock generation with a compiled graph or
a completed license audit.

This is a **candidate pin**, not a claim of native validation. The direct GPUI
manifest reports 0.2.2 and gpui_platform reports 0.1.0 at this revision. Both
manifests declare Apache-2.0. macOS glyph rendering requires `font-kit`; Linux
requires a windowing backend and includes the corresponding renderer/text
system; Windows platform routing provides Win32/DirectWrite without an additional
font feature.

## License boundary

Space's existing MIT license remains unchanged. No Zed `ui` GPL component,
Zed application assets, icons, bundled proprietary fonts, or copied GPL controls
are intentionally imported. Direct use of Apache-2.0 GPUI does **not** clear every
transitive dependency or satisfy notice requirements by itself.

`python3 scripts/dependency-inventory.py METADATA_JSON OUTPUT_JSON` records the
resolved package name, version, source, declared license/license-file path and
repository. CI supplies full locked Cargo metadata filtered for each target.
The report includes build/dev dependencies and can over-approximate shipped code;
it is not an SPDX audit or a complete third-party notice bundle. Missing licenses,
license-file-only entries and non-registry sources require manual inspection.
Do not distribute executable archives while license clearance is incomplete.
The CI workflow deliberately uploads only non-executable evidence; generated
archives remain ephemeral inside the runner. A separate reviewed change may enable
binary uploads only after the required license/NOTICE bundle is included.

Before release:
1. Resolve the lock and inspect target feature trees and inventory for all four
   targets; inspect the actual license and NOTICE files, including vendored code.
2. Separate linked/runtime code, build tools and unused target dependencies.
   Verify compatibility and attribution/source-offer obligations where applicable.
3. Include required third-party license/NOTICE texts with each package. The
   machine-readable inventory alone is insufficient.
4. Inventory all shipped assets and font licenses. System-rendered fonts are not
   bundled assets. Any future GPL addition requires an explicit compatibility
   review and coordinated LICENSE/README/notice changes.
5. Record resolved findings and reviewers here. The full transitive and asset
   review is currently **pending**, and blocks a public release.

## Pinned primary sources

- [GPUI manifest](https://github.com/zed-industries/zed/blob/9f63850f7affbd57772a4f99554d4fd333b50a6f/crates/gpui/Cargo.toml)
- [Platform manifest](https://github.com/zed-industries/zed/blob/9f63850f7affbd57772a4f99554d4fd333b50a6f/crates/gpui_platform/Cargo.toml)
- [Linux backend manifest](https://github.com/zed-industries/zed/blob/9f63850f7affbd57772a4f99554d4fd333b50a6f/crates/gpui_linux/Cargo.toml)
- [macOS backend manifest](https://github.com/zed-industries/zed/blob/9f63850f7affbd57772a4f99554d4fd333b50a6f/crates/gpui_macos/Cargo.toml)
- [Upstream Linux build prerequisites](https://github.com/zed-industries/zed/blob/9f63850f7affbd57772a4f99554d4fd333b50a6f/script/linux)

The upstream Linux script installs dependencies for the entire editor. Space's CI
uses a narrower candidate set and must validate it in an actual build. Do not run
upstream provisioning scripts blindly or inherit unrelated server/database needs.
