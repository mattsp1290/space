//! Data-fed explorer shell. App-owned filters, query lists, Changes and cache
//! panels remain composable children; this module has no source or store access.
use crate::dashboard::{self, Ticket};
use gpui::{
    AnyElement, Context, Div, SharedString, Stateful, Window, div, prelude::*, px, relative, rgb,
};
use space_core::NodeId;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum View {
    #[default]
    Treemap,
    Folders,
    Largest,
}

#[derive(Clone, Debug)]
pub struct SidebarItem {
    pub ticket: Ticket,
    pub name: String,
    pub status: String,
    pub connected: bool,
}
#[derive(Clone, Debug)]
pub struct Breadcrumb {
    pub node: NodeId,
    pub label: String,
}
#[derive(Clone, Debug, Default)]
pub struct Model {
    pub ticket: Option<Ticket>,
    pub cards: Vec<SidebarItem>,
    pub sidebar_page: usize,
    pub name: String,
    pub location: String,
    pub metric_label: String,
    pub scope_label: String,
    pub status: String,
    pub breadcrumbs: Vec<Breadcrumb>,
    pub view: View,
    pub entire_disk: bool,
    pub can_up: bool,
    pub can_open: bool,
    pub can_rescan: bool,
    pub busy: bool,
    pub stopping: bool,
    pub list_visible: bool,
    pub inspector_visible: bool,
    pub filters_visible: bool,
    pub offline: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub ticket: Option<Ticket>,
    pub action: Action,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Home,
    Select(Ticket),
    SidebarPage(usize),
    Rescan,
    Stop,
    View(View),
    Scope,
    Metric,
    Filters,
    Changes,
    Cache,
    Up,
    Breadcrumb(NodeId),
    OpenFolder,
    ToggleList,
    ToggleInspector,
    Copy,
    Reveal,
    Aliases,
}
impl Model {
    pub fn event(&self, action: Action) -> Event {
        Event {
            ticket: self.ticket,
            action,
        }
    }
    pub fn page_count(&self) -> usize {
        self.cards
            .len()
            .min(dashboard::MAX_CARDS)
            .div_ceil(dashboard::CARDS_PER_PAGE)
            .max(1)
    }
    pub fn visible_cards(&self) -> Vec<&SidebarItem> {
        self.cards
            .iter()
            .take(dashboard::MAX_CARDS)
            .filter(|card| card.connected)
            .chain(
                self.cards
                    .iter()
                    .take(dashboard::MAX_CARDS)
                    .filter(|card| !card.connected),
            )
            .skip(self.sidebar_page.min(self.page_count() - 1) * dashboard::CARDS_PER_PAGE)
            .take(dashboard::CARDS_PER_PAGE)
            .collect()
    }
    /// A UI guard only. The app must still validate native paths and snapshot
    /// eligibility before executing actions. NodeIds are never standalone events.
    pub fn allows(&self, event: Event) -> bool {
        if event.ticket != self.ticket {
            return false;
        }
        match event.action {
            Action::Home | Action::Cache => true,
            Action::Stop => self.busy && !self.stopping,
            Action::SidebarPage(page) => page < self.page_count(),
            Action::Select(ticket) => {
                ticket.scan.is_some()
                    && self
                        .visible_cards()
                        .iter()
                        .any(|card| card.ticket == ticket)
            }
            _ if self.ticket.is_none_or(|ticket| ticket.scan.is_none()) => false,
            Action::Rescan => self.can_rescan && !self.busy && !self.offline,
            Action::Up => self.can_up && self.breadcrumbs.len() > 1,
            Action::Breadcrumb(node) => self
                .breadcrumbs
                .iter()
                .take(64)
                .any(|crumb| crumb.node == node),
            Action::OpenFolder => self.can_open,
            Action::Reveal => !self.offline,
            Action::View(_)
            | Action::Scope
            | Action::Metric
            | Action::Filters
            | Action::Changes
            | Action::ToggleList
            | Action::ToggleInspector
            | Action::Copy
            | Action::Aliases => true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub sidebar_width: Option<f32>,
    pub inspector_width: Option<f32>,
    pub inspector_height: Option<f32>,
    pub content_width: f32,
    pub map_height: f32,
}
/// Move requested details below the primary content before reducing the map
/// below a useful width. The All disks action remains visible at every width.
pub fn layout(width: f32, height: f32, inspector_visible: bool) -> Layout {
    let width = if width.is_finite() { width.max(0.) } else { 0. };
    let height = if height.is_finite() {
        height.max(0.)
    } else {
        0.
    };
    let sidebar_width = (width >= 920.).then_some(220.);
    let inspector_width = (width >= 1200. && inspector_visible).then_some(280.);
    let available_height = (height - if width < 920. { 260. } else { 190. }).max(0.);
    let inspector_height = (inspector_visible && inspector_width.is_none())
        .then_some((available_height * 0.36).min(240.));
    let content_width =
        (width - sidebar_width.unwrap_or(0.) - inspector_width.unwrap_or(0.) - 32.).max(0.);
    Layout {
        sidebar_width,
        inspector_width,
        inspector_height,
        content_width,
        map_height: (available_height - inspector_height.unwrap_or(0.)).max(0.),
    }
}

fn sidebar_box(width: f32) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_3()
        .w(px(width))
        .min_w_0()
        .min_h_0()
        .flex_shrink_0()
        .p_3()
        .border_r_1()
        .border_color(rgb(0x304352))
        .bg(rgb(0x182631))
}
fn content_box() -> Div {
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .overflow_hidden()
}
/// Both placements retain the same inspector child and its alias actions.
/// Inline details are height-bounded and scroll independently; their percentage
/// maximum leaves the primary content visible even when the header wraps.
fn inspector_box(layout: Layout) -> Stateful<Div> {
    let panel = div()
        .id("selection-inspector")
        .min_w_0()
        .min_h_0()
        .flex_shrink_0()
        .p_3()
        .overflow_hidden()
        .overflow_y_scroll()
        .border_color(rgb(0x304352));
    if let Some(width) = layout.inspector_width {
        panel.w(px(width)).border_l_1()
    } else {
        panel
            .w_full()
            .h(px(layout.inspector_height.unwrap_or(0.)))
            .max_h(relative(0.45))
            .border_t_1()
    }
}

fn action_button<T: 'static>(
    model: &Model,
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    action: Action,
    active: bool,
    cx: &mut Context<T>,
    on_event: fn(&mut T, Event, &mut Window, &mut Context<T>),
) -> Stateful<Div> {
    let event = model.event(action);
    let enabled = model.allows(event);
    dashboard::button(id, label, enabled, active).on_click(cx.listener(
        move |this, _, window, cx| {
            if enabled {
                on_event(this, event, window, cx);
            }
        },
    ))
}

pub fn render<T: 'static>(
    model: &Model,
    content: AnyElement,
    inspector: Option<AnyElement>,
    width: f32,
    height: f32,
    cx: &mut Context<T>,
    on_event: fn(&mut T, Event, &mut Window, &mut Context<T>),
) -> Div {
    let layout = layout(width, height, model.inspector_visible);
    let mut header = div()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .gap_2()
        .p_3()
        .border_b_1()
        .border_color(rgb(0x304352))
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_3()
                        .child(action_button(
                            model,
                            "all-disks",
                            "← All disks",
                            Action::Home,
                            false,
                            cx,
                            on_event,
                        ))
                        .child(div().text_lg().child(model.name.clone())),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(action_button(
                            model,
                            "explorer-rescan",
                            if model.busy {
                                if model.stopping {
                                    "Stopping…"
                                } else {
                                    "Stop"
                                }
                            } else {
                                "Rescan"
                            },
                            if model.busy {
                                Action::Stop
                            } else {
                                Action::Rescan
                            },
                            true,
                            cx,
                            on_event,
                        ))
                        .child(action_button(
                            model,
                            "explorer-changes",
                            "Changes",
                            Action::Changes,
                            false,
                            cx,
                            on_event,
                        ))
                        .child(action_button(
                            model,
                            "explorer-cache",
                            "Cache",
                            Action::Cache,
                            false,
                            cx,
                            on_event,
                        )),
                ),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(0xa7b9c8))
                .line_clamp(1)
                .child(model.location.clone()),
        );
    let mut breadcrumbs = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_1()
        .child(action_button(
            model,
            "explorer-up",
            "Up",
            Action::Up,
            false,
            cx,
            on_event,
        ));
    for (index, crumb) in model.breadcrumbs.iter().take(64).enumerate() {
        breadcrumbs = breadcrumbs.child(action_button(
            model,
            ("breadcrumb", index),
            crumb.label.clone(),
            Action::Breadcrumb(crumb.node),
            false,
            cx,
            on_event,
        ));
    }
    header = header
        .child(breadcrumbs)
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(action_button(
                    model,
                    "view-treemap",
                    "Treemap",
                    Action::View(View::Treemap),
                    model.view == View::Treemap,
                    cx,
                    on_event,
                ))
                .child(action_button(
                    model,
                    "view-folders",
                    "Folders",
                    Action::View(View::Folders),
                    model.view == View::Folders,
                    cx,
                    on_event,
                ))
                .child(action_button(
                    model,
                    "view-largest",
                    "Largest files",
                    Action::View(View::Largest),
                    model.view == View::Largest,
                    cx,
                    on_event,
                ))
                .child(action_button(
                    model,
                    "explorer-scope",
                    if model.entire_disk {
                        "Entire disk"
                    } else {
                        "Current folder"
                    },
                    Action::Scope,
                    false,
                    cx,
                    on_event,
                ))
                .child(action_button(
                    model,
                    "explorer-metric",
                    model.metric_label.clone(),
                    Action::Metric,
                    false,
                    cx,
                    on_event,
                ))
                .child(action_button(
                    model,
                    "explorer-filters",
                    if model.filters_visible {
                        "Hide filters"
                    } else {
                        "Search and filters"
                    },
                    Action::Filters,
                    model.filters_visible,
                    cx,
                    on_event,
                ))
                .child(action_button(
                    model,
                    "explorer-list",
                    if model.list_visible {
                        "Hide list"
                    } else {
                        "Show list"
                    },
                    Action::ToggleList,
                    model.list_visible,
                    cx,
                    on_event,
                ))
                .child(action_button(
                    model,
                    "explorer-inspector",
                    if model.inspector_visible {
                        "Hide details"
                    } else {
                        "Item details"
                    },
                    Action::ToggleInspector,
                    model.inspector_visible,
                    cx,
                    on_event,
                )),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(0xa7b9c8))
                .line_clamp(1)
                .child(model.scope_label.clone()),
        );
    if model.offline {
        header = header.child(
            div()
                .text_sm()
                .text_color(rgb(0xf2cf91))
                .child("Offline · browsing a saved scan. Current files may differ."),
        );
    }
    if !model.status.is_empty() {
        header = header.child(
            div()
                .text_xs()
                .text_color(rgb(0xa7b9c8))
                .line_clamp(2)
                .child(model.status.clone()),
        );
    }
    let mut workspace = div().flex().flex_1().min_w_0().min_h_0().overflow_hidden();
    if let Some(sidebar_width) = layout.sidebar_width {
        let mut sidebar = sidebar_box(sidebar_width)
            .id("disk-sidebar")
            .overflow_y_scroll()
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(0xa7b9c8))
                    .child("YOUR DISKS AND FOLDERS"),
            );
        for card in model.visible_cards() {
            let selected = model
                .ticket
                .is_some_and(|ticket| ticket.card == card.ticket.card);
            sidebar = sidebar.child(action_button(
                model,
                ("sidebar-disk", card.ticket.card.0 as usize),
                format!(
                    "{}{}\n{}",
                    if selected { "• " } else { "" },
                    card.name,
                    card.status
                ),
                Action::Select(card.ticket),
                selected,
                cx,
                on_event,
            ));
        }
        let page = model.sidebar_page.min(model.page_count() - 1);
        if model.page_count() > 1 {
            sidebar = sidebar.child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_1()
                    .when(page > 0, |element| {
                        element.child(action_button(
                            model,
                            "sidebar-previous",
                            "Previous",
                            Action::SidebarPage(page - 1),
                            false,
                            cx,
                            on_event,
                        ))
                    })
                    .child(format!("{} / {}", page + 1, model.page_count()))
                    .when(page + 1 < model.page_count(), |element| {
                        element.child(action_button(
                            model,
                            "sidebar-next",
                            "Next",
                            Action::SidebarPage(page + 1),
                            false,
                            cx,
                            on_event,
                        ))
                    }),
            );
        }
        workspace = workspace.child(sidebar);
    }
    let mut primary = content_box().child(content_box().child(content));
    let mut side_inspector = None;
    if model.inspector_visible
        && let Some(inspector) = inspector
    {
        let panel = inspector_box(layout).child(inspector);
        if layout.inspector_width.is_some() {
            side_inspector = Some(panel);
        } else {
            primary = primary.child(panel);
        }
    }
    workspace = workspace.child(primary);
    if let Some(inspector) = side_inspector {
        workspace = workspace.child(inspector);
    }
    let footer = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_2()
        .p_2()
        .flex_shrink_0()
        .border_t_1()
        .border_color(rgb(0x304352))
        .child(action_button(
            model,
            "open-folder",
            "Open folder",
            Action::OpenFolder,
            false,
            cx,
            on_event,
        ))
        .child(action_button(
            model,
            "copy-path",
            "Copy path",
            Action::Copy,
            false,
            cx,
            on_event,
        ))
        .child(action_button(
            model,
            "reveal-path",
            "Reveal",
            Action::Reveal,
            false,
            cx,
            on_event,
        ))
        .child(action_button(
            model,
            "item-aliases",
            "Aliases",
            Action::Aliases,
            false,
            cx,
            on_event,
        ))
        .child(
            div()
                .text_xs()
                .text_color(rgb(0xa7b9c8))
                .child("Click selects · double-click or Enter opens a folder"),
        );
    div()
        .flex()
        .flex_col()
        .size_full()
        .min_w_0()
        .min_h_0()
        .bg(rgb(0x111b24))
        .text_color(rgb(0xe7eef5))
        .child(header)
        .child(workspace)
        .child(footer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disk_model::CardId;
    use space_core::ScanId;
    fn model() -> Model {
        Model {
            ticket: Some(Ticket {
                card: CardId(1),
                scan: ScanId::new(7).ok(),
                query_epoch: 4,
            }),
            breadcrumbs: vec![
                Breadcrumb {
                    node: NodeId::new(1).unwrap(),
                    label: "Disk".into(),
                },
                Breadcrumb {
                    node: NodeId::new(2).unwrap(),
                    label: "Folder".into(),
                },
            ],
            can_up: true,
            can_open: true,
            can_rescan: true,
            ..Default::default()
        }
    }
    #[test]
    fn navigation_is_bound_to_current_scan_and_query() {
        let mut model = model();
        let event = model.event(Action::Breadcrumb(NodeId::new(1).unwrap()));
        assert!(model.allows(event));
        assert!(!model.allows(model.event(Action::Breadcrumb(NodeId::new(3).unwrap()))));
        model.ticket.as_mut().unwrap().query_epoch += 1;
        assert!(!model.allows(event));
        let event = model.event(Action::OpenFolder);
        model.ticket.as_mut().unwrap().scan = ScanId::new(8).ok();
        assert!(!model.allows(event));
        let event = model.event(Action::Copy);
        model.ticket.as_mut().unwrap().card = CardId(2);
        assert!(!model.allows(event));
    }
    #[test]
    fn offline_and_stopping_guard_live_actions_but_keep_cached_navigation() {
        let mut model = model();
        model.offline = true;
        assert!(!model.allows(model.event(Action::Reveal)));
        assert!(!model.allows(model.event(Action::Rescan)));
        assert!(model.allows(model.event(Action::Copy)));
        assert!(model.allows(model.event(Action::OpenFolder)));
        model.busy = true;
        assert!(model.allows(model.event(Action::Stop)));
        model.stopping = true;
        assert!(!model.allows(model.event(Action::Stop)));
        assert!(model.allows(model.event(Action::Home)));
    }
    #[test]
    fn normal_and_narrow_layouts_prioritize_map_and_preserve_disk_access() {
        let normal = layout(1280., 800., true);
        assert_eq!(normal.sidebar_width, Some(220.));
        assert_eq!(normal.inspector_width, Some(280.));
        assert!(normal.content_width >= 700.);
        assert!(normal.map_height > 500.);
        let hidden = layout(1280., 800., false);
        assert!(hidden.content_width > normal.content_width);
        for width in [320., 390., 800., 919.] {
            let narrow = layout(width, 800., true);
            assert_eq!(narrow.sidebar_width, None);
            assert_eq!(narrow.inspector_width, None);
            assert!(narrow.content_width <= width);
            assert!(model().allows(model().event(Action::Home)));
        }
        assert_eq!(layout(1000., 800., true).inspector_width, None);
        assert!(layout(1280., 1000., false).map_height > normal.map_height);
    }
    #[test]
    fn requested_inspector_is_inline_and_scrollable_on_narrow_windows() {
        for width in [320., 390., 800., 1000., 1199.] {
            let policy = layout(width, 800., true);
            assert_eq!(policy.inspector_width, None);
            let height = policy
                .inspector_height
                .expect("requested details remain reachable");
            assert!(height > 0. && height <= 240.);
            assert!(policy.map_height >= 300.);
            assert!(policy.content_width <= width);
            let mut panel = inspector_box(policy);
            let style = panel.style();
            assert_eq!(style.size.width, Some(relative(1.).into()));
            assert_eq!(style.size.height, Some(px(height).into()));
            assert_eq!(style.max_size.height, Some(relative(0.45).into()));
            assert_eq!(style.min_size.width, Some(px(0.).into()));
            assert_eq!(style.min_size.height, Some(px(0.).into()));
            assert_eq!(style.overflow.x, Some(gpui::Overflow::Hidden));
            assert_eq!(style.overflow.y, Some(gpui::Overflow::Scroll));
            let collapsed = layout(width, 800., false);
            assert_eq!(collapsed.inspector_height, None);
            assert!(collapsed.map_height > policy.map_height);
            assert!(model().allows(model().event(Action::ToggleInspector)));
        }
        let wide = layout(1280., 800., true);
        assert_eq!(wide.inspector_height, None);
        let mut panel = inspector_box(wide);
        assert_eq!(panel.style().size.width, Some(px(280.).into()));
        assert_eq!(panel.style().max_size.height, None);
    }
    #[test]
    fn sidebar_pagination_and_callbacks_use_exact_target_ticket() {
        let mut model = model();
        model.cards = (1..=20)
            .map(|id| SidebarItem {
                ticket: Ticket {
                    card: CardId(id),
                    scan: ScanId::new(id).ok(),
                    query_epoch: 3,
                },
                name: format!("Disk {id}"),
                status: "Ready".into(),
                connected: id % 2 == 0,
            })
            .collect();
        assert_eq!(model.page_count(), 3);
        assert!(model.visible_cards().iter().all(|card| card.connected));
        let ticket = model.visible_cards()[0].ticket;
        assert!(model.allows(model.event(Action::Select(ticket))));
        assert!(!model.allows(model.event(Action::Select(Ticket {
            query_epoch: 2,
            ..ticket
        }))));
        model.sidebar_page = 1;
        assert!(!model.allows(model.event(Action::Select(ticket))));
    }
    #[test]
    fn actual_gpui_workspace_styles_allow_flex_shrink_and_independent_scrolling() {
        let mut content = content_box();
        let style = content.style();
        assert_eq!(style.min_size.width, Some(px(0.).into()));
        assert_eq!(style.min_size.height, Some(px(0.).into()));
        assert_eq!(style.overflow.x, Some(gpui::Overflow::Hidden));
        assert_eq!(style.overflow.y, Some(gpui::Overflow::Hidden));
        let mut sidebar = sidebar_box(220.);
        assert_eq!(sidebar.style().size.width, Some(px(220.).into()));
        assert_eq!(sidebar.style().flex_shrink, Some(0.));
    }
}
