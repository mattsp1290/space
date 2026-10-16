//! Data-fed Changes inspector. It never opens a database, scans a source, or
//! deletes files. All events are requests; the caller revalidates their epoch,
//! comparison identity, scan eligibility, and any endpoint before acting.
use gpui::{Context, Div, Role, SharedString, Stateful, Window, div, prelude::*, px, rgb};
use space_core::ScanId;

pub const MAX_BASELINES: usize = 100;
pub const MAX_ROWS: usize = 100;
pub const MAX_BREADCRUMBS: usize = 64;
pub const MAX_DETAILS: usize = 20;
const MAX_TEXT_CHARS: usize = 1200;

/// An opaque comparison-local identity, deliberately distinct from `NodeId`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowId(pub u64);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum View {
    #[default]
    Hierarchy,
    LargestIncreases,
    LargestDecreases,
    GrowingDirectories,
}

impl View {
    pub fn label(self) -> &'static str {
        match self {
            Self::Hierarchy => "Hierarchy",
            Self::LargestIncreases => "Largest increases",
            Self::LargestDecreases => "Largest decreases",
            Self::GrowingDirectories => "Fastest-growing directories (observed byte increase)",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Before,
    After,
}

#[derive(Clone, Debug)]
pub struct Observation {
    pub scan: ScanId,
    pub root: String,
    /// Both endpoints of the observation interval, including timezone.
    pub interval: String,
    pub lineage: String,
    pub policy: String,
    pub metric: String,
    pub coverage: String,
}

#[derive(Clone, Debug)]
pub struct Baseline {
    pub observation: Observation,
    /// An already-known incompatibility. Absence does not establish eligibility:
    /// the store must still validate published status, identity and semantics.
    pub unavailable_reason: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Amounts {
    /// Raw endpoint sizes on rows; comparable contributions on the summary.
    pub before: String,
    pub after: String,
    pub net: String,
    pub gross_added: String,
    pub gross_removed: String,
}

#[derive(Clone, Debug)]
pub struct Endpoint {
    pub path: String,
    pub can_copy: bool,
    /// A display guard only; the caller must validate the live endpoint again.
    pub can_reveal: bool,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub id: RowId,
    pub name: String,
    pub kind: String,
    pub amounts: Amounts,
    /// Include any Unknown, Newly observed, or Accounting redistribution status.
    pub status: String,
    pub coverage: String,
    pub before: Option<Endpoint>,
    pub after: Option<Endpoint>,
    pub can_open: bool,
    pub details: Vec<String>,
}

impl Row {
    pub fn endpoint(&self, side: Side) -> Option<&Endpoint> {
        match side {
            Side::Before => self.before.as_ref(),
            Side::After => self.after.as_ref(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Breadcrumb {
    pub id: RowId,
    pub label: String,
}

#[derive(Clone, Debug)]
pub struct Comparison {
    pub id: u64,
    pub before: Observation,
    pub after: Observation,
    pub amounts: Amounts,
    pub coverage: String,
    /// Full captured filter, independent of later explorer edits.
    pub filter: String,
}

#[derive(Clone, Copy, Debug)]
pub struct Build {
    pub request: u64,
    pub before: ScanId,
    pub after: ScanId,
}

#[derive(Clone, Debug, Default)]
pub struct Model {
    /// Advance on every navigation/request that invalidates an old callback.
    pub epoch: u64,
    pub current: Option<Observation>,
    /// A bounded page of published candidates supplied by the caller.
    pub baselines: Vec<Baseline>,
    pub selected_baseline: Option<ScanId>,
    pub baseline_next: Option<ScanId>,
    /// The full explorer filter captured when opening this panel.
    pub filter: String,
    pub building: Option<Build>,
    /// May retain a previous result while a new comparison builds.
    pub comparison: Option<Comparison>,
    pub view: View,
    pub rows: Vec<Row>,
    pub selected: Option<RowId>,
    /// Root first, current directory last. Only a bounded prefix is rendered.
    pub breadcrumbs: Vec<Breadcrumb>,
    pub next: Option<u64>,
    pub busy: bool,
    pub error: Option<String>,
    pub notice: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub epoch: u64,
    pub action: Action,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    BaselineSelected(ScanId),
    BaselineFirstPage,
    BaselineNextPage(ScanId),
    CompareRequested {
        before: ScanId,
        after: ScanId,
    },
    CancelRequested {
        request: u64,
    },
    CloseRequested,
    RowSelected {
        comparison: u64,
        row: RowId,
    },
    Open {
        comparison: u64,
        row: RowId,
    },
    Breadcrumb {
        comparison: u64,
        row: RowId,
    },
    UpRequested {
        comparison: u64,
    },
    FirstPage {
        comparison: u64,
    },
    NextPage {
        comparison: u64,
        cursor: u64,
    },
    ShowView {
        comparison: u64,
        view: View,
    },
    CopyRequested {
        comparison: u64,
        row: RowId,
        side: Side,
    },
    RevealRequested {
        comparison: u64,
        row: RowId,
        side: Side,
    },
}

impl Model {
    /// Invalidate queued callbacks immediately after requesting cancellation,
    /// while retaining the old result. Only the worker acknowledgement may clear
    /// `busy`; this helper does not itself signal cancellation or release pins.
    pub fn stop_build_pending_ack(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.building = None;
        self.busy = true;
    }

    pub fn event(&self, action: Action) -> Event {
        Event {
            epoch: self.epoch,
            action,
        }
    }

    pub fn selected_baseline(&self) -> Option<&Baseline> {
        self.baselines
            .iter()
            .take(MAX_BASELINES)
            .find(|baseline| Some(baseline.observation.scan) == self.selected_baseline)
    }

    pub fn selected_row(&self) -> Option<&Row> {
        self.rows
            .iter()
            .take(MAX_ROWS)
            .find(|row| Some(row.id) == self.selected)
    }

    fn result_is_current(&self, comparison: u64) -> bool {
        self.comparison
            .as_ref()
            .is_some_and(|ready| ready.id == comparison)
    }

    /// Only a UI guard. The controller/store remain authoritative. In particular,
    /// row IDs cannot be resolved against another comparison, or as scan NodeIds.
    pub fn allows(&self, event: Event) -> bool {
        if event.epoch != self.epoch {
            return false;
        }
        match event.action {
            Action::CloseRequested => return true,
            Action::CancelRequested { request } => {
                return self.building.is_some_and(|build| build.request == request);
            }
            _ => {}
        }
        if self.busy || self.building.is_some() {
            return false;
        }
        match event.action {
            Action::CloseRequested | Action::CancelRequested { .. } => false,
            Action::BaselineSelected(scan) => self
                .baselines
                .iter()
                .take(MAX_BASELINES)
                .any(|baseline| baseline.observation.scan == scan),
            Action::BaselineFirstPage => self.current.is_some(),
            Action::BaselineNextPage(scan) => {
                self.current.is_some() && self.baseline_next == Some(scan)
            }
            Action::CompareRequested { before, after } => {
                before != after
                    && self
                        .current
                        .as_ref()
                        .is_some_and(|current| current.scan == after)
                    && self.selected_baseline().is_some_and(|baseline| {
                        baseline.observation.scan == before && baseline.unavailable_reason.is_none()
                    })
            }
            Action::RowSelected { comparison, row } => {
                self.result_is_current(comparison)
                    && self.rows.iter().take(MAX_ROWS).any(|item| item.id == row)
            }
            Action::Open { comparison, row } => {
                self.result_is_current(comparison)
                    && self
                        .selected_row()
                        .is_some_and(|item| item.id == row && item.can_open)
            }
            Action::Breadcrumb { comparison, row } => {
                self.result_is_current(comparison)
                    && self
                        .breadcrumbs
                        .iter()
                        .take(MAX_BREADCRUMBS)
                        .take(self.breadcrumbs.len().saturating_sub(1))
                        .any(|crumb| crumb.id == row)
            }
            Action::UpRequested { comparison } => {
                self.result_is_current(comparison) && self.breadcrumbs.len() > 1
            }
            Action::FirstPage { comparison } | Action::ShowView { comparison, .. } => {
                self.result_is_current(comparison)
            }
            Action::NextPage { comparison, cursor } => {
                self.result_is_current(comparison) && self.next == Some(cursor)
            }
            Action::CopyRequested {
                comparison,
                row,
                side,
            } => {
                self.result_is_current(comparison)
                    && self
                        .selected_row()
                        .filter(|item| item.id == row)
                        .and_then(|item| item.endpoint(side))
                        .is_some_and(|endpoint| endpoint.can_copy)
            }
            Action::RevealRequested {
                comparison,
                row,
                side,
            } => {
                self.result_is_current(comparison)
                    && self
                        .selected_row()
                        .filter(|item| item.id == row)
                        .and_then(|item| item.endpoint(side))
                        .is_some_and(|endpoint| endpoint.can_reveal)
            }
        }
    }
}

/// Bound and escape cached labels, including newline/terminal/bidi controls.
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

fn amounts_text(amounts: &Amounts) -> String {
    format!(
        "Comparable before contribution: {} · Comparable after contribution: {} · Net: {} · Gross additions: {} · Gross removals: {}",
        display_text(&amounts.before),
        display_text(&amounts.after),
        display_text(&amounts.net),
        display_text(&amounts.gross_added),
        display_text(&amounts.gross_removed)
    )
}

fn row_amounts_text(amounts: &Amounts) -> String {
    format!(
        "Raw cached before: {} · Raw cached after: {} · Comparable net: {} · Gross additions: {} · Gross removals: {}",
        display_text(&amounts.before),
        display_text(&amounts.after),
        display_text(&amounts.net),
        display_text(&amounts.gross_added),
        display_text(&amounts.gross_removed)
    )
}

fn observation_text(side: &str, observation: &Observation) -> String {
    format!(
        "{side}: scan {} · Root: {}\nObservation interval: {}\nLineage: {} · Policy: {} · Metric: {}\nCoverage: {}",
        observation.scan.get(),
        display_text(&observation.root),
        display_text(&observation.interval),
        display_text(&observation.lineage),
        display_text(&observation.policy),
        display_text(&observation.metric),
        display_text(&observation.coverage)
    )
}

// GPUI rendering is intentionally separate from the pure display/guard model.
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
        // Pinned GPUI maps Enter/Space keyup to on_click for focused controls.
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
    let allowed = |action| {
        let event = model.event(action);
        model.allows(event).then_some(event)
    };
    let mut panel = div().flex().flex_col().gap_2().p_2()
        .child("Changes · local comparison of published observations")
        .child("Tab / Shift-Tab to focus controls; Enter or Space to activate. Select a baseline, then explicitly compare.")
        .child("Matches before or after")
        .child(format!("Captured filter: {}. Return to the explorer to edit filters for a new comparison.", display_text(&model.filter)))
        .child("Observations cover intervals, not atomic filesystem snapshots. Matching uses raw relative paths; renames appear as removed + added. No trend forecasting.");
    if let Some(current) = &model.current {
        panel = panel.child(observation_text("Comparison target", current));
    } else {
        panel = panel.child("Choose a published scan in the explorer to compare.");
    }
    if let Some(notice) = &model.notice {
        panel = panel.child(format!("Comparison notice: {}", display_text(notice)));
    }
    if let Some(error) = &model.error {
        panel = panel.child(format!("Comparison error: {}", display_text(error)));
    }
    panel = panel.child(format!("Published baseline candidates (at most {MAX_BASELINES} per page). Volume, directory identity, lineage, policy and metric compatibility are validated before comparison."));
    let mut baselines = div()
        .id("changes-baselines")
        .max_h(px(140.))
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_1();
    for (index, baseline) in model.baselines.iter().take(MAX_BASELINES).enumerate() {
        let observation = &baseline.observation;
        let selected = model.selected_baseline == Some(observation.scan);
        let label = format!(
            "{}Scan {} · {} · Interval: {} · Coverage: {}{}",
            if selected { "Selected baseline: " } else { "" },
            observation.scan.get(),
            display_text(&observation.root),
            display_text(&observation.interval),
            display_text(&observation.coverage),
            baseline
                .unavailable_reason
                .as_ref()
                .map(|reason| format!(" · Unavailable: {}", display_text(reason)))
                .unwrap_or_default()
        );
        baselines = baselines.child(
            button(
                ("changes-baseline", index),
                label,
                allowed(Action::BaselineSelected(observation.scan)),
                cx,
                on_event,
            )
            .bg(rgb(if selected { 0x366984 } else { 0x17212b })),
        );
    }
    if model.baselines.is_empty() && !model.busy {
        baselines = baselines.child("No published baseline candidates on this page. Keep or create another compatible finished scan.");
    }
    let compare = model
        .selected_baseline()
        .zip(model.current.as_ref())
        .and_then(|(before, after)| {
            allowed(Action::CompareRequested {
                before: before.observation.scan,
                after: after.scan,
            })
        });
    panel = panel.child(baselines).child(
        div()
            .flex()
            .flex_wrap()
            .gap_2()
            .child(button(
                "changes-baseline-first",
                "First baseline page".into(),
                allowed(Action::BaselineFirstPage),
                cx,
                on_event,
            ))
            .child(button(
                "changes-baseline-next",
                "Next baseline page".into(),
                model
                    .baseline_next
                    .and_then(|next| allowed(Action::BaselineNextPage(next))),
                cx,
                on_event,
            ))
            .child(button(
                "changes-compare",
                "Compare selected baseline".into(),
                compare,
                cx,
                on_event,
            ))
            .child(button(
                "changes-cancel",
                "Cancel comparison".into(),
                model.building.and_then(|build| {
                    allowed(Action::CancelRequested {
                        request: build.request,
                    })
                }),
                cx,
                on_event,
            ))
            .child(button(
                "changes-close",
                "Close Changes".into(),
                allowed(Action::CloseRequested),
                cx,
                on_event,
            )),
    );
    if let Some(baseline) = model.selected_baseline() {
        panel = panel.child(observation_text("Selected baseline", &baseline.observation));
    }
    if let Some(build) = model.building {
        panel = panel.child(format!("Building requested comparison: scan {} → scan {}. The current explorer remains available; any result below is the previously displayed comparison.", build.before.get(), build.after.get()));
    } else if model.busy {
        panel = panel.child("Loading comparison metadata or a bounded page…");
    }
    let Some(comparison) = &model.comparison else {
        return panel;
    };
    let comparison_id = comparison.id;
    panel = panel.child(format!("Displayed comparison: scan {} → scan {}", comparison.before.scan.get(), comparison.after.scan.get()))
        .child(observation_text("Before", &comparison.before))
        .child(observation_text("After", &comparison.after))
        .child(format!("Matches before or after · Displayed comparison filter: {}", display_text(&comparison.filter)))
        .child(amounts_text(&comparison.amounts))
        .child(format!("Comparison coverage: {}", display_text(&comparison.coverage)))
        .child("Unknown: incomplete evidence; missing entries are not necessarily removals. Newly observed: now visible, not proven newly created. Accounting redistribution: charged ownership changed; this alone is not growth. A mixed directory can also have the known gross growth or removal shown alongside this warning.")
        .child("Snapshots store metadata only. Reveal before/after opens whatever exists at that recorded location now, after root and existence checks; it does not restore historical file contents.")
        .child("Raw cached before/after sizes are unfiltered endpoint snapshot totals. Net and gross values use filter-matched, comparable contributions; accounting redistribution can make these differ from raw size differences.")
        .child("Net and gross values are separate: offsetting additions and removals can leave net change at zero. Sizes are observed included bytes, not reclaimable space.");
    let mut views = div().flex().flex_wrap().gap_2();
    for (index, view) in [
        View::Hierarchy,
        View::LargestIncreases,
        View::LargestDecreases,
        View::GrowingDirectories,
    ]
    .into_iter()
    .enumerate()
    {
        views = views.child(
            button(
                ("changes-view", index),
                view.label().into(),
                allowed(Action::ShowView {
                    comparison: comparison_id,
                    view,
                }),
                cx,
                on_event,
            )
            .bg(rgb(if model.view == view {
                0x366984
            } else {
                0x17212b
            })),
        );
    }
    panel = panel
        .child(views)
        .child(format!("View: {}", model.view.label()));
    let mut breadcrumbs = div().flex().flex_wrap().gap_1();
    for (index, crumb) in model.breadcrumbs.iter().take(MAX_BREADCRUMBS).enumerate() {
        breadcrumbs = breadcrumbs.child(button(
            ("changes-breadcrumb", index),
            display_text(&crumb.label),
            allowed(Action::Breadcrumb {
                comparison: comparison_id,
                row: crumb.id,
            }),
            cx,
            on_event,
        ));
    }
    if model.breadcrumbs.len() > MAX_BREADCRUMBS {
        breadcrumbs = breadcrumbs.child("… (remaining breadcrumbs omitted; Up remains available)");
    }
    panel = panel.child(breadcrumbs);
    let mut rows = div()
        .id("changes-rows")
        .max_h(px(260.))
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_1();
    for (index, row) in model.rows.iter().take(MAX_ROWS).enumerate() {
        let selected = model.selected == Some(row.id);
        let label = format!(
            "{}{} · {}\n{}\nStatus: {} · Coverage: {}",
            if selected { "Selected: " } else { "" },
            display_text(&row.name),
            display_text(&row.kind),
            row_amounts_text(&row.amounts),
            display_text(&row.status),
            display_text(&row.coverage)
        );
        rows = rows.child(
            button(
                ("changes-row", index),
                label,
                allowed(Action::RowSelected {
                    comparison: comparison_id,
                    row: row.id,
                }),
                cx,
                on_event,
            )
            .bg(rgb(if selected { 0x366984 } else { 0x17212b })),
        );
    }
    if model.rows.is_empty() && !model.busy && model.building.is_none() {
        rows = rows.child("No comparison entries on this page. Unknown coverage can still prevent a complete result.");
    }
    panel = panel
        .child(rows)
        .child(format!("At most {MAX_ROWS} comparison rows per page."))
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .child(button(
                    "changes-up",
                    "Up".into(),
                    allowed(Action::UpRequested {
                        comparison: comparison_id,
                    }),
                    cx,
                    on_event,
                ))
                .child(button(
                    "changes-open",
                    "Open selected comparison folder".into(),
                    model.selected.and_then(|row| {
                        allowed(Action::Open {
                            comparison: comparison_id,
                            row,
                        })
                    }),
                    cx,
                    on_event,
                ))
                .child(button(
                    "changes-first",
                    "First comparison page".into(),
                    allowed(Action::FirstPage {
                        comparison: comparison_id,
                    }),
                    cx,
                    on_event,
                ))
                .child(button(
                    "changes-next",
                    "Next comparison page".into(),
                    model.next.and_then(|cursor| {
                        allowed(Action::NextPage {
                            comparison: comparison_id,
                            cursor,
                        })
                    }),
                    cx,
                    on_event,
                )),
        );
    if let Some(row) = model.selected_row() {
        panel = panel
            .child(format!(
                "Selected comparison entry: {}",
                display_text(&row.name)
            ))
            .child(row_amounts_text(&row.amounts))
            .child(format!(
                "Status: {} · Coverage: {}",
                display_text(&row.status),
                display_text(&row.coverage)
            ));
        for (index, (side, label)) in [(Side::Before, "Before"), (Side::After, "After")]
            .into_iter()
            .enumerate()
        {
            panel = panel
                .child(format!(
                    "{label} path: {}",
                    row.endpoint(side)
                        .map(|endpoint| display_text(&endpoint.path))
                        .unwrap_or_else(|| "No observed endpoint".into())
                ))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(button(
                            ("changes-copy", index),
                            format!("Copy {label} path"),
                            allowed(Action::CopyRequested {
                                comparison: comparison_id,
                                row: row.id,
                                side,
                            }),
                            cx,
                            on_event,
                        ))
                        .child(button(
                            ("changes-reveal", index),
                            format!("Reveal {label} path"),
                            allowed(Action::RevealRequested {
                                comparison: comparison_id,
                                row: row.id,
                                side,
                            }),
                            cx,
                            on_event,
                        )),
                );
        }
        for detail in row.details.iter().take(MAX_DETAILS) {
            panel = panel.child(display_text(detail));
        }
        if row.details.len() > MAX_DETAILS {
            panel = panel.child("Additional detail lines omitted from this bounded view.");
        }
    }
    panel
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(id: u64) -> ScanId {
        ScanId::new(id).unwrap()
    }
    fn observation(id: u64) -> Observation {
        Observation {
            scan: scan(id),
            root: "root".into(),
            interval: "start → end UTC".into(),
            lineage: "lineage".into(),
            policy: "policy".into(),
            metric: "allocated bytes".into(),
            coverage: "complete".into(),
        }
    }
    fn row(id: u64) -> Row {
        Row {
            id: RowId(id),
            name: "folder".into(),
            kind: "directory".into(),
            amounts: Amounts::default(),
            status: "Unknown".into(),
            coverage: "incomplete".into(),
            before: Some(Endpoint {
                path: "/before".into(),
                can_copy: true,
                can_reveal: false,
            }),
            after: Some(Endpoint {
                path: "/after".into(),
                can_copy: true,
                can_reveal: true,
            }),
            can_open: true,
            details: vec![],
        }
    }
    fn model() -> Model {
        Model {
            epoch: 7,
            current: Some(observation(2)),
            baselines: vec![Baseline {
                observation: observation(1),
                unavailable_reason: None,
            }],
            selected_baseline: Some(scan(1)),
            baseline_next: Some(scan(3)),
            comparison: Some(Comparison {
                id: 10,
                before: observation(1),
                after: observation(2),
                amounts: Amounts::default(),
                coverage: "incomplete".into(),
                filter: "all entries".into(),
            }),
            rows: vec![row(11)],
            selected: Some(RowId(11)),
            breadcrumbs: vec![
                Breadcrumb {
                    id: RowId(10),
                    label: "root".into(),
                },
                Breadcrumb {
                    id: RowId(11),
                    label: "folder".into(),
                },
            ],
            next: Some(12),
            ..Default::default()
        }
    }
    fn allows(model: &Model, action: Action) -> bool {
        model.allows(model.event(action))
    }

    #[test]
    fn baseline_must_be_selected_visible_distinct_and_not_known_incompatible() {
        let mut m = model();
        let compare = Action::CompareRequested {
            before: scan(1),
            after: scan(2),
        };
        assert!(allows(&m, compare));
        assert!(!allows(
            &m,
            Action::CompareRequested {
                before: scan(1),
                after: scan(3)
            }
        ));
        m.baselines[0].unavailable_reason =
            Some("Policy changed; rescan with matching policy".into());
        assert!(!allows(&m, compare));
        m.baselines[0].unavailable_reason = None;
        m.selected_baseline = None;
        assert!(!allows(&m, compare));
        m.baselines = (1..=101)
            .map(|id| Baseline {
                observation: observation(id),
                unavailable_reason: None,
            })
            .collect();
        m.selected_baseline = Some(scan(101));
        assert!(m.selected_baseline().is_none());
        assert!(!allows(&m, Action::BaselineSelected(scan(101))));
        m.selected_baseline = Some(scan(2));
        assert!(!allows(
            &m,
            Action::CompareRequested {
                before: scan(2),
                after: scan(2)
            }
        ));
    }

    #[test]
    fn stale_epoch_and_other_comparison_ids_cannot_act() {
        let mut m = model();
        let event = m.event(Action::RowSelected {
            comparison: 10,
            row: RowId(11),
        });
        assert!(m.allows(event));
        m.epoch += 1;
        assert!(!m.allows(event));
        assert!(!m.allows(Event {
            epoch: 7,
            action: Action::CloseRequested
        }));
        for action in [
            Action::RowSelected {
                comparison: 9,
                row: RowId(11),
            },
            Action::Open {
                comparison: 9,
                row: RowId(11),
            },
            Action::UpRequested { comparison: 9 },
            Action::FirstPage { comparison: 9 },
            Action::NextPage {
                comparison: 9,
                cursor: 12,
            },
            Action::ShowView {
                comparison: 9,
                view: View::GrowingDirectories,
            },
        ] {
            assert!(!allows(&m, action));
        }
    }

    #[test]
    fn building_keeps_old_result_but_only_current_cancel_and_close_are_enabled() {
        let mut m = model();
        m.building = Some(Build {
            request: 20,
            before: scan(1),
            after: scan(2),
        });
        m.busy = true;
        assert!(m.comparison.is_some());
        assert!(allows(&m, Action::CancelRequested { request: 20 }));
        assert!(!allows(&m, Action::CancelRequested { request: 19 }));
        assert!(allows(&m, Action::CloseRequested));
        for action in [
            Action::BaselineSelected(scan(1)),
            Action::BaselineFirstPage,
            Action::BaselineNextPage(scan(3)),
            Action::CompareRequested {
                before: scan(1),
                after: scan(2),
            },
            Action::RowSelected {
                comparison: 10,
                row: RowId(11),
            },
            Action::FirstPage { comparison: 10 },
        ] {
            assert!(!allows(&m, action));
        }
        m.building = None;
        assert!(!allows(&m, Action::CancelRequested { request: 20 }));
        assert!(allows(&m, Action::CloseRequested));
        assert!(!allows(&m, Action::BaselineFirstPage));
    }

    #[test]
    fn cancelled_build_waits_for_ack_before_retained_result_can_navigate() {
        let mut m = model();
        let actions = [
            Action::RowSelected {
                comparison: 10,
                row: RowId(11),
            },
            Action::Open {
                comparison: 10,
                row: RowId(11),
            },
            Action::Breadcrumb {
                comparison: 10,
                row: RowId(10),
            },
            Action::UpRequested { comparison: 10 },
            Action::FirstPage { comparison: 10 },
            Action::NextPage {
                comparison: 10,
                cursor: 12,
            },
            Action::ShowView {
                comparison: 10,
                view: View::Hierarchy,
            },
            Action::ShowView {
                comparison: 10,
                view: View::LargestIncreases,
            },
            Action::ShowView {
                comparison: 10,
                view: View::LargestDecreases,
            },
            Action::ShowView {
                comparison: 10,
                view: View::GrowingDirectories,
            },
        ];
        let queued_events = actions.map(|action| m.event(action));
        assert!(queued_events.iter().all(|event| m.allows(*event)));
        m.building = Some(Build {
            request: 20,
            before: scan(1),
            after: scan(2),
        });
        m.busy = false; // The transition must set busy, not depend on its old value.
        let cancelled_epoch = m.epoch;
        m.stop_build_pending_ack();
        assert_eq!(m.epoch, cancelled_epoch + 1);
        assert!(m.building.is_none());
        assert!(m.busy);
        assert_eq!(m.comparison.as_ref().unwrap().id, 10);
        assert_eq!(m.selected_row().unwrap().id, RowId(11));
        assert_eq!(m.next, Some(12));
        assert_eq!(m.breadcrumbs.len(), 2);
        for action in actions {
            assert!(!allows(&m, action));
        }
        assert!(allows(&m, Action::CloseRequested));
        assert!(!allows(&m, Action::CancelRequested { request: 20 }));
        assert!(queued_events.iter().all(|event| !m.allows(*event)));
        m.busy = false; // Simulate acknowledgement from the cancelled worker.
        for action in actions {
            assert!(allows(&m, action));
        }
        assert!(queued_events.iter().all(|event| !m.allows(*event)));
    }

    #[test]
    fn pages_rows_and_breadcrumbs_are_bounded_and_guarded() {
        let mut m = model();
        assert!(allows(&m, Action::BaselineNextPage(scan(3))));
        assert!(!allows(&m, Action::BaselineNextPage(scan(4))));
        assert!(allows(
            &m,
            Action::NextPage {
                comparison: 10,
                cursor: 12
            }
        ));
        assert!(!allows(
            &m,
            Action::NextPage {
                comparison: 10,
                cursor: 13
            }
        ));
        assert!(allows(
            &m,
            Action::Breadcrumb {
                comparison: 10,
                row: RowId(10)
            }
        ));
        assert!(!allows(
            &m,
            Action::Breadcrumb {
                comparison: 10,
                row: RowId(11)
            }
        ));
        assert!(allows(&m, Action::UpRequested { comparison: 10 }));
        m.breadcrumbs.truncate(1);
        assert!(!allows(&m, Action::UpRequested { comparison: 10 }));
        m.breadcrumbs = (1..=66)
            .map(|id| Breadcrumb {
                id: RowId(id),
                label: "folder".into(),
            })
            .collect();
        assert!(!allows(
            &m,
            Action::Breadcrumb {
                comparison: 10,
                row: RowId(65)
            }
        ));
        m.rows = (1..=101).map(row).collect();
        m.selected = Some(RowId(101));
        assert!(m.selected_row().is_none());
        assert!(!allows(
            &m,
            Action::RowSelected {
                comparison: 10,
                row: RowId(101)
            }
        ));
        assert!(!allows(
            &m,
            Action::Open {
                comparison: 10,
                row: RowId(101)
            }
        ));
    }

    #[test]
    fn endpoint_requests_need_selected_visible_row_and_available_side() {
        let mut m = model();
        let copy = |side| Action::CopyRequested {
            comparison: 10,
            row: RowId(11),
            side,
        };
        let reveal = |side| Action::RevealRequested {
            comparison: 10,
            row: RowId(11),
            side,
        };
        assert!(allows(&m, copy(Side::Before)));
        assert!(!allows(&m, reveal(Side::Before)));
        assert!(allows(&m, reveal(Side::After)));
        m.rows[0].after = None;
        assert!(!allows(&m, copy(Side::After)));
        assert!(!allows(&m, reveal(Side::After)));
        m.selected = None;
        assert!(!allows(&m, copy(Side::Before)));
        m.selected = Some(RowId(11));
        m.rows[0].can_open = false;
        assert!(!allows(
            &m,
            Action::Open {
                comparison: 10,
                row: RowId(11)
            }
        ));
    }

    #[test]
    fn display_keeps_net_gross_intervals_policy_metric_and_unknowns_explicit() {
        let text = amounts_text(&Amounts {
            before: "100 B".into(),
            after: "100 B".into(),
            net: "0 B".into(),
            gross_added: "40 B".into(),
            gross_removed: "40 B".into(),
        });
        assert!(text.contains("Net: 0 B"));
        assert!(text.contains("Comparable before contribution: 100 B"));
        let row_text = row_amounts_text(&Amounts {
            before: "200 B".into(),
            after: "100 B".into(),
            net: "0 B".into(),
            ..Default::default()
        });
        assert!(row_text.contains("Raw cached before: 200 B"));
        assert!(row_text.contains("Raw cached after: 100 B"));
        assert!(row_text.contains("Comparable net: 0 B"));
        assert!(text.contains("Gross additions: 40 B"));
        assert!(text.contains("Gross removals: 40 B"));
        let mut observed = observation(1);
        observed.coverage = "Unknown".into();
        let text = observation_text("Before", &observed);
        for label in [
            "Root: root",
            "Observation interval: start → end UTC",
            "Lineage: lineage",
            "Policy: policy",
            "Metric: allocated bytes",
            "Coverage: Unknown",
        ] {
            assert!(text.contains(label));
        }
        assert!(
            View::GrowingDirectories
                .label()
                .contains("observed byte increase")
        );
    }

    #[test]
    fn bounded_unicode_labels_escape_controls_without_splitting_characters() {
        assert_eq!(
            display_text("a\n\u{202e}b\u{1b}"),
            "a\\u{a}\\u{202e}b\\u{1b}"
        );
        assert_eq!(
            display_text(&"é".repeat(MAX_TEXT_CHARS)),
            "é".repeat(MAX_TEXT_CHARS)
        );
        assert_eq!(
            display_text(&"é".repeat(MAX_TEXT_CHARS + 1)),
            format!("{}…", "é".repeat(MAX_TEXT_CHARS))
        );
    }
}
