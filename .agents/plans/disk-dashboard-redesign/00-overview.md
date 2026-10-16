# Disk dashboard redesign
Status: Ready. Two independent reviews and one fresh adversarial review completed with no material findings. This plan describes new work; redesign implementation has not occurred.

## Application context
```json
{"application_context":{"has_active_users":false,"backward_compatibility_required":false,"feature_flags":"not-applicable","confirmation_digest":"cf06650b6b0b503826c08d9326beed568c3cc1969e3f4f44775f032894730f9b","confirmed_at":"2026-10-16T15:31:22Z"}}
```
The user explicitly answered these questions for Space earlier today: no users, no backwards compatibility, no feature flags. These answers remain applicable to this same application. No compatibility layer or feature flags are required. Preserve existing scan correctness and published-cache durability as product invariants, not legacy compatibility. No persistent schema change is planned.

## Request and authority
User: “The UX of this thing isn’t very pleasant. Have various subagents argue for a new design and pick your favorite pieces. We minimum should have an easy way to click button to scan all of your disks and then be able to see a treemap for each”. Subsequently: “Use the implementation plan skill to make an appropriate plan again. Once you have one then get to work”.
This authorizes plan then implementation. Existing authorization permits pushes to feat/desktop-disk-explorer and draft PR #1. It does not authorize merge or release. CI is the user-approved acceptance gate; physical desktop checks remain deferred.

## Facts and change model
This is a substantial UI/workflow redesign of the existing Rust/GPUI app, not a new scanner. Current main.rs has one running scan, one displayed snapshot, stacked global controls, saved-root cycling and a 240px treemap. backend.rs exposes immutable pinned Snapshot, ScanRequest, scan, query and open_root. volumes.rs supplies verified mounted targets and best-effort discovery issues. Existing cache, filtering, comparisons, native-path actions and cancellation must remain reachable.
Before: choose target, press separate Scan, switch visualization, navigate raw status-heavy screen.
After: All disks home -> one Scan all disks action -> per-target queue/results -> focused explorer with disk sidebar and large treemap.

## Decisions and success
- Dashboard wins first-run and multi-disk visibility; treemap-first wins focused exploration; guided state transitions win progress and recovery. No forced wizard.
- One primary Scan all disks button begins a frozen batch of discovered supported mounted local targets, including external drives. No auto-scan on launch.
- Each target has its own status and cached treemap. Finishing one target never steals explorer focus. Scan failures do not abort unrelated queued targets.
- Disk cards have capacity bars, readable sizes, cached timestamps, issue badges and Explore. Capacities are never summed across potentially overlapping/shared volumes. Preview areas are normalized independently.
- Explorer has sidebar, breadcrumbs, dominant map, synchronized optional list/details, Largest files and clear current scope. Cache/diagnostics/aliases move to secondary surfaces.
- Stop remains Stopping until active worker acknowledgement, clears queued work, and preserves published results.
Acceptance: headless controller tests prove two distinct disks each receive the correct independent map after one action; stale results cannot cross disks; actual GPUI dashboard/controller wiring compiles in native CI. Layout/render tests cover empty, queued, scanning, ready, partial, error and offline cards, plus narrow windows. Full existing regression suites and all four native target CI jobs pass on exact published head.

## Scope and risks
No deletion, hydration, privileged access, network scanning, automatic mount/unlock, watchers, binary distribution or new UI framework. Preserve MIT and pinned GPUI. No reusable cross-repository capability is being extracted: this queue and presentation belong to Space.
Risks: identity aliases, mount changes, cache pin lifetimes, UI-thread database destruction, stale callbacks, too many cards, and regressions hidden by headless tests. Corresponding gates appear below. Hardware visual polish remains unobserved until later real-device validation; do not claim CI proves appearance. No blocking product questions remain; exact spacing/colors are non-blocking implementation choices within the layout contract.

## Document map
- [01-batch-and-dashboard-state.md](01-batch-and-dashboard-state.md): identity-safe batches and bounded result models.
- [02-dashboard-and-explorer.md](02-dashboard-and-explorer.md): user-visible layout and interaction contract.
- [03-verification.md](03-verification.md): tests and cross-platform gates.
- [04-execution-handoff.md](04-execution-handoff.md): ownership, ordering and completion.
