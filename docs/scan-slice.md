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
