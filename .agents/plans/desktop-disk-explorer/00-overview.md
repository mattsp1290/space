# Space: desktop disk explorer

## Acceptance update — 2026-10-16
Matt explicitly accepts passing CI as the current platform acceptance gate. Real desktop/laptop UI, GPU, accessibility and clean-machine checks are deferred follow-up validation, not blockers for G0 or subsequent feature work. Native-target CI must pass formatting, Clippy, tests, release builds, package assembly and extracted CLI smoke on all four targets. Do not describe this as observed GUI or real-hardware success. The detailed native procedures below remain the later validation checklist. License/distribution safeguards remain separate.

Status: Ready. First work package: G0 platform feasibility; subsequent feature implementation depends on passing that gate. Planning only; no application implementation or native validation has occurred.

## Application context
```json
{
  "application_context": {
    "has_active_users": false,
    "backward_compatibility_required": false,
    "feature_flags": "not-applicable",
    "confirmation_digest": "cf06650b6b0b503826c08d9326beed568c3cc1969e3f4f44775f032894730f9b",
    "confirmed_at": "2026-10-16T15:31:22Z"
  }
}
```

Matt confirmed no users, no backward compatibility, and no feature flags on 2026-10-16. Feature flags are therefore not applicable. He approved the proposed scope and permits GPL if needed. This does not itself change LICENSE. There is no existing application data to migrate; future schema evolution and crash recovery still need deliberate handling.

## Repository facts
Baseline: 477d1571415ebff7556008dc4a90e51077f14a56 on main. Existing files: README.md ("Simple gui app to see what is taking space."), LICENSE (MIT), .gitignore (Rust template). No Cargo manifest, application, tests, CI, or repository AGENTS.md was present. Change type: greenfield cross-platform desktop product. All code paths and symbols below are proposed/new, not existing APIs. No external project request is needed: scanner, cache and UI are cohesive parts of this app, with no demonstrated independent consumer.

## Approved product
Rust + GPUI desktop application for Windows, macOS and Linux. Scan a selected drive or folder, persist the scan, browse a hierarchy, largest files and linked treemap, and filter cached metadata. Reopen disconnected-drive scans. Follow with a Changes view comparing two completed scans. Local-only operation, explicit coverage and stale state, read-only file inspection through copy/reveal. No content indexing or file deletion.

## Architecture decision
Use a Cargo workspace with new crates/space-core (domain, aggregation/query contracts), crates/space-scan (bounded traversal and platform adapters), crates/space-store (SQLite and queries), and crates/space-app (GPUI). Each new directory is rooted at the existing repository root; create crates/ during bootstrap. Prefer small modules inside these crates over more crates. Scanner and persistence must not depend on GPUI. One writer owns the database; workers send bounded batches. UI requests are asynchronous, generation-tagged and cancelable. It queries immutable published scans and receives bounded preview/progress updates for a separate working scan.

Use GPUI/platform directly initially. Pin a tested common release/revision and toolchain, rather than wildcard versions. Preserve MIT unless a concrete component materially reduces work and requires GPL; inspect its dependency/asset licenses and document the selected compatible license before importing it. GPL is user-permitted, not required. Never silently copy GPL Zed UI into an MIT-only distribution.

## Feature priorities and debate disposition
The minimalist case wins the first milestone: tree and largest files with instant cached reopening. The history advocate wins the storage boundary: independent immutable scans rather than a mutable disposable index. The platform advocate wins correctness: partial coverage, native paths, accounting and real OS acceptance come before extra features. A linked treemap is included in the first complete release because the user wants multiple views, but follows functional lists. Changes follows that release. Duplicate hashing, deletion/cleanup, background schedules/watchers, remote services, privileged acceleration, and filesystem snapshot management are deferred.

## Success criteria
- Native-target CI builds and tests packages on all three OSes. Actual core UI workflow and Linux X11/Wayland runtime validation are deferred.
- Metadata-only scans expose inaccessible paths and changing files, never silently turn unknown into zero, and do not intentionally hydrate cloud-only files.
- Cached results open offline without traversing the source volume; failed/cancelled refresh leaves the prior published scan intact.
- Tree, largest-files and treemap use identical scope, metric and selection semantics.
- Million-entry cache fixture: on a documented 16 GB reference machine with SSD, target first cached view <=2 s, indexed top-100 query <=250 ms p95, UI interaction response <=100 ms p95, process RSS <=1 GiB. These are acceptance targets to measure, not achieved claims. Scanner throughput is measured, not promised at a fabricated universal rate.
- Cancellation updates UI within 250 ms and prevents later batches publishing; a blocked OS call may outlive the UI cancellation and is not falsely promised to stop instantly.
- Changes never treats coverage failures as deletions and reports source intervals rather than claiming atomic filesystem snapshots.

## Risks and decisions
Prerequisite G0 is the platform/dependency spike; failure blocks feature implementation until an acceptable solution is demonstrated. Signing credentials remain a distribution consideration; native test machines are deferred. CI is accepted for current development progression, without claiming runtime evidence. Exact minimum OS/GPU versions and Linux libc baseline are selected and recorded by G0. Default initial architecture proposal: Windows x86_64, macOS arm64 and x86_64, Linux x86_64; additional ARM64 targets deferred. This matrix is a proposal to validate, not a claim of existing support. General OS support remains required even if the initial architecture matrix is narrowed by an explicit user decision.

No unresolved user application-context decisions remain. No existing compatibility promise requires preservation. Do not change the operating context during implementation without asking Matt. Scope changes, privileged execution and deletion are not authorized by this plan.

## Document map
- [01-platform-and-bootstrap.md](01-platform-and-bootstrap.md): pinned native feasibility and release baseline.
- [02-scan-and-accounting.md](02-scan-and-accounting.md): traversal, identities, coverage and metrics.
- [03-cache-and-queries.md](03-cache-and-queries.md): persistent scan lifecycle, query boundaries and privacy.
- [04-desktop-experience.md](04-desktop-experience.md): coordinated UI and user-visible states.
- [05-changes-view.md](05-changes-view.md): bounded follow-on comparison.
- [06-verification-and-handoff.md](06-verification-and-handoff.md): ordered work packages, acceptance and handoff.
- [07-research.md](07-research.md): primary references and source limits.
