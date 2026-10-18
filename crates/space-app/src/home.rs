//! Scan launchpad: one acquisition action, progress beside each source.
pub use crate::ticket::Ticket;
use crate::{theme, tree, treemap_view};
use gpui::{Context, Div, Role, Window, div, prelude::*, px, rgb};
use std::{cell::RefCell, rc::Rc, sync::Arc};
pub const MAX_PREVIEWS: usize = 24;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Idle,
    Queued {
        position: usize,
        total: usize,
    },
    Scanning {
        entries: u64,
        ordinal: usize,
        total: usize,
    },
    Stopping,
    Ready,
    Partial,
    Failed,
    Cancelled,
    Offline,
}
impl Status {
    fn kind(&self) -> theme::StatusKind {
        match self {
            Self::Queued { .. } | Self::Scanning { .. } | Self::Stopping => {
                theme::StatusKind::Progress
            }
            Self::Partial | Self::Failed | Self::Cancelled => theme::StatusKind::Warning,
            Self::Ready => theme::StatusKind::Good,
            Self::Idle | Self::Offline => theme::StatusKind::Muted,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Scanning { ordinal, total, .. } => format!("Scanning {ordinal}/{total}"),
            Self::Idle => "Not scanned".into(),
            Self::Queued { position, total } => format!("Queued {position}/{total}"),
            Self::Stopping => "Stopping…".into(),
            Self::Ready => "Ready".into(),
            Self::Partial => "Partial".into(),
            Self::Failed => "Failed".into(),
            Self::Cancelled => "Cancelled".into(),
            Self::Offline => "Offline".into(),
        }
    }
}
pub struct Capacity {
    pub total: u64,
    pub available: u64,
}
pub struct Card {
    pub ticket: Ticket,
    pub name: String,
    pub location: String,
    pub kind: String,
    pub connected: bool,
    pub capacity: Option<Capacity>,
    pub status: Status,
    pub last_scan: Option<String>,
    pub cached_total: String,
    pub preview: Option<Arc<tree::Tree>>,
    pub geometry: Rc<RefCell<Option<treemap_view::GeometryCache>>>,
    pub error: Option<String>,
    pub opening: bool,
    pub can_rescan: bool,
}
pub struct Model {
    pub cards: Vec<Card>,
    pub included_targets: usize,
    pub discovering: bool,
    pub unavailable: bool,
    pub busy: bool,
    pub stopping: bool,
    pub batch_summary: Option<String>,
    pub metric_label: String,
    pub discovery_issues: Vec<String>,
    pub issues_open: bool,
    pub notice: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    ScanAll,
    Stop,
    ScanFolder,
    RefreshDiscovery,
    ToggleIssues,
    Explore(Ticket),
    Rescan(Ticket),
    Retry(Ticket),
}
impl Model {
    pub fn allows(&self, event: Event) -> bool {
        match event {
            Event::Explore(t) => self
                .cards
                .iter()
                .any(|c| c.ticket == t && t.scan.is_some() && !c.opening),
            Event::Rescan(t) | Event::Retry(t) => {
                !self.unavailable
                    && !self.busy
                    && self.cards.iter().any(|c| c.ticket == t && c.can_rescan)
            }
            Event::Stop => self.busy && !self.stopping,
            Event::ToggleIssues => !self.discovery_issues.is_empty(),
            _ => !self.unavailable && !self.busy && !self.discovering,
        }
    }
    pub fn primary(&self) -> (&'static str, Event, bool) {
        if self.stopping {
            ("Stopping…", Event::Stop, false)
        } else if self.busy {
            ("Stop", Event::Stop, true)
        } else {
            (
                if self.cards.iter().any(|c| c.ticket.scan.is_some()) {
                    "Rescan all disks"
                } else {
                    "Scan all disks"
                },
                Event::ScanAll,
                self.allows(Event::ScanAll),
            )
        }
    }
    pub fn status_line(&self) -> String {
        if self.discovering {
            "Finding your disks…".into()
        } else if self.busy {
            self.batch_summary
                .clone()
                .unwrap_or_else(|| "Preparing scan…".into())
        } else {
            let offline = self.cards.iter().filter(|c| !c.connected).count();
            format!(
                "{} disks found{}",
                self.included_targets,
                if offline > 0 {
                    format!(" · {offline} offline saved scans")
                } else {
                    String::new()
                }
            )
        }
    }
}
pub struct Layout {
    pub columns: usize,
    pub card_width: f32,
    pub preview_height: f32,
    pub hero_height: f32,
}
pub fn layout(width: f32, height: f32, count: usize) -> Layout {
    let columns = if width >= 1500. {
        3
    } else if width >= 900. {
        2
    } else {
        1
    };
    Layout {
        columns,
        card_width: ((width - 2. * theme::SPACE_6 - (columns - 1) as f32 * theme::SPACE_5)
            / columns as f32)
            .max(1.),
        preview_height: if columns >= 2 {
            150.
        } else if count == 1 {
            (height - 400.).clamp(180., 420.)
        } else {
            150.
        },
        hero_height: 112.,
    }
}
fn action<T: 'static>(
    id: impl Into<gpui::ElementId>,
    label: impl Into<gpui::SharedString>,
    event: Event,
    model: &Model,
    primary: bool,
    cx: &mut Context<T>,
    handler: fn(&mut T, Event, &mut Window, &mut Context<T>),
) -> gpui::Stateful<Div> {
    let enabled = model.allows(event);
    (if primary {
        theme::button(id, label, enabled, true)
    } else {
        theme::text_button(id, label, enabled)
    })
    .on_click(cx.listener(move |this, _, window, cx| {
        cx.stop_propagation();
        if enabled {
            handler(this, event, window, cx);
        }
    }))
}
fn card_box(width: f32) -> Div {
    div().w(px(width)).flex_shrink_0()
}
pub fn render<T: 'static>(
    model: &Model,
    width: f32,
    height: f32,
    cx: &mut Context<T>,
    handler: fn(&mut T, Event, &mut Window, &mut Context<T>),
) -> Div {
    let l = layout(width, height, model.cards.len());
    let (primary, event, _) = model.primary();
    let hero = div()
        .id("home-hero")
        .debug_selector(|| "home-hero".into())
        .h(px(l.hero_height))
        .flex_shrink_0()
        .flex()
        .justify_between()
        .px(px(theme::SPACE_6))
        .py(px(12.))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(
                    div()
                        .text_size(px(theme::TEXT_2XL))
                        .line_height(px(34.))
                        .child("What’s taking up space?"),
                )
                .child(
                    div()
                        .flex()
                        .gap(px(12.))
                        .items_center()
                        .child(action("scan-all", primary, event, model, true, cx, handler))
                        .child(model.status_line()),
                )
                .child(
                    action(
                        "refresh-disks",
                        "Refresh disks",
                        Event::RefreshDiscovery,
                        model,
                        false,
                        cx,
                        handler,
                    )
                    .h(px(14.))
                    .py_0()
                    .text_size(px(theme::TEXT_XS))
                    .line_height(px(12.))
                    .debug_selector(|| "home-refresh".into()),
                ),
        )
        .child(action(
            "scan-folder",
            "Scan a folder…",
            Event::ScanFolder,
            model,
            false,
            cx,
            handler,
        ));
    let mut grid = div()
        .id("disk-grid")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px(px(theme::SPACE_6));
    let mut rows = vec![];
    for chunk in model.cards.chunks(l.columns) {
        let mut row = div().flex().gap(px(theme::SPACE_5)).mb(px(theme::SPACE_5));
        for card in chunk {
            let ticket = card.ticket;
            let enabled = model.allows(Event::Explore(ticket));
            let first = model.cards.first().is_some_and(|c| c.ticket == ticket);
            let mut item = card_box(l.card_width)
                .id(("disk", ticket.card.0))
                .debug_selector(move || {
                    if first {
                        "first-disk-card".into()
                    } else {
                        format!("disk-card-{}", ticket.card.0)
                    }
                })
                .role(Role::Button)
                .aria_label(format!("{}, {}, explore", card.name, card.status.label()))
                .overflow_hidden()
                .rounded(px(theme::RADIUS_3))
                .bg(rgb(theme::SURFACE))
                .p(px(12.))
                .on_click(cx.listener(move |this, _, window, cx| {
                    if enabled {
                        handler(this, Event::Explore(ticket), window, cx);
                    }
                }))
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_ellipsis()
                                .font_weight(gpui::FontWeight::BOLD)
                                .child(card.name.clone()),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .items_end()
                                .child(theme::pill(card.status.label(), card.status.kind()))
                                .when_some(card.error.clone(), |d, error| {
                                    d.child(
                                        div()
                                            .text_size(px(theme::TEXT_XS))
                                            .text_color(rgb(theme::WARNING))
                                            .child(error),
                                    )
                                }),
                        ),
                )
                .child(
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(rgb(theme::TEXT_MUTED))
                        .text_ellipsis()
                        .child(format!("{} · {}", card.kind, card.location)),
                );
            if let Some(capacity) = &card.capacity {
                item = item
                    .child(
                        div()
                            .mt(px(8.))
                            .text_size(px(theme::TEXT_XS))
                            .child(format!(
                                "{} free of {}",
                                tree::format_bytes(capacity.available.into()),
                                tree::format_bytes(capacity.total.into())
                            )),
                    )
                    .child(
                        div()
                            .h(px(6.))
                            .mt(px(4.))
                            .bg(rgb(theme::BORDER))
                            .rounded(px(3.))
                            .id(("capacity", ticket.card.0))
                            .aria_label("OS capacity, separate from observed sizes")
                            .tooltip(theme::tooltip("OS capacity, separate from observed sizes"))
                            .child(
                                div()
                                    .h_full()
                                    .w(gpui::relative(if capacity.total == 0 {
                                        0.
                                    } else {
                                        1. - capacity.available as f32 / capacity.total as f32
                                    }))
                                    .bg(rgb(theme::ACCENT)),
                            ),
                    );
            }
            let preview = div()
                .h(px(l.preview_height))
                .w_full()
                .my(px(12.))
                .overflow_hidden()
                .rounded(px(theme::RADIUS_1));
            item = item.child(if let Status::Scanning { entries, .. } = card.status {
                preview
                    .bg(rgb(theme::MAP_BG))
                    .p(px(8.))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(
                        div()
                            .id(("scan-progress", ticket.card.0))
                            .h(px(6.))
                            .w_full()
                            .flex()
                            .children((0..16).map(|i| {
                                div().flex_1().h_full().bg(rgb(if i % 2 == 0 {
                                    theme::ACCENT
                                } else {
                                    theme::BORDER
                                }))
                            })),
                    )
                    .child(format!("{entries} entries observed"))
            } else if let Some(tree) = &card.preview {
                preview.child(treemap_view::render(
                    tree.clone(),
                    treemap_view::Params::PREVIEW,
                    None,
                    None,
                    card.geometry.clone(),
                    cx,
                    Rc::new(|_, _, _, _| {}),
                ))
            } else {
                preview
                    .bg(rgb(theme::MAP_BG))
                    .child(if card.ticket.scan.is_some() {
                        "Map not loaded"
                    } else {
                        "Scan to build a map"
                    })
            });
            let retry = matches!(card.status, Status::Failed | Status::Cancelled);
            item = item
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .items_center()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_ellipsis()
                                .text_size(px(theme::TEXT_XS))
                                .text_color(rgb(theme::TEXT_MUTED))
                                .child(format!(
                                    "{} · {}",
                                    card.cached_total,
                                    card.last_scan.as_deref().unwrap_or("No saved scan")
                                )),
                        )
                        .child(action(
                            ("rescan", ticket.card.0),
                            if retry { "Retry" } else { "Rescan" },
                            if retry {
                                Event::Retry(ticket)
                            } else {
                                Event::Rescan(ticket)
                            },
                            model,
                            false,
                            cx,
                            handler,
                        )),
                )
                .child(div().text_color(rgb(theme::ACCENT)).child(if card.opening {
                    "Opening…"
                } else if enabled {
                    "Explore →"
                } else {
                    ""
                }));
            row = row.child(item);
        }
        rows.push(row);
    }
    grid = grid.children(rows);
    if model.cards.is_empty() {
        grid = grid.child(if model.discovering {
            "Finding your disks"
        } else {
            "No supported local disks found"
        });
    }
    let mut root = div().size_full().flex().flex_col().child(hero);
    if let Some(notice) = &model.notice {
        root = root.child(
            div()
                .px(px(24.))
                .text_color(rgb(theme::WARNING))
                .child(notice.clone()),
        );
    }
    if model.unavailable {
        root = root.child(
            div()
                .px(px(24.))
                .child("Cache unavailable. Scanning is disabled."),
        );
    }
    root = root.child(grid).child(
        div()
            .flex()
            .gap(px(12.))
            .px(px(24.))
            .py(px(8.))
            .text_size(px(theme::TEXT_XS))
            .child(model.metric_label.clone())
            .when(!model.discovery_issues.is_empty(), |d| {
                d.child(action(
                    "discovery-notices",
                    format!("{} discovery notices", model.discovery_issues.len()),
                    Event::ToggleIssues,
                    model,
                    false,
                    cx,
                    handler,
                ))
            }),
    );
    if model.issues_open {
        root = root.child(
            div()
                .id("discovery-issues")
                .max_h(px(160.))
                .overflow_y_scroll()
                .children(
                    model
                        .discovery_issues
                        .iter()
                        .take(64)
                        .map(|s| div().child(s.clone())),
                ),
        );
    }
    root
}
#[cfg(test)]
mod tests {
    use super::*;
    fn model() -> Model {
        Model {
            cards: vec![],
            included_targets: 2,
            discovering: false,
            unavailable: false,
            busy: false,
            stopping: false,
            batch_summary: None,
            metric_label: String::new(),
            discovery_issues: vec![],
            issues_open: false,
            notice: None,
        }
    }
    #[test]
    fn h1_primary_h2_allows_h4_status() {
        let mut m = model();
        assert_eq!(m.primary(), ("Scan all disks", Event::ScanAll, true));
        m.busy = true;
        assert_eq!(m.primary(), ("Stop", Event::Stop, true));
        m.stopping = true;
        assert!(!m.primary().2);
        m.discovering = true;
        assert_eq!(m.status_line(), "Finding your disks…");
        m.unavailable = true;
        assert!(!m.allows(Event::ScanFolder));
    }
    #[test]
    fn h5_card_dimensions_and_progress_labels() {
        let width = layout(1280., 800., 2).card_width;
        let mut card = card_box(width);
        assert_eq!(card.style().size.width, Some(px(width).into()));
        assert_eq!(card.style().flex_shrink, Some(0.));
        assert_eq!(
            Status::Queued {
                position: 2,
                total: 3
            }
            .label(),
            "Queued 2/3"
        );
        assert_eq!(
            Status::Scanning {
                entries: 123,
                ordinal: 1,
                total: 3
            }
            .label(),
            "Scanning 1/3"
        );
    }
    #[test]
    fn h3_layout() {
        assert_eq!(layout(800., 800., 2).columns, 1);
        assert_eq!(layout(900., 800., 2).columns, 2);
        assert_eq!(layout(1499., 800., 2).columns, 2);
        assert_eq!(layout(1500., 800., 2).columns, 3);
        assert!(layout(800., 1000., 1).preview_height > layout(800., 600., 1).preview_height);
    }
}
