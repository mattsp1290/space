//! Bounded assembly shared by normal and name-filtered views.
use super::*;
use crate::tree::{Tree, TreeBudget, TreeNode};
use std::collections::{BTreeMap, BinaryHeap};

pub struct TreeAbort(pub Arc<AtomicU64>, pub u64);
impl TreeAbort {
    fn superseded(&self) -> bool {
        self.0.load(Ordering::Acquire) != self.1
    }
}
trait Source {
    fn entry(&self, node: NodeId) -> Result<StoredEntry>;
    fn summary(&self, node: NodeId, limit: usize) -> Result<ChildrenSummary>;
    fn children(&self, node: NodeId, limit: usize) -> Result<Page>;
}
struct Normal<'a> {
    backend: &'a Backend,
    scan: ScanId,
    metric: Metric,
}
impl Source for Normal<'_> {
    fn entry(&self, node: NodeId) -> Result<StoredEntry> {
        read(self.backend)?.entry(self.scan, node).map_err(error)
    }
    fn summary(&self, node: NodeId, limit: usize) -> Result<ChildrenSummary> {
        read(self.backend)?
            .children_summary(self.scan, node, self.metric, limit)
            .map_err(error)
    }
    fn children(&self, node: NodeId, limit: usize) -> Result<Page> {
        read(self.backend)?
            .children_sorted(self.scan, node, self.metric, None, limit)
            .map_err(error)
    }
}
struct FilterSource<'a>(&'a Filtered);
impl Source for FilterSource<'_> {
    fn entry(&self, node: NodeId) -> Result<StoredEntry> {
        if node == self.0.root.entry.node {
            return Ok(self.0.root.clone());
        }
        self.0
            .view
            .lock()
            .map_err(error)?
            .entry(node)
            .map_err(error)
    }
    fn summary(&self, node: NodeId, limit: usize) -> Result<ChildrenSummary> {
        self.0
            .view
            .lock()
            .map_err(error)?
            .children_summary(node, limit)
            .map_err(error)
    }
    fn children(&self, node: NodeId, limit: usize) -> Result<Page> {
        self.0
            .view
            .lock()
            .map_err(error)?
            .children(node, None, limit)
            .map_err(error)
    }
}
pub fn tree(
    backend: &Backend,
    scan: ScanId,
    parent: NodeId,
    metric: Metric,
    budget: TreeBudget,
    abort: &TreeAbort,
) -> Result<Option<Tree>> {
    assemble(
        &Normal {
            backend,
            scan,
            metric,
        },
        parent,
        metric,
        budget,
        abort,
    )
}
pub fn tree_filtered(
    filtered: &Filtered,
    parent: NodeId,
    metric: Metric,
    budget: TreeBudget,
    abort: &TreeAbort,
) -> Result<Option<Tree>> {
    assemble(&FilterSource(filtered), parent, metric, budget, abort)
}
fn total(row: &StoredEntry, metric: Metric) -> MetricTotal {
    if metric == Metric::Logical {
        row.logical
    } else {
        row.allocated
    }
}
fn node(row: StoredEntry, depth: u8, metric: Metric) -> TreeNode {
    let total = total(&row, metric);
    TreeNode {
        node: row.entry.node,
        label: row.entry.display_name,
        bytes: total.observed_bytes,
        unknown_count: total.unknown_count,
        is_directory: row.entry.kind == EntryKind::Directory,
        depth,
        children: vec![],
        other_bytes: 0,
        other_count: 0,
        truncated: false,
    }
}
fn assemble(
    source: &impl Source,
    parent: NodeId,
    metric: Metric,
    budget: TreeBudget,
    abort: &TreeAbort,
) -> Result<Option<Tree>> {
    if abort.superseded() {
        return Ok(None);
    }
    if budget.max_calls < 2 || budget.max_nodes == 0 || budget.max_depth == 0 {
        return Err(
            "Tree budget must allow root entry and summary calls and at least one level/node."
                .into(),
        );
    }
    let root = source.entry(parent)?;
    if abort.superseded() {
        return Ok(None);
    }
    if root.entry.kind != EntryKind::Directory {
        return Err("A treemap scope must be a directory.".into());
    }
    let summary = source.summary(parent, budget.root_limit.min(budget.max_nodes))?;
    if abort.superseded() {
        return Ok(None);
    }
    let total_bytes = u128::from(total(&root, metric).observed_bytes);
    let root_ids: Vec<_> = summary.page.entries.iter().map(|r| r.entry.node).collect();
    let mut nodes: BTreeMap<_, _> = summary
        .page
        .entries
        .into_iter()
        .map(|r| {
            let n = node(r, 1, metric);
            (n.node, n)
        })
        .collect();
    let mut heap = BinaryHeap::new();
    let eligible = |n: &TreeNode| {
        n.is_directory
            && n.depth < budget.max_depth
            && total_bytes > 0
            && n.bytes as f64 >= budget.expand_fraction * total_bytes as f64
    };
    for n in nodes.values() {
        if eligible(n) {
            heap.push((n.bytes, std::cmp::Reverse(n.node)));
        }
    }
    let mut links = BTreeMap::new();
    let mut calls = 2;
    while let Some((_, std::cmp::Reverse(id))) = heap.pop() {
        if abort.superseded() {
            return Ok(None);
        }
        if calls >= budget.max_calls || nodes.len() >= budget.max_nodes {
            nodes.get_mut(&id).unwrap().truncated = true;
            continue;
        }
        let depth = nodes[&id].depth + 1;
        let page = source.children(id, budget.child_limit.min(budget.max_nodes - nodes.len()))?;
        calls += 1;
        if abort.superseded() {
            return Ok(None);
        }
        let child_bytes = page
            .entries
            .iter()
            .map(|r| u128::from(total(r, metric).observed_bytes))
            .sum::<u128>();
        let ids = page
            .entries
            .iter()
            .map(|r| r.entry.node)
            .collect::<Vec<_>>();
        let n = nodes.get_mut(&id).unwrap();
        n.other_bytes = u128::from(n.bytes).saturating_sub(child_bytes);
        n.other_count = u64::from(page.next.is_some());
        for row in page.entries {
            let n = node(row, depth, metric);
            if eligible(&n) {
                heap.push((n.bytes, std::cmp::Reverse(n.node)));
            }
            nodes.insert(n.node, n);
        }
        links.insert(id, ids);
    }
    let count = nodes.len();
    fn take(
        id: NodeId,
        nodes: &mut BTreeMap<NodeId, TreeNode>,
        links: &mut BTreeMap<NodeId, Vec<NodeId>>,
    ) -> TreeNode {
        let mut n = nodes.remove(&id).expect("unique cached tree node");
        n.children = links
            .remove(&id)
            .unwrap_or_default()
            .into_iter()
            .map(|id| take(id, nodes, links))
            .collect();
        n
    }
    let children = root_ids
        .into_iter()
        .map(|id| take(id, &mut nodes, &mut links))
        .collect();
    Ok(Some(Tree {
        root: parent,
        root_label: root.entry.display_name,
        total_bytes,
        metric,
        nodes: count,
        children,
        other_bytes: u128::from(summary.other_bytes),
        other_count: summary.other_count,
        unknown_children: summary.unknown_count,
        zero_children: summary.zero_count,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::{self, LayoutParams, Rect, Target};
    #[test]
    fn t4_nested_fixture_and_t6_budgets_and_abort() {
        let fixture = crate::test_fixture::Fixture::new();
        let snapshot = open_root(&fixture.backend, fixture.roots[0].clone()).unwrap();
        let parent = snapshot.root_entry.entry.node;
        let abort = TreeAbort(Arc::new(AtomicU64::new(1)), 1);
        let result = tree(
            &fixture.backend,
            snapshot.scan,
            parent,
            Metric::Logical,
            TreeBudget::EXPLORE,
            &abort,
        )
        .unwrap()
        .unwrap();
        assert!(result.nodes <= TreeBudget::EXPLORE.max_nodes);
        let mut unique = std::collections::BTreeSet::new();
        fn verify(
            nodes: &[TreeNode],
            limit: u128,
            unique: &mut std::collections::BTreeSet<NodeId>,
        ) {
            assert!(nodes.iter().map(|n| u128::from(n.bytes)).sum::<u128>() <= limit);
            for n in nodes {
                assert!(unique.insert(n.node));
                assert!(n.depth <= 3);
                verify(&n.children, u128::from(n.bytes), unique);
            }
        }
        verify(&result.children, result.total_bytes, &mut unique);
        assert_eq!(unique.len(), result.nodes);
        let geometry = tree::layout(
            &result,
            Rect {
                x: 0.,
                y: 0.,
                width: 964.,
                height: 664.,
            },
            LayoutParams::EXPLORE,
        );
        assert!(
            geometry
                .tiles
                .iter()
                .any(|t| t.depth == 2 && t.label_visible)
        );
        for tile in &geometry.tiles {
            if tile.depth == 1
                && tile.is_directory
                && tile.bytes as f64 / result.total_bytes as f64 >= 0.02
            {
                assert!(tile.header.is_some());
            }
        }
        for t in &geometry.tiles {
            if let Target::Node(id) = t.target {
                assert!(result.find(id).is_some());
                assert!(result.family_index(id).is_some());
            }
        }
        let small = tree(
            &fixture.backend,
            snapshot.scan,
            parent,
            Metric::Logical,
            TreeBudget {
                max_calls: 3,
                ..TreeBudget::EXPLORE
            },
            &abort,
        )
        .unwrap()
        .unwrap();
        assert!(
            small
                .children
                .iter()
                .filter(|n| !n.children.is_empty())
                .count()
                <= 1
        );
        assert!(
            small
                .children
                .iter()
                .any(|n| n.truncated || n.children.iter().any(|c| c.truncated))
        );
        let no_expand = tree(
            &fixture.backend,
            snapshot.scan,
            parent,
            Metric::Logical,
            TreeBudget {
                expand_fraction: 1.,
                ..TreeBudget::EXPLORE
            },
            &abort,
        )
        .unwrap()
        .unwrap();
        assert!(no_expand.children.iter().all(|n| n.children.is_empty()));
        abort.0.store(2, Ordering::Release);
        assert!(
            tree(
                &fixture.backend,
                snapshot.scan,
                parent,
                Metric::Logical,
                TreeBudget::EXPLORE,
                &abort
            )
            .unwrap()
            .is_none()
        );
    }
}
