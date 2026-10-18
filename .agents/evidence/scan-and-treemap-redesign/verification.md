# Scan-and-treemap verification

Worktree: `space-scan-and-treemap-redesign`, branch `feat/scan-and-treemap-redesign`.
Baseline: main fast-forwarded to `1688e30d100120a3297eca41bf5496cc9685997c` with owner authorization.

## Automated evidence

- Tree geometry/assembly tests pass, including the owner-approved T1 exception.
- 70 app tests pass; 21 real-entity journey/drawn tests cover scan-all, stop,
  three-level drill-in, map area, deepest hover/selection, stale-frame events,
  search epochs, metric rebuild, bookmarks, lazy opening, preview pins and rescan.
- Core: 34 passed; scanner: 23 passed on macOS; store: 71 passed, 5 benchmarks ignored.
- Workspace Clippy with `-D warnings`, formatting and `git diff --check` pass.
- Native release build passed locally; exact final-head gates and CI will be
  recorded after review fixes.
- Native test-platform events may pump automatically scheduled frames. D5 sends
  mouse down/up before executor pumping, so the old rendered listeners receive a
  real click after the controller invalidates their query generation.

## Desktop evidence in progress

- M1: native macOS Home at 1280×800 logical pixels, 2× Retina, one primary action
  and readable type. `M1-home-idle.png`.
- M2: one click started serial scanning, with other disks queued. Observed entries
  rose from 13,671 to 265,734, then 485,969. `M2-scan-progress.png`. Completion,
  retained maps after Stop, and the remaining checks are still being collected.
- M3–M5: pending native interaction checks.
- M6: Linux/Windows desktop acceptance remains deferred as planned. Fresh CI pending.
- M7: pending a published real multi-million-entry scan and whole-query timings.

No native or CI result is implied by the drawn in-process tests.

Standard dual review: both REQUEST_CHANGES; canonical auto fixer addressed six Important findings and one deduplicated suggestion. Full local fmt/Clippy/workspace tests/release build/diff gates passed after fixes. Deferred scan coordinator follow-up: https://github.com/mattsp1290/space/issues/2.
