//! Disk dashboard presentation. The controller supplies bounded cached data;
//! this module never opens a database, walks a source, or owns a snapshot pin.
use crate::{disk_model::CardId, treemap};
use gpui::{
    Context, Div, Role, SharedString, Stateful, Window, div, prelude::*, px, relative, rgb,
};
use space_core::ScanId;

pub const CARDS_PER_PAGE: usize = 8;
pub const MAX_CARDS: usize = 4096;

pub fn format_bytes(bytes: u64) -> String {
    treemap::format_bytes(u128::from(bytes))
}

/// A rendered callback is meaningful only for this exact card and publication.
/// The epoch also rejects old preview callbacks within the same publication.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ticket {
    pub card: CardId,
    pub scan: Option<ScanId>,
    pub query_epoch: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Idle,
    Queued,
    Scanning {
        entries: u64,
    },
    Stopping,
    Ready,
    Partial,
    Failed,
    Cancelled,
    Offline,
}
impl Status {
    pub fn label(self) -> String {
        match self {
            Self::Idle => "Not scanned yet".into(),
            Self::Queued => "Queued".into(),
            Self::Scanning { entries } => format!("Scanning · {entries} entries observed"),
            Self::Stopping => "Stopping…".into(),
            Self::Ready => "Ready".into(),
            Self::Partial => "Some locations could not be read".into(),
            Self::Failed => "Scan failed".into(),
            Self::Cancelled => "Scan stopped".into(),
            Self::Offline => "Offline · cached scan".into(),
        }
    }
    fn color(self) -> u32 {
        match self {
            Self::Scanning { .. } | Self::Queued | Self::Stopping => 0xb3daf4,
            Self::Partial | Self::Failed | Self::Cancelled => 0xf2cf91,
            Self::Ready => 0xb5d9c4,
            Self::Idle | Self::Offline => 0xa7b9c8,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capacity {
    pub total: u64,
    pub free: u64,
}
impl Capacity {
    pub fn used_fraction(self) -> f32 {
        if self.total == 0 {
            0.
        } else {
            (self.total.saturating_sub(self.free) as f64 / self.total as f64) as f32
        }
    }
    pub fn label(self) -> String {
        format!(
            "{} available of {}",
            treemap::format_bytes(u128::from(self.free)),
            treemap::format_bytes(u128::from(self.total))
        )
    }
}

#[derive(Clone, Debug)]
pub struct Card {
    pub ticket: Ticket,
    pub name: String,
    pub location: String,
    /// "Disk" or "Saved folder", supplied from actual target classification.
    pub kind: String,
    pub connected: bool,
    pub capacity: Option<Capacity>,
    pub status: Status,
    pub last_scan: Option<String>,
    pub cached_total: String,
    pub preview: Option<treemap::Model>,
    pub top_items: Vec<(String, String)>,
    pub issue_summary: Option<String>,
    pub can_rescan: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Model {
    /// Metadata may include offscreen targets; only the visible eight should
    /// have loaded previews. Controller query/pin budgets remain authoritative.
    pub cards: Vec<Card>,
    pub page: usize,
    pub included_targets: usize,
    pub discovering: bool,
    pub unavailable: bool,
    pub busy: bool,
    pub stopping: bool,
    pub batch_summary: Option<String>,
    pub notice: Option<String>,
    pub metric_label: String,
    pub discovery_issues: Vec<String>,
    pub discovery_details_visible: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    ScanAll,
    Stop,
    ScanFolder,
    RefreshDiscovery,
    Cache,
    ToggleDiscoveryDetails,
    Page(usize),
    Explore(Ticket),
    Rescan(Ticket),
    Details(Ticket),
    Map(Ticket, treemap::Event),
}

impl Model {
    pub fn page_count(&self) -> usize {
        self.cards
            .len()
            .min(MAX_CARDS)
            .div_ceil(CARDS_PER_PAGE)
            .max(1)
    }
    /// Stable partition, preserving the controller's order within both groups.
    pub fn visible_cards(&self) -> Vec<&Card> {
        self.cards
            .iter()
            .take(MAX_CARDS)
            .filter(|card| card.connected)
            .chain(
                self.cards
                    .iter()
                    .take(MAX_CARDS)
                    .filter(|card| !card.connected),
            )
            .skip(self.page.min(self.page_count() - 1) * CARDS_PER_PAGE)
            .take(CARDS_PER_PAGE)
            .collect()
    }
    pub fn allows(&self, event: Event) -> bool {
        if self.unavailable
            && matches!(
                event,
                Event::ScanAll
                    | Event::ScanFolder
                    | Event::RefreshDiscovery
                    | Event::Cache
                    | Event::Rescan(_)
            )
        {
            return false;
        }
        match event {
            Event::ScanAll => !self.busy && !self.discovering && self.included_targets > 0,
            Event::Stop => self.busy && !self.stopping,
            Event::ScanFolder => !self.busy && !self.discovering,
            Event::RefreshDiscovery => !self.discovering && !self.busy,
            Event::Cache => !self.busy,
            Event::ToggleDiscoveryDetails => !self.discovery_issues.is_empty(),
            Event::Page(page) => page < self.page_count(),
            Event::Explore(ticket)
            | Event::Rescan(ticket)
            | Event::Details(ticket)
            | Event::Map(ticket, _) => {
                let Some(card) = self
                    .visible_cards()
                    .into_iter()
                    .find(|card| card.ticket == ticket)
                else {
                    return false;
                };
                match event {
                    Event::Explore(_) => card.ticket.scan.is_some() && card.preview.is_some(),
                    Event::Rescan(_) => {
                        !self.busy && !self.discovering && card.connected && card.can_rescan
                    }
                    Event::Details(_) => card.issue_summary.is_some(),
                    Event::Map(_, action) => {
                        card.ticket.scan.is_some()
                            && card.preview.as_ref().is_some_and(|preview| match action {
                                treemap::Event::OpenOther => true,
                                treemap::Event::Select(node) => preview
                                    .items
                                    .iter()
                                    .take(treemap::MAX_TILES)
                                    .any(|item| item.node == node),
                                treemap::Event::Open(node) => preview
                                    .items
                                    .iter()
                                    .take(treemap::MAX_TILES)
                                    .any(|item| item.node == node && item.is_directory),
                            })
                    }
                    _ => false,
                }
            }
        }
    }
}

/// Production sizing policy, also tested without a running native window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub columns: usize,
    pub card_width: f32,
    pub preview_height: f32,
}
pub fn layout(width: f32, height: f32, count: usize) -> Layout {
    let width = if width.is_finite() { width.max(0.) } else { 0. };
    let height = if height.is_finite() {
        height.max(0.)
    } else {
        0.
    };
    let columns = if count > 1 && width >= 1060. { 2 } else { 1 };
    Layout {
        columns,
        card_width: ((width - 48. - (columns - 1) as f32 * 20.) / columns as f32).max(0.),
        preview_height: if count == 1 {
            (height - 380.).clamp(220., 620.)
        } else {
            240.
        },
    }
}

fn card_box(width: f32) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_3()
        .w(px(width))
        .min_w_0()
        .flex_shrink_0()
        .p_4()
        .rounded_lg()
        .border_1()
        .border_color(rgb(0x304352))
        .bg(rgb(0x1b2935))
}

pub(crate) fn button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    enabled: bool,
    primary: bool,
) -> Stateful<Div> {
    let label = label.into();
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label.clone())
        .focusable()
        .tab_stop(enabled)
        .border_1()
        .rounded_md()
        .px_3()
        .py_2()
        .border_color(rgb(if primary { 0x6fafd0 } else { 0x405463 }))
        .bg(rgb(if primary { 0x315f78 } else { 0x233441 }))
        .text_color(rgb(if enabled { 0xf0f5fa } else { 0x8093a3 }))
        .text_sm()
        .when(enabled, |element| {
            element
                .cursor_pointer()
                .hover(|style| style.bg(rgb(0x375267)))
        })
        .focus(|style| style.border_color(rgb(0xc3e8ff)))
        .child(label)
}

fn action_button<T: 'static>(
    model: &Model,
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    event: Event,
    primary: bool,
    cx: &mut Context<T>,
    on_event: fn(&mut T, Event, &mut Window, &mut Context<T>),
) -> Stateful<Div> {
    let enabled = model.allows(event);
    button(id, label, enabled, primary).on_click(cx.listener(move |this, _, window, cx| {
        if enabled {
            on_event(this, event, window, cx);
        }
    }))
}

pub fn view<T: 'static>(
    model: &Model,
    width: f32,
    height: f32,
    cx: &mut Context<T>,
    on_event: fn(&mut T, Event, &mut Window, &mut Context<T>),
) -> Div {
    let visible = model.visible_cards();
    let layout = layout(width, height, model.cards.len());
    let primary = if model.busy {
        Event::Stop
    } else {
        Event::ScanAll
    };
    let primary_label = if model.stopping {
        "Stopping…"
    } else if model.busy {
        "Stop"
    } else if model.cards.iter().any(|card| card.ticket.scan.is_some()) {
        "Rescan all disks"
    } else {
        "Scan all disks"
    };
    let mut header = div()
        .flex()
        .flex_col()
        .gap_3()
        .flex_shrink_0()
        .p_4()
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap_3()
                .child(div().text_xl().child("Space"))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        // The primary action is first in keyboard focus order.
                        .child(action_button(
                            model,
                            "scan-all",
                            primary_label,
                            primary,
                            true,
                            cx,
                            on_event,
                        ))
                        .child(action_button(
                            model,
                            "scan-folder",
                            "Scan folder",
                            Event::ScanFolder,
                            false,
                            cx,
                            on_event,
                        )),
                ),
        )
        .child(
            div()
                .text_sm()
                .text_color(rgb(0xa7b9c8))
                .child(if model.discovering {
                    "Finding local disks…".into()
                } else {
                    format!(
                        "{} local {} included · external drives included when supported",
                        model.included_targets,
                        if model.included_targets == 1 {
                            "target"
                        } else {
                            "targets"
                        }
                    )
                }),
        );
    if let Some(summary) = &model.batch_summary {
        header = header.child(div().text_sm().child(summary.clone()));
    }
    if let Some(notice) = &model.notice {
        header = header.child(
            div()
                .text_sm()
                .text_color(rgb(0xf2cf91))
                .child(notice.clone()),
        );
    }
    if model.busy {
        header = header.child(
            div()
                .text_xs()
                .text_color(rgb(0xa7b9c8))
                .child("Scan folder and rescans are available when this batch finishes or stops."),
        );
    }
    header = header.child(
        div()
            .flex()
            .flex_wrap()
            .gap_2()
            .child(action_button(
                model,
                "refresh-disks",
                "Refresh disks",
                Event::RefreshDiscovery,
                false,
                cx,
                on_event,
            ))
            .child(action_button(
                model,
                "dashboard-cache",
                "Cache & diagnostics",
                Event::Cache,
                false,
                cx,
                on_event,
            ))
            .when(!model.discovery_issues.is_empty(), |element| {
                element.child(action_button(
                    model,
                    "discovery-details",
                    format!(
                        "{} discovery notices · Details",
                        model.discovery_issues.len()
                    ),
                    Event::ToggleDiscoveryDetails,
                    false,
                    cx,
                    on_event,
                ))
            }),
    );
    if model.discovery_details_visible {
        header = header.child(
            div()
                .id("discovery-issues")
                .max_h(px(120.))
                .overflow_y_scroll()
                .text_sm()
                .children(
                    model
                        .discovery_issues
                        .iter()
                        .take(64)
                        .map(|issue| div().child(issue.clone())),
                ),
        );
    }
    let mut grid = div()
        .id("disk-cards")
        .flex()
        .flex_wrap()
        .items_start()
        .gap_5()
        .p_4()
        .min_w_0()
        .min_h_0()
        .flex_1()
        .overflow_y_scroll();
    if visible.is_empty() {
        grid = grid.child(
            div()
                .w_full()
                .p_5()
                .rounded_lg()
                .bg(rgb(0x1b2935))
                .flex()
                .flex_col()
                .gap_3()
                .child(div().text_lg().child(if model.discovering {
                    "Finding your disks"
                } else {
                    "No supported local disks found"
                }))
                .child(if model.discovering {
                    "Reading mount information. Your files are not being scanned."
                } else {
                    "Try Refresh disks after connecting a drive, or scan a folder you can access."
                }),
        );
    }
    let mut offline_heading = false;
    for card in visible {
        if !card.connected && !offline_heading {
            grid = grid.child(div().w_full().text_lg().child("Offline saved scans"));
            offline_heading = true;
        }
        let ticket = card.ticket;
        let mut panel = card_box(layout.card_width)
            .id(("disk-card", ticket.card.0 as usize))
            .child(
                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w_0()
                            .gap_1()
                            .child(div().text_lg().line_clamp(2).child(card.name.clone()))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(0xa7b9c8))
                                    .line_clamp(2)
                                    .child(format!("{} · {}", card.kind, card.location)),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(card.status.color()))
                            .child(card.status.label()),
                    ),
            );
        if let Some(capacity) = card.capacity.filter(|_| card.connected) {
            panel = panel
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(0xa7b9c8))
                                .child(capacity.label()),
                        )
                        .child(
                            div()
                                .h(px(6.))
                                .w_full()
                                .rounded_md()
                                .overflow_hidden()
                                .bg(rgb(0x344656))
                                .child(
                                    div()
                                        .h_full()
                                        .w(relative(capacity.used_fraction()))
                                        .bg(rgb(0x7ba8bc)),
                                ),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(0xa7b9c8))
                        .child("OS capacity · separate from observed file sizes"),
                );
        } else if card.connected && card.kind == "Disk" {
            panel = panel.child(
                div()
                    .text_xs()
                    .text_color(rgb(0xa7b9c8))
                    .child("Disk capacity unavailable"),
            );
        }
        if let Some(preview) = &card.preview {
            panel = panel
                .child(
                    div()
                        .text_sm()
                        .child(format!("{} · {}", card.cached_total, model.metric_label)),
                )
                .child(treemap::view_sized(
                    preview,
                    None,
                    treemap::Size::Height(layout.preview_height),
                    cx,
                    move |this, event, window, cx| {
                        on_event(this, Event::Map(ticket, event), window, cx)
                    },
                ));
        } else {
            let empty = if ticket.scan.is_some() {
                "Loading cached map…"
            } else {
                match card.status {
                    Status::Scanning { .. } => {
                        "Building the first scan. The map appears after it is saved."
                    }
                    Status::Queued => "Waiting for its turn to scan.",
                    Status::Stopping | Status::Cancelled => {
                        "Stopped before a completed scan was saved."
                    }
                    Status::Failed => "No completed scan yet. Retry to see what is taking space.",
                    _ => "Scan to see what is taking space.",
                }
            };
            panel = panel.child(
                div()
                    .h(px(layout.preview_height))
                    .p_4()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(rgb(0x111b24))
                    .rounded_md()
                    .text_color(rgb(0xa7b9c8))
                    .child(empty),
            );
        }
        if !card.top_items.is_empty() {
            panel = panel
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(0xa7b9c8))
                        .child("Largest included items"),
                )
                .children(card.top_items.iter().take(3).map(|(name, bytes)| {
                    div()
                        .flex()
                        .justify_between()
                        .gap_2()
                        .text_sm()
                        .child(div().min_w_0().line_clamp(1).child(name.clone()))
                        .child(div().flex_shrink_0().child(bytes.clone()))
                }));
        }
        panel = panel.child(div().text_xs().text_color(rgb(0xa7b9c8)).child(
            card.last_scan.as_ref().map_or_else(
                || "No saved scan".into(),
                |time| format!("Last saved scan · {time}"),
            ),
        ));
        if let Some(issue) = &card.issue_summary {
            panel = panel.child(
                div()
                    .text_sm()
                    .text_color(rgb(0xf2cf91))
                    .line_clamp(2)
                    .child(issue.clone()),
            );
        }
        panel = panel.child(
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .child(action_button(
                    model,
                    ("explore", ticket.card.0 as usize),
                    if ticket.scan.is_some() && card.preview.is_none() {
                        "Loading map…"
                    } else {
                        "Explore"
                    },
                    Event::Explore(ticket),
                    true,
                    cx,
                    on_event,
                ))
                .child(action_button(
                    model,
                    ("rescan", ticket.card.0 as usize),
                    if card.status == Status::Failed {
                        "Retry"
                    } else if ticket.scan.is_some() {
                        "Rescan"
                    } else {
                        "Scan"
                    },
                    Event::Rescan(ticket),
                    false,
                    cx,
                    on_event,
                ))
                .when(card.issue_summary.is_some(), |element| {
                    element.child(action_button(
                        model,
                        ("details", ticket.card.0 as usize),
                        "Details",
                        Event::Details(ticket),
                        false,
                        cx,
                        on_event,
                    ))
                }),
        );
        grid = grid.child(panel);
    }
    let page = model.page.min(model.page_count() - 1);
    div()
        .flex()
        .flex_col()
        .size_full()
        .min_w_0()
        .min_h_0()
        .bg(rgb(0x111b24))
        .text_color(rgb(0xe7eef5))
        .child(header)
        .child(grid)
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap_2()
                .p_3()
                .flex_shrink_0()
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(0xa7b9c8))
                        .child("Maps are independent · cached observations, not reclaimable space"),
                )
                .when(model.page_count() > 1, |element| {
                    element.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .when(page > 0, |element| {
                                element.child(action_button(
                                    model,
                                    "previous-disks",
                                    "Previous",
                                    Event::Page(page - 1),
                                    false,
                                    cx,
                                    on_event,
                                ))
                            })
                            .child(format!("Page {} of {}", page + 1, model.page_count()))
                            .when(page + 1 < model.page_count(), |element| {
                                element.child(action_button(
                                    model,
                                    "next-disks",
                                    "Next",
                                    Event::Page(page + 1),
                                    false,
                                    cx,
                                    on_event,
                                ))
                            }),
                    )
                }),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use space_core::NodeId;

    fn card(id: u64, connected: bool, status: Status) -> Card {
        Card {
            ticket: Ticket {
                card: CardId(id),
                scan: ScanId::new(id).ok(),
                query_epoch: 4,
            },
            name: format!("Disk {id}"),
            location: format!("/disk-{id}"),
            kind: "Disk".into(),
            connected,
            capacity: Some(Capacity {
                total: 100,
                free: 25,
            }),
            status,
            last_scan: Some("2026-10-16 12:00 UTC".into()),
            cached_total: "75 B".into(),
            preview: Some(treemap::Model {
                items: vec![treemap::Item {
                    node: NodeId::new(1).unwrap(),
                    label: format!("folder-{id}"),
                    detail: format!("/disk-{id}/folder"),
                    bytes: id * 100,
                    unknown_count: 0,
                    is_directory: true,
                }],
                ..Default::default()
            }),
            top_items: vec![],
            issue_summary: None,
            can_rescan: true,
        }
    }
    #[test]
    fn unavailable_cache_disables_acquisition_without_faking_discovery_or_running() {
        let model = Model {
            unavailable: true,
            notice: Some("Cache unavailable".into()),
            ..Default::default()
        };
        assert!(!model.discovering);
        assert!(!model.busy);
        for event in [
            Event::ScanAll,
            Event::ScanFolder,
            Event::RefreshDiscovery,
            Event::Cache,
            Event::Stop,
        ] {
            assert!(!model.allows(event));
        }
    }
    #[test]
    fn card_maps_and_callbacks_are_bound_to_card_scan_and_query() {
        let mut model = Model {
            cards: vec![card(1, true, Status::Ready), card(2, true, Status::Ready)],
            ..Default::default()
        };
        let a = model.cards[0].ticket;
        let b = model.cards[1].ticket;
        let node = NodeId::new(1).unwrap();
        assert!(model.allows(Event::Map(a, treemap::Event::Open(node))));
        assert!(model.allows(Event::Map(b, treemap::Event::Select(node))));
        assert_eq!(model.cards[0].preview.as_ref().unwrap().items[0].bytes, 100);
        assert_eq!(model.cards[1].preview.as_ref().unwrap().items[0].bytes, 200);
        assert!(!model.allows(Event::Map(
            Ticket { card: b.card, ..a },
            treemap::Event::Select(node)
        )));
        model.cards[0].ticket.query_epoch += 1;
        assert!(!model.allows(Event::Explore(a)));
        assert!(!model.allows(Event::Map(a, treemap::Event::Select(node))));
        let current = model.cards[0].ticket;
        model.cards[0].preview = None;
        assert!(!model.allows(Event::Map(current, treemap::Event::OpenOther)));
        model.cards[0].ticket.scan = None;
        assert!(!model.allows(Event::Explore(model.cards[0].ticket)));
    }
    #[test]
    fn cards_are_paginated_and_offline_cards_follow_connected() {
        let mut model = Model {
            cards: (1..=19)
                .map(|id| card(id, id % 2 == 0, Status::Ready))
                .collect(),
            ..Default::default()
        };
        assert_eq!(model.page_count(), 3);
        let first = model.visible_cards();
        assert_eq!(first.len(), 8);
        assert!(first.iter().all(|card| card.connected));
        let stale = first[0].ticket;
        model.page = 1;
        let second = model.visible_cards();
        assert!(second[0].connected);
        assert!(second[1..].iter().all(|card| !card.connected));
        assert!(!model.allows(Event::Explore(stale)));
        model.page = 100;
        assert_eq!(model.visible_cards().len(), 3);
        assert!(!model.allows(Event::Page(3)));
    }
    #[test]
    fn empty_queued_scanning_partial_error_and_offline_are_explicit() {
        let mut model = Model::default();
        assert!(model.visible_cards().is_empty());
        assert!(!model.allows(Event::ScanAll));
        assert!(model.allows(Event::ScanFolder));
        model.included_targets = 2;
        assert!(model.allows(Event::ScanAll));
        model.discovering = true;
        assert!(!model.allows(Event::ScanAll));
        model.discovering = false;
        for status in [
            Status::Idle,
            Status::Queued,
            Status::Scanning { entries: 42 },
            Status::Stopping,
            Status::Ready,
            Status::Partial,
            Status::Failed,
            Status::Cancelled,
            Status::Offline,
        ] {
            assert!(!status.label().is_empty());
        }
        assert_eq!(Status::Partial.label(), "Some locations could not be read");
        assert!(
            Status::Scanning { entries: 42 }
                .label()
                .contains("42 entries observed")
        );
        model.cards.push(card(1, true, Status::Ready));
        model.busy = true;
        assert!(!model.allows(Event::Rescan(model.cards[0].ticket)));
        assert!(!model.allows(Event::ScanFolder));
        assert!(model.allows(Event::Stop));
        assert!(model.allows(Event::Explore(model.cards[0].ticket)));
        model.stopping = true;
        assert!(!model.allows(Event::Stop));
        model.busy = false;
        model.cards[0].connected = false;
        assert!(!model.allows(Event::Rescan(model.cards[0].ticket)));
        assert!(model.allows(Event::Explore(model.cards[0].ticket)));
    }
    #[test]
    fn production_breakpoints_keep_generous_bounded_columns() {
        let normal = layout(1280., 800., 2);
        assert_eq!(normal.columns, 2);
        assert!(normal.card_width >= 600.);
        for width in [320., 390., 800., 1059.] {
            let narrow = layout(width, 800., 10);
            assert_eq!(narrow.columns, 1);
            assert!(narrow.card_width + 32. <= width);
        }
        let one = layout(1280., 800., 1);
        assert_eq!(one.columns, 1);
        assert!(one.preview_height >= 400.);
        assert!(layout(1280., 1000., 1).preview_height > one.preview_height);
        assert_eq!(layout(4000., 800., 20).columns, 2);
        assert!(layout(f32::NAN, f32::INFINITY, 1).card_width.is_finite());
    }
    #[test]
    fn capacity_is_os_metadata_and_readable_without_invented_totals() {
        assert_eq!(
            Capacity {
                total: 100,
                free: 25
            }
            .used_fraction(),
            0.75
        );
        assert_eq!(Capacity { total: 0, free: 0 }.used_fraction(), 0.);
        assert_eq!(
            Capacity {
                total: 100,
                free: 200
            }
            .used_fraction(),
            0.
        );
        assert_eq!(format_bytes(1024), "1.0 KiB");
        assert_eq!(
            Capacity {
                total: 2048,
                free: 1024
            }
            .label(),
            "1.0 KiB available of 2.0 KiB"
        );
    }
    #[test]
    fn actual_gpui_card_style_obeys_the_production_width() {
        for width in [390., 1280.] {
            let policy = layout(width, 800., 2);
            let mut card = card_box(policy.card_width);
            let style = card.style();
            assert_eq!(style.size.width, Some(px(policy.card_width).into()));
            assert_eq!(style.min_size.width, Some(px(0.).into()));
            assert_eq!(style.flex_shrink, Some(0.));
        }
    }
}
