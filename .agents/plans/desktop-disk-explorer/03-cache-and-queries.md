# Cache and queries
New space-store owns a bundled/explicitly pinned SQLite integration and proposed schema. New modules: schema, writer, queries, recovery. Keep persistence versioned and UI-independent. No existing stored data requires migration.

## Proposed schema
- roots: internal ID, native selected path, observed volume identity/strength, selected root object identity/strength, comparison-lineage ID, display label.
- scans: ID, root ID, schema/policy version, normalized scan exclusions, metric policy, start/end time, lifecycle status and coverage summary.
- entries: scan ID, node ID, parent ID, native name/encoding, display/search name, kind, optional identity, logical/allocated bytes, timestamps, link/placeholder state, coverage and deterministic attribution.
- directory_totals: scan ID/node/metric, aggregate observed bytes and unknown/error counts.
- errors: scan/node/path reference, stable error class and bounded display detail.
- root_publication: root ID -> latest finished scan ID.
Index parent-child queries, largest-file metric queries and the common filters. SQL parameter binding is mandatory. Store sizes with a checked integer representation and explicit overflow handling; SQLite signed integers must not silently wrap u64 filesystem sizes.

## Lifecycle
Acquire an OS-enforced exclusive per-cache process lock before recovery, migration, retention or any writes. Hold it for the app/cache lifetime. A second launch activates the owner when a safe local activation mechanism exists, otherwise shows Cache in use and exits without touching recovery state. Do not use a stale PID file as proof of ownership. OS lock release on owner crash permits the next process to recover. Test simultaneous launches, owner crash/restart, open historical views and lock failures. In-memory scan pins are valid only under this exclusive process contract.

Scanning -> finalizing -> published, or cancelled/failed/interrupted. Published scans are immutable; a finished traversal can be published with explicit coverage gaps. Never call it fully covered if errors occurred. Partial working results are separate from the previously published scan. Batch transactions permit bounded memory and recovery. Finalize totals/identities, then atomically change publication pointer and final status in one transaction. Kill during each phase must retain the previous usable publication and never expose half-finalized aggregates.

On startup mark abandoned working scans interrupted and offer discard/rescan; no resumable traversal promise in v1. Keep the last two finished scans per root by default to prepare Changes; retain failed working records only for diagnostics under a documented bound, then discard rows safely. Retention runs after successful publication and never deletes a scan being viewed or compared. A scan in use gets a lifetime pin; remove only after UI releases it. Do not delete a user-pinned baseline in later retention features.

Local per-user application-data location via OS conventions. Restrict cache permissions to the current account where supported, never put filename metadata in telemetry, and default to no telemetry/network traffic. No content bytes stored. Cache self-exclusion prevents feedback loops. Show storage footprint and provide explicit remove-scan/remove-all-cache controls; describe recovery limits before permanent cache removal. Database migration must preserve readable old scans or fail with a recoverable diagnostic; never silently wipe them. Corruption: quarantine only with user approval, retain original, offer a new cache/rescan. If disk fills, roll back batch, stop scan, retain prior published result.

## Query contract
Asynchronous paged keyset queries for children, largest files and filtered results, with stable tie breakers. UI holds only visible pages and a bounded LRU. Sorting/filtering invalidates query generation; late query results cannot overwrite new view. Search is literal case-folded display name/path search initially, clearly unable to distinguish identical lossy displays; actions still use native keys. Extension/size/time filters have documented unknown-value behavior (unknown excluded unless selected). Filtered tree/treemap aggregates must use the same query definition, not unfiltered totals. Large filter recomputation is cancelable and stays off UI thread. Never execute a full descendant walk per rendered row.

Verify offline cached reopening performs zero source-volume enumeration, independent concurrent reading during refresh, cache schema rejection/migration, disk full, process termination, corruption and retention with open views. Benchmark 1M entries including deep paths; report cache size and peak memory. No comparison of different root identities without explicit validation.
