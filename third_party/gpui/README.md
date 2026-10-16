# GPUI text input attribution

`crates/space-app/src/text_input.rs` adapts the GPUI `examples/input.rs`
example from Zed commit `9f63850f7affbd57772a4f99554d4fd333b50a6f`:
https://github.com/zed-industries/zed/blob/9f63850f7affbd57772a4f99554d4fd333b50a6f/crates/gpui/examples/input.rs

Copyright 2022 - 2025 Zed Industries, Inc. Licensed under Apache-2.0.
`LICENSE-APACHE` is the exact upstream license obtained by resolving
`crates/gpui/LICENSE-APACHE` to the pinned checkout's root `LICENSE-APACHE`.
The pinned GPUI crate and repository root contain no NOTICE file.

Space modifications add single-line normalization, bounded undo/redo,
composition commit events, robust range conversion, accessibility metadata,
horizontal caret scrolling and headless editing tests.

This attribution covers the adapted source only. It does not replace the full
transitive license inventory required before binary distribution.
