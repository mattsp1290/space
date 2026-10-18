# Developing Space

The current implementation gate is full four-target native CI success, under
the owner-approved 2026-10-16 [acceptance revision](platform-support.md). The first scan/cache slice is described in [scan-slice.md](scan-slice.md). No command below constitutes
proof of native UI behavior merely because it exits successfully.

## Prerequisites

Install Rust 1.98.1 through the official Rust distribution. `rust-toolchain.toml`
pins the compiler; rustfmt and clippy are required. Git and network access to the
locked dependency sources are needed for an initial build. Never paste credentials
into scripts or commit them. Source changes do not authorize release signing or
publication.

Linux candidate build host: Ubuntu 24.04 x86_64, a C/C++ compiler, clang/libclang,
CMake, pkg-config, Fontconfig/FreeType, Wayland, X11/XCB/xkbcommon development
libraries, Vulkan loader, OpenSSL, zstd and GLib development libraries. The precise
CI apt package list is in `.github/workflows/ci.yml`; it is a proposed narrowed
set until a successful native build confirms it. Do not claim these packages were
installed in the cloud shell. Desktop testing additionally requires a real X11
or Wayland session, a working renderer/driver, system fonts, session D-Bus,
desktop portal/backend and a file manager. Installing Xvfb alone does not establish
native GPU, picker, accessibility, or both-backend acceptance.

macOS: full Xcode with macOS and Metal components, selected with xcode-select;
verify `xcrun --find metal` and `xcrun --find metallib`. Command-line tools alone
may lack Metal tools. Xcode's license must already have been accepted by the
operator. CI uses macOS 15 separately for ARM and Intel and sets
`MACOSX_DEPLOYMENT_TARGET=15.0`. Check each runner's actual Xcode version. Install
CMake if a resolved native dependency requires it.

Windows: official Rust MSVC toolchain, Visual Studio C++ Build Tools and Windows
SDK, including `fxc.exe` and the resource compiler. Initialize a Developer shell
if tools are not discovered. Release GPUI shaders need fxc; debug success is not
release evidence. `GPUI_FXC_PATH` can point at the installed SDK compiler. Use
PowerShell 7 for packaging. CMake must be available if a resolved dependency
requires it. MSYS/MinGW builds are not a substitute for the declared MSVC target.

## Build and test

From the repository root:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --release --locked
cargo run -p space-app --locked
```

Run the last command only in an interactive native desktop for UI testing. A
headless environment may initialize without a visible window or fail to open it.
Review [dependencies](dependencies.md) before changing the shared GPUI revision.

CI builds explicit native targets. For example, on the Linux build baseline:

```sh
rustup target add x86_64-unknown-linux-gnu
cargo build --workspace --release --target x86_64-unknown-linux-gnu --locked
cargo metadata --format-version 1 --locked --filter-platform x86_64-unknown-linux-gnu > target/dependency-metadata.json
python3 scripts/dependency-inventory.py target/dependency-metadata.json target/dependency-inventory-x86_64-unknown-linux-gnu.json
bash scripts/package.sh x86_64-unknown-linux-gnu
```

Use `aarch64-apple-darwin` or `x86_64-apple-darwin` on the corresponding Mac,
including `MACOSX_DEPLOYMENT_TARGET=15.0` during compilation. On Windows build
`x86_64-pc-windows-msvc`, generate its inventory with the same arguments, then run:

```powershell
./scripts/package-windows.ps1 -Target x86_64-pc-windows-msvc
```

The scripts package already-built binaries; they do not fetch dependencies,
install software, sign, or publish. Archives land in `dist/` for local testing only. Do not distribute them while
third-party license/NOTICE review is pending. They contain the
binary/app bundle, project license, docs, dependency inventory and build identity.
The .app metadata is provisional for G0 and must be versioned before release.
Linux/macOS also include linked-library evidence. Build from a clean working tree
when producing review artifacts so the recorded Git revision identifies the code.

The workflow runs formatting, Clippy, tests, native release build, inventory,
packaging, and extracted-binary `--version` checks on all four native targets.
It keeps non-executable evidence artifacts for 14 days. `dist/` archives and
executables are explicitly excluded from artifact upload until a reviewed license
notice bundle and separate workflow change permit distribution. CI does not exercise dialogs, screen readers,
rendering or developer-checkout-free installation. `--version` intentionally exits
before starting the UI. Record interactive acceptance separately.

## Validation and stopping condition

Before moving beyond G0, all four native CI targets must pass formatting, Clippy,
unit tests, release build, package assembly and extracted CLI smoke. Report build
failures and observed GPU/accessibility defects honestly. Real-hardware checks in
`platform-support.md` are deferred and missing hardware is not a current
implementation blocker. Never silently remove a platform or switch frameworks
to produce a green build.

Local checks include shell syntax, workflow YAML parsing, the four-target matrix,
and a dependency-inventory fixture. Linux archive assembly was tested with a
synthetic C/ELF fixture in a temporary directory; this does not validate Space.
G0 native CI passed all four targets at source commit
`1fb9e0a0a3e7eef710411bcd18ea71f2ff93ec84` and documentation head
`c93ff354b6847871cf1830741266a7cbfead509a`. Current scanner/store/core and
headless backend tests ran locally during that original Linux bootstrap; its full
local GPUI build was blocked by unavailable Fontconfig development metadata.
The scan-and-treemap redesign additionally builds, packages and runs on the
implementer's Apple Silicon macOS desktop; see its committed acceptance evidence.
New slice commits require fresh native CI validation.

For eventual distribution, complete the license notice bundle and signing gates.
See the platform document for required macOS Developer ID signing/notarization and
Windows signing status. These CI artifacts are development evidence, not releases.

## Scan-and-treemap desktop acceptance

Record M1–M7 with screenshots and timing in the PR; the GPUI in-process tests
provide drawn-element evidence, while these checks use the native desktop.
The redesign's [execution record](../.agents/evidence/scan-and-treemap-redesign/verification.md)
links its desktop captures, review checkpoints and final PR results.

- M1: Home at 1280×800, one primary action, capacity cards and readable ≥11 px text.
- M2: Scan all once; queued/scanning status, rising entry counts, automatic maps;
  stop waits for acknowledgement and retains prior publications.
- M3: Explore, hover tooltip, select, header zoom, Backspace, breadcrumbs, side
  list sync, Largest paths, file-name search and clear.
- M4: Sweep the map for ten seconds at 1280×800 and Retina fullscreen; no visible
  stutter. Reduce the node budget and retest if needed.
- M5: Resize to the 900×600 minimum; 260 px list, collapsed breadcrumbs, no overflow.
- M6: Linux/Windows desktop checks remain deferred per platform-support.md;
  all four native build/test jobs must pass on the exact PR head.
- M7: On a real multi-million-entry main-disk scan, time complete Explore tree
  queries at the root and largest depth-1 folder. Each must take ≤300 ms; lower
  tree budgets and record them if necessary. Remove temporary instrumentation.
