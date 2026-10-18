//! Ordinary accessible GPUI tiles, sized from the actual measured map bounds.
use crate::{
    theme,
    tree::{self, Geometry, LayoutParams, Rect, Target},
};
use gpui::{
    App, Context, Div, IntoElement, Pixels, Point, Role, Size, WeakEntity, Window, canvas,
    container_query, div, point, prelude::*, px, rgb,
};
use space_core::NodeId;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
};
pub struct GeometryCache {
    pub tree: Arc<tree::Tree>,
    pub size: Size<Pixels>,
    pub geometry: Rc<Geometry>,
}
#[derive(Clone, Copy)]
pub struct Params {
    pub layout: LayoutParams,
    pub interactive: bool,
}
impl Params {
    pub const PREVIEW: Self = Self {
        layout: LayoutParams::PREVIEW,
        interactive: false,
    };
    pub const EXPLORE: Self = Self {
        layout: LayoutParams::EXPLORE,
        interactive: true,
    };
}
#[derive(Clone, Copy)]
pub enum TileEvent {
    Select(NodeId),
    Zoom(NodeId),
    Hover(Option<Target>),
    OpenOther(NodeId),
}
pub type TileHandler<T> = Rc<dyn Fn(&mut T, TileEvent, &mut Window, &mut Context<T>)>;
fn dispatch<T: 'static>(
    entity: &WeakEntity<T>,
    handler: &TileHandler<T>,
    event: TileEvent,
    window: &mut Window,
    cx: &mut App,
) {
    let _ = entity.update(cx, |this, cx| handler(this, event, window, cx));
}
fn other_caption(parent: NodeId, root: NodeId, count: u64) -> String {
    if parent == root {
        format!("{count} more items")
    } else {
        "More items; open to browse".into()
    }
}
fn tile_box(rect: Rect, origin: Rect) -> Div {
    div()
        .absolute()
        .left(px(rect.x - origin.x))
        .top(px(rect.y - origin.y))
        .w(px(rect.width))
        .h(px(rect.height))
        .min_w_0()
        .min_h_0()
        .overflow_hidden()
}
struct Frame<T: 'static> {
    tree: Arc<tree::Tree>,
    geometry: Rc<Geometry>,
    entity: WeakEntity<T>,
    handler: TileHandler<T>,
    params: Params,
    selected: Option<NodeId>,
    hovered: Option<Target>,
}
impl<T: 'static> Frame<T> {
    fn tile(&self, index: usize, origin: Rect) -> gpui::Stateful<Div> {
        let tile = &self.geometry.tiles[index];
        let target = tile.target;
        let selected = matches!(target,Target::Node(n) if self.selected==Some(n));
        let state = if selected {
            theme::TileState::Selected
        } else if self.hovered == Some(target) {
            theme::TileState::Hovered
        } else {
            theme::TileState::Normal
        };
        let color = match target {
            Target::Node(n) => theme::tile_color(
                self.tree.family_index(n).unwrap_or(0),
                tile.depth,
                tile.is_directory,
                state,
            ),
            Target::Other { .. } => rgb(theme::OTHER_TILE).into(),
        };
        let mut element = tile_box(tile.rect, origin)
            .id(("tile", index))
            .debug_selector(move || format!("tile-{index}"))
            .rounded(px(theme::RADIUS_1))
            .bg(color);
        if self.params.interactive {
            let entity = self.entity.clone();
            let handler = self.handler.clone();
            let directory = tile.is_directory;
            element = element
                .role(Role::Button)
                .aria_label(format!(
                    "{} · {}",
                    tile.label,
                    tree::format_bytes(tile.bytes)
                ))
                .cursor_pointer()
                .on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    let event = match target {
                        Target::Node(n) if directory && event.click_count() >= 2 => {
                            TileEvent::Zoom(n)
                        }
                        Target::Node(n) => TileEvent::Select(n),
                        Target::Other { parent } => TileEvent::OpenOther(parent),
                    };
                    dispatch(&entity, &handler, event, window, cx);
                });
        }
        if tile.label_visible {
            let label = div()
                .absolute()
                .top_0()
                .left_0()
                .w_full()
                .h(px(tile.header.map_or(
                    if !tile.is_directory && tile.rect.height >= 34. {
                        32.
                    } else {
                        16.
                    },
                    |h| h.height,
                )))
                .px(px(4.))
                .text_size(px(theme::TEXT_XS))
                .overflow_hidden()
                .child(div().text_ellipsis().child(match target {
                    Target::Other { parent } => format!(
                        "Other · {} · {}",
                        tree::format_bytes(tile.bytes),
                        other_caption(parent, self.tree.root, self.tree.other_count)
                    ),
                    _ if tile.header.is_some() => {
                        format!("{} · {}", tile.label, tree::format_bytes(tile.bytes))
                    }
                    _ => tile.label.clone(),
                }))
                .when(
                    !tile.is_directory
                        && !matches!(target, Target::Other { .. })
                        && tile.rect.height >= 34.,
                    |d| d.child(div().text_ellipsis().child(tree::format_bytes(tile.bytes))),
                );
            if self.params.interactive
                && tile.header.is_some()
                && let Target::Node(n) = target
            {
                let entity = self.entity.clone();
                let handler = self.handler.clone();
                element = element.child(
                    label
                        .id(("tile-head", index))
                        .debug_selector(move || format!("tile-head-{index}"))
                        .role(Role::Button)
                        .aria_label(format!("Zoom into {}", tile.label))
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            dispatch(&entity, &handler, TileEvent::Zoom(n), window, cx);
                        }),
                );
            } else {
                element = element.child(label);
            }
        }
        let mut body = tile_box(tile.body, tile.rect);
        for child in tile.children.clone() {
            body = body.child(self.tile(child, tile.body));
        }
        element = element.child(body);
        if selected {
            element = element.child(
                div()
                    .absolute()
                    .size_full()
                    .border_2()
                    .border_color(rgb(theme::ACCENT))
                    .rounded(px(theme::RADIUS_1)),
            );
        }
        element
    }
}
pub fn render<T: 'static>(
    tree: Arc<tree::Tree>,
    params: Params,
    selected: Option<NodeId>,
    hovered: Option<Target>,
    cache: Rc<RefCell<Option<GeometryCache>>>,
    cx: &mut Context<T>,
    handler: TileHandler<T>,
) -> impl IntoElement {
    let entity = cx.entity().downgrade();
    container_query(move |size, _, _| {
        let geometry = {
            let mut cached = cache.borrow_mut();
            if cached
                .as_ref()
                .is_none_or(|c| !Arc::ptr_eq(&tree, &c.tree) || c.size != size)
            {
                *cached = Some(GeometryCache {
                    tree: tree.clone(),
                    size,
                    geometry: Rc::new(tree::layout(
                        &tree,
                        Rect {
                            x: 0.,
                            y: 0.,
                            width: size.width.into(),
                            height: size.height.into(),
                        },
                        params.layout,
                    )),
                });
            }
            cached.as_ref().unwrap().geometry.clone()
        };
        let frame = Frame {
            tree: tree.clone(),
            geometry: geometry.clone(),
            entity: entity.clone(),
            handler: handler.clone(),
            params,
            selected,
            hovered,
        };
        let mut root = div()
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(rgb(theme::MAP_BG))
            .id("treemap");
        if params.interactive {
            let origin = Rc::new(Cell::new(point(px(0.), px(0.))));
            let probe = origin.clone();
            root = root.child(
                canvas(
                    |bounds, _, _| bounds.origin,
                    move |_, point, _, _| probe.set(point),
                )
                .absolute()
                .size_full(),
            );
            let hit_geometry = geometry.clone();
            let hit_entity = entity.clone();
            let hit_handler = handler.clone();
            root = root.on_mouse_move(move |event, window, cx| {
                let Point { x, y } = event.position - origin.get();
                let target = tree::hit_test(&hit_geometry, x.into(), y.into())
                    .map(|i| hit_geometry.tiles[i].target);
                if target != hovered {
                    dispatch(
                        &hit_entity,
                        &hit_handler,
                        TileEvent::Hover(target),
                        window,
                        cx,
                    );
                }
            });
            let leave_entity = entity.clone();
            let leave_handler = handler.clone();
            root = root.on_hover(move |inside, window, cx| {
                if !inside {
                    dispatch(
                        &leave_entity,
                        &leave_handler,
                        TileEvent::Hover(None),
                        window,
                        cx,
                    );
                }
            });
        }
        for (i, tile) in geometry
            .tiles
            .iter()
            .enumerate()
            .filter(|(_, t)| t.parent == tree.root)
        {
            let _ = tile;
            root = root.child(frame.tile(i, geometry.bounds));
        }
        if params.interactive
            && let Some(target) = hovered
            && let Some(tile) = geometry.tiles.iter().find(|t| t.target == target)
        {
            let detail = match target {
                Target::Node(n) => tree
                    .find(n)
                    .map(|n| {
                        format!(
                            "{} · {} shown{} · {} unknown",
                            if n.is_directory { "folder" } else { "file" },
                            n.children.len(),
                            if n.truncated || n.other_count > 0 {
                                " + more"
                            } else {
                                ""
                            },
                            n.unknown_count
                        )
                    })
                    .unwrap_or_default(),
                Target::Other { parent } => other_caption(parent, tree.root, tree.other_count),
            };
            root = root.child(
                div()
                    .id("explore-tooltip")
                    .debug_selector(|| "explore-tooltip".into())
                    .absolute()
                    .left(px(
                        (tile.rect.x + 8.).min((geometry.bounds.width - 260.).max(0.))
                    ))
                    .top(px(
                        (tile.rect.y + 26.).min((geometry.bounds.height - 70.).max(0.))
                    ))
                    .w(px(260.))
                    .max_h(px(70.))
                    .overflow_hidden()
                    .text_ellipsis()
                    .p(px(8.))
                    .rounded(px(6.))
                    .bg(rgb(theme::SURFACE_RAISED))
                    .text_size(px(theme::TEXT_XS))
                    .child(format!(
                        "{} · {} {} · {} of {}",
                        tile.label,
                        tree::format_bytes(tile.bytes),
                        crate::metric_label(tree.metric),
                        tree::percent(tile.bytes, tree.total_bytes),
                        tree.root_label
                    ))
                    .child(div().child(detail)),
            );
        }
        root
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn e6_tile_geometry_has_no_border_or_padding() {
        let mut tile = tile_box(
            Rect {
                x: 2.,
                y: 3.,
                width: 0.1,
                height: 0.2,
            },
            Rect::default(),
        );
        let style = tile.style();
        assert_eq!(style.position, Some(gpui::Position::Absolute));
        assert_eq!(style.size.width, Some(px(0.1).into()));
        assert_eq!(style.border_widths.left, None);
        assert_eq!(style.padding.left, None);
        assert_eq!(style.overflow.x, Some(gpui::Overflow::Hidden));
        assert_eq!(style.overflow.y, Some(gpui::Overflow::Hidden));
    }
    #[test]
    fn review_other_caption_preserves_count_uncertainty() {
        let root = NodeId::new(1).unwrap();
        assert_eq!(other_caption(root, root, 89), "89 more items");
        assert_eq!(
            other_caption(NodeId::new(2).unwrap(), root, 1),
            "More items; open to browse"
        );
    }
}
