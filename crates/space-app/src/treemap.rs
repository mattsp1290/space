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

/// A chart can fill a constrained flex workspace or use a caller-computed height.
/// Height is available for dashboard previews; Fill is for the focused explorer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Size {
    Fill,
    Height(f32),
}

/// Human-readable observed bytes. Units are binary, including for large Other
/// aggregates; this is display rounding, never input to accounting or geometry.
pub fn format_bytes(bytes: u128) -> String {
    const UNITS: [&str; 9] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB", "ZiB", "YiB"];
    let mut amount = bytes as f64;
    let mut unit = 0;
    while amount >= 1024. && unit + 1 < UNITS.len() {
        amount /= 1024.;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{amount:.1} {}", UNITS[unit])
    }
}

fn chart_box(size: Size) -> Div {
    let chart = div()
        .relative()
        .w_full()
        .min_w_0()
        .min_h_0()
        .overflow_hidden()
        .bg(rgb(0x111b24));
    match size {
        Size::Fill => chart.flex_1(),
        Size::Height(height) => chart.h(px(if height.is_finite() {
            height.max(0.)
        } else {
            0.
        })),
    }
}

/// Stable restrained colors follow the name, never the current sort position.
/// Selection also has a white outline and text, so color is not its only cue.
fn tile_color(label: &str, active: bool) -> u32 {
    if active {
        return 0x386e86;
    }
    const PALETTE: [u32; 6] = [0x294f5d, 0x36546b, 0x4d4e67, 0x3e5c50, 0x615345, 0x4f5f6b];
    let hash = label.bytes().fold(0usize, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(byte as usize)
    });
    PALETTE[hash % PALETTE.len()]
}

/// Normalized geometry scales with the GPUI container without reordering tiles.
/// The closure captures the caller's complete card/scan/query ticket. The caller
/// must revalidate it before resolving a NodeId or opening the list alternative.
/// Individual tiles deliberately do not add 128 keyboard tab stops; the always
/// available folder-list action exposes equivalent keyboard navigation.
pub fn view_sized<T: 'static, F>(
    model: &Model,
    selected: Option<NodeId>,
    size: Size,
    cx: &mut Context<T>,
    on_event: F,
) -> Div
where
    F: Fn(&mut T, Event, &mut Window, &mut Context<T>) + Clone + 'static,
{
    let tiles = layout(model, 1., 1.);
    let has_other = tiles.iter().any(|t| t.target == Target::Other) || model.other_count > 0;
    let mut chart = chart_box(size);
    if tiles.is_empty() {
        chart =
            chart.child(
                div().p_4().text_sm().text_color(rgb(0xa7b9c8)).child(
                    "No observed non-zero bytes in this folder. Unknown sizes have no area.",
                ),
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
                "{}{} · {}{}",
                if active { "Selected: " } else { "" },
                item.label,
                format_bytes(tile.bytes),
                if item.unknown_count > 0 {
                    format!(" + {} unknown", item.unknown_count)
                } else {
                    String::new()
                }
            ),
            None => format!("Other · {} · open list", format_bytes(tile.bytes)),
        };
        let accessible =
            item.map_or_else(|| label.clone(), |i| format!("{} · {}", i.detail, label));
        let directory = item.is_some_and(|i| i.is_directory);
        let target = tile.target;
        let callback = on_event.clone();
        chart = chart.child(
            tile_box(tile.rect)
                .id(("treemap-tile", index))
                .role(Role::Button)
                .aria_label(SharedString::from(accessible))
                .bg(rgb(tile_color(
                    item.map_or("Other", |i| i.label.as_str()),
                    active,
                )))
                .text_color(rgb(0xf2f6fa))
                .text_xs()
                .cursor_pointer()
                .hover(|style| style.bg(rgb(0x426478)))
                // Decoration is clipped inside the borderless weighted box.
                // It cannot enlarge the outer hit area, even for tiny entries.
                .child(
                    div()
                        .size_full()
                        .overflow_hidden()
                        .border_1()
                        .border_color(rgb(if active { 0xffffff } else { 0x17212b }))
                        .p_1()
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
                        callback(this, action, window, cx);
                    }),
                ),
        );
    }
    let mut result = div()
        .flex()
        .flex_col()
        .min_w_0()
        .min_h_0()
        .gap_1()
        .when(size == Size::Fill, |element| element.flex_1().h_full())
        .child(chart);
    if model.unknown_count > 0 || model.zero_count > 0 {
        result = result.child(div().text_xs().text_color(rgb(0xa7b9c8)).child(format!(
            "{} unknown sizes · {} zero-byte entries · no proportional area",
            model.unknown_count, model.zero_count
        )));
    }
    result.child(
        div()
            .id("treemap-list")
            .role(Role::Button)
            .aria_label("Browse folder list, including Other and unknown sizes")
            .focusable()
            .tab_stop(true)
            .border_1()
            .border_color(rgb(0x344656))
            .rounded_md()
            .px_2()
            .py_1()
            .text_xs()
            .text_color(rgb(0xc4d8e6))
            .focus(|style| style.border_color(rgb(0x90d5ff)))
            .cursor_pointer()
            .child(if has_other || model.unknown_count > 0 {
                "Browse folder list, Other and unknown sizes →"
            } else {
                "Browse folder list →"
            })
            .on_click(
                cx.listener(move |this, _, window, cx| {
                    on_event(this, Event::OpenOther, window, cx)
                }),
            ),
    )
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
    fn available_area_styles_replace_the_fixed_height() {
        let mut fill = chart_box(Size::Fill);
        assert_eq!(fill.style().size.height, None);
        assert_eq!(fill.style().min_size.height, Some(px(0.).into()));
        assert_eq!(fill.style().flex_grow, Some(1.));
        for height in [220., 420., 610.] {
            let mut chart = chart_box(Size::Height(height));
            assert_eq!(chart.style().size.height, Some(px(height).into()));
        }
    }
    #[test]
    fn labels_are_readable_and_colors_follow_names() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.0 GiB");
        assert_eq!(
            tile_color("Documents", false),
            tile_color("Documents", false)
        );
        assert_ne!(
            tile_color("Documents", false),
            tile_color("Documents", true)
        );
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
