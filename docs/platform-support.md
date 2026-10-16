# Native platform status

Space is at **G0, an unvalidated native bootstrap**, not a disk-explorer release.
The scanner, persistence, complete browsing workflow and Changes are later work.
A green compile or `--version` check is not a native-window, GPU or accessibility
pass. No platform has completed the acceptance matrix below.

## Candidate baseline

These are deliberately explicit build/test candidates, not established minimum
supported versions. Narrowing the approved OS/architecture scope requires an
explicit decision; a failing platform does not silently disappear.

| Candidate | Native CI target | Runtime acceptance needed |
| --- | --- | --- |
| Linux x86_64, Ubuntu 24.04 / glibc 2.39 | `x86_64-unknown-linux-gnu` | Actual X11 **and** Wayland sessions, renderer/driver, desktop portal, clipboard and file manager |
| Windows x86_64, Windows 11 | `x86_64-pc-windows-msvc`, built on Windows Server 2022 | Windows 11 interactive desktop, supported Direct3D renderer/driver, native dialogs and accessibility |
| macOS 15, Apple silicon | `aarch64-apple-darwin` | Metal-capable Mac with an interactive desktop and VoiceOver |
| macOS 15, Intel | `x86_64-apple-darwin` | Intel Mac with Metal and interactive desktop/VoiceOver |

The macOS deployment target and bundle minimum are provisionally 15.0. Separate
Intel and ARM archives avoid implying an untested universal binary. Windows ARM,
Linux ARM, Windows 10 and earlier macOS versions are not currently validated.
Exact minimum GPU API/feature levels, driver versions and OS minima remain G0
research/test gaps. Zed's published end-user requirements are not automatically
Space's minimum requirements.

Linux bundles do not include the system's GPU driver, compositor, desktop portal,
fonts or dynamic runtime libraries. Archive evidence includes `ldd` and ELF symbol
versions; validate those against the declared baseline before claiming portable
compatibility. This cloud shell is Debian 13/glibc 2.41, **not** the Linux packaging
baseline. Do not distribute a binary built here as proven Ubuntu 24.04-compatible.

The GPUI pin's Linux backends integrate desktop portals and accessibility via
AccessKit. Availability of those dependencies is not proof that Space's labels,
focus or folder picker work. Verify a functioning session bus and a suitable
`xdg-desktop-portal` backend in each desktop session. A headless fallback can start
without presenting a usable window.

## Native acceptance record

Each row is pending for Linux X11, Linux Wayland, Windows, macOS ARM and macOS
Intel. Record commit, archive SHA-256, OS version, architecture, GPU/driver,
renderer/backend, display scaling, tester, date, observed result and evidence.
Never replace pending with pass merely because CI is green.

- Extract the package on a machine without a developer checkout; start through
  the OS desktop. Confirm visible window, readable glyphs and virtualized rows.
- Choose a folder with keyboard-only navigation; test selection and cancellation.
- Tab/shortcut navigation has visible focus and reliable activation. Verify
  screen-reader labels, roles, focus and list announcements with VoiceOver,
  Narrator/NVDA, and Orca as appropriate. Missing capabilities block release unless
  explicitly accepted as a limitation.
- Background progress stays responsive; close during work, reopen, resize and
  change display scaling. Observe both 100% and HiDPI scaling.
- Copy and paste the selected path in another application. Reveal it in the
  native file manager. No shell-string interpolation or file-content opening.
- Confirm the actually selected Linux backend in both X11 and Wayland sessions;
  one session or XWayland alone is not proof of both.
- Exercise an unavailable/unsupported renderer in a controlled test environment;
  record actionable failure behavior rather than an unhandled panic or silent
  background process. Do not change a user's GPU/security configuration to test.
- Inspect platform permissions and native runtime dependencies on a clean account.

If an advertised OS cannot open the UI or select a folder, stop at G0 and report
the exact backend/dependency failure. Native GUI tests cannot be replaced with
headless Cargo tests. Later feature work must wait for this gate.

## Package and release boundaries

CI creates ephemeral, local-only Linux tar.gz, Windows portable zip, and macOS
.app zip archives to test package assembly. Executable archives are **not uploaded**
or distributed: the workflow uploads only dependency inventory/feature trees and
non-executable build/linkage evidence. Binary distribution remains blocked until
required third-party license and NOTICE texts are assembled and reviewed. Enabling
binary artifact upload requires a separate reviewed workflow change. No installer, signing, notarization, publishing or auto-update is
performed. `--version` runs from an extracted archive to test loading, but does
not test the desktop launch path or a clean machine's dependencies.

Windows archives are unsigned and the bootstrap does not select static CRT.
Treat the Microsoft Visual C++ runtime as a prerequisite until native dependency
inspection and a clean-machine test prove the exact DLL requirements. Validate
SmartScreen/install constraints as well. Do not bypass OS security warnings automatically.
macOS binaries may receive an ad-hoc linker signature; that is not Developer ID
signing or notarization. Gatekeeper may block downloaded development archives.

Before a release: complete native acceptance and dependency/asset license review;
provide authorized signing credentials; sign the final macOS bundle with Developer
ID and hardened runtime as appropriate, notarize and staple, verify on a clean Mac,
and then archive the verified bundle. A signed/notarized macOS release is required.
Windows signing support must be added using authorized credentials and its status
recorded. Do not fabricate certificates or treat local unsigned archives as releases.

## Evidence so far

Read-only cloud inspection on 2026-10-16 found no DISPLAY/WAYLAND_DISPLAY, X11
socket, /dev/dri, compositor, Vulkan ICD in standard locations, or checked
X11/Wayland/font development headers. Rust was initially absent and subsequently
provisioned by the bootstrap work. This is environment evidence, not native
validation. Bootstrap verification reported formatting/diff checks, three standalone probe
logic tests, and zero-test runs for the three placeholder crates. Full compilation
was blocked while fetching dependencies (HTTP CONNECT 403); an offline metadata
attempt still lacked accesskit_macos 0.26.3. System dependency provisioning could
not write to the read-only environment. No full native build, Space package or UI
pass has occurred. Record later build results with the actual CI runs.
