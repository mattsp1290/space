# 01 — Treemap engine and tree query

Goal: a pure, tested geometry engine for a nested squarified treemap plus a bounded backend function that assembles the nested data. No GPUI rendering in this file; rendering is in [03-explore-screen.md](03-explore-screen.md).

Prerequisite state: baseline gate G0 passed. No other work package is required first. WP1 and WP2 ship in the same PR (see [06-execution-handoff.md](06-execution-handoff.md), finding on bin-crate dead code).

## Repository evidence

- Current engine: `crates/space-app/src/treemap.rs`. `Model { items, other_bytes, other_count, unknown_count, zero_count }` (`:21`), `Item` (`:9`), `layout(model, width, height) -> Vec<Tile>` (`:63`) using balanced-by-count bisection (`partition`, `:113`), `MAX_TILES = 128` (`:6`), `format_bytes` (`:176`), `tile_color` by name hash (`:211`), and rendering `view_sized` (`:227`). Its tests (`:354-540`) check area preservation, no overlap, determinism, invalid bounds, the Other bucket, and the borderless tile box. Those invariants carry forward. The old API has 45 call sites outside the file (`dashboard.rs:98,134,203,206`, `dashboard_controller.rs:173,501`, `explorer.rs`, `dashboard_render.rs`, `main.rs`), so the old file is deleted only in WP2 together with its consumers.
- Store queries: `Cache::children_summary(scan, parent, metric, limit) -> Result<ChildrenSummary>` (`crates/space-store/src/queries.rs:286`; it runs `children_sorted`, `entry`, a `count(*)` and a zero-count join per call) and `Cache::children_sorted(scan, parent, metric, after, limit) -> Result<Page>` (`:150`, one indexed read of `child_sizes`); reader variants at `:343,:373`. `ChildrenSummary { page, total_count, total_bytes, unknown_count, zero_count, other_bytes, other_count }` (`:140`); `other_bytes = total_bytes − shown` by construction. `Page { entries: Vec<StoredEntry>, next }` (`crates/space-store/src/lib.rs:102`). `StoredEntry { entry: EntryRecord, logical: MetricTotal, allocated: MetricTotal, coverage, canonical, ordinal }` (`lib.rs:88`). `EntryRecord { node, parent, display_name, kind: EntryKind, .. }` (`crates/space-core/src/model.rs:167`). `MetricTotal { observed_bytes, unknown_count, estimated_count }` (`model.rs:261`). `Metric::{Logical, Allocated}` (`model.rs:233`). `MAX_PAGE_SIZE = 500` (`lib.rs:26`).
- Backend facade: `Backend { writer, reader, filter_reader, comparison_reader }` (`crates/space-app/src/backend.rs:18-23`); every read path shares the one `reader` mutex through `read(backend)` (`:64-75`), so a long-held lock blocks the side-list query and Home previews. `preview` (`:177`), `query` (`:221`), `PAGE_SIZE = 100` (`:26`), `Result<T> = std::result::Result<T, String>` (`:25`). `model_from_summary(summary, metric)` in `dashboard_controller.rs` builds `treemap::Model` from a summary (called at `:503`).
- `Filtered { view: Mutex<FilteredView>, root, matching_count }` (`backend.rs:188`); `FilteredView` exposes `children_summary(parent, limit)`, `children(parent, after, limit)` and `entry(node)` (`crates/space-store/src/filters.rs`, pub method list), so a search-scoped tree is assembled in WP4 with the same code.

## Change surface

All in `crates/space-app/src/`.

### `tree.rs` (`new`) — the engine

A new module so WP1 compiles next to the old `treemap.rs`. WP2 deletes `treemap.rs` and `treemap_view.rs` (new in 03) renders from `tree.rs`. Names below are `proposed`.

```text
pub struct TreeBudget { pub root_limit: usize, pub child_limit: usize, pub max_depth: u8, pub expand_fraction: f64, pub max_nodes: usize, pub max_calls: usize }
impl TreeBudget { pub const EXPLORE: Self; pub const PREVIEW: Self; }
pub struct TreeNode { pub node: NodeId, pub label: String, pub bytes: u64, pub unknown_count: u64, pub is_directory: bool,
                      pub depth: u8, pub children: Vec<TreeNode>, pub other_bytes: u128, pub other_count: u64, pub truncated: bool }
pub struct Tree { pub root: NodeId, pub root_label: String, pub total_bytes: u128, pub metric: Metric, pub nodes: usize,
                  pub children: Vec<TreeNode>, pub other_bytes: u128, pub other_count: u64, pub unknown_children: u64, pub zero_children: u64 }
impl Tree { pub fn find(&self, node: NodeId) -> Option<&TreeNode>; pub fn family_index(&self, node: NodeId) -> Option<usize> /* index of the depth-1 ancestor */ }
pub enum Target { Node(NodeId), Other { parent: NodeId } }   // parent == tree.root for the root-level Other
pub struct Rect { pub x: f32, pub y: f32, pub width: f32, pub height: f32 }   // pixels
pub struct Tile { pub target: Target, pub parent: NodeId, pub depth: u8, pub bytes: u128, pub rect: Rect, pub body: Rect,
                  pub header: Option<Rect>, pub label_visible: bool, pub is_directory: bool, pub label: String, pub children: Range<usize> /* indexes into Geometry.tiles */ }
pub struct Geometry { pub tiles: Vec<Tile>, pub bounds: Rect, pub omitted_area: f32 }
pub struct LayoutParams { pub gap: f32, pub header_height: f32, pub min_label_size: (f32, f32), pub min_tile_area: f32 }
impl LayoutParams { pub const EXPLORE: Self; pub const PREVIEW: Self; }
pub fn squarify(weights: &[u128], bounds: Rect) -> Vec<Rect>
pub fn layout(tree: &Tree, bounds: Rect, params: LayoutParams) -> Geometry
pub fn hit_test(geometry: &Geometry, x: f32, y: f32) -> Option<usize>     // index of the deepest tile containing the point
pub fn format_bytes(bytes: u128) -> String                                  // moved from treemap.rs:176, same behavior and tests
pub fn percent(part: u128, whole: u128) -> String                           // "12.3%" ; "<0.1%" ; "0%" when whole == 0
```

Behavioral contract:

1. `squarify` implements the Bruls–Huizing–van Wijk squarified algorithm. Weights are sorted descending with a stable tie-break on input index (an `Other` weight is sorted like any other weight; it has no special position). `layout` is responsible for order independence: it sorts each sibling list by `(bytes desc, NodeId asc)` with `Other` last among equal weights before calling `squarify`, so reversing a `Tree`'s child order yields identical geometry. Rows are laid along the shorter side of the remaining rectangle, and a row closes when adding the next weight would worsen the row's worst aspect ratio. Output rects are returned in input order. Sum of output areas equals the bounds area within `1e-3 * area`; no two rects overlap by more than `1e-3` px²; every rect stays inside bounds. Zero weights produce zero-area rects. Arithmetic is `f64` internally, `f32` on output. NaN or non-positive bounds return an empty vector.
2. `layout` walks `Tree` depth-first and emits parents before their children; `Tile.children` is the index range of the tile's direct child tiles. For each parent region it builds one weight list: each child's `bytes` plus, when the parent's `other_count > 0`, one `Target::Other` weight of `other_bytes`. Each sibling rect is inset by `gap / 2` on every side and clamped to non-negative size. A directory tile gets `header = Some(top strip of header_height)` and `body = rect minus header` only when `params.header_height > 0`, the inset rect is at least `min_label_size.0` wide, and the inset rect is at least `header_height + 12.0` tall (12 px minimum body); otherwise `header = None` and `body = rect`. `body` therefore always has non-negative size. Children are laid out inside `body`. A tile whose inset area is below `min_tile_area` is not emitted and not descended; its area is added to `Geometry.omitted_area`. `label_visible` is true only when the inset rect is at least `min_label_size`.
3. `Target::Node` tiles are unique per `NodeId`; `Target::Other` is unique per parent. `hit_test` returns the deepest emitted tile whose `rect` contains the point; among equal depth, the last emitted wins (deterministic).
4. `layout` is deterministic for equal inputs. Its cost is `O(n log n)` in emitted tiles; the view caches a `Geometry` per `(tree pointer, size)` so a mouse move never re-runs it (03).
5. `Tree.total_bytes` is the scope entry's own metric total, so percent-of-scope is stable regardless of truncation. For nested nodes assembled from `children_sorted`, `other_bytes = node.bytes − Σ shown children` and `other_count` is derived from the page continuation (assembly step 3).

Budget constants (initial values; one place to tune, see R1 in overview):

| Budget | root_limit | child_limit | max_depth | expand_fraction | max_nodes | max_calls |
|---|---|---|---|---|---|---|
| `EXPLORE` | 48 | 16 | 3 | 0.005 | 600 | 64 |
| `PREVIEW` | 24 | 8 | 2 | 0.02 | 160 | 16 |

`LayoutParams::EXPLORE = { gap: 2.0, header_height: 18.0, min_label_size: (56.0, 16.0), min_tile_area: 16.0 }`. `LayoutParams::PREVIEW = { gap: 1.0, header_height: 0.0, min_label_size: (48.0, 14.0), min_tile_area: 9.0 }` (no headers in previews; depth-2 children still render inside the parent rect).

### `backend.rs` (extended)

Add, next to `preview` (`:177`):

```text
pub struct TreeAbort(pub Arc<AtomicU64>, pub u64);   // (latest generation cell, this request's generation)
pub fn tree(backend: &Backend, scan: ScanId, parent: NodeId, metric: Metric, budget: tree::TreeBudget, abort: &TreeAbort) -> Result<Option<tree::Tree>>   // Ok(None) = superseded
pub fn tree_filtered(filtered: &Filtered, parent: NodeId, metric: Metric, budget: tree::TreeBudget, abort: &TreeAbort) -> Result<Option<tree::Tree>>   // used by the WP4 search session
```

Assembly algorithm (breadth-first so the budget is spent on the biggest items first):

1. Lock `read(backend)?` only for the duration of each store call (one lock per call, never across the whole assembly), so previews and the side list interleave.
2. Root level: `entry(scan, parent)` gives `total_bytes` (metric total `observed_bytes`), then `children_summary(scan, parent, metric, budget.root_limit)` gives the root `children`, `other_bytes`, `other_count`, `unknown_children`, `zero_children`.
3. Nested levels use `children_sorted(scan, node, metric, None, budget.child_limit)` only (one indexed read, no counts). For each expanded directory: `other_bytes = node.bytes.saturating_sub(Σ child bytes)`, `other_count = if page.next.is_some() { 1 } else { 0 }` (at least one omitted child exists when the page has a continuation; zero otherwise). An `Other` tile is rendered only when `other_count > 0`; a remainder with `other_count == 0` (the directory's own or unreadable bytes) is left as empty body space.
4. Expansion order: a max-heap of candidate directories keyed by `bytes`. Pop while `nodes < max_nodes`, `calls < max_calls`, candidate `depth < max_depth`, and `candidate.bytes >= expand_fraction * total_bytes` (`total_bytes == 0` disables expansion). Every store call increments `calls`. Push directory children as candidates. Set `truncated = true` on a directory that was eligible but skipped because `max_nodes` or `max_calls` was reached.
5. Between store calls check `abort`: if `abort.0.load(Acquire) != abort.1`, return `Ok(None)`. The explore controller bumps the cell when a newer scope request starts (03), so superseded assemblies stop within one store call.
6. Errors map through the existing `error` helper to `String`. A non-directory `parent` is a contract error surfaced as a `String`.

Query count bound: exactly `max_calls` store calls at most per tree (64 for `EXPLORE`, 16 for `PREVIEW`), because each popped directory costs one call regardless of how many children it returns.

Replace `preview` so the Home mini map uses `tree(.., TreeBudget::PREVIEW, ..)` with an abort cell owned by the preview job; `model_from_summary` in `dashboard_controller.rs` is deleted and `CardData.preview` becomes `Option<Arc<tree::Tree>>`.

`PAGE_SIZE` and `query` stay for the side list and largest-files list; `View::Treemap` and `Queried.summary` (`backend.rs:43-47,215-221,244-246,258-260`) lose their only consumer and are deleted in WP2 (04).

## Intended behavior and invariants

- No `NodeId` appears twice in a `Tree`.
- Root level: `Σ children.bytes + other_bytes == children_summary.total_bytes` holds by construction; the meaningful invariants tested are `Σ children.bytes ≤ total_bytes` and, for every node, `Σ its children.bytes ≤ node.bytes`.
- `layout` with bounds of zero width or height returns an empty `Geometry` without panicking.
- Assembly time is bounded by `max_calls`, not asserted in tests; manual check M7 in 05 measures a whole `backend::tree` on a real multi-million-entry scan with a 300 ms budget per tree.
- Dead-field rule for WP1/WP2: a struct field that no non-test code reads at the end of WP2 is a dead-code error under `-D warnings`. WP2's minimal `explore.rs`/`home.rs` renders therefore read `Tree.unknown_children`/`zero_children` (details strip), `TreeNode.truncated` and `Tree.nodes` (tooltip "+ n more"/status), `Tile.parent` (hit-test parent fallback) and `Geometry.omitted_area` (status line "n tiny items hidden"); any field the implementer cannot wire by the end of WP2 is omitted until the package that reads it.

## Tests (in `tree.rs` and `backend.rs` `#[cfg(test)]`)

- **T1 aspect ratio.** Weight sets: `[1,2,3,4,5]`, 50 Zipf-like weights (`1_000_000 / i`), 300 equal weights, `[u64::MAX, 1, 0, 7]`; bounds 964×664 and 300×900. Hard assertions on `squarify` output: every rect with ≥ 1% of total weight has aspect ratio (long/short) ≤ 4.0; weight-weighted mean ≤ 2.5. (Squarified layouts meet these on such sets; the current bisection does not on the Zipf set.) The same test also builds a one-level `Tree` from each set, runs `layout` with `EXPLORE` params, and prints the measures on inset rects as an informational record quoted in the PR; no assertion on that output.
- **T2 area, overlap, containment, determinism.** Port `verify` from the current tests (`git show 1688e30d:crates/space-app/src/treemap.rs`, lines 374-414) to pixel rects with the `1e-3` tolerances for `squarify`; `squarify` repeat-call determinism; `layout` determinism under reversed child order (including equal-weight children, satisfied by the `(bytes desc, NodeId asc)` sort).
- **T3 nesting invariants.** A synthetic `Tree` with 700 nodes at 964×664: every child tile's `rect` lies inside its parent's `body`; sibling tiles do not overlap; no emitted tile has inset area below `min_tile_area`; `Geometry.omitted_area + Σ depth-1 tile areas + depth-1 inset margins == bounds area` within `1e-2 * area` (margins computed from the gap and the number of depth-1 siblings); `header == None` for every tile when `header_height == 0`.
- **T4 nested labels and headers on the real fixture.** Build the fixture from `test_fixture.rs` (extended to `root/a/b/c` with 10–40 files at each level, see 05), scan it, assemble with `EXPLORE`, lay out at 964×664 with `LayoutParams::EXPLORE`. Assert at least 2 depth levels have `label_visible` tiles and that every depth-1 directory tile holding ≥ 2% of `total_bytes` has `header: Some(_)`. This is the fixture guarantee behind criterion 2; deeper folders are not guaranteed a header.
- **T5 hit test.** For every emitted tile, `hit_test` at the center of its `body` (or `rect` for leaves) returns that tile or one of its descendants; a point inside a parent's gap returns the parent; a point outside bounds returns `None`.
- **T6 tree assembly (backend).** Over the fixture scan: root level has `min(root_limit, child count)` nodes; `Σ children.bytes ≤ total_bytes`; for every node `Σ children.bytes ≤ node.bytes`; no duplicate `NodeId`; `nodes ≤ max_nodes`; depth ≤ `max_depth`; `expand_fraction = 1.0` yields depth 1 only; a budget with `max_calls = 3` yields at most one expanded directory and marks the next candidate `truncated`; bumping the abort cell before the call returns `Ok(None)`.
- **T7 format.** `percent(1, 1000) == "0.1%"`, `percent(1, 100000) == "<0.1%"`, `percent(0, 0) == "0%"`; existing `format_bytes` cases retained.

## Dependencies, risks, exclusions

- Depends on nothing else. WP3/WP4 depend on this.
- Dead code: `tree.rs` and `backend::tree` have no non-test callers until WP2 wires them. `cargo test -p space-app --locked` compiles them through their tests; `cargo clippy --all-targets -- -D warnings` on the bin target reports them as dead until WP2. WP1 and WP2 therefore land in one PR; gate G1 is `cargo test`, gate G2 adds clippy (06).
- Risk: f32 precision at 2560-wide windows is fine (sub-pixel error < 0.01 px). Internal f64 mitigates accumulation.
- Risk: `children_summary`/`children_sorted` limits are capped by `MAX_PAGE_SIZE = 500`; all budgets are far below.
- Exclusion: no caching of trees across scopes beyond the explore controller's current tree and the card bookmarks (03).

Execution clarification approved by the owner: T1 uses a weighted-mean aspect limit of 4.0 only for `[u64::MAX, 1, 0, 7]` at 300×900, whose dominant rectangle necessarily has aspect approximately 3.0. All other mean limits remain 2.5; the per-tile limit remains 4.0.
