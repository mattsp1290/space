//! Blocking comparison services; callers own asynchronous generations and retirement.
use super::*;
use space_store::{
    ChangeDirection, ComparisonCursor, ComparisonEndpoint, ComparisonEntry, ComparisonNodeId,
    ComparisonPage, ComparisonProgress, ComparisonView, ScanInfo,
};

pub struct Candidate {
    pub scan: ScanInfo,
    pub path: NativePath,
    pub unavailable: Option<String>,
}
pub struct Candidates {
    pub entries: Vec<Candidate>,
    pub next: Option<ScanId>,
}
pub fn candidates(
    backend: &Backend,
    after: &Snapshot,
    before: Option<ScanId>,
) -> Result<Candidates> {
    let reader = read(backend)?;
    let after_spec = reader.spec(after.scan).map_err(error)?;
    let page = reader.scans(before, PAGE_SIZE).map_err(error)?;
    let entries = page
        .scans
        .into_iter()
        .filter(|scan| {
            scan.status == ScanStatus::Published
                && scan.spec.root_id == after.root.id
                && scan.spec.key.id < after.scan
        })
        .map(|scan| {
            let path = reader.scan_path(scan.spec.key.id).map_err(error)?;
            let mut unavailable =
                validate_comparison(&scan.spec, scan.status, &after_spec, ScanStatus::Published)
                    .err()
                    .map(|e| e.to_string());
            if path.encoding != after.path.encoding {
                unavailable = Some("Native path encodings differ between observations.".into());
            }
            Ok(Candidate {
                scan,
                path,
                unavailable,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Candidates {
        entries,
        next: page.next,
    })
}

pub struct Compared {
    pub view: Mutex<ComparisonView>,
    pub before: Snapshot,
    pub after: Snapshot,
    pub root: ComparisonEntry,
    pub metric: Metric,
    pub filter: QueryFilter,
}
pub fn build(
    backend: &Backend,
    before: ScanId,
    after: ScanId,
    metric: Metric,
    filter: &QueryFilter,
    cancel: &Cancellation,
    progress: &ComparisonProgress,
) -> Result<Arc<Compared>> {
    let reader = backend
        .comparison_reader
        .lock()
        .map_err(|_| "Comparison reader poisoned")?;
    let view = reader
        .compare(before, after, metric, filter, cancel, progress)
        .map_err(error)?;
    let root = view.root().map_err(error)?;
    let before_spec = reader.spec(before).map_err(error)?;
    let after_spec = reader.spec(after).map_err(error)?;
    let before = snapshot(
        &reader,
        reader.root(before_spec.root_id).map_err(error)?,
        before,
    )?;
    let after = snapshot(
        &reader,
        reader.root(after_spec.root_id).map_err(error)?,
        after,
    )?;
    Ok(Arc::new(Compared {
        view: Mutex::new(view),
        before,
        after,
        root,
        metric,
        filter: filter.clone(),
    }))
}
#[derive(Clone, Copy)]
pub enum Ranking {
    Hierarchy,
    Increases,
    Decreases,
    GrowingDirectories,
}
pub struct Queried {
    pub page: ComparisonPage,
    pub current: ComparisonEntry,
    pub ancestors: Vec<(ComparisonNodeId, String)>,
}
pub fn query(
    compared: &Compared,
    parent: ComparisonNodeId,
    ranking: Ranking,
    cursor: Option<ComparisonCursor>,
) -> Result<Queried> {
    let view = compared
        .view
        .lock()
        .map_err(|_| "Comparison view poisoned")?;
    let current = view.entry(parent).map_err(error)?;
    let name = |entry: &ComparisonEntry| {
        entry
            .after
            .as_ref()
            .or(entry.before.as_ref())
            .map(|entry| entry.entry.display_name.clone())
            .unwrap_or_else(|| "Unknown path".into())
    };
    let mut ancestors = vec![(current.id, name(&current))];
    let mut previous = current.parent;
    while let Some(id) = previous {
        if ancestors.len() >= 4096 || ancestors.iter().any(|entry| entry.0 == id) {
            return Err("Invalid comparison ancestor chain.".into());
        }
        let entry = view.entry(id).map_err(error)?;
        previous = entry.parent;
        ancestors.push((entry.id, name(&entry)));
    }
    ancestors.reverse();
    let page = match ranking {
        Ranking::Hierarchy => view.children(parent, compared.metric, cursor, PAGE_SIZE),
        Ranking::Increases => view.largest(
            Some(parent),
            compared.metric,
            ChangeDirection::Increase,
            cursor,
            PAGE_SIZE,
        ),
        Ranking::Decreases => view.largest(
            Some(parent),
            compared.metric,
            ChangeDirection::Decrease,
            cursor,
            PAGE_SIZE,
        ),
        Ranking::GrowingDirectories => {
            view.growing_directories(Some(parent), compared.metric, cursor, PAGE_SIZE)
        }
    }
    .map_err(error)?;
    Ok(Queried {
        page,
        current,
        ancestors,
    })
}
pub fn path(
    backend: &Backend,
    compared: &Compared,
    row: ComparisonNodeId,
    endpoint: ComparisonEndpoint,
    reveal: bool,
) -> Result<PathBuf> {
    let entry = compared
        .view
        .lock()
        .map_err(|_| "Comparison view poisoned")?
        .entry(row)
        .map_err(error)?;
    let (snapshot, node) = match endpoint {
        ComparisonEndpoint::Before => (
            &compared.before,
            entry.before.as_ref().map(|e| e.entry.node),
        ),
        ComparisonEndpoint::After => (&compared.after, entry.after.as_ref().map(|e| e.entry.node)),
    };
    let node = node.ok_or("This comparison endpoint has no observed native path.")?;
    if reveal {
        validated_reveal_path(backend, snapshot, node)
    } else {
        native_path(backend, snapshot, node)
    }
}
