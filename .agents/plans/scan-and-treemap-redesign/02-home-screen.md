# 02 — Home screen

Goal: the first screen makes "scan everything" one obvious action, shows each disk's progress where the disk is, and turns into a launchpad of nested mini maps as scans publish.

Prerequisite state: WP2 (shell reset, [04-shell-reset-and-removals.md](04-shell-reset-and-removals.md), which includes WP1) complete on the branch. `Space.screen == Screen::Home` renders this module.

## Repository evidence

- Current presentation `crates/space-app/src/dashboard.rs`: `Ticket { card, scan, query_epoch }` (`:19`), `Status` enum with `label()`/`color()` (`:26-62`), `Capacity { total, free }` (`:65`), `Card` (`:87`), `Model` (`:105`), `Event` (`:123`), `allows` (`:161`), `layout(width, height, count) -> Layout { columns, card_width, preview_height }` (`:229`), `button` (`:263`), `view` (`:310`). Tests (`:761-967`) cover unavailable cache, ticket binding, pagination, explicit states, breakpoints, capacity text, and the card style.
- Controller projection `dashboard_controller.rs:801 dashboard_model(&self) -> dashboard::Model` maps `ScanBatch::status(card)` (`scan_batch.rs:29 CardStatus`) to `dashboard::Status`, reads `run.progress.entries` for live counts (`:808`), and fills `Ticket`s. Events are handled at `:756 dashboard_event`, gated by `allows`.
- Discovery: `backend::discovery(backend) -> (VolumeListing, Vec<RootRecord>)` (`backend.rs:167`); `MountedVolume { label, mount_path, aliases, identity, filesystem, removable: Option<bool>, capacity: Option<VolumeCapacity> }` and `VolumeCapacity { total_bytes, available_bytes, observed_at }` (`crates/space-scan/src/volumes.rs:14-33`). `VolumeListing.issues` and `.complete` (`:40`).
- Serial batch: `scan_batch::Event::{ScanAll, ScanTargets, Stop, ..}` and `Effect::{Discover, Start, Cancel}` (`scan_batch.rs:54,74`). `State.batch`, `State.discovering`, `State.folder_preflight` (`dashboard_controller.rs:40-70`).
- Scan progress is polled from `backend::Progress { entries: AtomicU64, batches }` (`backend.rs:59`); the card status reads it on render (`dashboard_controller.rs:808`), and `start_disk_scan` already runs a 250 ms `executor.timer` loop that calls `cx.notify()` while the run generation matches (`:366-385`). That loop is kept unchanged.
- Preview queue: `queue_visible_previews` (`dashboard_controller.rs:402-434`) selects the current page's eight cards (connected first), clears `snapshot`/`preview`/`metric` and bumps `epoch` for every other card, then enqueues `PreviewJob`s (`:440-480`) keyed by `self.metric`. `open_card` (`:668-681`) returns early when `data.snapshot` is `None`.

## Change surface

- `crates/space-app/src/home.rs` (`new`): replaces `dashboard.rs`. Insert in `main.rs` module list in place of `mod dashboard;`.
- `crates/space-app/src/dashboard_controller.rs` (kept; these edits land in WP2 so the crate compiles, WP3 refines the projection): rename `dashboard_model` to `home_model` returning `home::Model`; rename `dashboard_event` to `home_event` taking `home::Event`. Replace `model_from_summary` with `backend::tree(.., TreeBudget::PREVIEW, ..)` in `start_preview` (`:483-536`). Rewrite `queue_visible_previews` (below). Add the lazy snapshot path to `open_card` (below).
- `crates/space-app/src/theme.rs` (from WP2): all colors and sizes used here.

### `home.rs` public surface (proposed)

```text
pub use crate::ticket::Ticket;   // see 04: Ticket moves to the shared module `ticket.rs`
pub enum Status { Idle, Queued { position: usize, total: usize }, Scanning { entries: u64, ordinal: usize, total: usize }, Stopping, Ready, Partial, Failed, Cancelled, Offline }
pub struct Capacity { total: u64, available: u64 }      // renamed from free; label "x free of y"
pub struct Card { ticket, name, location, kind: CardKind /* reuse disk_model::CardKind */, connected, capacity: Option<Capacity>, status, last_scan: Option<String>, cached_total: Option<String>, preview: Option<Arc<tree::Tree>>, geometry: Rc<RefCell<Option<treemap_view::GeometryCache>>> /* per card */, error: Option<String> /* CardData.error, or the reason in scan_batch::CardStatus::Failed(String) */, opening: bool, can_rescan }
pub struct Model { cards: Vec<Card>, included_targets, discovering, unavailable, busy, stopping, done_count, total_count, notice: Option<String>, metric_label, discovery_issues: Vec<String>, issues_open: bool }
pub enum Event { ScanAll, Stop, ScanFolder, RefreshDiscovery, ToggleIssues, Explore(Ticket), Rescan(Ticket), Retry(Ticket) }
impl Model { pub fn allows(&self, Event) -> bool; pub fn primary(&self) -> (label: &'static str, Event, enabled: bool); }
pub struct Layout { columns: usize, card_width: f32, preview_height: f32, hero_height: f32 }
pub fn layout(width: f32, height: f32, count: usize) -> Layout
pub fn render<T: 'static>(model: &Model, width: f32, height: f32, cx: &mut Context<T>, on_event: fn(&mut T, Event, &mut Window, &mut Context<T>)) -> Div
```

Changes versus `dashboard.rs`:

- Pagination (`CARDS_PER_PAGE = 8`, `Page(usize)`, `visible_cards`) is removed. Cards render in a vertically scrolling grid. **`queue_visible_previews` rewrite:** drop `disks.page`; compute `visible` as the first `home::MAX_PREVIEWS = 24` (`proposed` const) card ids in connected-first order; keep the existing behavior for the rest (clear `preview`/`metric`, bump `epoch`) but **do not clear `snapshot`** for cards outside the cap, so an already-opened snapshot stays usable; enqueue jobs exactly as today for the visible set. Cards beyond the cap show a "Map not loaded" placeholder; scroll-driven loading is deferred.
- `Explore(ticket)` is allowed when `ticket.scan.is_some()` even if the preview has not loaded (`dashboard.rs:194` required the preview). **Lazy snapshot path in `open_card`:** when `data.snapshot` is `None` but `data.scan` is `Some`, spawn `backend::open_scan(&backend, root, scan)` on the background executor, set `data.opening = true` (new `CardData` field) so the card shows "Opening…", and on completion **always clear `opening` first**, then apply only if `revision` and `scan` still match (not `epoch`, which `queue_visible_previews` bumps for offscreen cards): store the `Arc<Snapshot>` and continue the normal `open_card` tail. On error, set `data.error` and stay on Home. A second `Explore` while opening is ignored by `allows`. Because the async continuation has no `Window`, focus is requested through `explore.focus_pending = true`, consumed by the next render (03). **Pins:** at most one snapshot pin exists outside the preview cap, the explored card's; when the user opens a different card, `remember_explorer` releases the previous card's snapshot if that card is outside the cap. Test H6 covers Explore-before-preview.
- `Cache`, `Details`, `Map(..)`, `Page(..)` events are removed. Clicking the mini map is the same as `Explore(ticket)` (whole card is one click target; the mini map is not interactive by tile).
- `Retry(ticket)` is `Rescan(ticket)` with a different label, shown only for `Failed`/`Cancelled`; it maps to the same controller path (`ScanTargets(vec![target])`).

### Layout contract

At any width:

```text
┌────────────────────────────────────────────────────────────┐
│ Space                                              [Scan a folder…] │  hero: 112 px
│ What's taking up space?                                     │
│ [ Scan all disks ]   3 disks found · external included       │
├────────────────────────────────────────────────────────────┤
│ ┌ card ───────────────┐ ┌ card ───────────────┐              │  grid, scrolls
│ │ Macintosh HD   Ready │ │ Backup   Scanning 2/3│              │
│ │ 412 GiB free of 1 TiB│ │ ▰▰▰▰▱▱ 1.2M entries  │              │
│ │ ▓▓▓▓▓▓▓░░░ capacity  │ │                      │              │
│ │ [ nested mini map  ] │ │ [ building…        ] │              │
│ │ 588 GiB observed · 2h ago · Explore →         │              │
│ └──────────────────────┘ └──────────────────────┘              │
└────────────────────────────────────────────────────────────┘
```

- `layout`: `columns = 1` below 900 px, `2` from 900, `3` from 1500; `card_width = (width - 2*theme::SPACE_6 - (columns-1)*theme::SPACE_5) / columns`; `preview_height = 150` when `columns >= 2`, `(height - 400).clamp(180, 420)` when `count == 1`, else `150`; `hero_height = 112`.
- The hero has exactly one primary button. Its label/event is `model.primary()`: `("Scan all disks", ScanAll)` when nothing is cached, `("Rescan all disks", ScanAll)` when any card has a scan, `("Stop", Stop)` while busy, `("Stopping…", Stop, disabled)` while stopping. `Scan a folder…` is a quiet secondary control at the top right; `Refresh disks` is a text link under the status line; both disabled while busy (existing `allows` rules carry over).
- Test-only discovery seam: `State` gains `#[cfg(test)] pub discovery_override: Option<Arc<dyn Fn() -> backend::Result<(space_scan::VolumeListing, Vec<RootRecord>)> + Send + Sync>>`; `discover_disks` uses it instead of `backend::discovery` when set. Journeys build a `VolumeListing` whose `MountedVolume`s point at the fixture directories with `identity = space_scan::inspect_root(path)` so `mounted_targets()` and root validation succeed. Without this seam `ScanAll` would enumerate the host's real disks (F15).
- The status line under the button reads, in order of precedence: discovering → "Finding your disks…"; busy → "Scanning {done_count + 1} of {total_count} · {active disk name}"; otherwise "{included_targets} disks found" plus " · {n} offline saved scans" when any card is disconnected.
- Each card: name (bold, 1 line, ellipsis), kind · location (muted, 1 line, ellipsis), status pill (color by `Status`, text by `Status::label`) with `error` shown as a second muted line under the pill when present (the only remaining destination for failure detail now that the Cache screen is gone), capacity bar (6 px, `available` is OS metadata: keep the existing caption "OS capacity, separate from observed sizes" as a tooltip on the bar, not a visible line), preview area of `preview_height`, footer line "{cached_total} observed · {last_scan}" and an `Explore →` affordance. The whole card is one `Role::Button` with `aria_label = "{name}, {status}, explore"`; its `on_click` sends `Explore(ticket)` when allowed. `Rescan`/`Retry` are a small secondary button inside the footer with `on_click` that stops propagation (`cx.stop_propagation()` in the listener) so it does not also Explore.
- While `Scanning`, the preview area shows a progress strip (a static striped bar is acceptable; `with_animation` optional) plus "{entries} entries observed". The count updates through the existing 250 ms notify loop (`dashboard_controller.rs:366-385`); no new ticker.
- Discovery issues: a single muted link "{n} discovery notices" under the grid toggles an inline list (max 64 rows, scrollable, `max_h(px(160))`). Existing `VolumeListing.issues` strings feed it.
- Empty states: discovering → centered "Finding your disks" panel; none found → "No supported local disks found" with Refresh and Scan a folder actions; cache unavailable (`model.unavailable`) → a notice above the grid and all acquisition events disabled (existing rule `dashboard.rs:162-173`).

## Behavior and invariants

- One click on the primary button when idle issues `scan_batch::Event::ScanAll` through the existing `batch_event` path; the batch runs serially (unchanged).
- A card's `Explore` while its scan is running opens the previous published snapshot if any (existing bookmark/`open_card` behavior); otherwise it is disabled.
- Finishing a background scan never changes `screen` (existing invariant from the earlier redesign; journey test J3 keeps it).
- `Stop` keeps "Stopping…" until the worker acknowledges (existing `CardStatus::Stopping` / `BatchPhase::Stopping`).

## Tests (`home.rs` tests and controller tests)

- **H1 primary mapping.** `primary()` for idle/no-cache, cached, busy, stopping.
- **H2 allows.** Port `unavailable_cache_disables_acquisition_without_faking_discovery_or_running` and `card_maps_and_callbacks_are_bound_to_card_scan_and_query` (read them with `git show 1688e30d:crates/space-app/src/dashboard.rs`, lines 801-845, since WP2 deletes the file) to the new events; `Explore` allowed without preview when `scan.is_some()` and not `opening`.
- **H3 layout.** Columns at 800/900/1499/1500 px; card width positive; single-card preview grows with height.
- **H4 status line precedence.** Discovering beats busy beats counts.
- **H5 style.** Card box `style()` has the production width and `flex_shrink == 0` (port of `dashboard.rs:957`).
- **H6 Explore before preview (controller, fixture).** With alpha cached and `queue_visible_previews` not yet run for it (or `MAX_PREVIEWS` set to 0 in the test), send `Explore(alpha)`; `settle()`; `screen == Explore`, `explore.snapshot` is alpha's scan, and a second `Explore` sent while `opening` was true was ignored.
- **H7 per-card geometry.** Two cards with different trees rendered at the same preview size produce different `GeometryCache` contents (keyed by tree pointer and size), so no card shows another card's map.
- Journey **J1** in 05 covers the one-click scan through the discovery seam.

## Exclusions

- No per-tile interaction inside mini maps.
- No scroll-driven preview loading (deferred); Explore still works for every card through the lazy snapshot path.
- No card reordering or hiding.
