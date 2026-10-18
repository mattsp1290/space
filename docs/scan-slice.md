# First scan/cache vertical slice

This slice does not complete WP3/WP4. It connects native metadata traversal,
transactional SQLite publication and a basic cached GPUI browser. No file contents
are read, uploaded or deleted by scanning. Temporary fixture tests own their data.

## Implemented boundaries

- Explicit folder selection/start; opening the app loads only cached metadata.
- One scan writer, bounded synchronous ingest batches, cooperative cancellation.
  Publication and cancellation have a defined commit ordering. An already committed
  scan remains published if cancellation arrives afterward.
- Independent read-only SQLite connection for cached navigation during writer work.
  Only published roots are navigable; previous snapshots remain pinned while viewed.
- Size-ordered folder pages and root/subtree largest regular files.
  Pages are bounded to 100 rows; rendered rows use GPUI virtualization. Logical and
  allocated totals distinguish observed bytes, unknown values and coverage gaps.
- Immutable per-scan root path and identity, traversal interval, retained old cache,
  disconnected browsing, root revalidation before live reveal.
- Cache self-exclusion uses OS directory identity as well as lexical components;
  an unverifiable cache identity or selecting the cache itself refuses the scan.

## Known limitations and conservative policies

- Bounded treemaps, explicit filters, mounted-target selection, cached diagnostics,
  hardlink alias choice and confirmed cache-only removal are integrated. Changes
  comparison and root relinking remain unfinished. Revision `2793d99` passed the
  complete native CI matrix, including new controls and extracted CLI smoke.
- Filters retain the previous view while their separate worker computes results.
  Empty filters use indexed base queries directly. Pages contain at most 100 rows;
  treemap Other includes all omitted weight and opens the full paged child scope.
- Windows held-directory traversal now passes native fixtures after replacing the
  unsupported directory ReOpenFile call with a checked NtCreateFile handle open.
  The corrected real sharing-conflict regression and Windows release/package
  smoke also passed in revision `2793d99`. Physical GUI checks remain deferred.
- No root path component may be a symlink/reparse redirect. On macOS, explicitly
  select the canonical location instead of an alias such as `/tmp` or `/var`.
- Linux requires statx mount IDs under the default policy and allows only known
  local filesystem types. Unknown and FUSE filesystems are conservatively refused.
  macOS uses opened device identity and fstatfs; unusual same-device mount boundaries
  remain a limitation. Native removable/cloud-provider behavior needs later tests.
- Default depth is 128 (hard cap 256), batch 256 (cap 4096), maximum 10 million
  entries. Cancellation cannot forcibly interrupt an OS I/O call.
- Hardware window rendering, dialogs, clipboard/reveal and assistive technology
  remain unverified. Native-target CI is the current implementation gate.

## Deterministic scanner fixture

`cargo run -p space-scan --bin scan-fixture --release --locked -- 10000`

The fixture creates its own temporary sparse-file tree and removes it afterward.
It reports fixed seed, platform, entry/issue counts, logical bytes and two timings;
initial-after-creation cache state is uncontrolled, not a cold-cache benchmark.
An observed Debian 13 x86_64 container run on 2026-10-16 produced 10,101 entries,
zero issues and 5,224,618,888 logical bytes in 21 ms initial and 20 ms warm. Peak
RSS was not measured. This is not reference-machine performance acceptance.

The store's ignored million-entry fixture was also run explicitly in this cloud
container: 27.996 s ingestion, 104.238 s finalization, 857.547 ms reopen plus top
100, and 2.603 ms warm top-100 p95 over 20 queries. Peak RSS was 14,224 KiB and
cache size 582,737,920 bytes. The host reported Xeon 8573C, 10 GB RAM and glibc
2.41; this warm OS-cache measurement is not the planned 16 GB SSD reference
machine, cold-cache or UI performance result. Run the ignored store benchmark
with `cargo test -p space-store --release million_entry_fixture -- --ignored --nocapture`.

## Filter query measurements

An optimized one-million-file full-match filter took 10.270 seconds in the same
cloud Linux environment; no-op filtering took 0.450 ms. Filtered top-100 warm p95
was 6.741 ms, with 32,456 KiB peak RSS. A prior equivalent warm run measured
child-summary computation at 49.93 ms.
These are development measurements, not cold-cache or reference-hardware gates.
Filter Apply is explicit and cancellation preserves the previous displayed view.

## Observation comparisons (Changes)

Changes captures the displayed scan, applied filter and metric. Choose an older
published observation of the same root from bounded pages, then explicitly build
the comparison. Matching requires compatible lineage, strong root identity,
normalized scan policy and native path encoding. A separate disk-backed SQLite
working view pins both observations; computation and connection destruction stay
on workers. Cancellation and failed builds retain the previous explorer/result.

The hierarchy and largest increase, largest decrease, and growing-directory
rankings are paged. Ranked ancestors and descendants overlap and must not be
summed. Each row shows its raw cached before/after totals separately from the
comparison's accounting contributions, known gross additions/removals and net.
Unknown or unobserved scopes never establish deletion. Renames are removed plus
added. Pure hardlink ownership redistribution has zero gross growth. Mixed
directories retain their independently known gross growth/removal alongside the
redistribution warning and uncertain net. Genuine shared-group growth is charged once to its
shared ancestor. Filters match a full endpoint predicate before OR after pairing.
Copy/reveal chooses an observed endpoint; reveal still validates its live root
and path existence. It opens the current item at that recorded location, never
historical contents or a restored file.

Changes is under native CI validation. No million-entry comparison benchmark or
physical GUI acceptance is claimed by the existing scanner/filter measurements.

Before the full-key pagination correction, a development comparison fixture
with 100,000 entries took 35.011 seconds to
build its disk-backed working view after correcting an adverse SQLite join plan;
warm top-100 p95 was 46.346 ms with 35,832 KiB peak RSS. Complete pagination
visited all 100,000 records. That source replay ran alongside other
verification; a previous sample measured 30.461 s / 9.345 ms, showing container
variability. A 500-level fixture built in 112.857 ms and queried its subtree in
0.221 ms. This is cloud development evidence, not a
million-entry or reference-machine acceptance result. Comparison construction can
be lengthy, so phase progress/cancellation and retained prior views are required.

Equal-rank comparison pages now seek the complete rank/identity tuple. Production-
SQL regressions cover first/late pages, both metrics, all four views and maximum
ranks, preserving stable ascending identity order without rescanning earlier ties.
Post-correction large-fixture measurements are pending.


## Scan-and-treemap redesign

Home discovers supported local disks without enumerating their contents. One
primary action freezes the target set and runs the existing serial scan
coordinator. Stop clears queued work and waits for the active worker. Failed or
cancelled scans retain their previous publication. Saved offline sources remain
browsable; capacity is OS metadata, separate from observed bytes.

Home renders a scrolling grid with up to 24 nested previews. Cards beyond the cap
retain publication metadata and open through a worker that lazily pins their
exact scan. Only the explored card retains an extra snapshot outside that cap.
Explore has a 44 px top bar, 36 px breadcrumb bar and 40 px details strip. The map
measures 964×664 at 1280×800 with the 300 px side list visible. The list narrows to
260 px at the 900×600 minimum. Folder headers zoom; tile bodies select. Backspace,
Alt+Up, breadcrumbs and Enter provide keyboard navigation. Largest files are
restricted to the current subtree. File-name search keeps ancestors of matching
files and rebuilds its filtered session when the metric changes.

Tree assembly uses per-call reader locks, a largest-directory-first queue and
cancellation checks between calls. `TreeBudget::EXPLORE` is 48 root children,
16 children per expansion, depth 3, 0.005 scope fraction, 600 nodes and 64 calls.
`PREVIEW` is 24/8/depth 2/0.02/160 nodes/16 calls. A measured geometry cache avoids
relayout on hover. Nested event listeners stop propagation and carry the rendered
card, scan and query generation. Publication, disk switches and search epochs
cannot commit stale work. Dropped SQLite/filter resources retire on workers.

Removed app surfaces: Changes, cache management/diagnostics, hardlink aliases and
the seven-field filter panel. Store comparison APIs remain library-only; schema
version 3 and scanner/store/core are unchanged.

Local tests: 65 app tests (including 17 real-entity journey/drawn tests), 34 core,
23 scanner and 71 store tests pass on macOS; five store benchmarks remain ignored.
Headless journeys exercise scan-all through a fixture-only discovery seam, stop,
bookmarks, offline/lazy navigation, pending scopes, search and stale callbacks.
Drawn tests pump frames and deliver real mouse/keyboard events for map area,
headers, nested selection, hover tooltips, disk-menu occlusion and stale-frame
clicks. Native test-platform frame requests may draw during executor pumping;
the stale-frame test therefore dispatches both native mouse events before pumping
the executor. Manual desktop acceptance and exact-head four-target CI are separate
gates, recorded in the PR evidence.

T1 informational weight-weighted aspect means (`squarify` / inset layout):

| Weights | 964×664 | 300×900 |
|---|---|---|
| 1,2,3,4,5 | 1.622 / 1.627 | 1.268 / 1.271 |
| 50 Zipf | 1.279 / 1.283 | 1.266 / 1.272 |
| 300 equal | 1.076 / 1.080 | 1.000 / 1.000 |
| u64::MAX,1,0,7 | 1.990 / 1.453 | 3.260 / 3.013 |

The owner approved a 4.0 mean limit only for the dominant-weight 300×900 case:
its dominant tile necessarily approaches aspect 3. Other means must remain ≤2.5,
and every tile holding ≥1% of weight must have aspect ≤4.0. Geometry also checks
area, containment, overlap, deterministic sibling order, nesting, headers and
hit-testing. No normalized stretching is used.
