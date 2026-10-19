# Scan-and-treemap redesign

Status: Implemented on `feat/scan-and-treemap-redesign`; final independent code reviews approve. Execution evidence is in [verification.md](../../evidence/scan-and-treemap-redesign/verification.md) and [PR #3](https://github.com/mattsp1290/space/pull/3). The PR remains draft until M7 and exact-head native CI satisfy G3. The two independent and one adversarial planning reviews were completed before implementation.

## Application context

```json
{
  "application_context": {
    "has_active_users": false,
    "backward_compatibility_required": false,
    "feature_flags": "not-applicable",
    "confirmation_digest": "65f57f58a2cbf2da2fbe9cbccdbd3bc711f7ceee49963dc32e6d6ee7bbfe5bb0",
    "confirmed_at": "2026-10-18T17:58:57Z"
  }
}
```

The user confirmed on 2026-10-18: no active users, no backward-compatibility requirement, feature flags not applicable. Consequences: no compatibility layer, no flags, no migration shims. Scan correctness and published-cache durability remain product invariants, not compatibility promises. This plan changes no SQLite schema (`crates/space-store/src/schema.rs` stays at `VERSION = 3`), so stored scans remain readable as a side effect, not as a requirement.

The user also decided scope on 2026-10-18: **drop Changes (scan comparison), Cache & diagnostics, hardlink aliases, and the seven-field filter panel from the UI.** The UI keeps scan, treemap, drill-in, largest files, and a single name search. Backend and store code for the dropped surfaces may stay only where it compiles without dead-code warnings (see [04-shell-reset-and-removals.md](04-shell-reset-and-removals.md)).

## Request

Verbatim: "our UX and design is bad. Redesign it from the ground up. Make sure to focus it around the experience of making it easy to scan and then see a treemap view that you can drill into to explore what is taking up space".

Mid-planning the user added: "feat/desktop-disk-explorer will be merged to main before our plan executes".

## Baseline and planning target

- The application exists only on branch `feat/desktop-disk-explorer`, head `1688e30d100120a3297eca41bf5496cc9685997c`. `main` holds README, LICENSE and `.gitignore`.
- Every repository path in this plan is relative to the repository root and resolves on that branch head. The user stated the branch will be merged to `main` before execution. **Gate G0:** before any implementation, confirm either `git merge-base --is-ancestor 1688e30d100120a3297eca41bf5496cc9685997c HEAD` succeeds or, after a squash/rebase merge, `git diff --quiet 1688e30d100120a3297eca41bf5496cc9685997c HEAD -- crates Cargo.toml Cargo.lock rust-toolchain.toml .github docs` is empty. If both fail, stop and ask the user which branch to base on.
- Toolchain is pinned by `rust-toolchain.toml` to Rust 1.98.1. GPUI is pinned in `Cargo.toml` to Zed revision `9f63850f7affbd57772a4f99554d4fd333b50a6f`. The GPUI APIs cited in this plan were read from that exact revision.
- Existing CI (`.github/workflows/ci.yml`) is the acceptance gate recorded in `docs/development.md` ("owner-approved 2026-10-16 acceptance revision"). Its commands do not change. The manual desktop checks in 05 are an additional PR requirement introduced by this plan.

## Change type and affected areas

Change type: ground-up UI/UX redesign of the GPUI desktop application in `crates/space-app`, plus a new bounded tree query in the app backend module. No scanner, store, or schema changes.

Affected areas (all under `crates/space-app/src/` unless noted):

| Area | Current file(s) | Fate |
|---|---|---|
| App root, routing, actions, key bindings | `main.rs` | Rewritten. `Screen` enum replaces flag routing. |
| Treemap geometry and model | `treemap.rs` | Replaced by new `tree.rs`: squarified, nested, pixel-space, hit-testable. `treemap.rs` is deleted in WP2 with its 45 call sites. |
| Treemap element rendering | inside `treemap.rs` (`view_sized`) | Replaced by new `treemap_view.rs`. |
| Home (disks) presentation | `dashboard.rs` | Replaced by new `home.rs`. |
| Explorer presentation | `explorer.rs`, `dashboard_render.rs` | Replaced by new `explore.rs`; explorer state and queries move to new `explore_controller.rs`. |
| Disk registry, serial scan coordinator, previews, card data | `disk_model.rs`, `scan_batch.rs`, `dashboard_controller.rs` | Kept. `dashboard_controller.rs` is trimmed and re-pointed at the new models. |
| Backend facade | `backend.rs` | Trimmed (comparison module and reader, cache inspection/removal, aliases, reload removed) and extended with `tree` and `tree_filtered`. |
| Dropped UI surfaces | `changes_controller.rs`, `changes_panel.rs`, `comparison_backend.rs`, `cache_management.rs`, `filters.rs` | Deleted. |
| Text input | `text_input.rs` | Kept for the single search field (one `Entity<TextInput>` on `Space`). |
| Theme tokens | none | New `theme.rs`. |
| Journey tests | `dashboard_transition_tests.rs` | Replaced by new `journey_tests.rs` using the same harness pattern. |
| Docs | `README.md`, `docs/scan-slice.md`, `docs/development.md` | Updated to describe the new surfaces and removed ones. |

## Requested outcome and success criteria

Outcome: a person opens Space, presses one button, watches each disk get scanned, and then explores a large nested treemap by clicking into folders, with the chrome kept out of the way.

Measurable criteria (each maps to a test or a manual check named in [05-verification.md](05-verification.md)):

1. **One action to scan.** On Home with no cache, exactly one click on the primary button starts the serial batch across all discovered local disks, and each disk's map appears when its scan publishes, with no further clicks. Journey test J1.
2. **Drill depth.** On the test fixture at 1280×800, reaching the folder three levels below the root takes four single clicks (one Explore, three header clicks). The engine guarantees a header strip for every depth-1 folder holding at least 2% of its scope at 964×664 (geometry test T4); deeper or smaller folders may lack a header and then zoom by double-click, Enter, or the details strip's Zoom in. Drawn test D2 proves the header clicks on the fixture; journey J2 proves the navigation state.
3. **Map dominance.** At a 1280×800 window with the side list visible, the rendered map element covers at least 62% of the window area; with the side list hidden, at least 75%. Chrome above the map is two fixed-height bars totalling at most 88 px. Layout policy test E2 and drawn test D1 (measures the real element bounds).
4. **Nested visibility.** Against the extended test fixture (root `alpha` with `a/b/c` and files at each level, see 05), the explorer map at 1280×800 shows at least two hierarchy levels with labels. Geometry test T4 on the pure layout function.
5. **Tile shape.** For the distributions in T1, `squarify` output rects with at least 1% of the total weight have aspect ratio at most 4.0 and the weight-weighted mean is at most 2.5 (hard assertions). T1 also prints the same measures on `layout` output (after gap inset and headers) as an informational record quoted in the PR.
6. **Hover feedback.** Moving the mouse over a tile sets the hovered target on that mouse-move event (only when it changes) and renders a tooltip anchored to the tile with name, size and percent of scope. Drawn test D3 (real mouse move hits the tile under the cursor), controller test E3, manual check M3.
7. **Safe staleness.** A click, hover or search completion that arrives after the scope, scan, card or search epoch changed is ignored, including a real click delivered from a stale frame. Journey J4, drawn test D5, controller tests E9/E10.
8. **Gates.** `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`, and all four native CI jobs pass on the exact head. Manual desktop checks M1–M7 completed with screenshots and the M7 timing attached to the PR.
9. **Search.** Typing text in the search field narrows the map and the side list to files whose names contain the text (the store's filter matches files only and keeps their ancestor folders, `crates/space-store/src/filters.rs:330`); clearing restores the unfiltered scope; a stale search completion cannot change the scope. Journey J6, controller test E9, manual check M3.

## Scope

In scope:

- Two screens only: **Home** (disks, scan control) and **Explore** (nested treemap, breadcrumbs, side list, details strip, largest files under the current scope, disk switcher, name search). Search is a required part of the Explore package, not a follow-up.
- Squarified nested treemap with bounded data, pixel-accurate layout from the measured container size, hover tooltip, selection, drill-in, up, breadcrumb jump.
- Theme tokens (color, spacing, type scale) used by every new module.
- Removal of the dropped surfaces and the state that only served them.
- Tests at the geometry, model, controller and journey layers; docs; CI stays green.

Inherited behaviors kept as they are today, not new scope: scan a folder, disk cards for offline saved scans, discovery notices, the metric toggle (logical/allocated), copy/reveal, rescan of the explored disk (today a rescan publishes silently; the plan adds a one-line banner so the user can switch). The disk switcher replaces the current explorer sidebar.

Non-goals:

- No scanner, store, schema, cache-retention, or identity changes. No deletion of files, no hydration, no network disks, no privilege escalation, no watchers, no auto-scan on launch.
- No light theme, no user settings, no window-state persistence.
- No new UI framework, no new crates for layout or charts. Pinned GPUI only.
- No binary distribution or signing changes.
- No cross-repository extraction. The squarified layout is small and app-specific; no request document is filed.

Constraints:

- `unsafe_code = "forbid"` (workspace lints) stays.
- Every tile click, hover and keyboard action is validated against the current `Ticket`/scan/query generation before it mutates state (existing pattern: `explorer.rs:111` and the `old_sibling_chart_event_is_rejected_while_next_scope_loads` test at `main.rs:1322`, which is removed with `main.rs` and re-established as E1/J4/D2). Tile listeners capture the `(ticket, generation)` of the frame that created them (03).
- Gate **G0** has a content-equality fallback so a squash merge of the feature branch does not stop implementation spuriously (06).
- No store work on the UI thread: all `backend::*` calls run on `cx.background_executor()` and return through `this.update(cx, ..)` (existing pattern, `dashboard_controller.rs:328` and `main.rs:999`).
- `Deferred` ownership for worker-dropped resources stays (`deferred.rs`).

## Repository findings that drive the design

F1. The treemap is one level deep and partitioned by balanced-by-count bisection (`treemap.rs:99-149`). Aspect ratios are not controlled, there is no nesting, and the view is normalized to a 1×1 box then stretched with `relative()` (`treemap.rs:237`, `:154-164`), so the layout cannot react to the real aspect ratio. Drill-in needs a double-click (`treemap.rs:301`) and replaces the whole map (`main.rs:933-938`).

F2. The explorer stacks three wrapping rows of up to 14 buttons plus two status lines above the map (`explorer.rs:259-489`), subtracts 190–260 px of chrome (`explorer.rs:168`), and the Treemap view also reserves a fixed 200 px list below the map when the list is visible (`dashboard_render.rs:142-158`).

F3. Home stacks two action rows, a notice, a busy hint and a discovery-notices row above the cards (`dashboard.rs:333-462`). Explore is only allowed once the preview has loaded (`dashboard.rs:194`).

F4. Routing is by flags, not a screen enum: `disks.home`, `cache_visible`, `changes.visible`, `filters_visible`, `inspector_visible`, `list_visible` (`dashboard_render.rs:8-80`, `main.rs:93-152`). `Space` has about 60 fields; most belong to filters, aliases, cache and changes.

F5. The store already has what a nested map needs: `Cache::children_summary(scan, parent, metric, limit) -> ChildrenSummary { page, total_count, total_bytes, unknown_count, zero_count, other_bytes, other_count }` (`crates/space-store/src/queries.rs:286`, reader variant `:373`) served by the `child_sizes` table and its `children_logical`/`children_allocated` indexes (`crates/space-store/src/schema.rs`, `QUERY_INDEX`). There is no descendant-depth query, so the nested tree is assembled in `crates/space-app/src/backend.rs` by bounded recursive calls. No schema change.

F6. Pinned GPUI provides `container_query(render: FnOnce(Size<Pixels>, &mut Window, &mut App) -> E)` (`crates/gpui/src/elements/container_query.rs:11`), re-exported from `gpui::elements`. It gives the render closure the measured **size only** (no origin and no `Context<T>`), so tiles can be laid out in pixels while staying ordinary `div`s with `Role::Button`, `aria_label` and `on_click`; the map origin for hit-testing comes from a `canvas` probe child (`elements/canvas.rs:10` passes `Bounds<Pixels>`), and view access inside the `'static` closure goes through a captured `WeakEntity<T>`. GPUI hit-testing marks every hitbox under the pointer as hovered and runs all their click listeners in bubble order unless one calls `App::stop_propagation` (`window.rs:1173-1195`, `app.rs:2723`), so nested tiles must stop propagation. `on_mouse_move` (`div.rs:1006`), `on_hover` (`div.rs:1661`), `Styled::text_ellipsis`, `text_ellipsis_middle`, `line_clamp`, `font_weight`, `text_size`, `opacity` (`styled.rs:89,105,145,522,538,746`) exist.

F7. Pinned GPUI test support (`gpui` feature `test-support`, already a dev-dependency in `crates/space-app/Cargo.toml`) provides `TestAppContext::single`, `add_window`, `add_window_view`, `run_until_parked`, `simulate_resize`, `dispatch_action`, `simulate_keystrokes`, and `VisualTestContext::simulate_mouse_move`, `simulate_click`, `debug_bounds` (`crates/gpui/src/app/test_context.rs:279,347,461,600,617,924-1025`). `skip_drawing` is opt-in (`:196`); the existing harness (`dashboard_transition_tests.rs:13-262`) opted in. The new harness keeps a headless mode for journeys and adds a drawn mode for element-level assertions.

F8. `scan_batch.rs` is a pure serial coordinator (`reduce(&mut self, Event) -> Vec<Effect>`, `:135`) and `disk_model.rs` a pure registry (`reconcile`, `register_folder`, `mounted_targets`). `dashboard_controller.rs` owns discovery, `start_disk_scan`, `publish_card`, `start_preview` and `open_card` (`:219,288,389,483,668`). These are tested and independent of presentation; they stay.

F9. `backend::preview` (`backend.rs:177`) loads one `children_summary` page of 100 for the root; `backend::query` (`backend.rs:221`) serves Folders/Treemap/Largest pages with `PAGE_SIZE = 100` and optional filtered sessions.

F10. Existing defect, out of scope: `scan_batch.rs:209` compares a target's `expected_root` with itself, so the "cannot be verified" branch is unreachable. Record it as a follow-up; do not fix it inside this redesign.

F11. `text_input.rs` is a complete custom single-line input with IME and undo (`TextInput::new(label, placeholder, cx)`, `text()`, `set_text(..)`, event `TextInputEvent::Edited`). It is adequate for a single search field.

F12. A 250 ms progress notify loop already exists: `start_disk_scan` spawns `executor.timer(250 ms)` in a loop that calls `cx.notify()` while the run generation matches (`dashboard_controller.rs:366-385`), and the card status reads `progress.entries` (`:808`). No new ticker is needed.

F13. `queue_visible_previews` (`dashboard_controller.rs:402-434`) is page-based (`disks.page * 8`) and clears `snapshot`/`preview` for offscreen cards; `open_card` returns early when `data.snapshot` is `None` (`:668-681`). Removing pagination requires rewriting this function and giving `Explore` a lazy snapshot-open path.

F14. `Space.metric` is read by the kept controller (`dashboard_controller.rs:394,448-478,874`); `maintenance_busy` is set only by the cache-removal flow (`main.rs:568-597`) and read by kept guards (`dashboard_controller.rs:220,403,484,541,674,870`); `Backend.comparison_reader` (`backend.rs:23,116,136`) is used only by the comparison module.

F15. `ScanAll` always discovers first: `scan_batch.rs:137-140` turns it into `Effect::Discover`, handled by `discover_disks` (`dashboard_controller.rs:219-262`), which runs `backend::discovery` → `space_scan::enumerate_volumes()` on the host and then `registry.mounted_targets()` (cards with a `volume`, `disk_model.rs:230`). The existing harness never exercises this path (it seeds `roots` and calls `reconcile_disks`, `dashboard_transition_tests.rs:140-154`). A one-click journey therefore needs a test-only discovery seam (02, 05).

F16. The store's filter builder skips directories before matching (`crates/space-store/src/filters.rs:330`): a name search matches files only, and ancestor folders of matches remain in the filtered view. `FilteredView` fixes its metric at build time, so a metric change needs a rebuilt session.

F17. The GPUI test platform draws only when a frame is pumped: `window.draw(cx).clear(cx)` inside `update_window` (pattern at `crates/gpui/src/app/test_app.rs:503-511`); `run_until_parked` does not draw, and `simulate_*` events hit-test the last drawn frame (`app/test_context.rs:1055`, `window.rs:1173`). Text in tests uses a no-op text system (`platform/test/platform.rs:133`), so text-dependent bounds are approximate.

## Key decisions

D1. **Two screens, one enum.** `Screen::{Home, Explore}` on `Space` replaces every routing flag. Cache, Changes, filters panel and inspector flags are deleted with their surfaces. Rejected: keeping flag routing and hiding buttons, because the flags are what let chrome accrete.

D2. **Nested squarified treemap rendered as nested divs under `container_query`.** Geometry is computed in pixels from the measured size, cached per `(tree pointer, size)`, and child tiles nest inside their parent's body element; every tile click stops propagation and hover styling comes from the single hit-tested target. Rejected: a custom `Element` painting quads (loses roles/labels and the `.style()` test pattern); keeping normalized 1×1 layout (cannot squarify without the aspect ratio); a flat tile list (ancestor listeners would also fire).

D3. **Tree data assembled app-side with explicit budgets.** New `backend::tree` performs one `children_summary` for the root level and bounded `children_sorted` calls for nested levels, locking the reader per call and aborting when a newer scope request supersedes it. Budgets live in one `TreeBudget` value so the manual performance check (M7) can tune them in one place. Rejected: a new store query over `child_sizes` with recursive CTE (schema/query work the store crate's tests would need to cover; not needed for the budgets chosen).

D4. **Interaction grammar.** Single click selects any tile and fills the details strip. Clicking a folder tile's header strip, double-clicking a folder tile, pressing Enter on a selected folder, or clicking "Zoom in" drills in. Backspace, Alt+Up, the "Up" button and breadcrumb chips go up. Hover shows a tooltip. Rejected: single-click zoom on the tile body (selection and navigation would collide; files would have no selectable affordance).

D5. **Keep the coordination layer, replace the presentation layer.** `scan_batch.rs`, `disk_model.rs`, and the coordination half of `dashboard_controller.rs` stay. Presentation modules are new files so the old ones can be deleted whole instead of edited into shape.

D6. **Theme tokens before pixels.** `theme.rs` defines the dark palette, spacing scale, radius and type scale. No module introduces an `rgb(0x…)` literal outside `theme.rs` (test enforces by grep in `05-verification.md`, check S1).

D7. **Search is part of the Explore package.** The old filter flow in `main.rs` is deleted with the filter panel; the explore controller owns a new, smaller session (`backend::filter` with `QueryFilter { name, .. }`, `Deferred<Filtered>` retired off-thread, epoch-guarded) and `backend::tree_filtered` assembles the tree from `Filtered.view`. Rejected: deferring search to a follow-up PR, because the user listed it among the kept features.

## Target architecture

```text
main.rs            Space { screen: Screen, disks: dashboard_controller::State, explore: explore_controller::State, metric, backend, executor, focus handles, running scan, search_input }
theme.rs           tokens: colors, spacing, radius, type scale, treemap palette
tree.rs            Tree/TreeNode model, TreeBudget (incl. max_calls), squarify, layout -> Geometry, hit_test, format_bytes, percent
treemap_view.rs    render(tree, params, selected, hovered, geometry cache, cx, on_event) -> container_query(nested div tiles + canvas origin probe + tooltip)
home.rs            home::Model/Event/allows/layout/render   (replaces dashboard.rs)
explore.rs         explore::Model/Event/allows/layout/render (replaces explorer.rs + dashboard_render.rs)
explore_controller.rs  impl Space: open scope, request tree/list/largest, selection, hover, up/zoom, path actions, search
dashboard_controller.rs (kept, trimmed) impl Space: discovery, batch effects, start_disk_scan, publish_card, start_preview, open_card, home_model
scan_batch.rs, disk_model.rs, deferred.rs, intent.rs, paths.rs, probe.rs, text_input.rs (kept)
backend.rs         (trimmed) + tree(), tree_filtered(), TreeAbort
journey_tests.rs   TestAppContext journeys against the real Space
```

Before: Home(flags) → Explore(flags) → three header rows → 1-level normalized map + 200 px list → double-click replaces map.
After: Home → one primary action → per-disk cards with nested mini maps → Explore: 2 thin bars → nested pixel-accurate map (≥62% of window) with optional side list → click header to zoom, Backspace to go up.

Data flow for Explore: `open_card` (kept) stages the snapshot → `enter_scope_from_bookmark` commits it and calls `enter_scope(parent)`, which increments `query_generation`, publishes it to the shared abort cell, and spawns `backend::tree` plus `backend::query` (side list) on the background executor → results apply only if the generation and scan match → render reads `explore_model()`; `container_query` reuses the cached geometry for the current `(tree pointer, size)` or recomputes it; mouse moves hit-test that geometry and set `hovered` only when it changes.

## Risks, assumptions, gates

- **R1 Performance of many absolutely positioned divs.** Budget caps the tree at 600 nodes, and tiles smaller than 4×4 px are not emitted. Gate: manual check M4 at 1280×800 and 2560×1440 reports no visible lag while hovering; if lag is observed, reduce `TreeBudget` constants (single site) before merge.
- **R2 Drawing in tests.** Headless journeys skip drawing, so they never run `container_query`. The plan therefore adds a drawn harness mode (D0–D7 in 05) using the pinned test platform's `simulate_mouse_move`/`simulate_click`/`debug_bounds`. Gate G2 includes D0 to prove the drawn mode works before WP3/WP4 rely on it; if it cannot work, implementation stops and reports rather than downgrading D1–D7 to headless.
- **R3 `container_query` gives size only.** Verified at the pinned revision (F6). The origin needed for hit-testing comes from a `canvas` probe; if the probe's bounds and the container's size disagree (padding), keep padding on an outer wrapper, never on the measured container.
- **R4 Stale callbacks across generation changes.** Every event carries a `Ticket` or `(scan, query_generation)` pair checked in the handler (existing pattern). Journey J4 proves rejection.
- **R5 Scope creep back toward the removed surfaces.** The removal list in 04 is explicit; clippy with `-D warnings` forbids leaving dead code behind, so removed features cannot linger half-wired.
- **R6 Tree assembly cost on large disks.** Bounded by `TreeBudget.max_calls` store calls per scope (64 for Explore), each taking the reader lock briefly; superseded assemblies abort at the next call. Manual check M7 times a whole `backend::tree` on a real multi-million-entry scan with a 300 ms budget per tree.
- **G0** (above): baseline check, ancestry or content-equality. **G1:** WP1 engine tests green (`cargo test`), T1 hard thresholds pass. **G2:** after WP2 the workspace passes fmt/clippy/test with zero `allow(dead_code)` additions and the drawn smoke test D0 passes. **G3:** manual desktop evidence M1–M7 attached before the PR leaves draft.

Unresolved decisions: none blocking. Non-blocking, implementer's call within the contract: exact hue values, label font sizes, tooltip delay.

## Document map

- [01-treemap-engine-and-tree-query.md](01-treemap-engine-and-tree-query.md): squarified nested geometry, `Tree` model, budgets, `backend::tree`, hit-testing, tests.
- [02-home-screen.md](02-home-screen.md): Home layout and interaction contract, `home.rs` model/events, cards and progress.
- [03-explore-screen.md](03-explore-screen.md): Explore layout, `explore.rs`, `treemap_view.rs`, `explore_controller.rs`, keyboard, side list, largest files, details strip, disk switcher, search.
- [04-shell-reset-and-removals.md](04-shell-reset-and-removals.md): `Screen` enum, `theme.rs`, `Space` restructure, exact deletions, backend trimming, key bindings.
- [05-verification.md](05-verification.md): test matrix, CI, manual desktop checklist, docs updates, acceptance mapping.
- [06-execution-handoff.md](06-execution-handoff.md): dependency-ordered work packages, ownership, gates, definition of done, deferred items.

Execution clarification approved by the owner: T1 uses a weighted-mean aspect limit of 4.0 only for `[u64::MAX, 1, 0, 7]` at 300×900, whose dominant rectangle necessarily has aspect approximately 3.0. All other mean limits remain 2.5; the per-tile limit remains 4.0.
