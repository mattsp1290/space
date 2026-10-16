//! Synchronous service boundary. Every function is called off the GPUI thread.
#[path = "comparison_backend.rs"]
pub mod comparison;
use space_core::*;
use space_store::{
    Cache, CacheReader, ChildrenSummary, FilteredView, Page, PageCursor, QueryFilter, RootRecord,
    ScanPin, StoredEntry,
};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone)]
pub struct Backend {
    writer: Arc<Mutex<Cache>>,
    reader: Arc<Mutex<CacheReader>>,
    filter_reader: Arc<Mutex<CacheReader>>,
    comparison_reader: Arc<Mutex<CacheReader>>,
}
pub type Result<T> = std::result::Result<T, String>;
pub const PAGE_SIZE: usize = 100;

pub struct Snapshot {
    pub root: RootRecord,
    pub scan: ScanId,
    pub spec: ScanSpec,
    pub path: NativePath,
    pub interval: (Timestamp, Option<Timestamp>),
    pub root_entry: StoredEntry,
    pub _pin: ScanPin,
}
pub struct Opened {
    pub backend: Backend,
    pub roots: Vec<RootRecord>,
    pub snapshot: Option<Snapshot>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Folders,
    Largest,
    Treemap,
}
#[derive(Clone)]
pub struct Query {
    pub scan: ScanId,
    pub parent: NodeId,
    pub view: View,
    pub metric: Metric,
    pub cursor: Option<PageCursor>,
    pub subtree_only: bool,
    pub filtered: Option<crate::deferred::Deferred<Filtered>>,
}
#[derive(Default)]
pub struct Progress {
    pub entries: AtomicU64,
    pub batches: AtomicU64,
}

fn lock(backend: &Backend) -> Result<std::sync::MutexGuard<'_, Cache>> {
    backend
        .writer
        .lock()
        .map_err(|_| "Cache worker lock poisoned; restart the application.".into())
}
fn read(backend: &Backend) -> Result<std::sync::MutexGuard<'_, CacheReader>> {
    backend
        .reader
        .lock()
        .map_err(|_| "Cache reader lock poisoned; restart the application.".into())
}
fn published_roots(cache: &CacheReader) -> Result<Vec<RootRecord>> {
    cache
        .roots()
        .map_err(error)?
        .into_iter()
        .filter_map(|root| match cache.latest(root.id) {
            Ok(Some(_)) => Some(Ok(root)),
            Ok(None) => None,
            Err(e) => Some(Err(error(e))),
        })
        .collect()
}
fn error(error: impl std::fmt::Display) -> String {
    error.to_string()
}
fn timestamp() -> Result<Timestamp> {
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(error)?;
    Timestamp::new(
        i64::try_from(time.as_secs()).map_err(error)?,
        time.subsec_nanos(),
    )
    .map_err(error)
}
fn snapshot(cache: &CacheReader, root: RootRecord, scan: ScanId) -> Result<Snapshot> {
    Ok(Snapshot {
        spec: cache.spec(scan).map_err(error)?,
        path: cache.scan_path(scan).map_err(error)?,
        interval: cache.interval(scan).map_err(error)?,
        root_entry: cache.root_entry(scan).map_err(error)?,
        _pin: cache.pin(scan).map_err(error)?,
        root,
        scan,
    })
}
pub fn open(directory: &Path) -> Result<Opened> {
    let cache = Cache::open(directory).map_err(error)?;
    let reader = cache.reader().map_err(error)?;
    let filter_reader = cache.reader().map_err(error)?;
    let comparison_reader = cache.reader().map_err(error)?;
    let roots = published_roots(&reader)?;
    let mut latest = None;
    for root in &roots {
        if let Some(scan) = cache.latest(root.id).map_err(error)?
            && latest
                .as_ref()
                .is_none_or(|(_, previous): &(RootRecord, ScanId)| scan > *previous)
        {
            latest = Some((root.clone(), scan));
        }
    }
    let snapshot = latest
        .map(|(root, scan)| snapshot(&reader, root, scan))
        .transpose()?;
    Ok(Opened {
        backend: Backend {
            writer: Arc::new(Mutex::new(cache)),
            reader: Arc::new(Mutex::new(reader)),
            filter_reader: Arc::new(Mutex::new(filter_reader)),
            comparison_reader: Arc::new(Mutex::new(comparison_reader)),
        },
        roots,
        snapshot,
    })
}
pub fn open_root(backend: &Backend, root: RootRecord) -> Result<Snapshot> {
    let cache = read(backend)?;
    let scan = cache
        .latest(root.id)
        .map_err(error)?
        .ok_or("This saved root has no published scan.")?;
    snapshot(&cache, root, scan)
}
pub struct Filtered {
    pub view: Mutex<FilteredView>,
    pub root: StoredEntry,
    pub matching_count: u64,
}
pub fn filter(
    backend: &Backend,
    scan: ScanId,
    metric: Metric,
    config: &QueryFilter,
    cancel: &Cancellation,
) -> Result<Arc<Filtered>> {
    let reader = backend
        .filter_reader
        .lock()
        .map_err(|_| "Filter reader lock poisoned")?;
    let view = reader
        .filtered(scan, metric, config, cancel)
        .map_err(error)?;
    let root = view.root_entry().map_err(error)?;
    let matching_count = view.matching_count().map_err(error)?;
    Ok(Arc::new(Filtered {
        view: Mutex::new(view),
        root,
        matching_count,
    }))
}
pub struct Queried {
    pub page: Page,
    pub current: StoredEntry,
    pub summary: Option<ChildrenSummary>,
    pub display_paths: Vec<String>,
}
pub fn query(backend: &Backend, query: Query) -> Result<Queried> {
    let subtree = query.subtree_only.then_some(query.parent);
    if let Some(filtered) = &query.filtered {
        let view = filtered
            .view
            .lock()
            .map_err(|_| "Filtered view lock poisoned")?;
        let current = view.entry(query.parent).map_err(error)?;
        let summary = (query.view == View::Treemap)
            .then(|| view.children_summary(query.parent, PAGE_SIZE))
            .transpose()
            .map_err(error)?;
        let page = match query.view {
            View::Folders | View::Treemap => view.children(query.parent, query.cursor, PAGE_SIZE),
            View::Largest => view.largest(subtree, query.cursor, PAGE_SIZE),
        }
        .map_err(error)?;
        let display_paths = if query.view == View::Largest {
            paths_for_page(&page, |node| view.native_path(node).map_err(error))?
        } else {
            Vec::new()
        };
        return Ok(Queried {
            page,
            current,
            summary,
            display_paths,
        });
    }
    let cache = read(backend)?;
    let current = cache.entry(query.scan, query.parent).map_err(error)?;
    let summary = (query.view == View::Treemap)
        .then(|| cache.children_summary(query.scan, query.parent, query.metric, PAGE_SIZE))
        .transpose()
        .map_err(error)?;
    let page = match query.view {
        View::Folders | View::Treemap => cache.children_sorted(
            query.scan,
            query.parent,
            query.metric,
            query.cursor,
            PAGE_SIZE,
        ),
        View::Largest => {
            cache.largest_in(query.scan, subtree, query.metric, query.cursor, PAGE_SIZE)
        }
    }
    .map_err(error)?;
    let display_paths = if query.view == View::Largest {
        paths_for_page(&page, |node| {
            cache.native_path(query.scan, node).map_err(error)
        })?
    } else {
        Vec::new()
    };
    Ok(Queried {
        page,
        current,
        summary,
        display_paths,
    })
}

fn paths_for_page(
    page: &Page,
    mut path: impl FnMut(NodeId) -> Result<NativePath>,
) -> Result<Vec<String>> {
    page.entries
        .iter()
        .map(|row| {
            path(row.entry.node)?
                .to_path_buf()
                .map(|p| p.to_string_lossy().into_owned())
                .map_err(error)
        })
        .collect()
}
pub struct Aliases {
    pub entries: Vec<(NodeId, String)>,
    pub next: Option<PageCursor>,
}
pub fn aliases(
    backend: &Backend,
    scan: ScanId,
    node: NodeId,
    cursor: Option<PageCursor>,
) -> Result<Aliases> {
    let reader = read(backend)?;
    let page = reader.aliases(scan, node, cursor, 100).map_err(error)?;
    let entries = page
        .entries
        .into_iter()
        .map(|entry| {
            let path = reader
                .native_path(scan, entry.node)
                .map_err(error)?
                .to_path_buf()
                .map_err(error)?;
            Ok((entry.node, path.to_string_lossy().into_owned()))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Aliases {
        entries,
        next: page.next,
    })
}

pub struct ScanRequest {
    pub path: PathBuf,
    pub cache_directory: PathBuf,
    /// Present for refresh; identity mismatch must require explicit new selection.
    pub refresh_root: Option<RootRecord>,
    pub generation: u64,
}
pub struct Completed {
    pub roots: Vec<RootRecord>,
    pub snapshot: Snapshot,
    pub maintenance_warning: Option<String>,
}

fn unpublished_outcome(outcome: TraversalOutcome) -> Result<Option<Completed>> {
    match outcome {
        TraversalOutcome::Cancelled => Ok(None),
        _ => Err(
            "Traversal failed before publication; root may have changed or disconnected.".into(),
        ),
    }
}

pub fn scan(
    backend: &Backend,
    request: ScanRequest,
    cancel: &Cancellation,
    progress: &Progress,
) -> Result<Option<Completed>> {
    let observed = space_scan::inspect_root(&request.path).map_err(error)?;
    let mut policy = crate::paths::scan_policy(&request.path, &request.cache_directory)?;
    // OS identity is authoritative even if casing or another native alias differs.
    let cache_identity = space_scan::inspect_root(&request.cache_directory).map_err(error)?;
    if !cache_identity.is_valid_exclusion() {
        return Err(
            "Cache identity cannot be verified; scanning is disabled to prevent feedback.".into(),
        );
    }
    if observed.validate_against(&cache_identity) == IdentityValidation::MatchingObservation {
        return Err("The application's own cache folder cannot be scanned.".into());
    }
    policy.excluded_directory_identities.push(cache_identity);
    policy.normalize();
    policy.validate().map_err(error)?;
    let spec = {
        let mut cache = lock(backend)?;
        if cancel.is_cancelled() {
            return Ok(None);
        }
        let matching = if let Some(root) = request.refresh_root {
            if root.identity.validate_against(&observed) != IdentityValidation::MatchingObservation
            {
                return Err("The selected root identity changed or cannot be verified. Choose the folder explicitly to create a new scan lineage.".into());
            }
            Some(root)
        } else {
            cache.roots().map_err(error)?.into_iter().find(|root| {
                root.path == NativePath::from_path(&request.path)
                    && root.identity.validate_against(&observed)
                        == IdentityValidation::MatchingObservation
            })
        };
        let (root_id, lineage_id) = match matching {
            Some(root) => (root.id, root.lineage),
            None => {
                let root = cache.next_root_id().map_err(error)?;
                (root, LineageId::try_from(root.get()).map_err(error)?)
            }
        };
        let spec = ScanSpec {
            key: ScanKey {
                id: cache.next_scan_id().map_err(error)?,
                generation: request.generation,
            },
            root_id,
            lineage_id,
            expected_root: observed,
            policy,
        };
        cache
            .begin(&spec, &NativePath::from_path(&request.path), timestamp()?)
            .map_err(error)?;
        spec
    };
    let finish = space_scan::scan(
        &request.path,
        &spec,
        cancel,
        space_scan::ScanLimits::default(),
        |batch| {
            if cancel.is_cancelled() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "Scan cancelled",
                ));
            }
            let entries = batch
                .records
                .iter()
                .filter(|record| matches!(record, ScanRecord::Entry(_)))
                .count() as u64;
            lock(backend)
                .map_err(std::io::Error::other)?
                .ingest(&batch)
                .map_err(std::io::Error::other)?;
            progress.entries.fetch_add(entries, Ordering::Relaxed);
            progress.batches.fetch_add(1, Ordering::Relaxed);
            Ok(())
        },
    );
    let mut cache = lock(backend)?;
    if cancel.is_cancelled() {
        cache.cancel(spec.key).map_err(error)?;
        return Ok(None);
    }
    let finish = match finish {
        Ok(finish) => finish,
        Err(failure) => {
            cache.fail(spec.key).map_err(error)?;
            return Err(error(failure));
        }
    };
    if let Err(failure) = cache.publish_cancellable(&finish, timestamp()?, cancel) {
        if cache.status(spec.key.id).map_err(error)? == ScanStatus::Cancelled {
            return Ok(None);
        }
        return Err(error(failure));
    }
    if finish.outcome != TraversalOutcome::Finished {
        return unpublished_outcome(finish.outcome);
    }
    let maintenance_warning = cache.maintenance_warning().map(str::to_owned);
    drop(cache);
    let cache = read(backend)?;
    let root = cache.root(spec.root_id).map_err(error)?;
    let snapshot = snapshot(&cache, root, spec.key.id)?;
    Ok(Some(Completed {
        roots: published_roots(&cache)?,
        snapshot,
        maintenance_warning,
    }))
}

/// Rebuild only one selected path through its bounded parent chain; never use display text.
pub fn native_path(backend: &Backend, snapshot: &Snapshot, node: NodeId) -> Result<PathBuf> {
    read(backend)?
        .native_path(snapshot.scan, node)
        .map_err(error)?
        .to_path_buf()
        .map_err(error)
}

pub fn validated_reveal_path(
    backend: &Backend,
    snapshot: &Snapshot,
    node: NodeId,
) -> Result<PathBuf> {
    let root = snapshot.path.to_path_buf().map_err(error)?;
    let identity = space_scan::inspect_root(&root).map_err(error)?;
    if snapshot.root.identity.validate_against(&identity) != IdentityValidation::MatchingObservation
    {
        return Err("Root is disconnected, replaced or identity is unavailable. Cached browsing still works; choose the folder again before live actions.".into());
    }
    let path = native_path(backend, snapshot, node)?;
    if !path.try_exists().map_err(error)? {
        return Err("This entry no longer exists. Cached metadata is still available.".into());
    }
    Ok(path)
}

pub struct CacheRow {
    pub info: space_store::ScanInfo,
    pub path: NativePath,
    pub totals: Option<StoredEntry>,
}
pub struct Inspection {
    pub rows: Vec<CacheRow>,
    pub next: Option<ScanId>,
    pub footprint: space_store::CacheFootprint,
    pub issues: Option<space_store::IssuePage>,
}
pub fn inspect_cache(
    backend: &Backend,
    before: Option<ScanId>,
    selected: Option<ScanId>,
    after: Option<i64>,
) -> Result<Inspection> {
    let cache = read(backend)?;
    let page = cache.scans(before, 100).map_err(error)?;
    let rows = page
        .scans
        .into_iter()
        .map(|info| {
            let scan = info.spec.key.id;
            Ok(CacheRow {
                path: cache.scan_path(scan).map_err(error)?,
                totals: if info.status == ScanStatus::Published {
                    Some(cache.root_entry(scan).map_err(error)?)
                } else {
                    None
                },
                info,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Inspection {
        rows,
        next: page.next,
        footprint: cache.footprint().map_err(error)?,
        issues: selected
            .map(|scan| cache.issues(scan, after, 50))
            .transpose()
            .map_err(error)?,
    })
}
pub struct Reloaded {
    pub roots: Vec<RootRecord>,
    pub snapshot: Option<Snapshot>,
}
pub fn reload(backend: &Backend) -> Result<Reloaded> {
    let cache = read(backend)?;
    let roots = published_roots(&cache)?;
    let mut latest = None;
    for root in &roots {
        if let Some(scan) = cache.latest(root.id).map_err(error)?
            && latest
                .as_ref()
                .is_none_or(|(_, previous): &(RootRecord, ScanId)| scan > *previous)
        {
            latest = Some((root.clone(), scan));
        }
    }
    let snapshot = latest
        .map(|(root, scan)| snapshot(&cache, root, scan))
        .transpose()?;
    Ok(Reloaded { roots, snapshot })
}
/// Caller must confirm this exact cache-only scope and release active view pins.
pub fn remove_cached(backend: &Backend, scan: Option<ScanId>) -> Result<()> {
    let mut cache = lock(backend)?;
    match scan {
        Some(scan) => cache.remove_scan(scan),
        None => cache.clear_cache(),
    }
    .map_err(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "space-app-test-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn comparison_preserves_endpoint_paths_pins_and_cancelled_prior_view() {
        use comparison::{Ranking, build, candidates, path as endpoint_path, query};
        use space_store::{ComparisonEndpoint, ComparisonProgress};
        let fixture = Fixture::new();
        let source = fixture.0.join("source");
        let directory = fixture.0.join("cache");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("growth.txt"), b"123").unwrap();
        std::fs::write(source.join("removed.txt"), b"12345").unwrap();
        let opened = open(&directory).unwrap();
        let first = scan(
            &opened.backend,
            ScanRequest {
                path: source.clone(),
                cache_directory: directory.clone(),
                refresh_root: None,
                generation: 1,
            },
            &Cancellation::new(),
            &Progress::default(),
        )
        .unwrap()
        .unwrap();
        std::fs::write(source.join("growth.txt"), b"123456789").unwrap();
        std::fs::remove_file(source.join("removed.txt")).unwrap();
        std::fs::create_dir_all(source.join("nested/deep")).unwrap();
        std::fs::write(source.join("nested/deep/new.txt"), b"12").unwrap();
        let second = scan(
            &opened.backend,
            ScanRequest {
                path: source.clone(),
                cache_directory: directory,
                refresh_root: Some(first.snapshot.root.clone()),
                generation: 2,
            },
            &Cancellation::new(),
            &Progress::default(),
        )
        .unwrap()
        .unwrap();
        let options = candidates(&opened.backend, &second.snapshot, None).unwrap();
        assert_eq!(options.entries.len(), 1);
        assert_eq!(options.entries[0].scan.spec.key.id, first.snapshot.scan);
        assert_eq!(options.entries[0].path, first.snapshot.path);
        assert!(options.entries[0].unavailable.is_none());
        let before = first.snapshot.scan;
        let after = second.snapshot.scan;
        let compared = build(
            &opened.backend,
            before,
            after,
            Metric::Logical,
            &QueryFilter::default(),
            &Cancellation::new(),
            &ComparisonProgress::default(),
        )
        .unwrap();
        assert_eq!(compared.root.logical.net, Some(3));
        let rankings = query(
            &compared,
            compared.root.id,
            Ranking::GrowingDirectories,
            None,
        )
        .unwrap();
        let deep = rankings
            .page
            .entries
            .iter()
            .find(|row| {
                row.after
                    .as_ref()
                    .is_some_and(|e| e.entry.display_name == "deep")
            })
            .unwrap();
        let drilled = query(&compared, deep.id, Ranking::Hierarchy, None).unwrap();
        assert_eq!(drilled.ancestors.len(), 3);
        assert_eq!(drilled.ancestors.first().unwrap().0, compared.root.id);
        assert_eq!(drilled.ancestors.last().unwrap().0, deep.id);
        assert_eq!(drilled.page.entries.len(), 1);
        let page = query(&compared, compared.root.id, Ranking::Hierarchy, None).unwrap();
        let removed = page
            .page
            .entries
            .iter()
            .find(|row| {
                row.before
                    .as_ref()
                    .is_some_and(|e| e.entry.display_name == "removed.txt")
            })
            .unwrap();
        assert_eq!(removed.change, ChangeKind::Removed);
        assert_eq!(
            endpoint_path(
                &opened.backend,
                &compared,
                removed.id,
                ComparisonEndpoint::Before,
                false
            )
            .unwrap(),
            source.join("removed.txt")
        );
        assert!(
            endpoint_path(
                &opened.backend,
                &compared,
                removed.id,
                ComparisonEndpoint::Before,
                true
            )
            .is_err()
        );
        assert!(
            endpoint_path(
                &opened.backend,
                &compared,
                removed.id,
                ComparisonEndpoint::After,
                false
            )
            .is_err()
        );
        let cancel = Cancellation::new();
        cancel.cancel();
        assert!(
            build(
                &opened.backend,
                before,
                after,
                Metric::Logical,
                &QueryFilter::default(),
                &cancel,
                &ComparisonProgress::default()
            )
            .is_err()
        );
        assert_eq!(
            query(&compared, compared.root.id, Ranking::Hierarchy, None)
                .unwrap()
                .page
                .entries
                .len(),
            3
        );
        drop(first);
        drop(second);
        assert!(remove_cached(&opened.backend, Some(before)).is_err());
        drop(compared);
        remove_cached(&opened.backend, Some(before)).unwrap();
        assert_eq!(
            std::fs::read(source.join("growth.txt")).unwrap(),
            b"123456789"
        );
    }
    #[test]
    fn filtered_hierarchy_largest_and_chart_share_charged_totals() {
        let fixture = Fixture::new();
        let path = fixture.0.join("source");
        let directory = fixture.0.join("cache");
        std::fs::create_dir_all(path.join("nested")).unwrap();
        std::fs::write(path.join("keep.txt"), b"12345").unwrap();
        std::fs::write(path.join("skip.bin"), b"123456789").unwrap();
        std::fs::write(path.join("nested/keep.txt"), b"123").unwrap();
        let opened = open(&directory).unwrap();
        let done = scan(
            &opened.backend,
            ScanRequest {
                path,
                cache_directory: directory,
                refresh_root: None,
                generation: 1,
            },
            &Cancellation::new(),
            &Progress::default(),
        )
        .unwrap()
        .unwrap();
        let config = QueryFilter {
            extension: Some("TXT".into()),
            ..QueryFilter::default()
        };
        let filtered = filter(
            &opened.backend,
            done.snapshot.scan,
            Metric::Logical,
            &config,
            &Cancellation::new(),
        )
        .unwrap();
        assert_eq!(filtered.matching_count, 2);
        assert_eq!(filtered.root.logical.observed_bytes, 8);
        let query_spec = Query {
            scan: done.snapshot.scan,
            parent: done.snapshot.root_entry.entry.node,
            view: View::Treemap,
            metric: Metric::Logical,
            cursor: None,
            subtree_only: true,
            filtered: Some(crate::deferred::Deferred::new(filtered.clone(), drop)),
        };
        let result = query(&opened.backend, query_spec.clone()).unwrap();
        let summary = result.summary.unwrap();
        assert_eq!(summary.total_bytes, 8);
        assert_eq!(
            summary.other_bytes
                + summary
                    .page
                    .entries
                    .iter()
                    .map(|r| r.logical.observed_bytes)
                    .sum::<u64>(),
            8
        );
        assert_eq!(result.current.logical.observed_bytes, 8);
        assert_eq!(result.page.entries[0].entry.display_name, "keep.txt");
        let largest = query(
            &opened.backend,
            Query {
                view: View::Largest,
                ..query_spec
            },
        )
        .unwrap();
        assert_eq!(
            largest
                .page
                .entries
                .iter()
                .map(|r| r.logical.observed_bytes)
                .collect::<Vec<_>>(),
            [5, 3]
        );
        let cancel = Cancellation::new();
        cancel.cancel();
        assert!(
            filter(
                &opened.backend,
                done.snapshot.scan,
                Metric::Logical,
                &config,
                &cancel
            )
            .is_err()
        );
    }
    #[test]
    fn cache_removal_refuses_pins_and_reloads_empty_without_source_changes() {
        let fixture = Fixture::new();
        let path = fixture.0.join("source");
        let directory = fixture.0.join("cache");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("file"), b"keep").unwrap();
        let opened = open(&directory).unwrap();
        let done = scan(
            &opened.backend,
            ScanRequest {
                path: path.clone(),
                cache_directory: directory,
                refresh_root: None,
                generation: 1,
            },
            &Cancellation::new(),
            &Progress::default(),
        )
        .unwrap()
        .unwrap();
        let scan = done.snapshot.scan;
        assert!(remove_cached(&opened.backend, Some(scan)).is_err());
        drop(done);
        remove_cached(&opened.backend, Some(scan)).unwrap();
        let empty = reload(&opened.backend).unwrap();
        assert!(empty.snapshot.is_none());
        assert!(empty.roots.is_empty());
        remove_cached(&opened.backend, None).unwrap();
        assert_eq!(std::fs::read(path.join("file")).unwrap(), b"keep");
    }
    #[test]
    fn failed_traversal_is_not_reported_as_cancellation() {
        assert!(unpublished_outcome(TraversalOutcome::Failed).is_err());
        assert!(
            unpublished_outcome(TraversalOutcome::Cancelled)
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn saved_navigation_omits_unpublished_roots() {
        let fixture = Fixture::new();
        let directory = fixture.0.join("cache");
        let opened = open(&directory).unwrap();
        let mut published = Vec::new();
        for name in ["a", "b", "c"] {
            let path = fixture.0.join(name);
            std::fs::create_dir(&path).unwrap();
            if name == "b" {
                let mut cache = lock(&opened.backend).unwrap();
                let id = cache.next_root_id().unwrap();
                let spec = ScanSpec {
                    key: ScanKey {
                        id: cache.next_scan_id().unwrap(),
                        generation: 1,
                    },
                    root_id: id,
                    lineage_id: LineageId::try_from(id.get()).unwrap(),
                    expected_root: space_scan::inspect_root(&path).unwrap(),
                    policy: ScanPolicy::default(),
                };
                cache
                    .begin(&spec, &NativePath::from_path(&path), timestamp().unwrap())
                    .unwrap();
                cache.cancel(spec.key).unwrap();
            } else {
                let done = scan(
                    &opened.backend,
                    ScanRequest {
                        path,
                        cache_directory: directory.clone(),
                        refresh_root: None,
                        generation: 1,
                    },
                    &Cancellation::new(),
                    &Progress::default(),
                )
                .unwrap()
                .unwrap();
                published.push(done.snapshot.root.id);
                assert!(done.roots.iter().all(|r| published.contains(&r.id)));
            }
        }
        drop(opened);
        let reopened = open(&directory).unwrap();
        assert_eq!(
            reopened.roots.iter().map(|r| r.id).collect::<Vec<_>>(),
            published
        );
    }
    #[test]
    fn scan_publish_browse_and_reopen_disconnected() {
        let fixture = Fixture::new();
        let source = fixture.0.join("source");
        let directory = fixture.0.join("cache");
        std::fs::create_dir_all(source.join("nested")).unwrap();
        std::fs::write(source.join("one.bin"), b"1234567").unwrap();
        std::fs::write(source.join("nested/two.bin"), b"12345").unwrap();
        let opened = open(&directory).unwrap();
        assert!(opened.snapshot.is_none());
        let completed = scan(
            &opened.backend,
            ScanRequest {
                path: source.clone(),
                cache_directory: directory.clone(),
                refresh_root: None,
                generation: 1,
            },
            &Cancellation::new(),
            &Progress::default(),
        )
        .unwrap()
        .unwrap();
        if completed.snapshot.root_entry.logical.observed_bytes != 12 {
            let spec = lock(&opened.backend)
                .unwrap()
                .spec(completed.snapshot.scan)
                .unwrap();
            let finish = space_scan::scan(
                &source,
                &spec,
                &Cancellation::new(),
                space_scan::ScanLimits::default(),
                |batch| {
                    eprintln!("Unexpected fixture scan batch: {batch:?}");
                    Ok(())
                },
            );
            eprintln!(
                "Unexpected fixture finish: {finish:?}; root: {:?}",
                completed.snapshot.root_entry
            );
        }
        assert_eq!(completed.snapshot.root_entry.logical.observed_bytes, 12);
        let page = query(
            &opened.backend,
            Query {
                scan: completed.snapshot.scan,
                parent: completed.snapshot.root_entry.entry.node,
                view: View::Largest,
                metric: Metric::Logical,
                cursor: None,
                subtree_only: false,
                filtered: None,
            },
        )
        .unwrap();
        let page = page.page;
        assert_eq!(page.entries.len(), 2);
        assert_eq!(page.entries[0].logical.observed_bytes, 7);
        assert_eq!(
            native_path(
                &opened.backend,
                &completed.snapshot,
                page.entries[0].entry.node
            )
            .unwrap(),
            source.join("one.bin")
        );
        let scan_id = completed.snapshot.scan;
        drop(completed);
        drop(opened);
        std::fs::rename(&source, fixture.0.join("disconnected")).unwrap();
        let reopened = open(&directory).unwrap();
        let snapshot = reopened.snapshot.as_ref().unwrap();
        assert_eq!(snapshot.scan, scan_id);
        assert_eq!(snapshot.root_entry.logical.observed_bytes, 12);
        assert!(
            validated_reveal_path(&reopened.backend, snapshot, snapshot.root_entry.entry.node)
                .is_err()
        );
    }
    #[test]
    fn cancelled_or_replaced_root_refresh_retains_previous_publication() {
        let fixture = Fixture::new();
        let source = fixture.0.join("source");
        let directory = fixture.0.join("cache");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("file"), b"data").unwrap();
        let opened = open(&directory).unwrap();
        let completed = scan(
            &opened.backend,
            ScanRequest {
                path: source.clone(),
                cache_directory: directory.clone(),
                refresh_root: None,
                generation: 1,
            },
            &Cancellation::new(),
            &Progress::default(),
        )
        .unwrap()
        .unwrap();
        let cancel = Cancellation::new();
        cancel.cancel();
        assert!(
            scan(
                &opened.backend,
                ScanRequest {
                    path: source.clone(),
                    cache_directory: directory.clone(),
                    refresh_root: Some(completed.snapshot.root.clone()),
                    generation: 2
                },
                &cancel,
                &Progress::default()
            )
            .unwrap()
            .is_none()
        );
        std::fs::rename(&source, fixture.0.join("original")).unwrap();
        std::fs::create_dir_all(&source).unwrap();
        assert!(
            scan(
                &opened.backend,
                ScanRequest {
                    path: source,
                    cache_directory: directory,
                    refresh_root: Some(completed.snapshot.root.clone()),
                    generation: 3
                },
                &Cancellation::new(),
                &Progress::default()
            )
            .is_err()
        );
        assert_eq!(
            lock(&opened.backend)
                .unwrap()
                .latest(completed.snapshot.root.id)
                .unwrap(),
            Some(completed.snapshot.scan)
        );
    }
}
