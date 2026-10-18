# 04 — Shell reset and removals

Goal: replace flag routing with a `Screen` enum, introduce theme tokens, shrink `Space` to the state the two screens need, and delete the dropped surfaces completely so clippy's `-D warnings` proves nothing is left dangling.

Prerequisite state: WP1 in the same PR (so `tree::Tree` exists for `CardData.preview`). This package must leave the workspace green: Home and Explore are rendered by minimal versions of `home.rs`/`explore.rs`/`explore_controller.rs`/`treemap_view.rs` that already consume `tree.rs` and `backend::tree` (so nothing is dead), which WP3/WP4 then complete.

## Repository evidence

- `main.rs:1-18` module list; `:34-68` `actions!` list; `:93-152` `Space` fields; `:154-174` `Drop`; `:239-328` `Space::new` (creates seven `TextInput`s and subscriptions); `:1168-1213` `Render` registering ~25 actions; `:1215-1266` `main` (window 1280×800, `bind_keys` with `ctrl-*` on every platform, Linux display check, `--version`).
- `text_input.rs:699-735 init(cx)` chooses `cmd` on macOS and `ctrl` elsewhere via `cfg(target_os)`; this is the precedent for platform-correct modifiers.
- Cross-module uses of the dropped surfaces: `dashboard_render.rs:29,61,74`; `dashboard_transition_tests.rs:16,20,67,421`; `main.rs:2-4,13,17,95,99,136,244,248,296,320,433,515,522,577,706,778,1226`; `backend.rs:2-3` (`pub mod comparison`) and `:624` (comparison test uses).
- Backend items that only serve dropped surfaces: `comparison` module (`comparison_backend.rs`), `aliases`/`Aliases` (`backend.rs:298-327`), `inspect_cache`/`CacheRow`/`Inspection` (`:508-552`), `reload`/`Reloaded` (`:553-575`), `remove_cached` (`:576`). `filter`/`Filtered` (`:188-214`) stays for the WP4 search session.
- Store crate `crates/space-store/src/comparison*.rs` is library code with its own tests; it is not touched. Only the app's use of it is removed.
- Colors currently hard-coded across modules: `0x111b24`, `0x12212c`, `0x182631`, `0x1b2935`, `0x20313e`, `0x233441`, `0x285166`, `0x304352`, `0x315f78`, `0x31566c`, `0x344656`, `0x375267`, `0x386e86`, `0x405463`, `0x426478`, `0x6fafd0`, `0x7ba8bc`, `0x8093a3`, `0x90d5ff`, `0xa7b9c8`, `0xb3daf4`, `0xb5d9c4`, `0xc3e8ff`, `0xc4d8e6`, `0xe6edf3`, `0xe7eef5`, `0xf0f5fa`, `0xf2cf91`, `0xf2f6fa` (from `treemap.rs`, `dashboard.rs`, `explorer.rs`, `dashboard_render.rs`, `main.rs:207-222,1209-1210`).

## Change surface

### Deletions (whole files)

`crates/space-app/src/`: `changes_controller.rs`, `changes_panel.rs`, `comparison_backend.rs`, `cache_management.rs`, `filters.rs`, `explorer.rs`, `dashboard.rs`, `dashboard_render.rs`, `treemap.rs` (replaced by `tree.rs` + `treemap_view.rs`), `dashboard_transition_tests.rs` (replaced by `journey_tests.rs` in 05; its fixture builder moves to `test_fixture.rs` first).

### `backend.rs` trims

Remove `pub mod comparison` and its `#[path]` attribute, the `Backend.comparison_reader` field and its construction (`:23,116,136`), `aliases`, `Aliases`, `inspect_cache`, `CacheRow`, `Inspection`, `reload`, `Reloaded`, `remove_cached`, `View::Treemap` and `Queried.summary` (their only consumer was the old map; `query` keeps `Folders` and `Largest`), and any private helper that becomes unused. `dashboard::format_bytes` (`dashboard.rs:12`, used at `dashboard_controller.rs:815` and `main.rs:228`) is replaced by `tree::format_bytes`. Keep `open`, `open_root`, `open_scan`, `discovery`, `filter`, `Filtered`, `query`, `Query`, `Queried`, `View`, `scan`, `ScanRequest`, `Completed`, `Progress`, `native_path`, `validated_reveal_path`, `PAGE_SIZE`, `Snapshot`, `Opened`. Add `tree` (WP1). Rule: no `#[allow(dead_code)]` may be added; delete instead. Tests inside `backend.rs` that exercised the removed functions are deleted with them; tests that cover kept functions stay.

### `ticket.rs` (`new`, tiny)

Move `Ticket { card: CardId, scan: Option<ScanId>, query_epoch: u64 }` out of `dashboard.rs:19` into `crates/space-app/src/ticket.rs` so `home.rs`, `explore.rs`, and `dashboard_controller.rs` share it without a dependency on a presentation module.

### `theme.rs` (`new`)

```text
pub const BG, SURFACE, SURFACE_RAISED, BORDER, BORDER_STRONG, TEXT, TEXT_MUTED, TEXT_FAINT, ACCENT, ACCENT_TEXT, WARNING, SUCCESS, INFO, MAP_BG, OTHER_TILE: u32
pub const SPACE_1..SPACE_8: f32   // 4, 8, 12, 16, 20, 24, 32, 48
pub const RADIUS_1, RADIUS_2, RADIUS_3: f32   // 4, 6, 10
pub const TEXT_XS, TEXT_SM, TEXT_BASE, TEXT_LG, TEXT_XL, TEXT_2XL: f32   // 11, 13, 14, 16, 20, 28  (px via text_size)
pub enum TileState { Normal, Hovered, Selected }
pub fn tile_color(family: usize, depth: u8, is_directory: bool, state: TileState) -> gpui::Hsla
pub fn status_color(kind: StatusKind) -> u32   // StatusKind::{Progress, Warning, Good, Muted}
pub fn button(id, label, enabled, primary) -> Stateful<Div>      // moved from dashboard.rs:263 and main.rs:207, restyled
pub fn text_button(id, label, enabled) -> Stateful<Div>
pub fn pill(label, kind: StatusKind) -> Div
```

`tile_color` builds from eight base hues spaced around the wheel (families cycle modulo 8), saturation 0.42 for directories and 0.22 for files, lightness `0.30 + 0.07 * depth` capped at 0.58, hovered +0.06 lightness, selected uses the directory lightness plus the accent ring drawn by the view. Values are the implementer's call within these ranges; S1 in 05 enforces that no other module contains `rgb(0x`, `rgba(0x` or `hsla(` literals.

Fonts: keep the GPUI default system font; set `window.set_rem_size` only if the implementer finds the default rem makes `text_xs` smaller than 11 px on macOS (verify in M1; optional).

### `main.rs` (rewritten)

```text
mod backend; mod dashboard_controller; mod deferred; mod disk_model; mod explore; mod explore_controller; mod home; mod intent;
mod paths; mod probe; mod scan_batch; mod text_input; mod theme; mod ticket; mod tree; mod treemap_view;
#[cfg(test)] mod journey_tests; #[cfg(test)] mod test_fixture;

pub(crate) enum Screen { Home, Explore }
struct Space {
    screen: Screen,
    disks: dashboard_controller::State,
    explore: explore_controller::State,
    executor: gpui::BackgroundExecutor,
    backend: Option<deferred::Deferred<Backend>>,
    cache_directory: Option<PathBuf>,
    roots: Vec<RootRecord>,
    volumes: Option<space_scan::VolumeListing>,
    running: Option<Running>,
    generation: u64,
    metric: Metric,             // shared by Home previews and Explore (read at dashboard_controller.rs:394,448-478,874)
    search_input: gpui::Entity<text_input::TextInput>,
    search_subscription: gpui::Subscription,   // forwards TextInputEvent::Edited to explore_controller::search_changed
    picker_open: bool,
    intent: intent::Intent,
    focus: FocusHandle, list_focus: FocusHandle, scroll: UniformListScrollHandle,
    status: String,
}
```

Delete every filter/alias/cache/changes field and handler. Delete `maintenance_busy` together with its readers (`dashboard_controller.rs:220,403,484,541,674,870`; it was set only by the removed cache-removal flow, `main.rs:568-597`). `Running` and its `Drop` stay (`:71-90`). `Space::drop` keeps the batch `Shutdown` and drops any pending explore request and search session off-thread (the session is a `Deferred`, so its drop already retires on a worker).

Actions: replace the `actions!` list with `[ChooseFolder, Rescan, Cancel, CopyPath, RevealPath, NextRow, PreviousRow, OpenRow, GoUp, ZoomIn, ToggleList, ToggleMetric, NextPage, FirstPage, FocusSearch, FocusNext, FocusPrevious, CloseWindow]`. `Render` registers one `on_action` per action and routes by `self.screen`.

Key bindings in `main()`: define `const PRIMARY: &str = "cmd"` on macOS and `"ctrl"` elsewhere (same `cfg(target_os)` shape as `text_input.rs:718-724`), then bind `"{PRIMARY}-o"` ChooseFolder, `"{PRIMARY}-r"` Rescan, `escape` Cancel, `"{PRIMARY}-shift-c"` CopyPath, `"{PRIMARY}-shift-r"` RevealPath, `backspace` and `alt-up` GoUp (context `SpaceMap`), `enter` ZoomIn (`SpaceMap`), `down`/`up`/`enter` in `SpaceList`, `"{PRIMARY}-l"` ToggleList, `"{PRIMARY}-f"` FocusSearch, `tab`/`shift-tab` focus moves, `"{PRIMARY}-w"` CloseWindow. The map container carries `.key_context("SpaceMap")` and `.track_focus(&self.focus)`; the list keeps `SpaceList`. Backspace must not fire while the search input is focused: the text input has its own `SpaceTextInput` context and consumes keys first (existing behavior).

Focus: the map container has `.track_focus(&self.focus)`; `enter_scope_from_bookmark`, every scope change and every tile click focus it unless the search input is focused (03, test E8).

Window: keep 1280×800 default and the title; set `window_min_size: Some(size(px(900.), px(600.)))` on `WindowOptions` (`crates/gpui/src/platform.rs:2552,2610`) so the Explore layout never drops the list; keep the Linux display check and `--version`.

### `dashboard_controller.rs` trims

- Delete `explorer_model`, `explorer_event`, `remember_scope_page`/`saved_scope_page` (page cursors move to `explore_controller`), `model_from_summary`, and every reference to `changes`, `cache_visible`, `filter_*`, `rebuild_filters`, `maintenance_busy`. `open_card` ends by calling `self.enter_scope_from_bookmark(bookmark, cx)` (defined in 03; no `Window`, focus is deferred through `focus_pending`), which sets `screen = Screen::Explore`; it gains the lazy snapshot path from 02. `queue_visible_previews` is rewritten per 02. `publish_card` sets `explore.new_scan_available` when the published card is the explored one (03). `Bookmark` loses `view`/`subtree`/`cursor` and gains `list_mode`.
- `State` field audit: keep `registry`, `batch`, `selected`, `discovering`, `folder_preflight`, `preview_busy`, `initializing`, `notice`, `cards`, `preview_queue`, `preview_active`, `bookmarks`, `pending_bookmark`, `pending_card`, `pending_notice`, `revision`, `discovery_generation`, `folder_cancelled`; add `#[cfg(test)] discovery_override` (02). Delete `home` (replaced by `screen`), `page`, `pages`, `page_cursor`, `details_visible`, `list_visible`, `inspector_visible`, `advanced_filters`, `restore_selection`, `displayed_scope`, `explorer_notice` (their roles move to `explore_controller::State` or vanish with the removed surfaces). `CardData` gains `opening: bool` and `geometry` (02) and loses nothing else; `preview` becomes `Option<Arc<tree::Tree>>`.

### Docs touched by removal (content in 05)

`README.md` "Current status" paragraph, `docs/scan-slice.md` "Disk dashboard redesign" section, `docs/development.md` manual checklist bullets that mention Changes/filters/aliases.

## Behavior and invariants after this package

- Launch → Home renders (placeholder acceptable until WP3), discovery runs, scan-all works, Explore opens a scope and renders a placeholder (until WP4) while the controller already loads a `Tree`.
- No references to removed modules; `cargo clippy --workspace --all-targets --locked -- -D warnings` passes with no new `allow` attributes (gate G2; check S2 in 05 scans for `allow`/`expect` of `dead_code`/`unused` under `crates/space-app/src/`).
- Stored caches with comparison data remain readable; the store is untouched.

## Tests

- Existing pure tests in `scan_batch.rs`, `disk_model.rs`, `deferred.rs`, `intent.rs`, `backend.rs` (kept functions) still pass; `tree.rs` tests (WP1) pass.
- `journey_tests.rs` scaffold (fixture + `empty_space` builder for the new `Space` shape) compiles with one headless smoke journey (launch → two cards discovered → `ScanAll` → both `Ready` → `Explore(card 0)` → `screen == Explore` and `explore.tree.is_some()` after `settle()`) and the drawn smoke D0 (05).
- S1/S2 checks from 05 pass.
- Deleted files remain readable for porting tests with `git show 1688e30d100120a3297eca41bf5496cc9685997c:<path>`.

## Exclusions

- No restyle of `text_input.rs` beyond reading tokens from `theme.rs` for its border/background colors.
- `probe.rs` and `paths.rs` untouched.
