# Execution handoff
## Order and ownership
1. WP1: introduce tested identity-safe serial scan coordinator and disk model. Backend owner edits new model/coordinator and necessary backend ScanRequest contract; coordinate every call-site change with app integrator.
2. WP2: bounded per-disk preview/cache/session integration. Depends on WP1 contracts; integrator owns main.rs orchestration. Preserve Deferred ownership.
3. WP3: dashboard and explorer render modules and responsive treemap. Can develop against agreed presentation models in parallel after contracts are frozen. UI worker owns new modules/treemap, integrator alone owns main.rs wiring. Keep Changes/filter/cache behavior reachable.
4. WP4: independent reviewers inspect integration, tests and lifetimes; integrator runs verification and fixes, publishes to draft PR, then monitors exact-head native CI to terminal.
No shared-file concurrent edits. Agree interfaces before delegating. Baseline is clean local fb985959 matching published75b76ab tree; verify current branch/head before changing. Do not overwrite unrelated user work.

## First action
Read 01-batch-and-dashboard-state.md, then define proposed disk identity and queue-event interfaces in new modules beneath crates/space-app/src/ with tests before wiring GPUI.

## Completion
One click starts the eligible-disk batch; each scanned disk displays its own actual cached treemap; dashboard-to-explorer interaction is coherent; errors/cancellation/staleness are explicit without a wall of diagnostics. Existing cache safety, filtering, comparisons and actions remain accessible. All specified tests and independent reviews pass; final exact-head CI is green. Deliver PR link, meaningful UX changes and any known limitations. Do not merge or release.

## Deferred
Physical GUI/hardware validation, package distribution license clearance, deletion, content indexing, network disks, privileged scanning and automatic background scheduling remain outside this redesign. No further approval is required to begin implementation once the required plan reviews are resolved because user explicitly requested plan then work.
