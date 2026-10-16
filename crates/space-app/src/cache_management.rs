//! Data-fed cache inspector. This module never opens a database or mutates
//! cached/source files. Destructive events are requests for app confirmation.
use gpui::{Context, Div, Role, SharedString, Stateful, Window, div, prelude::*, px, rgb};
use space_core::ScanId;
use space_store::CacheFootprint;

pub const MAX_ROWS: usize = 100;
pub const MAX_DIAGNOSTICS: usize = 50;
const MAX_TEXT_CHARS: usize = 1200;

#[derive(Clone, Debug)]
pub struct Row {
    pub scan: ScanId,
    pub root: String,
    pub status: String,
    pub coverage: String,
    pub started: String,
    pub ended: String,
    /// Observed source-byte totals, not an estimate of SQLite space per scan.
    pub sizes: String,
    pub removable: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Model {
    pub rows: Vec<Row>,
    pub selected: Option<ScanId>,
    pub footprint: Option<CacheFootprint>,
    pub diagnostics: Vec<String>,
    pub next: Option<ScanId>,
    pub diagnostic_next: Option<i64>,
    pub busy: bool,
    pub error: Option<String>,
    pub warning: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Select(ScanId),
    FirstPage,
    NextPage(ScanId),
    MoreDiagnostics(i64),
    RefreshRequested,
    /// Must be confirmed by the app; never authorization to delete source files.
    RemoveRequested(ScanId),
    /// Must be confirmed by the app; never authorization to delete source files.
    ClearRequested,
}

impl Model {
    pub fn selected_row(&self) -> Option<&Row> {
        self.rows
            .iter()
            .take(MAX_ROWS)
            .find(|row| Some(row.scan) == self.selected)
    }

    /// A UI guard only. The app must revalidate after confirmation and the store
    /// must still reject active/pinned scans, regardless of these display flags.
    pub fn allows(&self, event: Event) -> bool {
        if self.busy {
            return false;
        }
        match event {
            Event::RefreshRequested | Event::FirstPage => true,
            Event::Select(scan) => self.rows.iter().take(MAX_ROWS).any(|r| r.scan == scan),
            Event::NextPage(scan) => self.next == Some(scan),
            Event::MoreDiagnostics(cursor) => {
                self.selected_row().is_some() && self.diagnostic_next == Some(cursor)
            }
            Event::RemoveRequested(scan) => {
                self.error.is_none()
                    && self
                        .selected_row()
                        .is_some_and(|r| r.scan == scan && r.removable)
            }
            Event::ClearRequested => self.error.is_none() && self.footprint.is_some(),
        }
    }
}

/// Bound untrusted labels and escape terminal/newline/bidi controls rather than
/// allowing a cached filename or error to impersonate another row or button.
fn display_text(text: &str) -> String {
    let mut result = String::new();
    for (index, ch) in text.chars().enumerate() {
        if index == MAX_TEXT_CHARS {
            result.push('…');
            break;
        }
        if ch.is_control() || matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
            result.extend(ch.escape_unicode());
        } else {
            result.push(ch);
        }
    }
    result
}

fn footprint_text(footprint: CacheFootprint) -> String {
    format!(
        "Cache database: {} B · WAL: {} B · reusable database space: {} B. Free pages remain in the database; this is not secure erasure.",
        footprint.database_bytes, footprint.wal_bytes, footprint.unused_database_bytes
    )
}

fn button<T: 'static>(
    id: impl Into<gpui::ElementId>,
    label: String,
    event: Option<Event>,
    cx: &mut Context<T>,
    on_event: fn(&mut T, Event, &mut Window, &mut Context<T>),
) -> Stateful<Div> {
    let enabled = event.is_some();
    let mut control = div()
        .id(id)
        .role(Role::Button)
        .aria_label(SharedString::from(if enabled {
            label.clone()
        } else {
            format!("{label} (unavailable)")
        }))
        .tab_stop(enabled)
        .border_2()
        .border_color(rgb(0x285166))
        .px_2()
        .py_1()
        .bg(rgb(if enabled { 0x285166 } else { 0x17212b }))
        .child(label);
    if let Some(event) = event {
        // Pinned GPUI supplies Enter/Space click activation for focused elements.
        control = control
            .focusable()
            .focus(|s| s.border_color(rgb(0x90d5ff)))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, window, cx| on_event(this, event, window, cx)));
    }
    control
}

pub fn render<T: 'static>(
    model: &Model,
    cx: &mut Context<T>,
    on_event: fn(&mut T, Event, &mut Window, &mut Context<T>),
) -> Div {
    let allowed = |event| model.allows(event).then_some(event);
    let mut panel = div().flex().flex_col().gap_2().p_2()
        .child("Cache management")
        .child("Cached metadata only. Removing it cannot be undone; rescan accessible source folders to rebuild current metadata. Historical snapshots cannot be recreated. Source files are never deleted.")
        .child("Tab / Shift-Tab to focus controls; Enter or Space to activate. Select a scan to inspect its diagnostics.");
    panel = panel.child(match model.footprint {
        Some(footprint) => footprint_text(footprint),
        None => "Cache footprint unavailable.".into(),
    });
    if let Some(warning) = &model.warning {
        panel = panel.child(format!("Cache notice: {}", display_text(warning)));
    }
    if let Some(error) = &model.error {
        panel = panel.child(format!(
            "Cache error: {}. Refresh to retry.",
            display_text(error)
        ));
    }
    if model.busy {
        panel = panel
            .child("Loading or changing cached metadata… Controls are temporarily unavailable.");
    } else if model.rows.is_empty() && model.error.is_none() {
        panel = panel.child("No cached scans on this page.");
    }
    let mut rows = div()
        .id("cache-scan-rows")
        .max_h(px(240.))
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_1();
    for (index, row) in model.rows.iter().take(MAX_ROWS).enumerate() {
        let selected = model.selected == Some(row.scan);
        let label = format!(
            "{}Scan {} · {}\nStatus: {} · Coverage: {}\nStarted: {} · Ended: {}\nObserved source sizes: {}",
            if selected { "Selected: " } else { "" },
            row.scan.get(),
            display_text(&row.root),
            display_text(&row.status),
            display_text(&row.coverage),
            display_text(&row.started),
            display_text(&row.ended),
            display_text(&row.sizes)
        );
        rows = rows.child(
            button(
                ("cache-scan", index),
                label,
                allowed(Event::Select(row.scan)),
                cx,
                on_event,
            )
            .bg(rgb(if selected { 0x366984 } else { 0x17212b })),
        );
    }
    panel = panel.child(rows);
    let mut controls = div()
        .flex()
        .flex_wrap()
        .gap_2()
        .child(button(
            "cache-refresh",
            "Refresh cache information".into(),
            allowed(Event::RefreshRequested),
            cx,
            on_event,
        ))
        .child(button(
            "cache-first",
            "First scan page".into(),
            allowed(Event::FirstPage),
            cx,
            on_event,
        ))
        .child(button(
            "cache-next",
            "Next scan page".into(),
            model.next.and_then(|next| allowed(Event::NextPage(next))),
            cx,
            on_event,
        ));
    controls = controls
        .child(button(
            "cache-remove",
            "Remove selected cached scan…".into(),
            model
                .selected
                .and_then(|scan| allowed(Event::RemoveRequested(scan))),
            cx,
            on_event,
        ))
        .child(button(
            "cache-clear",
            "Clear all cached metadata…".into(),
            allowed(Event::ClearRequested),
            cx,
            on_event,
        ));
    panel = panel.child(controls);
    if let Some(row) = model.selected_row() {
        panel = panel.child(format!(
            "Diagnostics for cached scan {} (at most {} per page)",
            row.scan.get(),
            MAX_DIAGNOSTICS
        ));
        if model.diagnostics.is_empty() && !model.busy {
            panel = panel.child("No recorded diagnostics on this page.");
        }
        let mut diagnostics = div()
            .id("cache-diagnostic-rows")
            .max_h(px(140.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_1();
        for diagnostic in model.diagnostics.iter().take(MAX_DIAGNOSTICS) {
            diagnostics = diagnostics.child(display_text(diagnostic));
        }
        panel = panel.child(diagnostics).child(button(
            "cache-diagnostics-next",
            "Next diagnostics page".into(),
            model
                .diagnostic_next
                .and_then(|cursor| allowed(Event::MoreDiagnostics(cursor))),
            cx,
            on_event,
        ));
    }
    panel
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(id: u64) -> Row {
        Row {
            scan: ScanId::new(id).unwrap(),
            root: "root".into(),
            status: "Published".into(),
            coverage: "Complete".into(),
            started: "start".into(),
            ended: "end".into(),
            sizes: "0 B".into(),
            removable: true,
        }
    }
    fn model() -> Model {
        Model {
            rows: vec![row(1)],
            selected: Some(row(1).scan),
            footprint: Some(CacheFootprint {
                database_bytes: 100,
                unused_database_bytes: 20,
                wal_bytes: 8,
            }),
            ..Default::default()
        }
    }
    #[test]
    fn removal_requires_visible_selected_removable_scan() {
        let mut m = model();
        assert!(m.allows(Event::RemoveRequested(row(1).scan)));
        assert!(!m.allows(Event::RemoveRequested(row(2).scan)));
        m.rows[0].removable = false;
        assert!(!m.allows(Event::RemoveRequested(row(1).scan)));
        m.rows.clear();
        assert!(!m.allows(Event::RemoveRequested(row(1).scan)));
        assert!(m.allows(Event::ClearRequested)); // Orphaned roots may remain.
    }
    #[test]
    fn busy_blocks_every_action_and_errors_block_destructive_requests() {
        let mut m = model();
        m.next = Some(row(2).scan);
        m.diagnostic_next = Some(3);
        m.busy = true;
        for event in [
            Event::Select(row(1).scan),
            Event::FirstPage,
            Event::NextPage(row(2).scan),
            Event::MoreDiagnostics(3),
            Event::RefreshRequested,
            Event::RemoveRequested(row(1).scan),
            Event::ClearRequested,
        ] {
            assert!(!m.allows(event));
        }
        m.busy = false;
        m.error = Some("failure".into());
        assert!(!m.allows(Event::ClearRequested));
        assert!(!m.allows(Event::RemoveRequested(row(1).scan)));
        assert!(m.allows(Event::RefreshRequested));
    }
    #[test]
    fn cursors_and_selection_must_match_current_page() {
        let mut m = model();
        m.next = Some(row(2).scan);
        m.diagnostic_next = Some(9);
        assert!(m.allows(Event::NextPage(row(2).scan)));
        assert!(!m.allows(Event::NextPage(row(3).scan)));
        assert!(m.allows(Event::MoreDiagnostics(9)));
        assert!(!m.allows(Event::MoreDiagnostics(8)));
        m.selected = None;
        assert!(!m.allows(Event::MoreDiagnostics(9)));
        m.rows = (1..=101).map(row).collect();
        m.selected = Some(row(101).scan);
        assert!(m.selected_row().is_none());
        assert!(!m.allows(Event::Select(row(101).scan)));
    }
    #[test]
    fn bounded_unicode_labels_escape_controls_and_preserve_char_boundaries() {
        assert_eq!(display_text("a\n\u{202e}b"), "a\\u{a}\\u{202e}b");
        assert_eq!(
            display_text(&"é".repeat(MAX_TEXT_CHARS)),
            "é".repeat(MAX_TEXT_CHARS)
        );
        assert_eq!(
            display_text(&"é".repeat(MAX_TEXT_CHARS + 1)),
            format!("{}…", "é".repeat(MAX_TEXT_CHARS))
        );
    }
    #[test]
    fn footprint_keeps_database_wal_and_reusable_space_distinct() {
        let text = footprint_text(model().footprint.unwrap());
        assert!(text.contains("database: 100 B"));
        assert!(text.contains("WAL: 8 B"));
        assert!(text.contains("reusable database space: 20 B"));
        assert!(text.contains("not secure erasure"));
        assert!(!Model::default().allows(Event::ClearRequested));
    }
}
