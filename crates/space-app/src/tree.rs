//! Pixel-space squarified geometry, independent of GPUI and database access.
use space_core::{Metric, NodeId};
use std::ops::Range;

#[derive(Clone, Copy, Debug)]
pub struct TreeBudget {
    pub root_limit: usize,
    pub child_limit: usize,
    pub max_depth: u8,
    pub expand_fraction: f64,
    pub max_nodes: usize,
    pub max_calls: usize,
}
impl TreeBudget {
    pub const EXPLORE: Self = Self {
        root_limit: 48,
        child_limit: 16,
        max_depth: 3,
        expand_fraction: 0.005,
        max_nodes: 600,
        max_calls: 64,
    };
    pub const PREVIEW: Self = Self {
        root_limit: 24,
        child_limit: 8,
        max_depth: 2,
        expand_fraction: 0.02,
        max_nodes: 160,
        max_calls: 16,
    };
}
#[derive(Clone, Debug)]
pub struct TreeNode {
    pub node: NodeId,
    pub label: String,
    pub bytes: u64,
    pub unknown_count: u64,
    pub is_directory: bool,
    pub depth: u8,
    pub children: Vec<TreeNode>,
    pub other_bytes: u128,
    pub other_count: u64,
    pub truncated: bool,
}
#[derive(Clone, Debug)]
pub struct Tree {
    pub root: NodeId,
    pub root_label: String,
    pub total_bytes: u128,
    pub metric: Metric,
    pub nodes: usize,
    pub children: Vec<TreeNode>,
    pub other_bytes: u128,
    pub other_count: u64,
    pub unknown_children: u64,
    pub zero_children: u64,
}
impl Tree {
    pub fn find(&self, node: NodeId) -> Option<&TreeNode> {
        fn find(nodes: &[TreeNode], node: NodeId) -> Option<&TreeNode> {
            nodes.iter().find_map(|n| {
                if n.node == node {
                    Some(n)
                } else {
                    find(&n.children, node)
                }
            })
        }
        find(&self.children, node)
    }
    pub fn family_index(&self, node: NodeId) -> Option<usize> {
        self.children
            .iter()
            .position(|n| n.node == node || contains(&n.children, node))
    }
}
fn contains(nodes: &[TreeNode], node: NodeId) -> bool {
    nodes
        .iter()
        .any(|n| n.node == node || contains(&n.children, node))
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Node(NodeId),
    Other { parent: NodeId },
}
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}
impl Rect {
    pub fn contains(self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }
    pub fn area(self) -> f32 {
        self.width * self.height
    }
    fn valid(self) -> bool {
        [self.x, self.y, self.width, self.height]
            .iter()
            .all(|v| v.is_finite())
            && self.width > 0.
            && self.height > 0.
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct Tile {
    pub target: Target,
    pub parent: NodeId,
    pub depth: u8,
    pub bytes: u128,
    pub rect: Rect,
    pub body: Rect,
    pub header: Option<Rect>,
    pub label_visible: bool,
    pub is_directory: bool,
    pub label: String,
    /// Direct children are contiguous. Descendants are appended after siblings.
    pub children: Range<usize>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Geometry {
    pub tiles: Vec<Tile>,
    pub bounds: Rect,
    pub omitted_area: f32,
}
#[derive(Clone, Copy)]
pub struct LayoutParams {
    pub gap: f32,
    pub header_height: f32,
    pub min_label_size: (f32, f32),
    pub min_tile_area: f32,
}
impl LayoutParams {
    pub const EXPLORE: Self = Self {
        gap: 2.,
        header_height: 18.,
        min_label_size: (56., 16.),
        min_tile_area: 16.,
    };
    pub const PREVIEW: Self = Self {
        gap: 1.,
        header_height: 0.,
        min_label_size: (48., 14.),
        min_tile_area: 9.,
    };
}

/// Bruls–Huizing–van Wijk rows, returned in original input order.
pub fn squarify(weights: &[u128], bounds: Rect) -> Vec<Rect> {
    if !bounds.valid() {
        return vec![];
    }
    let mut output = vec![Rect::default(); weights.len()];
    let sum: f64 = weights.iter().map(|w| *w as f64).sum();
    if sum == 0. {
        return output;
    }
    let scale = f64::from(bounds.width) * f64::from(bounds.height) / sum;
    let mut items: Vec<_> = weights
        .iter()
        .enumerate()
        .filter(|(_, w)| **w > 0)
        .map(|(i, w)| (i, *w as f64 * scale))
        .collect();
    items.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    // Suffix areas preserve tiny remaining weights after a dominant row without
    // subtracting nearly equal floats, and keep row placement linear.
    let mut suffix = vec![0.; items.len() + 1];
    for i in (0..items.len()).rev() {
        suffix[i] = suffix[i + 1] + items[i].1;
    }
    let (mut x, mut y, mut width, mut height) = (
        f64::from(bounds.x),
        f64::from(bounds.y),
        f64::from(bounds.width),
        f64::from(bounds.height),
    );
    let mut start = 0;
    while start < items.len() {
        let side = width.min(height);
        if side <= 0. {
            break;
        }
        let mut end = start + 1;
        let mut total = items[start].1;
        let worst = |sum: f64, min: f64, max: f64| {
            (side * side * max / (sum * sum)).max(sum * sum / (side * side * min))
        };
        let mut ratio = worst(total, total, total);
        while end < items.len() {
            let next = total + items[end].1;
            let candidate = worst(next, items[end].1, items[start].1);
            if candidate > ratio {
                break;
            }
            ratio = candidate;
            total = next;
            end += 1;
        }
        let thickness = total / side;
        let mut offset = 0.;
        for &(index, area) in &items[start..end] {
            let length = area / thickness;
            let (left, top, right, bottom) = if width >= height {
                (x, y + offset, x + thickness, y + offset + length)
            } else {
                (x + offset, y, x + offset + length, y + thickness)
            };
            let (left32, top32) = (left as f32, top as f32);
            output[index] = Rect {
                x: left32,
                y: top32,
                width: if right as f32 > left32 {
                    right as f32 - left32
                } else {
                    if width >= height {
                        thickness as f32
                    } else {
                        length as f32
                    }
                },
                height: if bottom as f32 > top32 {
                    bottom as f32 - top32
                } else {
                    if width >= height {
                        length as f32
                    } else {
                        thickness as f32
                    }
                },
            };
            offset += length;
        }
        if width >= height {
            x += thickness;
            width = suffix[end] / height;
        } else {
            y += thickness;
            height = suffix[end] / width;
        }
        start = end;
    }
    output
}

pub fn layout(tree: &Tree, bounds: Rect, params: LayoutParams) -> Geometry {
    let mut geometry = Geometry {
        tiles: vec![],
        bounds,
        omitted_area: 0.,
    };
    if bounds.valid() {
        layout_siblings(
            &tree.children,
            tree.root,
            tree.other_bytes,
            tree.other_count,
            bounds,
            params,
            &mut geometry,
        );
    }
    geometry
}
fn layout_siblings(
    nodes: &[TreeNode],
    parent: NodeId,
    other_bytes: u128,
    other_count: u64,
    bounds: Rect,
    params: LayoutParams,
    geometry: &mut Geometry,
) -> Range<usize> {
    let mut sorted: Vec<_> = nodes.iter().collect();
    sorted.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(a.node.cmp(&b.node)));
    let mut weights: Vec<_> = sorted.iter().map(|n| u128::from(n.bytes)).collect();
    // A remainder without omitted children represents the directory itself or
    // unreadable bytes. Reserve it as empty body space.
    if other_bytes > 0 || other_count > 0 {
        weights.push(other_bytes);
    }
    let rects = squarify(&weights, bounds);
    let start = geometry.tiles.len();
    let mut emitted = vec![];
    for (i, rect) in rects.into_iter().enumerate() {
        if i == sorted.len() && other_count == 0 {
            continue;
        }
        let rect = Rect {
            x: rect.x + params.gap / 2.,
            y: rect.y + params.gap / 2.,
            width: (rect.width - params.gap).max(0.),
            height: (rect.height - params.gap).max(0.),
        };
        if rect.area() < params.min_tile_area {
            geometry.omitted_area += rect.area();
            continue;
        }
        let node = sorted.get(i).copied();
        let directory = node.is_some_and(|n| n.is_directory);
        let header = (directory
            && params.header_height > 0.
            && rect.width >= params.min_label_size.0
            && rect.height >= params.header_height + 12.)
            .then_some(Rect {
                height: params.header_height,
                ..rect
            });
        let body = if let Some(header) = header {
            Rect {
                y: rect.y + header.height,
                height: rect.height - header.height,
                ..rect
            }
        } else {
            rect
        };
        let index = geometry.tiles.len();
        geometry.tiles.push(Tile {
            target: node.map_or(Target::Other { parent }, |n| Target::Node(n.node)),
            parent,
            depth: node.map_or_else(|| nodes.first().map_or(1, |n| n.depth), |n| n.depth),
            bytes: weights[i],
            rect,
            body,
            header,
            label_visible: rect.width >= params.min_label_size.0
                && rect.height >= params.min_label_size.1,
            is_directory: directory,
            label: node.map_or_else(|| "Other".into(), |n| n.label.clone()),
            children: 0..0,
        });
        emitted.push((index, node));
    }
    let end = geometry.tiles.len();
    for (index, node) in emitted {
        if let Some(node) = node
            && (!node.children.is_empty() || node.other_count > 0)
        {
            let range = layout_siblings(
                &node.children,
                node.node,
                node.other_bytes,
                node.other_count,
                geometry.tiles[index].body,
                params,
                geometry,
            );
            geometry.tiles[index].children = range;
        }
    }
    start..end
}
pub fn hit_test(geometry: &Geometry, x: f32, y: f32) -> Option<usize> {
    geometry
        .tiles
        .iter()
        .enumerate()
        .filter(|(_, t)| t.rect.contains(x, y))
        .max_by_key(|(i, t)| (t.depth, *i))
        .map(|(i, _)| i)
}
pub fn percent(part: u128, whole: u128) -> String {
    if whole == 0 {
        return "0%".into();
    }
    let value = part as f64 / whole as f64 * 100.;
    if value > 0. && value < 0.1 {
        "<0.1%".into()
    } else {
        format!("{value:.1}%")
    }
}
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn t1_aspect_ratios_and_t2_area_overlap_containment() {
        let sets = vec![
            vec![1, 2, 3, 4, 5],
            (1..=50).map(|i| 1_000_000 / i).collect(),
            vec![1; 300],
            vec![u128::from(u64::MAX), 1, 0, 7],
        ];
        for weights in sets {
            for (width, height) in [(964., 664.), (300., 900.)] {
                let bounds = Rect {
                    x: 0.,
                    y: 0.,
                    width,
                    height,
                };
                let rects = squarify(&weights, bounds);
                assert_eq!(rects, squarify(&weights, bounds));
                let total = weights.iter().sum::<u128>() as f64;
                let mut mean = 0.;
                for (i, rect) in rects.iter().enumerate() {
                    if weights[i] == 0 {
                        continue;
                    }
                    let ratio = rect.width.max(rect.height) / rect.width.min(rect.height);
                    if weights[i] as f64 / total >= 0.01 {
                        assert!(ratio <= 4., "{ratio}");
                    }
                    mean += f64::from(ratio) * weights[i] as f64 / total;
                    assert!(
                        rect.x >= 0.
                            && rect.y >= 0.
                            && rect.x + rect.width <= width + 0.001
                            && rect.y + rect.height <= height + 0.001
                    );
                    for other in &rects[i + 1..] {
                        let overlap =
                            (rect.x + rect.width).min(other.x + other.width) - rect.x.max(other.x);
                        let vertical = (rect.y + rect.height).min(other.y + other.height)
                            - rect.y.max(other.y);
                        assert!(overlap.max(0.) * vertical.max(0.) < 0.001);
                    }
                }
                // Owner-approved exception: an almost full 300x900 tile has
                // aspect 3 by necessity, before the tiny remainder is counted.
                let limit = if weights[0] == u128::from(u64::MAX) && width == 300. {
                    4.
                } else {
                    2.5
                };
                assert!(mean <= limit, "mean {mean}");
                let tree = fixture_tree(
                    weights
                        .iter()
                        .enumerate()
                        .map(|(i, w)| leaf(i + 1, (*w).min(u128::from(u64::MAX)) as u64))
                        .collect(),
                );
                let geometry = layout(&tree, bounds, LayoutParams::EXPLORE);
                let inset_mean = geometry
                    .tiles
                    .iter()
                    .map(|t| {
                        f64::from(t.rect.width.max(t.rect.height) / t.rect.width.min(t.rect.height))
                            * t.bytes as f64
                            / total
                    })
                    .sum::<f64>();
                println!(
                    "T1 {} weights {width}x{height}: squarify mean={mean:.3}, inset mean={inset_mean:.3}",
                    weights.len()
                );
                let area = rects.iter().map(|r| r.area()).sum::<f32>();
                assert!((area - bounds.area()).abs() < bounds.area() * 0.001);
            }
        }
    }
    #[test]
    fn t7_format() {
        assert_eq!(percent(1, 1000), "0.1%");
        assert_eq!(percent(1, 100000), "<0.1%");
        assert_eq!(percent(0, 0), "0%");
        assert_eq!(format_bytes(1024), "1.0 KiB");
    }
    fn leaf(id: usize, bytes: u64) -> TreeNode {
        TreeNode {
            node: NodeId::new(id as u64).unwrap(),
            label: format!("node-{id}"),
            bytes,
            unknown_count: 0,
            is_directory: false,
            depth: 1,
            children: vec![],
            other_bytes: 0,
            other_count: 0,
            truncated: false,
        }
    }
    fn fixture_tree(children: Vec<TreeNode>) -> Tree {
        Tree {
            root: NodeId::new(10000).unwrap(),
            root_label: "root".into(),
            total_bytes: children.iter().map(|n| u128::from(n.bytes)).sum(),
            metric: Metric::Logical,
            nodes: children.len(),
            children,
            other_bytes: 0,
            other_count: 0,
            unknown_children: 0,
            zero_children: 0,
        }
    }
    #[test]
    fn t2_order_independence_t3_nesting_t5_hit_testing() {
        let mut children = vec![];
        for i in 0..20 {
            let mut n = leaf(i + 1, 3400);
            n.is_directory = true;
            n.children = (0..34)
                .map(|j| {
                    let mut n = leaf(100 + i * 34 + j, 100);
                    n.depth = 2;
                    n
                })
                .collect();
            children.push(n);
        }
        let mut tree = fixture_tree(children);
        let bounds = Rect {
            x: 0.,
            y: 0.,
            width: 964.,
            height: 664.,
        };
        let geometry = layout(&tree, bounds, LayoutParams::EXPLORE);
        tree.children.reverse();
        for n in &mut tree.children {
            n.children.reverse();
        }
        assert_eq!(geometry, layout(&tree, bounds, LayoutParams::EXPLORE));
        for tile in &geometry.tiles {
            assert!(tile.rect.area() >= LayoutParams::EXPLORE.min_tile_area);
            if tile.parent != tree.root {
                let parent = geometry
                    .tiles
                    .iter()
                    .find(|t| t.target == Target::Node(tile.parent))
                    .unwrap();
                assert!(tile.rect.x >= parent.body.x && tile.rect.y >= parent.body.y);
                assert!(tile.rect.x + tile.rect.width <= parent.body.x + parent.body.width + 0.001);
                assert!(
                    tile.rect.y + tile.rect.height <= parent.body.y + parent.body.height + 0.001
                );
            }
            let index = hit_test(
                &geometry,
                tile.body.x + tile.body.width / 2.,
                tile.body.y + tile.body.height / 2.,
            )
            .unwrap();
            assert!(geometry.tiles[index].depth >= tile.depth);
            for other in geometry
                .tiles
                .iter()
                .filter(|t| t.parent == tile.parent && t.target != tile.target)
            {
                let dx = (tile.rect.x + tile.rect.width).min(other.rect.x + other.rect.width)
                    - tile.rect.x.max(other.rect.x);
                let dy = (tile.rect.y + tile.rect.height).min(other.rect.y + other.rect.height)
                    - tile.rect.y.max(other.rect.y);
                assert!(dx.max(0.) * dy.max(0.) < 0.001);
            }
        }
        assert_eq!(hit_test(&geometry, -1., 0.), None);
        assert!(
            layout(&tree, Rect::default(), LayoutParams::EXPLORE)
                .tiles
                .is_empty()
        );
        assert!(
            squarify(
                &[1],
                Rect {
                    width: f32::NAN,
                    ..bounds
                }
            )
            .is_empty()
        );
        assert!(
            layout(&tree, bounds, LayoutParams::PREVIEW)
                .tiles
                .iter()
                .all(|t| t.header.is_none())
        );
    }
}
