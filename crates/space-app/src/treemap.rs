//! Bounded one-level treemap. Inputs are cached, filtered, charged aggregates,
//! never raw filesystem sizes or the currently visible page of a list.
use gpui::{Context, Div, Role, SharedString, Window, div, prelude::*, px, relative, rgb};
use space_core::NodeId;

pub const MAX_TILES: usize = 128;

#[derive(Clone, Debug)]
pub struct Item {
    pub node: NodeId,
    pub label: String,
    /// Bounded cached display path/label and coverage details; full selected
    /// path may be loaded separately by the app.
    pub detail: String,
    pub bytes: u64,
    pub unknown_count: u64,
    pub is_directory: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Model {
    pub items: Vec<Item>,
    /// Aggregates for children omitted by the bounded database query.
    pub other_bytes: u128,
    pub other_count: u64,
    /// Whole filtered child scope, including both returned and omitted children.
    pub unknown_count: u64,
    pub zero_count: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Select(NodeId),
    Open(NodeId),
    /// Open the same filtered child scope in its paged list alternative.
    OpenOther,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Node(NodeId),
    Other,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tile {
    pub target: Target,
    pub bytes: u128,
    pub rect: Rect,
}

/// Stable descending weight, then stable node ID. Tiny entries and overflow
/// retain their entire weight in Other. Unknown/zero entries receive no area.
/// One hierarchy level and at most MAX_TILES rectangles are rendered.
pub fn layout(model: &Model, width: f64, height: f64) -> Vec<Tile> {
    if !width.is_finite() || !height.is_finite() || width <= 0. || height <= 0. {
        return vec![];
    }
    let total = model
        .items
        .iter()
        .map(|i| u128::from(i.bytes))
        .sum::<u128>()
        .saturating_add(model.other_bytes);
    if total == 0 {
        return vec![];
    }
    let mut items: Vec<_> = model.items.iter().filter(|i| i.bytes > 0).collect();
    items.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.node.cmp(&b.node)));
    let mut other = model.other_bytes;
    let mut tiles = Vec::with_capacity(MAX_TILES);
    for item in items {
        // Less than 0.1% is grouped, rather than becoming an unlabeled sliver.
        if tiles.len() >= MAX_TILES - 1 || u128::from(item.bytes) < total / 1000 {
            other = other.saturating_add(u128::from(item.bytes));
        } else {
            tiles.push(Tile {
                target: Target::Node(item.node),
                bytes: u128::from(item.bytes),
                rect: Rect::default(),
            });
        }
    }
    if other > 0 {
        tiles.push(Tile {
            target: Target::Other,
            bytes: other,
            rect: Rect::default(),
        });
    }
    // Balanced-by-count bisection bounds geometry recursion to seven levels.
    // All arithmetic ratios are finite, even for very skewed u64 sizes.
    partition(
        &mut tiles,
        Rect {
            x: 0.,
            y: 0.,
            width,
            height,
        },
    );
    tiles
}

fn partition(tiles: &mut [Tile], bounds: Rect) {
    if tiles.len() == 1 {
        tiles[0].rect = bounds;
        return;
    }
    if tiles.is_empty() {
        return;
    }
    let split = tiles.len() / 2;
    let (first, second) = tiles.split_at_mut(split);
    let a = first.iter().map(|t| t.bytes as f64).sum::<f64>();
    let b = second.iter().map(|t| t.bytes as f64).sum::<f64>();
    let ratio = (a / (a + b)).clamp(0., 1.);
    let (left, right) = if bounds.width >= bounds.height {
        let width = bounds.width * ratio;
        (
            Rect { width, ..bounds },
            Rect {
                x: bounds.x + width,
                width: (bounds.width - width).max(0.),
                ..bounds
            },
        )
    } else {
        let height = bounds.height * ratio;
        (
            Rect { height, ..bounds },
            Rect {
                y: bounds.y + height,
                height: (bounds.height - height).max(0.),
                ..bounds
            },
        )
    };
    partition(first, left);
    partition(second, right);
}

/// Keep layout and hit-test geometry free of border/padding minimums. Taffy
/// clamps a bordered box to at least its combined border widths, which would
/// otherwise enlarge a subpixel weighted tile beyond its assigned rectangle.
fn tile_box(rect: Rect) -> Div {
    div()
        .absolute()
        .left(relative(rect.x as f32))
        .top(relative(rect.y as f32))
        .w(relative(rect.width as f32))
        .h(relative(rect.height as f32))
        .min_w_0()
        .min_h_0()
        .overflow_hidden()
}

/// Normalized geometry scales with the GPUI container without reordering tiles
/// on resize. The app owns selection, navigation, and the keyboard list path.
pub fn view<T: 'static>(
    model: &Model,
    selected: Option<NodeId>,
    cx: &mut Context<T>,
    on_event: fn(&mut T, Event, &mut Window, &mut Context<T>),
) -> Div {
    let tiles = layout(model, 1., 1.);
    let has_other = tiles.iter().any(|t| t.target == Target::Other) || model.other_count > 0;
    let mut chart = div()
        .relative()
        .w_full()
        .h(px(240.))
        .overflow_hidden()
        .bg(rgb(0x101920));
    if tiles.is_empty() {
        chart = chart.child(
            "No observed non-zero bytes in this filtered folder. Unknown sizes have no area.",
        );
    }
    for (index, tile) in tiles.into_iter().enumerate() {
        let item = match tile.target {
            Target::Node(node) => model.items.iter().find(|i| i.node == node),
            Target::Other => None,
        };
        let active = item.is_some_and(|i| Some(i.node) == selected);
        let label = match item {
            Some(item) => format!(
                "{}{} · {} B{}",
                if active { "Selected: " } else { "" },
                item.label,
                tile.bytes,
                if item.unknown_count > 0 {
                    format!(" + {} unknown", item.unknown_count)
                } else {
                    String::new()
                }
            ),
            None => format!("Other · {} B · open list", tile.bytes),
        };
        let accessible =
            item.map_or_else(|| label.clone(), |i| format!("{} · {}", i.detail, label));
        let directory = item.is_some_and(|i| i.is_directory);
        let target = tile.target;
        chart = chart.child(
            tile_box(tile.rect)
                .id(("treemap-tile", index))
                .role(Role::Button)
                .aria_label(SharedString::from(accessible))
                .bg(rgb(if active {
                    0x366984
                } else if directory {
                    0x285166
                } else {
                    0x344853
                }))
                .text_xs()
                .cursor_pointer()
                // Decoration is clipped inside the borderless weighted box.
                // It has no event handler and cannot enlarge the outer hit area.
                .child(
                    div()
                        .size_full()
                        .overflow_hidden()
                        .border_1()
                        .border_color(rgb(if active { 0xffffff } else { 0x17212b }))
                        .child(label),
                )
                .on_click(
                    cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                        let action = match target {
                            Target::Other => Event::OpenOther,
                            Target::Node(node) if directory && event.click_count() >= 2 => {
                                Event::Open(node)
                            }
                            Target::Node(node) => Event::Select(node),
                        };
                        on_event(this, action, window, cx);
                    }),
                ),
        );
    }
    let mut result = div().flex().flex_col().gap_1()
        .child("Treemap · observed charged bytes · click to select, double-click folder to open")
        .child(chart)
        .child(format!("{} unknown size observations (no arbitrary area) · {} zero-byte children · one level, at most {} tiles", model.unknown_count, model.zero_count, MAX_TILES));
    if has_other {
        result = result.child(
            div()
                .id("treemap-other")
                .role(Role::Button)
                .aria_label("Open Other in folder list")
                .focusable()
                .tab_stop(true)
                .cursor_pointer()
                .child("Other: open the paged folder list to inspect grouped entries")
                .on_click(cx.listener(move |this, _, window, cx| {
                    on_event(this, Event::OpenOther, window, cx)
                })),
        );
    }
    if let Some(item) = model.items.iter().find(|i| Some(i.node) == selected) {
        result = result.child(format!(
            "Selected: {} · {} B · {} unknown",
            item.detail, item.bytes, item.unknown_count
        ));
    }
    result.child("The folder list provides keyboard navigation and full entry details.")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn model(weights: &[u64]) -> Model {
        Model {
            items: weights
                .iter()
                .enumerate()
                .map(|(i, &bytes)| Item {
                    node: NodeId::new(i as u64 + 1).unwrap(),
                    label: format!("item {i}"),
                    detail: format!("root/item {i}"),
                    bytes,
                    unknown_count: 0,
                    is_directory: false,
                })
                .collect(),
            ..Model::default()
        }
    }
    fn verify(model: &Model, width: f64, height: f64) {
        let tiles = layout(model, width, height);
        assert!(tiles.len() <= MAX_TILES);
        let total = tiles.iter().map(|t| t.bytes).sum::<u128>();
        let expected = model.other_bytes
            + model
                .items
                .iter()
                .map(|i| u128::from(i.bytes))
                .sum::<u128>();
        assert_eq!(total, expected);
        let area = width * height;
        let epsilon = area.max(1.) * 1e-10;
        assert!(
            (tiles
                .iter()
                .map(|t| t.rect.width * t.rect.height)
                .sum::<f64>()
                - area)
                .abs()
                <= epsilon
        );
        for (index, tile) in tiles.iter().enumerate() {
            let r = tile.rect;
            assert!(
                [r.x, r.y, r.width, r.height]
                    .iter()
                    .all(|n| n.is_finite() && *n >= 0.)
            );
            assert!(r.x + r.width <= width + 1e-9 && r.y + r.height <= height + 1e-9);
            assert!(
                (r.width * r.height - area * tile.bytes as f64 / total as f64).abs() <= epsilon
            );
            for other in &tiles[index + 1..] {
                let s = other.rect;
                let overlap_x = ((r.x + r.width).min(s.x + s.width) - r.x.max(s.x)).max(0.);
                let overlap_y = ((r.y + r.height).min(s.y + s.height) - r.y.max(s.y)).max(0.);
                assert!(overlap_x * overlap_y <= epsilon);
            }
        }
    }
    #[test]
    fn preserves_area_bounds_and_no_overlap() {
        for weights in [
            vec![1],
            vec![1, 2, 3, 4, 5],
            vec![u64::MAX, 1, 0, 7],
            (1..=300).collect(),
        ] {
            for (w, h) in [(800., 600.), (1., 2000.), (2000., 1.)] {
                verify(&model(&weights), w, h);
            }
        }
    }
    #[test]
    fn other_preserves_all_omitted_area_and_tile_budget() {
        let mut m = model(&vec![100; 400]);
        m.other_bytes = 777;
        m.other_count = 9;
        let tiles = layout(&m, 1., 1.);
        assert_eq!(tiles.len(), MAX_TILES);
        assert_eq!(tiles.last().unwrap().target, Target::Other);
        assert_eq!(tiles.last().unwrap().bytes, 27300 + 777);
        verify(&m, 1., 1.);
    }
    #[test]
    fn zeros_unknown_and_invalid_bounds_have_no_area() {
        let mut m = model(&[0, 0]);
        m.unknown_count = 2;
        assert!(layout(&m, 100., 100.).is_empty());
        assert!(layout(&Model::default(), 100., 100.).is_empty());
        for (w, h) in [(0., 1.), (-1., 1.), (f64::NAN, 1.), (1., f64::INFINITY)] {
            assert!(layout(&model(&[1]), w, h).is_empty());
        }
    }
    #[test]
    fn deterministic_across_input_order_and_repeat() {
        let m = model(&[5, 8, 5, 1, 9]);
        let expected = layout(&m, 900., 500.);
        assert_eq!(expected, layout(&m, 900., 500.));
        let mut reversed = m.clone();
        reversed.items.reverse();
        assert_eq!(expected, layout(&reversed, 900., 500.));
    }
    #[test]
    fn large_exact_weights_and_other_only_are_safe() {
        verify(&model(&vec![u64::MAX; 200]), 1200., 700.);
        let m = Model {
            other_bytes: u128::from(u64::MAX) * 200,
            other_count: 200,
            ..Model::default()
        };
        verify(&m, 1200., 700.);
        assert_eq!(layout(&m, 1., 1.)[0].target, Target::Other);
    }
    #[test]
    fn normalized_view_coordinates_scale_without_reordering() {
        let m = model(&[500, 100, 20, 4, 1]);
        let normal = layout(&m, 1., 1.);
        let scaled = layout(&m, 800., 800.);
        for (a, b) in normal.iter().zip(scaled) {
            assert_eq!(a.target, b.target);
            assert!((a.rect.x * 800. - b.rect.x).abs() < 1e-9);
            assert!((a.rect.y * 800. - b.rect.y).abs() < 1e-9);
            assert!((a.rect.width * 800. - b.rect.width).abs() < 1e-9);
            assert!((a.rect.height * 800. - b.rect.height).abs() < 1e-9);
        }
    }
    #[test]
    fn subpixel_hit_boxes_have_no_layout_border_or_padding() {
        for weights in [vec![u64::MAX, 1], vec![999, 1], vec![999_000, 1_000, 1]] {
            let m = model(&weights);
            verify(&m, 800., 240.);
            for tile in layout(&m, 1., 1.) {
                let mut element = tile_box(tile.rect);
                let style = element.style();
                assert_eq!(style.border_widths, Default::default());
                assert_eq!(style.padding, Default::default());
                assert_eq!(style.min_size.width, Some(px(0.).into()));
                assert_eq!(style.min_size.height, Some(px(0.).into()));
                assert_eq!(style.overflow.x, Some(gpui::Overflow::Hidden));
                assert_eq!(style.overflow.y, Some(gpui::Overflow::Hidden));
                assert_eq!(
                    style.size.width,
                    Some(relative(tile.rect.width as f32).into())
                );
                assert_eq!(
                    style.size.height,
                    Some(relative(tile.rect.height as f32).into())
                );
            }
        }
    }
    #[test]
    fn tiny_values_are_labeled_other_without_losing_bytes() {
        let m = model(&[1_000_000, 1, 2, 0]);
        let tiles = layout(&m, 1., 1.);
        assert_eq!(tiles.len(), 2);
        assert_eq!(tiles[1].target, Target::Other);
        assert_eq!(tiles[1].bytes, 3);
        verify(&m, 1., 1.);
    }
}
