# 03 — Explore screen

Goal: the treemap is the screen. Two thin bars frame a nested, pixel-accurate squarified map; hovering explains a tile, clicking selects it, clicking a folder header zooms in, Backspace goes up. A collapsible side list and a details strip support the map without competing with it. A single name search scopes the map and list.

Prerequisite state: WP2 (shell reset, which includes WP1) complete on the branch; WP3 may proceed in parallel. `Space.screen == Screen::Explore` renders this module.

## Repository evidence

- Current explorer chrome `crates/space-app/src/explorer.rs`: `Model` (`:30`), `Action` (`:59`), `allows` (`:111`), `layout(width, height, inspector_visible) -> Layout` with breakpoints 920/1200 (`:159-180`), `render` (`:249`). Current explorer body `dashboard_render.rs:54 explorer_content`, `render_entry_list` (`:275`, `uniform_list` with `Role::ListBox`, `key_context("SpaceList")`, `NextRow`/`PreviousRow`/`OpenRow` actions), `render_inspector` (`:361`).
- Navigation and filter state on `Space` (`main.rs:93-152`) and handlers: `chart_event` (`:919`), `request_query` (`:945`), `start_query` (`:985`), `open_row` (`:1106`), `up` (`:1118`), `path_action(reveal)` (`:1131-1167`, uses `backend::validated_reveal_path`, `cx.reveal_path(&path)`, `cx.write_to_clipboard(ClipboardItem::new_string(..))`), `move_selection` (`:1095`), `next_page`/`first_page` (`:1084-1094`), `metric` (`:1068`), `toggle_subtree` (`:915`, Largest honored `subtree_only`, default `true` at `:313`). Filter session flow: `rebuild_filters`/`start_filter`/`cancel_filter` (`:731-900`), `FilterSession = Deferred<backend::Filtered>` (`:92`) retired off the UI thread, `commit_pending_snapshot` (`:675`) called from inside `start_filter`, `filter_epoch`/`filter_pending`/`filter_cancel` fields. The old journey `superseded_filter_callback_cannot_commit_an_older_navigation` (`dashboard_transition_tests.rs:454`) covers staleness of that flow.
- Bookmarks per card: `Bookmark { scan, crumbs, selected, view, subtree, cursor }`, `stage_navigation`/`cancel_navigation`/`commit_navigation` (`dashboard_controller.rs:23,107,112,117`); `remember_explorer` (`:641`); `open_card` (`:668`) builds the initial bookmark, stages it, sets `pending_snapshot`, and currently ends with `rebuild_filters`; `restore_query_selection` (`:1043-1077`); `rescan_selected_disk` (`:619`) rescans the explored card; `publish_card` (`:389`) updates card data on publication without touching the explorer.
- Lists: `backend::query` with `View::Folders` returns `children_sorted` pages of `PAGE_SIZE = 100`; `View::Largest` returns `largest_in(scan, subtree, ..)` plus `display_paths` (`backend.rs:221-280`). `backend::filter(backend, scan, metric, &QueryFilter, &Cancellation) -> Result<Arc<Filtered>>` (`:193`); `QueryFilter { name: Option<String>, .. }` (`crates/space-store/src/filters.rs:61`).
- GPUI at the pinned revision: `container_query(render: FnOnce(Size<Pixels>, &mut Window, &mut App) -> E)` passes only the size (`elements/container_query.rs:11`); `canvas(prepaint, paint)` receives `Bounds<Pixels>` (`elements/canvas.rs:10`); interactive listeners are `Fn(&Event, &mut Window, &mut App)` (`div.rs:591,675,1006`), so a view method is reached through `cx.listener` outside a `'static` closure or through a captured `WeakEntity<T>` inside one; hit-testing marks every hitbox under the pointer as hovered and click listeners of all of them run in bubble order unless one calls `App::stop_propagation` (`window.rs:1173-1195`, `div.rs:2937-3075`, `app.rs:2723`); `on_hover(|hovered: &bool, ..|)` reports enter/leave (`div.rs:1661`); `on_mouse_down_out` is click-outside, not leave; `MouseMoveEvent.position` is window coordinates (`interactive.rs:494-496`); `Window::viewport_size` (`window.rs:2941`); `ClickEvent::click_count` (`interactive.rs:424`); `uniform_list` (existing use); `Styled::text_ellipsis_middle` (`styled.rs:105`). Test support: `VisualTestContext::simulate_mouse_move`, `simulate_click`, `simulate_resize`, `simulate_keystrokes`, `debug_bounds(selector)` (`app/test_context.rs:924-1025`); `skip_drawing` is opt-in (`:196`).

## Change surface

- `crates/space-app/src/explore.rs` (`new`): presentation model, events, layout policy, render. Replaces `explorer.rs` and the explorer half of `dashboard_render.rs`.
- `crates/space-app/src/treemap_view.rs` (`new`): renders a `tree::Tree` with `tree::layout` inside `container_query`. Used by Explore (interactive) and Home previews (non-interactive).
- `crates/space-app/src/explore_controller.rs` (`new`): `impl Space` for scope loading, selection, hover, zoom/up, list paging, largest toggle, metric toggle, path actions, rescan outcome, search session. Replaces the navigation/query/filter half of `main.rs` (`:675-1167`).
- `crates/space-app/src/dashboard_controller.rs` (kept): `open_card` stays the entry point; its tail becomes `self.enter_scope_from_bookmark(bookmark, cx)` (defined below) instead of `rebuild_filters`. `explorer_model`/`explorer_event` (`:885,:983`) are deleted. `publish_card` additionally sets `explore.new_scan_available = Some(scan)` when the published card is the explored card.

### `explore.rs` public surface (proposed)

```text
pub struct DiskChoice { ticket: Ticket, name: String, status_short: String, connected: bool, selected: bool }
pub struct Crumb { node: NodeId, label: String, bytes: u128 }
pub enum CrumbSlot { Crumb(usize), Ellipsis }
pub fn visible_crumbs(crumbs: &[Crumb]) -> Vec<CrumbSlot>   // all when len <= 5; else first, Ellipsis, last three
pub struct ListRow { node: NodeId, label: String, bytes: u128, percent: String, is_directory: bool, path: Option<String> /* largest only */ }
pub enum ListMode { Children, Largest }
pub struct Selection { node: NodeId, label: String, bytes: u128, percent: String, is_directory: bool, path: Option<String>, unknown_count: u64 }
pub struct SearchView { text: String, active: bool, busy: bool, matching: Option<u64>, error: Option<String> }
pub struct Model { ticket: Option<Ticket>, generation: u64, disks: Vec<DiskChoice>, disk_menu_open: bool, name: String, crumbs: Vec<Crumb>,
                   scope_total: u128, metric: Metric, metric_label: &'static str, busy: bool, stopping: bool, offline: bool, partial: bool,
                   tree: Option<Arc<tree::Tree>>, tree_loading: bool, selected: Option<NodeId>, hovered: Option<tree::Target>,
                   list_visible: bool, list_mode: ListMode, rows: Vec<ListRow>, list_selected: Option<usize>, has_next_page: bool,
                   selection: Option<Selection>, status: Option<String>, new_scan_available: bool, search: SearchView }
pub enum Action { Home, ChooseDisk(Ticket), ToggleDiskMenu, Rescan, Stop, ShowNewScan, Up, Crumb(NodeId), ZoomInto(NodeId), Select(Option<NodeId>),
                  Hover(Option<tree::Target>), OpenOther(NodeId /* parent */), ListMode(ListMode), ToggleList, ListSelect(usize), ListOpen(usize),
                  NextPage, FirstPage, ToggleMetric, Copy, Reveal, SearchChanged(String), SearchClear }
pub struct Event { ticket: Option<Ticket>, generation: u64, action: Action }
impl Model { pub fn event(&self, Action) -> Event; pub fn allows(&self, Event) -> bool; }
pub struct Layout { top_bar: f32, banner: f32 /* 0 or 28 */, crumb_bar: f32, details: f32, list_width: Option<f32>, map: tree::Rect }
pub fn layout(width: f32, height: f32, list_visible: bool, banner_visible: bool) -> Layout
pub fn render<T: 'static>(model: &Model, layout: Layout, search_input: &Entity<text_input::TextInput>, cx: &mut Context<T>, on_event: fn(&mut T, Event, &mut Window, &mut Context<T>)) -> Div
```

`allows` rejects an event whose `ticket` or `generation` differs from the model's, except `Home`, `Stop`, `Hover(None)` and `SearchChanged(_)` (the search input is debounced by the controller and carries its own epoch). `ZoomInto(n)` is allowed when `n` is a directory present in `tree` **or** in `rows` (list rows beyond the tree budget are zoomable). `Up` is disabled at the root. `Reveal` is disabled when `offline`. `OpenOther(p)` is allowed only when the tree node `p` (or the root) has `other_count > 0`. `ShowNewScan` is allowed only when `new_scan_available`.

### Layout contract

```text
┌ top bar 44 px ──────────────────────────────────────────────────────────────┐
│ ← Disks   Macintosh HD ▾   [ search names…      ]   Logical ▾  [≡]  [Rescan]│
├ crumb bar 36 px ────────────────────────────────────────────────────────────┤
│ ↑  Macintosh HD 588 GiB ›  Users 402 GiB ›  matt 398 GiB      [Children|Largest] │
├─────────────────────────────────────────────────────────┬ list 300 px ───────┤
│                                                         │ Library   180 GiB ▓▓▓▓ │
│   nested squarified map, fills remaining area           │ Movies     96 GiB ▓▓   │
│   folder tiles have an 18 px header strip: name · size  │ …                      │
│   hovering shows a tooltip; click selects; header zooms │ [Next page]            │
├ details strip 40 px ────────────────────────────────────┴────────────────────┤
│ 📁 Library · 180 GiB · 45% of matt · /Users/matt/Library   [Zoom in] [Reveal] [Copy path] │
└──────────────────────────────────────────────────────────────────────────────┘
```

- `layout`: `top_bar = 44`, `banner = 28` when `banner_visible` else `0`, `crumb_bar = 36`, `details = 40`, `list_width = Some(300)` when `list_visible && width >= 1100`, `Some(260)` when `list_visible` otherwise, `None` when hidden. The window has a minimum size of 900×600 (`WindowOptions.window_min_size`, 04), so the list is always reachable. `map = Rect { x: 8, y: 88 + banner, width: width − list_width.unwrap_or(0) − 16, height: height − 136 − banner }` (8 px map padding). At 1280×800 with the list and no banner: 964×664 = 640,096 px² = 62.5% of the window; list hidden: 1264×664 = 82.0%. The banner is transient and excluded from criterion 3.
- **`render` must size the bars, the list column and the details strip from `Layout` with explicit `px(..)` heights/widths and `flex_shrink_0`, and give the map container `.id("explore-map")` with `.debug_selector(|| "explore-map".into())` (signature `impl FnOnce() -> String`, `div.rs:885`; a no-op outside test builds) so the draw-enabled test D1 measures the real map bounds. The tooltip gets `.debug_selector(|| "explore-tooltip".into())`.** The bars never wrap: overflowing content is clipped with `text_ellipsis`, not wrapped.
- Top bar, left to right: `← Disks` (`Home`); disk switcher button "{name} ▾" (`ToggleDiskMenu`) with an absolutely positioned dropdown of `DiskChoice`s (`ChooseDisk`) that calls `.occlude()` (`div.rs:1250`) so clicks and hover do not fall through to the map, closed by choosing, by `Home`, by Escape, or by `on_mouse_down_out`; the search field (`text_input::TextInput`, placeholder "Search names in this disk", width 240, `cmd/ctrl-f` focuses it, Escape in the field clears it when non-empty) with a small "×" (`SearchClear`) and a matching-count caption when active; metric toggle (`ToggleMetric`); list toggle (`ToggleList`); `Rescan` / `Stop` / `Stopping…` at the far right. When `new_scan_available`, a slim banner sits under the top bar: "A newer scan of {name} finished · Show" (`ShowNewScan`).
- Crumb bar: `↑` (`Up`); crumb chips "{label} {bytes}" (`Crumb(node)`) via `visible_crumbs`; the last crumb is static text. Right: segmented `Children | Largest` (`ListMode`), rendered only when `list_visible`.
- Map: `treemap_view::render(..)`, interactive. Nothing below it inside the map column.
- Side list (when visible): `uniform_list` of `rows`, `Role::ListBox`, row height 32, "{glyph} {label}" left and bytes right with a thin bar (percent of `scope_total`), selected row highlighted; `on_click` → `ListSelect(i)`; double-click or Enter → `ListOpen(i)` (directories zoom, files select). `NextPage`/`FirstPage` footer when `has_next_page`. In `Largest` mode rows also show the relative path (`row.path`), and `ListOpen` on a file selects only. **Largest means the current scope's subtree** (the old `subtree_only = true` default); the segmented control's tooltip says "Largest files under {scope}".
- Details strip: when `selection` is `Some`: glyph, label (ellipsis), bytes, percent of scope, path (`text_ellipsis_middle`), then `Zoom in` (directories only, `ZoomInto`), `Reveal`, `Copy path`. When `None`: "{scope label} · {scope_total} · {unknown_children} unknown sizes" plus the hint "Click to select · click a folder's name to zoom in · Backspace goes up". `partial` adds an amber "Some locations could not be read" badge; `offline` adds "Offline · browsing a saved scan"; `search.active` adds "{matching} files match “{text}”".
- Search semantics: the store matches **file names only** and keeps the ancestor folders of matches (F16); the caption reads "{matching} files match “{text}”". Folder names are not searched; the docs say so.
- Tooltip: rendered by `treemap_view` as an absolutely positioned child of the map container, **anchored to the hovered tile's rect** (below the tile's top-left corner, offset 8 px, clamped inside the map bounds), not to the mouse point. Content: label, bytes, percent of scope, "folder · {k} shown{ + n more}" or "file"; for `Target::Other`: "Other · {bytes} · {count} more items". Because the anchor is the tile, the controller notifies only when the hovered target changes, never per mouse move.

### `treemap_view.rs` (proposed)

```text
pub struct Params { layout: tree::LayoutParams, interactive: bool, show_tooltip: bool }
pub enum TileEvent { Select(NodeId), Zoom(NodeId), Hover(Option<tree::Target>), OpenOther(NodeId /* parent */) }
pub struct GeometryCache { tree: *const tree::Tree /* Arc::as_ptr of the tree it was built from */, size: Size<Pixels>, geometry: Rc<tree::Geometry> }
// One cache per map: `explore.geometry` for Explore, `CardData.geometry` per card for Home previews. Replaced when (tree pointer, size) changes.
pub type TileHandler<T> = Rc<dyn Fn(&mut T, TileEvent, &mut Window, &mut Context<T>)>;
pub fn render<T: 'static>(tree: Arc<tree::Tree>, params: Params, selected: Option<NodeId>, hovered: Option<tree::Target>,
                          cache: Rc<RefCell<Option<GeometryCache>>>, cx: &mut Context<T>, on_event: TileHandler<T>) -> impl IntoElement
```

Rendering rules:

1. **Entity access and staleness inside `'static` closures.** `render` captures `let entity = cx.entity().downgrade()` (a `WeakEntity<T>`) and every listener created inside the `container_query` closure dispatches through `entity.update(cx, |this, cx| on_event(this, event, window, cx))`, ignoring the error when the entity is gone. **The caller's `TileHandler` is built per frame and captures that frame's `(ticket, generation)`:** Explore passes `Rc::new(move |this, tile_event, window, cx| { let event = explore::Event { ticket, generation, action: Action::from(tile_event) }; Self::explore_event(this, event, window, cx) })`, so a click delivered from a stale frame carries a stale generation and `allows` rejects it. Home previews are non-interactive and pass a no-op handler.
2. **Origin probe.** The map container is `div().relative().size_full().overflow_hidden()` whose first child is `canvas(|bounds, _, _| bounds.origin, move |_, origin, _, _| origin_cell.set(origin)).absolute().size_full()` with `origin_cell: Rc<Cell<Point<Pixels>>>`. The container's `on_mouse_move` converts `event.position − origin_cell.get()` to map-local pixels, calls `tree::hit_test` on the cached geometry, and emits `Hover(target)` only when the target differs from the current `hovered`. `on_hover(false)` on the container emits `Hover(None)`.
3. **Geometry.** Inside `container_query(move |size, window, cx| ..)`: if `cache` holds `(Arc::as_ptr(&tree), size)`, reuse it; otherwise compute `tree::layout(&tree, Rect{0,0,size}, params.layout)` and store it. The `Arc<Tree>` is also kept alive by the cache owner, so the pointer cannot be reused by a different tree while cached. Build one tile element per emitted tile. **Child tiles are nested inside their parent tile's body element** (`Tile.children` ranges), so each parent's body is `div().absolute()` at `body` with `relative()` children; this clips children naturally and keeps paint order correct.
4. **Tile element.** `div().id(("tile", index)).absolute().left(px(rect.x − parent_body.x)).top(..).w(px(rect.width)).h(px(rect.height)).min_w_0().min_h_0().overflow_hidden().rounded(theme::RADIUS_1).bg(color)`. Border/padding stay inside nested children (existing lesson, `treemap.rs:151-164`). Interactive tiles get `Role::Button`, `aria_label`, `cursor_pointer`, and `on_click` which **always calls `cx.stop_propagation()` first** so ancestor tiles do not also select, then maps `click_count >= 2 && is_directory` → `Zoom`, else `Select`. Directory tiles with `header: Some(_)` get a header child with `.id(("tile-head", index))`, `Role::Button`, `aria_label("Zoom into {label}")`, `on_click` → `stop_propagation` + `Zoom`. `Other` tiles: label "Other · {bytes} · {count} more items", `on_click` → `stop_propagation` + `OpenOther(parent)`. **No `.hover(..)` styling on tiles**: hover color comes from `hovered == Some(target)` so only the deepest tile lights up.
5. **Labels.** If `label_visible`, a single-line `text_ellipsis` label at `theme::TEXT_XS`; files show bytes on a second line when height ≥ 34; headerless folders show the label only.
6. **Colors.** `theme::tile_color(family, depth, is_directory, state)`; `family = tree.family_index(node)`; `Other` is neutral. Selected gets a 2 px `theme::ACCENT` inner ring child (`border_2`), never a size change.
7. **Non-interactive mode** (Home previews): no roles, no listeners, no probe, no tooltip, `cursor_default`.
8. **Tooltip** per the layout contract above, rendered last so it paints on top; it is not a hitbox (`.occlude()` is not used; it has no listeners).

### `explore_controller.rs` (proposed, `impl Space`)

State `pub(super) struct State` (replacing the explorer and filter fields on `Space`):

```text
snapshot: Option<Arc<Snapshot>>, pending_snapshot: Option<Arc<Snapshot>>, crumbs: Vec<(NodeId, String, u128)>,
query_generation: u64, latest_generation: Arc<AtomicU64> /* shared with TreeAbort */, tree: Option<Arc<tree::Tree>>, tree_busy: bool,
geometry: Rc<RefCell<Option<treemap_view::GeometryCache>>>, pending: Option<PendingScope>, focus_pending: bool,
list_rows: Vec<StoredEntry>, list_paths: Vec<String>, list_next: Option<PageCursor>, list_busy: bool, list_mode: ListMode,
list_visible: bool /* default true */, list_selected: Option<usize>, selected: Option<NodeId>, hovered: Option<tree::Target>,
selection_path: Option<(NodeId, String)>, scope_entry: Option<StoredEntry>, disk_menu_open: bool, status: Option<String>,
new_scan_available: Option<ScanId>, search: SearchState
```

`SearchState { text: String, epoch: u64, debounce: Option<Task<()>>, session: Option<deferred::Deferred<backend::Filtered>>, cancel: Option<Cancellation>, busy: bool, error: Option<String>, matching: Option<u64> }`. The session is a `Deferred` so its last drop happens on a worker (existing `FilterSession` rule, `main.rs:92`); `cancel` is sent from a background task, never on the UI thread (`Cancellation::cancel` doc, `crates/space-core/src/model.rs:304`).

Functions:

- `enter_scope_from_bookmark(bookmark, cx)`: called by `open_card` (which has no `Window`). Commits `pending_snapshot` into `snapshot` (replacing `commit_pending_snapshot`, `main.rs:675-689`), sets `crumbs` from the bookmark (`Bookmark.crumbs: Vec<(NodeId, String, u128)>`, root crumb from `snapshot.root_entry`), `list_mode` from the bookmark, clears `tree`, `search` (a disk switch drops any active search session via the worker retire path), `selected`, `hovered`, `new_scan_available`; sets `screen = Screen::Explore`; calls `commit_navigation(scan)` on `disks`; sets `focus_pending = true` (the next `render` calls `window.focus(&self.focus)` and clears the flag unless the search input is focused); then `enter_scope(crumbs.last().node, bookmark.selected, cx)`.
- `enter_scope(parent, restore: Option<NodeId>, cx)`: bumps `query_generation`, stores it into `latest_generation`, clears `tree`, `list_rows`, `selected`, `hovered`, `geometry`, sets `tree_loading = true`. **Completion state machine (one in-flight request):** if `tree_busy`, store `PendingScope { generation, parent, restore }` in `pending` (replacing any older pending) and return; otherwise set `tree_busy = true` and spawn one background task running `backend::tree(.., TreeBudget::EXPLORE, &TreeAbort(latest_generation.clone(), generation))` (or `tree_filtered` with the search session) and `backend::query(Query { view: Folders | Largest, subtree_only: true, filtered: session, .. })`. On completion, **always**: `tree_busy = false`; if `(scan, generation)` still match and the tree is `Some`, apply (tree, rows, `tree_loading = false`, restore selection); otherwise discard (an aborted `Ok(None)` or a stale generation); then if `pending` is `Some`, take it and start it immediately. `tree_loading` stays true until a matching result applies, so the map shows "Loading…" rather than stale tiles. Test E10 covers two zooms without `settle()` between them.
- `zoom_into(node, window, cx)`: requires `node` to be a directory in `tree` or `list_rows`; pushes a crumb (label and bytes from the node, or from the row's `StoredEntry`); saves the exited scope's `selected` into the card bookmark; `enter_scope(node, None)`; focuses the map.
- `up(window, cx)`: pops a crumb and re-enters the parent with `restore = Some(exited)`; focuses the map.
- `crumb(node)`: truncates crumbs after `node`; `enter_scope(node, None)`.
- `select(node)`: validates membership in `tree` or `list_rows`; sets `selected`, syncs `list_selected`; spawns one background `backend::native_path` guarded by `(scan, generation, node)` to fill `selection_path`; focuses the map.
- `hover(target)`: sets `hovered` and notifies only if changed. No backend work.
- `open_other(parent)`: `list_mode = Children`, `list_visible = true`, and if `parent != current scope` then `zoom_into(parent)`; otherwise just notify.
- `list_mode`, `toggle_list`, `next_page`, `first_page`, `toggle_metric` (flips `Space.metric`; if a search session is active, rebuilds it for the new metric with a new epoch, because `FilteredView` fixes its metric at build time (F16); then re-enters the current scope; Home previews re-queue through the existing `queue_visible_previews` metric check at `dashboard_controller.rs:448-478`), `copy`/`reveal` (ported `path_action`), `choose_disk(ticket)` (delegates to `open_card`), `home()` (`screen = Home`, `remember_explorer`).
- `show_new_scan(cx)`: when `new_scan_available == Some(scan)`, re-opens the explored card through `open_card` with a fresh root bookmark for that scan. **Rescan outcome:** pressing `Rescan` in Explore runs `rescan_selected_disk` (kept, `dashboard_controller.rs:619`); on publication `publish_card` sets `new_scan_available`; the displayed snapshot does not change until the user clicks Show (existing invariant; journey J3).
- Search: `search_changed(text)` stores text and arms a 300 ms debounce (`executor.timer`); on fire, if text is empty → `search_clear`; else bump `search.epoch`, cancel any running session from a background task, call `backend::filter(backend, scan, metric, &QueryFilter { name: Some(text), ..Default::default() }, &cancel)` on the background executor, and on completion apply only if `(scan, search.epoch)` match: wrap in `Deferred`, set `matching`, then `enter_scope(current scope, None)`. `search_clear()` drops the session via the worker retire path and re-enters the scope unfiltered. A disk switch, `Home`, or `Rescan` clears search.
- `explore_model(&self) -> explore::Model` and `explore_event(&mut self, Event, window, cx)` gated by `allows`.

Keyboard (bindings in 04): `backspace`, `alt-up` → `Up`; `enter` → `ZoomInto(selected)` when a directory is selected (map focus) or `ListOpen` (list focus); `escape` → close disk menu, else clear search if focused and non-empty, else clear selection, else `Cancel` a running scan; `down`/`up` in the list context move `list_selected` and `selected` together; `cmd/ctrl-r` → `Rescan`; `cmd/ctrl-shift-c` → `Copy`; `cmd/ctrl-shift-r` → `Reveal`; `cmd/ctrl-l` → `ToggleList`; `cmd/ctrl-f` → focus search. **Focus rule:** the map focus handle is focused on entering Explore, after every scope change, and on any tile click, unless the search input currently has focus.

## Behavior and invariants

- Exactly one in-flight scope load; a newer request supersedes the pending one, and the superseded tree assembly aborts at its next store call.
- `selected` always refers to a node in the current `tree` or `list_rows`; entering a scope clears it unless restoring.
- Hover never triggers backend work and never notifies when unchanged.
- Clicking a nested tile selects that tile, not an ancestor (stop_propagation on every tile).
- A superseded scope load never leaves `tree_busy` true or `pending` undispatched.
- A background publication for the explored disk shows the banner and does not replace the displayed snapshot (J3).
- Stale events (old ticket, old generation, old search epoch) are no-ops (J4, J6).

## Tests

- **E1 allows.** Stale generation/ticket rejected except `Home`, `Stop`, `Hover(None)`, `SearchChanged`; `Up` disabled at root; `Reveal` disabled offline; `ZoomInto` allowed for a directory in rows but not in the tree; `OpenOther` only when `other_count > 0`; `ShowNewScan` only when available.
- **E2 layout policy.** `layout(1280, 800, true, false).map` area ≥ 62% of the window; `layout(1280, 800, false, false).map` ≥ 75%; `top_bar + crumb_bar ≤ 88`; `layout(900, 600, true, false).list_width == Some(260)`; with `banner_visible` the map shrinks by exactly 28 px.
- **E3 hover (controller, TestAppContext).** `Hover(Some(t))` sets `hovered`, spawns nothing (`tree_busy` false, `query_generation` unchanged); a repeated identical `Hover` does not call `notify` (count notifies with `cx.observe`); `Select(n)` for an unknown `n` is a no-op.
- **E4 zoom/up/crumb (controller, fixture).** Enter root → `ZoomInto(a)` → `ZoomInto(b)` → `ZoomInto(c)` → crumbs 4 → `Up` ×3 restores `selected` each step → `Crumb(root)` from depth 2 lands with one crumb. Also `ZoomInto` for a directory only present in `list_rows`.
- **E5 list sync.** `ListSelect(i)` sets `selected`; `Select(node)` present in rows sets `list_selected`.
- **E6 style.** Tile box `style()` has absolute position, zero border/padding, `overflow hidden`; header child has `header_height`.
- **E7 crumb overflow.** `visible_crumbs` with 9 crumbs yields first, `Ellipsis`, last three; with 5 yields all.
- **E8 focus.** Headless: after `open_card` and `settle()`, `explore.focus_pending` is true. Drawn (D4): after `frame()`, `self.focus.is_focused(window)` is true and `focus_pending` is false; `simulate_keystrokes("backspace")` right after Explore pops a crumb when depth > 1.
- **E9 search staleness.** Port `superseded_filter_callback_cannot_commit_an_older_navigation` (`git show 1688e30d:crates/space-app/src/dashboard_transition_tests.rs`, lines 454-478) to the search session: an older session completion cannot change the scope or `matching`.
- **E10 double zoom.** `ZoomInto(a)` then immediately `ZoomInto(b)` (no `settle()` between); after `settle()`, `tree.root == b`, `tree_busy == false`, `pending == None`, `tree_loading == false`.
- **E11 metric with search.** With a search active, `ToggleMetric`; after `settle()`, `tree.metric` equals the new metric and `search.epoch` advanced.
- Draw-enabled **D0–D7** and journeys **J2, J4, J6** in 05.

## Exclusions

- No zoom animation, no scroll-wheel zoom, no right-click menu.
- No file-type icons beyond folder/file glyphs.
- No per-tile GPUI `tooltip()` builders.
- Search matches names only (`QueryFilter.name`); no path, extension, size or date fields.
