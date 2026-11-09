# Live scan browsing verification

## Behavior

Explore opens discovered folders before a scan publishes. Live refreshes retain
scope, selection, geometry, and the current list page. Publication resets the
provisional page cursor and retains the folder. Stopped scans remain browsable
for the session. Search and Largest files require a published scan.

## Automated checks

The final source passed these commands on Apple Silicon macOS.

- `cargo fmt --all --check`.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`.
- `cargo test --workspace --locked`. 214 tests passed. Five existing store stress
  tests remain ignored.
- `cargo build --workspace --release --locked`.

Three GPUI journeys pause a real scanner thread after actual ingested batches.
They verify first-scan entry, folder zoom, growing totals, Backspace, list-page
retention, publication with a queued provisional cursor, cancellation, and saved
foreground isolation during rescan. A cancelled view stays labelled stopped
when a different scan starts on the same card.

Store tests cover live-reader isolation, coherent reads during ingestion,
cancellation, final publication, and hardlinks within and across batches.
The complete unfiltered scope reads its map, list, and breadcrumbs in one SQLite
transaction.

## Native macOS check

A temporary acceptance build copies the app source and changes only the cache
location and window title. It uses `/private/tmp/space-live-native-cache` and
`Space Live Acceptance`, preserving the normal per-user cache.

The test source contains 240 folders, each with a nested folder and 250 real
files. Through the native picker, start its scan, open Explore while Stop is
available, zoom into a folder and its nested folder, and press Backspace. The
native window retained the folder while totals grew. Stop acknowledged
cancellation and left the map available with the stopped partial-size label.
The final-source build repeated this navigation after the review fixes.

This is functional desktop evidence. Multi-million-entry drive latency was not
benchmarked. Linux and Windows desktop checks and fresh four-target CI remain
outside the local run.

## Review

Independent review found publication cursor invalidation and final cancellation
refresh races. Both are fixed and covered by the live journeys. Scope reads now
share one transaction. The comment review removed the obsolete claim that a
publication never replaces the foreground snapshot. The optional cursor replaces
the previous page-position boolean and preserves pages during live refresh.
