# 05 — Verification

Goal: every success criterion in [00-overview.md](00-overview.md) maps to a named automated test or a named manual check, and the docs describe the shipped surfaces truthfully.

## Commands (unchanged from `.github/workflows/ci.yml` and `docs/development.md`)

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --release --locked
cargo run -p space-app --locked          # interactive desktop only
```

CI matrix stays: `ubuntu-24.04`, `windows-2022`, `macos-15` (arm64), `macos-15-intel`, plus the `windows-headless` fast job for core/scan/store. No workflow edits are part of this plan. Name the branch `feat/scan-and-treemap-redesign` so the `feat/**` push trigger applies.

## Static checks (new, `#[test]`s in `crates/space-app/src/theme.rs`)

- **S1 token discipline.** Read every `.rs` file under `crates/space-app/src/` except `theme.rs` (via `std::fs::read_dir` on `env!("CARGO_MANIFEST_DIR")`) and assert none contains `rgb(0x`, `rgba(0x` or `hsla(`.
- **S2 no suppressed dead code.** Assert no file under `crates/space-app/src/` except `theme.rs` contains `allow(dead_code)`, `allow(unused`, `expect(dead_code)` or `expect(unused`. Build the needles with `concat!("allow(", "dead_code)")` so the test file does not match itself.

## Test fixture (`crates/space-app/src/test_fixture.rs`, `new`, `#[cfg(test)]`)

Move the fixture builder from `dashboard_transition_tests.rs:112-124` and extend it: root `alpha` contains `a/b/c/` with 10–40 files at each level (sizes spread so each directory holds ≥ 2% of the root), plus the existing flat 105-file layout; root `bravo` keeps the flat layout. Both scan through the real `backend::open` + `backend::scan` as today (`:118-124`).

## Test matrix

| ID | Layer | Location | Proves criterion |
|---|---|---|---|
| T1–T7 | pure | `tree.rs`, `backend.rs` | 4, 5 |
| H1–H7 | model/controller | `home.rs`, `dashboard_controller.rs` | 1 |
| E1–E11 | model/controller | `explore.rs`, `explore_controller.rs` | 2, 3, 6, 7, 9 |
| S1–S2 | static | `theme.rs` | D6, G2 |
| D0–D7 | draw-enabled | `journey_tests.rs` | 2, 3, 6, 7 (rendered) |
| J1–J6 | journey (headless) | `journey_tests.rs` | 1, 2, 7, 9 |

### Journey harness (`crates/space-app/src/journey_tests.rs`, `new`)

Port `empty_space`, `Journey::new`, `settle`, `update`, `read`, `Drop` from `dashboard_transition_tests.rs:13-262` to the new `Space` shape. Keep `TestAppContext::single()`, the temp SQLite directory and the fixture scans. Discovery: the harness sets `disks.discovery_override` (02) to return a `VolumeListing` of `MountedVolume`s for the fixture directories (identity from `space_scan::inspect_root`) plus the cached `roots`, so `ScanAll` scans only fixtures and never the host's disks (F15). Two constructors: `Journey::headless()` calls `cx.skip_drawing()` as today; `Journey::drawn(size)` does not skip drawing, calls `cx.simulate_resize(window, size)` and uses the `VisualTestContext` returned by `add_window_view` for `simulate_mouse_move`, `simulate_click`, `simulate_keystrokes`, `debug_bounds` (`app/test_context.rs:347,924-1025`). **Frame pump:** the test platform draws only when asked (F17), so `Journey::frame()` runs `cx.update_window(window, |_, w, cx| { w.draw(cx).clear(cx); })` (pattern `app/test_app.rs:503-511`) and every drawn step is `settle(); frame();` before any `simulate_*` or `debug_bounds` call. Text uses the test platform's no-op text system, so bounds that depend on text width are approximate; D-tests assert on element bounds the layout fixes in px. **Gate G2 includes drawn smoke D0:** open Explore, `frame()`, `debug_bounds("explore-map")` is `Some`; `ToggleList`, `settle(); frame();`, the bounds changed. If D0 cannot pass on the pinned test platform, stop and report; do not silently downgrade D1–D7 to headless.

### Headless journeys

- **J1 one click scans everything.** Fresh cache, discovery seam returning two fixture volumes. Send `home::Event::ScanAll`; `settle()`. Discovery ran through the seam, both cards are `Status::Ready`, both have `preview: Some(tree)` with children, `screen == Home` throughout. Serial ordering is already proven by `scan_batch.rs` reducer tests (`:306-562`).
- **J2 drill in four actions.** After J1, `Explore(alpha)`; `settle()`; `ZoomInto(a)`, `ZoomInto(b)`, `ZoomInto(c)` with `settle()` between; crumbs length 4, `tree.root == c`, `query_generation` advanced exactly 4 since Explore. Then `dispatch_action(GoUp)` ×3 and assert selection restoration at each step. (The click-count claim is proven by D2.)
- **J3 background publication does not steal focus.** While exploring alpha, `ScanTargets(vec![bravo])` through `batch_event`; `settle()`; `screen == Explore`, displayed scan is alpha's, bravo's card is `Ready` with a new preview. Then rescan alpha itself via `Action::Rescan`; `settle()`; displayed scan unchanged, `new_scan_available == Some(new)`; `ShowNewScan` switches to it with one root crumb.
- **J4 stale events are ignored.** Capture an `explore::Event` with the current generation, `ZoomInto` to change it, deliver the old event: no change. Repeat with an old `Ticket` after `ChooseDisk(bravo)`. Repeat with a stale `Hover`.
- **J5 stop preserves the previous map.** With alpha cached, `ScanAll`, immediately `Stop`; `settle()`; alpha's card keeps its earlier preview; status is `Cancelled` or `Ready`; `primary()` returns `("Rescan all disks", ScanAll)` once idle.
- **J6 search scopes and clears.** Exploring alpha (fixture files named so that "zz" matches a known subset), `SearchChanged("zz")`; `advance_clock(300 ms)`; `settle()`; `search.active`, `matching == Some(n)` with `n` equal to the fixture's count, every **leaf** (file) node in `tree` and every file row in `rows` has a name containing "zz" (ancestor folders are present regardless of name, F16); `SearchClear`; `settle()`; tree equals the unfiltered tree again. Then start a search, immediately `ChooseDisk(bravo)`; `settle()`; bravo shows unfiltered and no session remains.

If timing control is needed, use `cx.executor().advance_clock(Duration)` (`BackgroundExecutor::advance_clock`, `crates/gpui/src/executor.rs:198`, test builds) followed by `run_until_parked`.

### Draw-enabled tests (`Journey::drawn(1280×800)`)

- **D1 map dominance (rendered).** Open Explore; `settle(); frame();`; `debug_bounds("explore-map")` area ≥ 62% of 1280×800 with the list visible; `ToggleList`; `settle(); frame();`; ≥ 75%.
- **D2 header click zooms, nested click selects.** `frame()`; read the cached `Geometry` from `explore.geometry`; pick a depth-1 directory tile with `header: Some(h)`; `simulate_click` at the header center → crumbs length 2. Then pick a depth-2 file tile and `simulate_click` at its center → `selected == that file`, not its parent.
- **D3 hover hits the tile under the cursor.** `simulate_mouse_move` to a depth-2 tile center → `hovered == Target::Node(that node)`; move into the gap between depth-1 tiles → `hovered` is the parent or `None`; the tooltip element (`debug_bounds("explore-tooltip")`) has bounds inside the map bounds.
- **D4 keyboard after mouse.** After D2's click, `simulate_keystrokes("backspace")` pops a crumb; `simulate_keystrokes("enter")` with a directory selected zooms.
- **D5 stale frame click is rejected.** `frame()`; capture a depth-1 header center; `ZoomInto` through the controller **without** `frame()`; `simulate_click` at the captured point (hits the stale frame's listener carrying the old generation) → crumbs unchanged beyond the one zoom; then `frame()` and click again → the new scope's tile is selected.
- **D6 dropdown occludes the map.** `ToggleDiskMenu`; `frame()`; `simulate_click` on a dropdown row that overlaps a tile → `ChooseDisk` ran and no tile `Select`/`Zoom` ran.
- **D7 two previews differ.** Home with two Ready cards at the same preview size: the per-card geometry caches hold different tile counts or targets.

## Manual desktop checklist (required before the PR leaves draft; macOS first, implementer's machine)

Run `cargo run -p space-app --locked` and record screenshots in the PR description under "Desktop evidence". Not CI evidence; stated as such.

- **M1 Home idle.** 1280×800. One primary button; hero + status line; cards with capacity bars; `text_xs` renders ≥ 11 px.
- **M2 Scan all.** One click. Cards show Queued/Scanning with a rising entries count (the existing 250 ms notify loop); finished cards show nested mini maps without a click. Stop mid-batch: "Stopping…" then Cancelled, earlier maps retained.
- **M3 Explore.** Click a Ready card. Map fills the area under two thin bars (compare against D1); hover tooltip within one frame; click selects; header click zooms; Backspace goes up and re-selects; crumbs jump; side list syncs; `Largest` lists files with paths; search for a name narrows the map and list and clearing restores them.
- **M4 Performance.** At 1280×800 and full screen on a Retina display, sweep the mouse across the map for ten seconds: no visible stutter. If stutter is visible, reduce `TreeBudget::EXPLORE.max_nodes` to 400 and re-check; record the final value in the PR.
- **M5 Narrow.** Resize to the 900×600 minimum: the list narrows to 260 px, crumbs collapse to first/…/last, nothing overflows; the window refuses to shrink below 900×600.
- **M6 Platform.** Linux and Windows desktop checks remain deferred per `docs/platform-support.md`; CI compiles and tests all four targets.
- **M7 Large scan timing.** On the implementer's main disk scan (multi-million entries), time a whole `backend::tree(EXPLORE)` call for the root and for the largest depth-1 folder (log via `eprintln!` behind a temporary local change, removed before commit): each call ≤ 300 ms total. If exceeded, lower `max_calls`/`child_limit` and record the values.

## Docs updates (same PR)

- `README.md`: replace "Current status: disk dashboard redesign" with "Current status: scan-and-treemap redesign" describing Home, Explore, the nested map, file-name search, removed surfaces (Changes, cache management, aliases, filter panel), that saved scans can no longer be removed from inside the app (delete the cache directory to reset), and that store comparison code remains library-only. Point plan links at `.agents/plans/scan-and-treemap-redesign/00-overview.md`; keep historical links.
- `docs/scan-slice.md`: replace "Disk dashboard redesign" with "Scan-and-treemap redesign": `TreeBudget` values, the T1 informational layout measures, which tests are drawn versus headless, removed UI surfaces, and test counts measured after implementation.
- `docs/development.md`: manual checklist bullets about filters, aliases and Changes removed; M1–M7 added.
- `docs/dependencies.md`: no new dependencies expected; record any that WP3/WP4 add.

## Acceptance mapping

| Criterion (00) | Evidence |
|---|---|
| 1 one action | J1, H1, M2 |
| 2 drill depth | T4 (fixture headers), D2 (real clicks), D5 (stale click rejected), J2, M3 |
| 3 map dominance | E2 (policy), D1 (rendered), M1 |
| 4 nested visibility | T4, M3 |
| 5 tile shape | T1 (hard `squarify` thresholds; informational layout measures quoted in the PR) |
| 6 hover feedback | E3, D3, D6, M3 |
| 7 safe staleness | J4, E1, E9, E10, D5 |
| 8 gates | CI four targets green on exact head; S1–S2; M1–M7 attached |
| 9 search (file names) | J6, E9, E11, M3 |
