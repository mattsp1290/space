//! Explore presentation and validation of events from a particular rendered frame.
use crate::{metric_label, text_input, theme, ticket::Ticket, tree, treemap_view};
use gpui::{
    Context, Div, Entity, FocusHandle, Role, UniformListScrollHandle, Window, div, prelude::*, px,
    rgb, uniform_list,
};
use space_core::{Metric, NodeId};
use std::{cell::RefCell, rc::Rc, sync::Arc};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ListMode {
    #[default]
    Children,
    Largest,
}
pub struct DiskChoice {
    pub ticket: Ticket,
    pub name: String,
    pub status: String,
    pub connected: bool,
}
#[derive(Clone)]
pub struct ListRow {
    pub node: NodeId,
    pub label: String,
    pub bytes: u128,
    pub is_directory: bool,
    pub unknown_count: u64,
    pub path: Option<String>,
}
pub struct Model {
    pub ticket: Option<Ticket>,
    pub generation: u64,
    pub disks: Vec<DiskChoice>,
    pub disk_menu_open: bool,
    pub name: String,
    pub crumbs: Vec<(NodeId, String, u128)>,
    pub scope_total: u128,
    pub metric: Metric,
    pub busy: bool,
    pub stopping: bool,
    pub offline: bool,
    pub partial: bool,
    pub tree: Option<Arc<tree::Tree>>,
    pub tree_loading: bool,
    pub selected: Option<NodeId>,
    pub hovered: Option<tree::Target>,
    pub list_visible: bool,
    pub list_mode: ListMode,
    pub rows: Vec<ListRow>,
    pub list_selected: Option<usize>,
    pub has_next_page: bool,
    pub selection_path: Option<String>,
    pub status: Option<String>,
    pub new_scan_available: bool,
    pub search_text: String,
    pub search_busy: bool,
    pub matching: Option<u64>,
    pub search_error: Option<String>,
}
#[derive(Clone, Debug)]
pub enum Action {
    Home,
    ChooseDisk(Ticket),
    ToggleDiskMenu,
    Rescan,
    Stop,
    ShowNewScan,
    Up,
    Crumb(NodeId),
    ZoomInto(NodeId),
    Select(Option<NodeId>),
    Hover(Option<tree::Target>),
    OpenOther(NodeId),
    ListMode(ListMode),
    ToggleList,
    ListSelect(usize),
    ListOpen(usize),
    NextPage,
    FirstPage,
    ToggleMetric,
    Copy,
    Reveal,
    SearchChanged(String),
    SearchClear,
}
#[derive(Clone, Debug)]
pub struct Event {
    pub ticket: Option<Ticket>,
    pub generation: u64,
    pub action: Action,
}
impl Model {
    pub fn event(&self, action: Action) -> Event {
        Event {
            ticket: self.ticket,
            generation: self.generation,
            action,
        }
    }
    pub fn node(&self, node: NodeId) -> Option<ListRow> {
        self.tree
            .as_ref()
            .and_then(|t| t.find(node))
            .map(|n| ListRow {
                node,
                label: n.label.clone(),
                bytes: u128::from(n.bytes),
                is_directory: n.is_directory,
                unknown_count: n.unknown_count,
                path: None,
            })
            .or_else(|| self.rows.iter().find(|r| r.node == node).cloned())
    }
    pub fn contains(&self, node: NodeId) -> bool {
        self.node(node).is_some()
    }
    pub fn allows(&self, event: &Event) -> bool {
        if !matches!(
            event.action,
            Action::Home | Action::Stop | Action::Hover(None) | Action::SearchChanged(_)
        ) && (event.ticket != self.ticket || event.generation != self.generation)
        {
            return false;
        }
        match &event.action {
            Action::Up => self.crumbs.len() > 1,
            Action::ZoomInto(n) => self.node(*n).is_some_and(|n| n.is_directory),
            Action::Select(Some(n)) => self.contains(*n),
            Action::Hover(Some(target)) => {
                !self.disk_menu_open
                    && self.tree.as_ref().is_some_and(|t| match target {
                        tree::Target::Node(n) => t.find(*n).is_some(),
                        tree::Target::Other { parent } => {
                            if *parent == t.root {
                                t.other_count > 0
                            } else {
                                t.find(*parent).is_some_and(|n| n.other_count > 0)
                            }
                        }
                    })
            }
            Action::OpenOther(parent) => self.tree.as_ref().is_some_and(|t| {
                if *parent == t.root {
                    t.other_count > 0
                } else {
                    t.find(*parent).is_some_and(|n| n.other_count > 0)
                }
            }),
            Action::Reveal => !self.offline && self.selected.is_some(),
            Action::Copy => self.selected.is_some(),
            Action::Rescan => !self.busy && !self.offline,
            Action::Stop => self.busy && !self.stopping,
            Action::ShowNewScan => self.new_scan_available,
            Action::ChooseDisk(ticket) => self
                .disks
                .iter()
                .any(|d| d.ticket == *ticket && ticket.scan.is_some()),
            Action::ListSelect(i) | Action::ListOpen(i) => *i < self.rows.len(),
            Action::NextPage => self.has_next_page,
            Action::Crumb(n) => self.crumbs.iter().any(|c| c.0 == *n),
            _ => true,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrumbSlot {
    Crumb(usize),
    Ellipsis,
}
pub fn visible_crumbs(count: usize) -> Vec<CrumbSlot> {
    if count <= 5 {
        (0..count).map(CrumbSlot::Crumb).collect()
    } else {
        vec![
            CrumbSlot::Crumb(0),
            CrumbSlot::Ellipsis,
            CrumbSlot::Crumb(count - 3),
            CrumbSlot::Crumb(count - 2),
            CrumbSlot::Crumb(count - 1),
        ]
    }
}
pub struct Layout {
    pub top_bar: f32,
    pub banner: f32,
    pub crumb_bar: f32,
    pub details: f32,
    pub list_width: Option<f32>,
    pub map: tree::Rect,
}
pub fn layout(width: f32, height: f32, list_visible: bool, banner_visible: bool) -> Layout {
    let list_width = list_visible.then_some(if width >= 1100. { 300. } else { 260. });
    let banner = if banner_visible { 28. } else { 0. };
    Layout {
        top_bar: 44.,
        banner,
        crumb_bar: 36.,
        details: 40.,
        list_width,
        map: tree::Rect {
            x: 8.,
            y: 88. + banner,
            width: (width - list_width.unwrap_or(0.) - 16.).max(0.),
            height: (height - 136. - banner).max(0.),
        },
    }
}
fn button<T: 'static>(
    id: impl Into<gpui::ElementId>,
    label: impl Into<gpui::SharedString>,
    action: Action,
    model: &Model,
    cx: &mut Context<T>,
    handler: fn(&mut T, Event, &mut Window, &mut Context<T>),
) -> gpui::Stateful<Div> {
    let event = model.event(action);
    let enabled = model.allows(&event);
    theme::button(id, label, enabled, false).on_click(cx.listener(move |this, _, window, cx| {
        cx.stop_propagation();
        if enabled {
            handler(this, event.clone(), window, cx);
        }
    }))
}
pub struct Controls<'a> {
    pub search: &'a Entity<text_input::TextInput>,
    pub map_focus: &'a FocusHandle,
    pub list_focus: &'a FocusHandle,
    pub scroll: &'a UniformListScrollHandle,
    pub geometry: Rc<RefCell<Option<treemap_view::GeometryCache>>>,
}
pub fn render<T: 'static>(
    model: &Model,
    l: Layout,
    controls: Controls<'_>,
    cx: &mut Context<T>,
    handler: fn(&mut T, Event, &mut Window, &mut Context<T>),
) -> Div {
    let mut root = div().relative().size_full().overflow_hidden();
    let top = div()
        .absolute()
        .top_0()
        .left_0()
        .w_full()
        .h(px(l.top_bar))
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(px(8.))
        .px(px(8.))
        .bg(rgb(theme::SURFACE))
        .child(button("home", "← Disks", Action::Home, model, cx, handler))
        .child(button(
            "disk-menu",
            format!("{} ▾", model.name),
            Action::ToggleDiskMenu,
            model,
            cx,
            handler,
        ))
        .child(
            div()
                .w(px(240.))
                .flex_shrink_0()
                .child(controls.search.clone()),
        )
        .child(button(
            "clear-search",
            "×",
            Action::SearchClear,
            model,
            cx,
            handler,
        ))
        .child(
            div()
                .flex_1()
                .text_size(px(theme::TEXT_XS))
                .text_ellipsis()
                .child(if model.search_busy {
                    "Searching…".into()
                } else {
                    model.matching.map_or_else(String::new, |n| {
                        format!("{n} files match “{}”", model.search_text)
                    })
                }),
        )
        .child(button(
            "metric",
            metric_label(model.metric),
            Action::ToggleMetric,
            model,
            cx,
            handler,
        ))
        .child(button(
            "toggle-list",
            "≡",
            Action::ToggleList,
            model,
            cx,
            handler,
        ))
        .child(button(
            "rescan",
            if model.stopping {
                "Stopping…"
            } else if model.busy {
                "Stop"
            } else {
                "Rescan"
            },
            if model.busy {
                Action::Stop
            } else {
                Action::Rescan
            },
            model,
            cx,
            handler,
        ));
    root = root.child(top);
    if l.banner > 0. {
        root = root.child(
            div()
                .absolute()
                .top(px(l.top_bar))
                .w_full()
                .h(px(l.banner))
                .flex()
                .items_center()
                .bg(rgb(theme::SURFACE_RAISED))
                .child(format!("A newer scan of {} finished", model.name))
                .child(button(
                    "show-new-scan",
                    "Show",
                    Action::ShowNewScan,
                    model,
                    cx,
                    handler,
                )),
        );
    }
    let mut crumbs = div()
        .absolute()
        .left_0()
        .top(px(l.top_bar + l.banner))
        .w_full()
        .h(px(l.crumb_bar))
        .flex_shrink_0()
        .flex()
        .items_center()
        .overflow_hidden()
        .gap(px(4.))
        .child(button("up", "↑", Action::Up, model, cx, handler));
    let slots = visible_crumbs(model.crumbs.len());
    let crumb_width = ((l.map.width + l.list_width.unwrap_or(0.) + 16. - 260.)
        / slots.len().max(1) as f32)
        .min(180.);
    for slot in slots {
        crumbs = match slot {
            CrumbSlot::Ellipsis => crumbs.child("…"),
            CrumbSlot::Crumb(i) => {
                let (n, label, bytes) = &model.crumbs[i];
                crumbs.child(
                    button(
                        ("crumb", i),
                        format!("{} {} ›", label, tree::format_bytes(*bytes)),
                        Action::Crumb(*n),
                        model,
                        cx,
                        handler,
                    )
                    .w(px(crumb_width)),
                )
            }
        };
    }
    crumbs = crumbs.child(div().flex_1());
    if model.list_visible {
        crumbs = crumbs
            .child(button(
                "children",
                if model.list_mode == ListMode::Children {
                    "• Children"
                } else {
                    "Children"
                },
                Action::ListMode(ListMode::Children),
                model,
                cx,
                handler,
            ))
            .child(button(
                "largest",
                if model.list_mode == ListMode::Largest {
                    "• Largest"
                } else {
                    "Largest"
                },
                Action::ListMode(ListMode::Largest),
                model,
                cx,
                handler,
            ));
    }
    root = root.child(crumbs);
    let map = div()
        .id("explore-map")
        .debug_selector(|| "explore-map".into())
        .absolute()
        .left(px(l.map.x))
        .top(px(l.map.y))
        .w(px(l.map.width))
        .h(px(l.map.height))
        .flex_shrink_0()
        .min_w_0()
        .min_h_0()
        .overflow_hidden()
        .key_context("SpaceMap")
        .track_focus(controls.map_focus);
    let map = if let Some(tree) = &model.tree {
        let ticket = model.ticket;
        let generation = model.generation;
        map.child(treemap_view::render(
            tree.clone(),
            treemap_view::Params::EXPLORE,
            model.selected,
            model.hovered,
            controls.geometry.clone(),
            cx,
            Rc::new(move |this, tile, window, cx| {
                let action = match tile {
                    treemap_view::TileEvent::Select(n) => Action::Select(Some(n)),
                    treemap_view::TileEvent::Zoom(n) => Action::ZoomInto(n),
                    treemap_view::TileEvent::Hover(t) => Action::Hover(t),
                    treemap_view::TileEvent::OpenOther(p) => Action::OpenOther(p),
                };
                handler(
                    this,
                    Event {
                        ticket,
                        generation,
                        action,
                    },
                    window,
                    cx,
                );
            }),
        ))
    } else {
        map.bg(rgb(theme::MAP_BG)).child(if model.tree_loading {
            "Loading…"
        } else {
            "No matching observed sizes"
        })
    };
    root = root.child(map);
    if let Some(width) = l.list_width {
        let rows = model.rows.clone();
        let selected = model.list_selected;
        let ticket = model.ticket;
        let generation = model.generation;
        let total = model.scope_total;
        let list = uniform_list(
            "scope-list",
            rows.len(),
            cx.processor(move |_this, range: std::ops::Range<usize>, _, cx| {
                range
                    .map(|i| {
                        let row = &rows[i];
                        let select = Event {
                            ticket,
                            generation,
                            action: Action::ListSelect(i),
                        };
                        let open = Event {
                            ticket,
                            generation,
                            action: Action::ListOpen(i),
                        };
                        div()
                            .id(("row", i))
                            .role(Role::ListItem)
                            .h(px(32.))
                            .overflow_hidden()
                            .bg(rgb(if selected == Some(i) {
                                theme::SURFACE_RAISED
                            } else {
                                theme::SURFACE
                            }))
                            .on_click(cx.listener(
                                move |this, event: &gpui::ClickEvent, window, cx| {
                                    handler(
                                        this,
                                        if event.click_count() >= 2 {
                                            open.clone()
                                        } else {
                                            select.clone()
                                        },
                                        window,
                                        cx,
                                    );
                                },
                            ))
                            .child(
                                div()
                                    .flex()
                                    .justify_between()
                                    .text_size(px(theme::TEXT_XS))
                                    .child(div().flex_1().min_w_0().text_ellipsis().child(format!(
                                        "{} {}",
                                        if row.is_directory { "▸" } else { "·" },
                                        row.label
                                    )))
                                    .child(tree::format_bytes(row.bytes)),
                            )
                            .child(
                                div()
                                    .h(px(2.))
                                    .w(gpui::relative(if total > 0 {
                                        (row.bytes as f64 / total as f64).min(1.) as f32
                                    } else {
                                        0.
                                    }))
                                    .bg(rgb(theme::ACCENT)),
                            )
                            .when_some(row.path.clone(), |d, p| {
                                d.child(
                                    div()
                                        .text_size(px(theme::TEXT_XS))
                                        .text_color(rgb(theme::TEXT_MUTED))
                                        .text_ellipsis_middle()
                                        .child(p),
                                )
                            })
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .size_full()
        .track_scroll(controls.scroll);
        root = root.child(
            div()
                .absolute()
                .right_0()
                .top(px(l.map.y))
                .w(px(width))
                .h(px(l.map.height))
                .flex_shrink_0()
                .flex()
                .flex_col()
                .key_context("SpaceList")
                .track_focus(controls.list_focus)
                .id("scope-list-container")
                .role(Role::ListBox)
                .child(div().flex_1().min_h_0().child(list))
                .child(
                    div()
                        .flex()
                        .child(button(
                            "first-page",
                            "First",
                            Action::FirstPage,
                            model,
                            cx,
                            handler,
                        ))
                        .child(button(
                            "next-page",
                            "Next",
                            Action::NextPage,
                            model,
                            cx,
                            handler,
                        )),
                ),
        );
    }
    let selection = model.selected.and_then(|n| model.node(n));
    let caption = if let Some(row) = &selection {
        format!(
            "{} · {} · {} · {} unknown · {}",
            row.label,
            tree::format_bytes(row.bytes),
            tree::percent(row.bytes, model.scope_total),
            row.unknown_count,
            model.selection_path.as_deref().unwrap_or("Loading path…")
        )
    } else {
        format!(
            "{} · {} · {} · Click to select · folder name to zoom · Backspace up",
            model.name,
            tree::format_bytes(model.scope_total),
            model.tree.as_ref().map_or_else(String::new, |t| format!(
                "{} unknown · {} zero · {} nodes",
                t.unknown_children, t.zero_children, t.nodes
            ))
        )
    };
    let mut details = div()
        .absolute()
        .bottom_0()
        .left_0()
        .w_full()
        .h(px(l.details))
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(px(8.))
        .px(px(8.))
        .bg(rgb(theme::SURFACE))
        .text_size(px(theme::TEXT_XS))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_ellipsis_middle()
                .child(caption),
        );
    if let Some(row) = selection {
        if row.is_directory {
            details = details.child(button(
                "zoom-in",
                "Zoom in",
                Action::ZoomInto(row.node),
                model,
                cx,
                handler,
            ));
        }
        details = details
            .child(button(
                "reveal",
                "Reveal",
                Action::Reveal,
                model,
                cx,
                handler,
            ))
            .child(button(
                "copy-path",
                "Copy path",
                Action::Copy,
                model,
                cx,
                handler,
            ));
    }
    if model.offline {
        details = details.child("Offline · saved scan");
    }
    if model.partial {
        details = details.child(
            div()
                .text_color(rgb(theme::WARNING))
                .child("Some locations could not be read"),
        );
    }
    root = root.child(details);
    if let Some(status) = model.search_error.as_ref().or(model.status.as_ref()) {
        root = root.child(
            div()
                .absolute()
                .bottom(px(l.details))
                .left(px(8.))
                .text_size(px(theme::TEXT_XS))
                .bg(rgb(theme::SURFACE_RAISED))
                .child(status.clone()),
        );
    }
    if let Some(cache) = controls.geometry.borrow().as_ref()
        && cache.geometry.omitted_area > 0.
    {
        root = root.child(
            div()
                .absolute()
                .left(px(8.))
                .bottom(px(l.details + 2.))
                .text_size(px(theme::TEXT_XS))
                .child("Tiny items hidden"),
        );
    }
    if model.disk_menu_open {
        let close = model.event(Action::ToggleDiskMenu);
        let mut menu = div()
            .id("disk-choices")
            .debug_selector(|| "disk-choices".into())
            .absolute()
            .top(px(l.top_bar))
            .left(px(90.))
            .w(px(300.))
            .max_h(px(400.))
            .overflow_y_scroll()
            .occlude()
            .bg(rgb(theme::SURFACE_RAISED))
            .p(px(8.))
            .on_mouse_down_out(
                cx.listener(move |this, _, window, cx| handler(this, close.clone(), window, cx)),
            );
        for (i, disk) in model.disks.iter().enumerate() {
            menu = menu.child(button(
                ("choose-disk", i),
                format!(
                    "{} · {}{}",
                    disk.name,
                    disk.status,
                    if disk.connected { "" } else { " · Offline" }
                ),
                Action::ChooseDisk(disk.ticket),
                model,
                cx,
                handler,
            ));
        }
        root = root.child(menu);
    }
    root
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn e2_layout() {
        let l = layout(1280., 800., true, false);
        assert!(l.map.area() >= 1280. * 800. * 0.62);
        assert!(layout(1280., 800., false, false).map.area() >= 1280. * 800. * 0.75);
        assert!(l.top_bar + l.crumb_bar <= 88.);
        assert_eq!(layout(900., 600., true, false).list_width, Some(260.));
        assert_eq!(
            l.map.height - layout(1280., 800., true, true).map.height,
            28.
        );
    }
    #[test]
    fn e7_crumb_overflow() {
        assert_eq!(
            visible_crumbs(9),
            vec![
                CrumbSlot::Crumb(0),
                CrumbSlot::Ellipsis,
                CrumbSlot::Crumb(6),
                CrumbSlot::Crumb(7),
                CrumbSlot::Crumb(8)
            ]
        );
        assert_eq!(visible_crumbs(5).len(), 5);
    }
}
