# 06 — Execution handoff

## Preconditions

- **G0 baseline.** `feat/desktop-disk-explorer` (head `1688e30d100120a3297eca41bf5496cc9685997c`) is on `main`. Either check suffices: `git merge-base --is-ancestor 1688e30d100120a3297eca41bf5496cc9685997c HEAD`, or (after a squash/rebase merge) `git diff --quiet 1688e30d100120a3297eca41bf5496cc9685997c HEAD -- crates Cargo.toml Cargo.lock rust-toolchain.toml .github docs`. Only if both fail, stop and ask the user which base to use.
- Branch: `feat/scan-and-treemap-redesign` from `main`. CI triggers on `feat/**`.
- Toolchain 1.98.1 via `rust-toolchain.toml`; first build needs network for the pinned Zed revision.
- Read [00-overview.md](00-overview.md) decisions D1–D7 before editing.

## Work packages in dependency order

| WP | Name | Files (`new` where so) | Depends on | Notes |
|---|---|---|---|---|
| 1 | Tree engine and tree query | `tree.rs` (`new`), `backend.rs` (+`tree`, `TreeAbort`), `test_fixture.rs` (`new`) | G0 | Verified by `cargo test` only; bin-target dead code until WP2. Same PR as WP2. |
| 2 | Shell reset and removals | `main.rs` (rewrite, min window size), `theme.rs` (`new`), `ticket.rs` (`new`), `dashboard_controller.rs` (trim per the 04 field audit, `home_model`/`home_event`, preview via `tree`, `queue_visible_previews` rewrite, lazy snapshot open, discovery seam), `backend.rs` (trim incl. `View::Treemap`/`Queried.summary`), deletions listed in 04 incl. `treemap.rs`, `journey_tests.rs` (`new`, harness with discovery seam + frame pump + D0 + smoke), minimal `home.rs`/`explore.rs`/`explore_controller.rs`/`treemap_view.rs` (`new`) that compile and read every `tree.rs` field (01 dead-field rule) | WP1 | Gate G2: full fmt/clippy/test green, S1–S2, D0 drawn smoke passes. |
| 3 | Home screen | `home.rs`, `dashboard_controller.rs` (`home_model` projection), `treemap_view.rs` (non-interactive mode) | WP2 | Parallel with WP4 presentation under the ownership rule. |
| 4 | Explore screen | `explore.rs`, `explore_controller.rs` (incl. search session), `treemap_view.rs` (interactive mode, origin probe, geometry cache), `dashboard_controller.rs` (`open_card` tail, `publish_card` banner hook), `main.rs` (actions/bindings routing, search `TextInput` entity) | WP2 | Search is part of this package, not optional. |
| 5 | Journeys, drawn tests, docs, desktop evidence | `journey_tests.rs` J1–J6, D1–D7; `README.md`, `docs/scan-slice.md`, `docs/development.md`; PR evidence M1–M7 | WP3, WP4 | Gate G3. |

Ownership when parallelizing WP3 and WP4: one integrator owns `main.rs`, `dashboard_controller.rs` and `treemap_view.rs`; presentation workers own `home.rs` and `explore.rs`+`explore_controller.rs` respectively and agree the `home::Model`/`explore::Model` field lists from 02/03 before starting. No two workers edit the same file concurrently.

## Per-package verification

| WP | Verify |
|---|---|
| 1 | `cargo test -p space-app --locked tree backend` green with T1–T7 (T1's informational layout measures quoted in the PR). Gate **G1**. Clippy is not required to pass until WP2. |
| 2 | `cargo fmt --all --check`; `cargo clippy --workspace --all-targets --locked -- -D warnings`; `cargo test --workspace --locked`; S1–S2; D0 drawn smoke; `grep -rn "changes_\|cache_management\|comparison_backend\|filters::\|mod treemap;" crates/space-app/src` returns nothing. Gate **G2**. App launches to Home on the implementer's desktop. |
| 3 | H1–H7 green; M1–M2 observed locally with screenshots. |
| 4 | E1–E11 green; M3–M5 observed locally with screenshots. |
| 5 | J1–J6 and D1–D7 green; docs updated; M7 timing recorded; all four native CI jobs green on the exact head; PR description carries M1–M7 evidence. Gate **G3**. |

## Integration and regression gates

- After WP2 and again after WP5: `cargo test --workspace --locked` shows the `space-core`, `space-scan`, `space-store` test counts unchanged and green (those crates are untouched; a change indicates accidental edits).
- Every push: CI four targets. Fix within scope; never regenerate `Cargo.lock` to hide a problem (`docs/dependencies.md`).
- Review: before the PR leaves draft, run the repository's dual-review gate (`/review`, then `/fix-review`) on the branch; reconcile findings against source. Non-trivial fixes get their own review pass.

## Definition of done

- Home and Explore are the only screens; the removed surfaces have no UI and no dead code (S2, clippy).
- One click scans all disks; each card shows live progress and then a nested mini map; Explore works before a preview has loaded.
- Explore shows a nested squarified map that fills ≥ 62% of a 1280×800 window with the list open (D1); hover tooltips, click-select (nested tile, not ancestor), header-click zoom, Backspace up, breadcrumb jumps, side list sync, largest files under the current scope, metric toggle (also with search active), copy/reveal, rescan banner, occluding disk menu, and file-name search all work and are covered by E/J/D tests.
- T1 hard thresholds hold on `squarify`; T6 budget invariants hold; M7 timing within budget or budgets lowered and recorded.
- Journeys scan only fixture directories through the discovery seam; no test touches the host's real volumes.
- S1/S2 pass; docs updated; CI green on the exact head; desktop evidence M1–M7 attached; dual review completed.
- No merge, no release, no binary distribution changes.

## Rollback

Revert the branch's commits. The store schema and cache files are untouched, so no data recovery is needed.

## Deferred and follow-ups

- Scroll-driven preview loading beyond the first 24 cards (cards past the cap still open through the lazy snapshot path in `open_card`; only their mini map is deferred).
- `scan_batch.rs:209` self-comparison defect (F10): file a tracker issue; not fixed here.
- Light theme, user settings, window-state persistence.
- Linux/Windows desktop acceptance remains deferred per `docs/platform-support.md`.
- Zoom animation and scroll-wheel zoom.
- Search over paths/extensions/sizes/dates, if ever wanted, starts from `QueryFilter`'s remaining fields.
- Re-introducing scan comparison or cache management as a separate screen, if ever wanted, starts from the store's retained comparison module.

## First action

Open [01-treemap-engine-and-tree-query.md](01-treemap-engine-and-tree-query.md) and implement `squarify` in `crates/space-app/src/tree.rs` with test T1 before touching any UI file.
