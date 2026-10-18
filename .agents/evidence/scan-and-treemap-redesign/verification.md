# Scan-and-treemap verification

Source checkpoint: `6739d6c26b2d5668c27e23abdc81b69f8a134aa5`.
Worktree: `space-scan-and-treemap-redesign`; branch: `feat/scan-and-treemap-redesign`.
Baseline `1688e30d100120a3297eca41bf5496cc9685997c` was fast-forwarded to main with owner authorization.
[PR #3](https://github.com/mattsp1290/space/pull/3) records final-head CI and the M7 measurement output.

## Automated acceptance

- 79 app tests pass, including 29 real scanner/store/controller journey and drawn tests.
- Core: 34 passed; scanner: 23 on macOS; store: 71, with 5 explicit benchmark ignores.
  Those crates, schema, dependencies and lockfile are unchanged.
- Formatting, workspace/all-target Clippy with `-D warnings`, workspace tests,
  release build and `git diff --check` pass.
- Explicit Apple Silicon native release build, dependency inventory, local-only
  package assembly and extracted `--version` smoke pass.
- All four native CI jobs, plus Windows fast tests, pass at the source checkpoint:
  [source CI](https://github.com/mattsp1290/space/actions/runs/39165705493).
  The PR links the authoritative run for its final documentation/evidence head.
- T1's sole owner-approved exception is the dominant-weight 300×900 case, whose
  weighted-mean limit is 4. All other mean limits remain 2.5; ≥1% tiles remain ≤4.
- Drawn tests measure the actual map:≥62% at 1280×800 with the list,≥75% hidden.
  They exercise nested/header clicks, Enter/Backspace, gap hover, tooltip containment,
  actual overlapping disk-menu rows, stale rendered listeners, and narrow controls.
- D5 sends mouse down/up before pumping the executor: GPUI otherwise automatically
  schedules frames during test pumping, replacing the stale listeners being tested.
- Regressions cover IME/preedit across redraw/reset, filtered ancestor totals and
  metric/bookmark changes, native relative paths, duplicate hover notifications,
  lazy opening/duplicate requests, and preview-cap ownership after reordering/Home.

## Native macOS evidence

The first scan-all used the normal per-user cache. Final UI acceptance used a
consistent SQLite backup in a private temporary cache so the stalled original
scan could remain intact. The native build imports the final source with only
the cache destination and window title (`Space Acceptance`) changed. The unique
title prevents automation from confusing the two Space windows. No production
code, OS permissions or privacy settings were changed for this isolation.

| Check | Observed result | Evidence |
|---|---|---|
| M1 Home |1280×800 logical, 2× Retina; one primary action; readable11px minimum type, capacity cards, quiet Refresh fully inside the112px hero.|[Home](M1-home-final.jpg)|
| M2 Scan all |One click starts serial scanning: Scanning1/2, Queued2/2, rising observed entries and striped progress. The first disk publishes an automatic nested preview; foreground browsing stays on the prior scan and offers Show. An early Stop cancels the running first disk and queued second disk, waits for acknowledgement, returns to Rescan all disks and retains prior maps.|[Progress](M2-progress-final.jpg), [retained publications](M2-published-and-waiting.jpg), [acknowledged cancellation](M2-cancelled-final.jpg)|
| M3 Explore |Nested tile selection, direct nested-header zoom, complete ancestry, Backspace restoration, breadcrumb jumps, synchronized list, scoped Largest paths, name search and clear all work. `Apple Color Emoji.ttc` gives one matching file and relative path `Fonts/Apple Color Emoji.ttc`; clearing restores the map.|[Nested zoom](M3-nested-final.jpg), [search/Largest](M3-search-final.jpg)|
| M4 Performance |Held-pointer sweeps through the actual hover path for 11.35 seconds at 1280×800 and over 10 seconds in Retina fullscreen (3024×1898 screenshot). No visible stall; tooltips remain responsive. Final Explore budget remains 600 nodes/64 store calls.|[Windowed](M4-windowed-final.jpg), [fullscreen](M4-fullscreen-final.jpg)|
| M5 Narrow |The window refuses to shrink below 900×600. The list is 260px, six-level ancestry collapses to first/…/last three, bars/details stay inside the window. Captured after the native resize settled.|[Minimum](M5-minimum-final.jpg)|
| M6 Platforms |Linux/Windows interactive desktop acceptance remains deferred as planned. Four-target native compilation/tests/release/package/smoke are required at the final PR head.|PR CI links|

The original `M1-home-idle.png` and `M2-scan-progress.png` preserve initial native
observations before review fixes. The `*-final.jpg` captures show the final UI.

## M7 real main-disk timing

The PR description contains the publication's real entry count and elapsed times
for complete `backend::tree(EXPLORE)` calls at its root and largest depth-1 folder.
The temporary harness imports the production backend/tree/store/scanner code;
query timing begins immediately before each whole backend call. No timing
instrumentation is committed to production.

The normal native Data-volume scan observed 5,438,816 entries before macOS held a
directory-open call. A process sample found the blocked `openat`; a local TCC log
records an iCloud File Provider request at the same time. The timing harness uses
a fresh private cache and existing scan-policy exclusions for `home`, `Volumes`,
`mnt` (mount containers), and the user's `Library/CloudStorage` and
`Library/Mobile Documents`. This avoids opening an autofs mount or invoking cloud
access while preserving a real multi-million-entry main-disk observation. Default
nested/network/virtual filesystem exclusions and no-hydration behavior remain.
The normal app's scanner policy and implementation are unchanged.

For comparison only, the earlier 485,969-entry published scan took 9.007ms at its
root and 15.554ms in its largest folder; these are not the multi-million-entry gate.

The PR stays draft until its M7 publication has at least 2,000,000 real entries,
both whole-tree calls take≤300ms, and all four native CI jobs pass on its exact head.
The PR records those checked results rather than inferring them from unit tests.

## Reviews and follow-ups

Both final independent reviewers APPROVE with zero findings. The complete
[gauntlet record](review-gauntlet.md) lists every exact review directory, fixer
summary, corrective commit and push checkpoint. The live pinned Cursor rubric's
two structural findings were fixed separately from the standard-review findings.
The final source also completes the explicit plan-fidelity and drawn assertions.

The pre-existing scan coordinator self-comparison defect is tracked in
[issue #2](https://github.com/mattsp1290/space/issues/2), as required by the plan.
