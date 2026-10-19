#[path = "tree_query.rs"]
mod tree_query;
pub use tree_query::{TreeAbort, breadcrumb_totals, tree, tree_filtered};
// Synchronous service boundary. Every function is called off the GPUI thread.
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
}
pub type Result<T> = std::result::Result<T, String>;
pub const PAGE_SIZE: usize = 100;

pub struct Snapshot {
    pub root: RootRecord,
    pub scan: ScanId,
    pub path: NativePath,
    pub interval: (Timestamp, Option<Timestamp>),
    pub root_entry: StoredEntry,
    pub _pin: ScanPin,
}
pub struct Opened {
    pub backend: Backend,
    pub roots: Vec<RootRecord>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Folders,
    Largest,
}
#[derive(Clone)]
pub struct Query {
    pub scan: ScanId,
    pub parent: NodeId,
    pub view: View,
    pub metric: Metric,
    pub cursor: Option<PageCursor>,
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
    let roots = published_roots(&reader)?;
    Ok(Opened {
        backend: Backend {
            writer: Arc::new(Mutex::new(cache)),
            reader: Arc::new(Mutex::new(reader)),
            filter_reader: Arc::new(Mutex::new(filter_reader)),
        },
        roots,
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
/// Reopen the exact immutable publication the card displayed, even if another
/// scan has since become latest. Never substitute another root or generation.
pub fn open_scan(backend: &Backend, root: RootRecord, scan: ScanId) -> Result<Snapshot> {
    let cache = read(backend)?;
    let spec = cache.spec(scan).map_err(error)?;
    if spec.root_id != root.id
        || spec.lineage_id != root.lineage
        || spec.expected_root != root.identity
    {
        return Err("This scan does not belong to the selected saved root.".into());
    }
    let root = cache.root(root.id).map_err(error)?;
    snapshot(&cache, root, scan)
}

/// Metadata-only target discovery plus saved published roots. This service does
/// not enumerate source contents and must run on a background worker.
pub fn discovery(backend: &Backend) -> Result<(space_scan::VolumeListing, Vec<RootRecord>)> {
    let listing = space_scan::enumerate_volumes().map_err(error)?;
    let cache = read(backend)?;
    let roots = published_roots(&cache)?;
    Ok((listing, roots))
}

/// Bounded cached child totals for an independently normalized dashboard map.
/// Use the explorer page budget so every visible tile can be selected immediately;
/// remaining child weight is represented by Other.
#[cfg(test)]
fn preview(backend: &Backend, snapshot: &Snapshot, metric: Metric) -> Result<ChildrenSummary> {
    read(backend)?
        .children_summary(
            snapshot.scan,
            snapshot.root_entry.entry.node,
            metric,
            PAGE_SIZE,
        )
        .map_err(error)
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
    pub display_paths: Vec<String>,
}
pub fn query(backend: &Backend, query: Query) -> Result<Queried> {
    let subtree = Some(query.parent);
    if let Some(filtered) = &query.filtered {
        let view = filtered
            .view
            .lock()
            .map_err(|_| "Filtered view lock poisoned")?;
        let page = match query.view {
            View::Folders => view.children(query.parent, query.cursor, PAGE_SIZE),
            View::Largest => view.largest(subtree, query.cursor, PAGE_SIZE),
        }
        .map_err(error)?;
        let display_paths = if query.view == View::Largest {
            paths_for_page(
                &page,
                view.native_path(query.parent).map_err(error)?,
                |node| view.native_path(node).map_err(error),
            )?
        } else {
            Vec::new()
        };
        return Ok(Queried {
            page,
            display_paths,
        });
    }
    let cache = read(backend)?;
    let page = match query.view {
        View::Folders => cache.children_sorted(
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
        paths_for_page(
            &page,
            cache.native_path(query.scan, query.parent).map_err(error)?,
            |node| cache.native_path(query.scan, node).map_err(error),
        )?
    } else {
        Vec::new()
    };
    Ok(Queried {
        page,
        display_paths,
    })
}

fn paths_for_page(
    page: &Page,
    scope: NativePath,
    mut path: impl FnMut(NodeId) -> Result<NativePath>,
) -> Result<Vec<String>> {
    let scope = scope.to_path_buf().map_err(error)?;
    page.entries
        .iter()
        .map(|row| {
            let path = path(row.entry.node)?.to_path_buf().map_err(error)?;
            path.strip_prefix(&scope)
                .map(|p| p.to_string_lossy().into_owned())
                .map_err(error)
        })
        .collect()
}
pub struct ScanRequest {
    pub path: PathBuf,
    pub cache_directory: PathBuf,
    /// Identity observed at selection or discovery, including a first scan.
    pub expected_root: Option<RootIdentity>,
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
    if cancel.is_cancelled() {
        return Ok(None);
    }
    let observed = space_scan::inspect_root(&request.path).map_err(error)?;
    // Validate before begin(): a queued, never-scanned mount can be replaced at
    // the same path while another disk is running. The scanner validates again
    // against this observation before traversal to close the later race.
    if request.expected_root.as_ref().is_some_and(|expected| {
        expected.validate_against(&observed) != IdentityValidation::MatchingObservation
    }) {
        return Err("The selected root changed or cannot be verified. Discover disks or choose the folder again before scanning.".into());
    }
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
                root.identity.validate_against(&observed) == IdentityValidation::MatchingObservation
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

#[cfg(test)]
mod tests {
    use super::*;
    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "space-app-test-{}-{}-{}",
                std::process::id(),
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            // Never silently reuse another test's directory on coarse Windows clocks.
            std::fs::create_dir(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn parallel_fixture_directories_are_distinct() {
        let handles: Vec<_> = (0..32).map(|_| std::thread::spawn(Fixture::new)).collect();
        let fixtures: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        let paths: std::collections::HashSet<_> =
            fixtures.iter().map(|fixture| &fixture.0).collect();
        assert_eq!(paths.len(), fixtures.len());
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
                expected_root: None,
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
            view: View::Folders,
            metric: Metric::Logical,
            cursor: None,

            filtered: Some(crate::deferred::Deferred::new(filtered.clone(), drop)),
        };
        let result = query(&opened.backend, query_spec.clone()).unwrap();
        let summary = filtered
            .view
            .lock()
            .unwrap()
            .children_summary(done.snapshot.root_entry.entry.node, PAGE_SIZE)
            .unwrap();
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
        assert_eq!(filtered.root.logical.observed_bytes, 8);
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
                        expected_root: None,
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
        assert!(opened.roots.is_empty());
        let completed = scan(
            &opened.backend,
            ScanRequest {
                path: source.clone(),
                cache_directory: directory.clone(),
                expected_root: None,
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
        let snapshot = &open_root(&reopened.backend, reopened.roots[0].clone()).unwrap();
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
                expected_root: None,
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
                    expected_root: None,
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
                    expected_root: None,
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
    fn guarded_request(source: &Path, cache: &Path, generation: u64) -> ScanRequest {
        ScanRequest {
            path: source.to_owned(),
            cache_directory: cache.to_owned(),
            expected_root: Some(space_scan::inspect_root(source).unwrap()),
            refresh_root: None,
            generation,
        }
    }

    #[test]
    fn first_scan_replacement_is_rejected_before_creating_any_cache_record() {
        let fixture = Fixture::new();
        let source = fixture.0.join("source");
        let directory = fixture.0.join("cache");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("original"), b"keep").unwrap();
        let opened = open(&directory).unwrap();
        let request = guarded_request(&source, &directory, 1);
        std::fs::rename(&source, fixture.0.join("disconnected")).unwrap();
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("replacement"), b"other").unwrap();
        assert!(
            scan(
                &opened.backend,
                request,
                &Cancellation::new(),
                &Progress::default()
            )
            .is_err()
        );
        let cache = lock(&opened.backend).unwrap();
        assert!(cache.roots().unwrap().is_empty());
        assert!(cache.scans(None, 100).unwrap().scans.is_empty());
        assert_eq!(std::fs::read(source.join("replacement")).unwrap(), b"other");
        assert_eq!(
            std::fs::read(fixture.0.join("disconnected/original")).unwrap(),
            b"keep"
        );
    }

    #[test]
    fn one_scan_all_action_publishes_two_independent_bounded_maps_and_exact_offline_views() {
        use crate::disk_model::DiskRegistry;
        use crate::scan_batch::{Effect, Event, ScanBatch, ScanOutcome};
        let fixture = Fixture::new();
        let directory = fixture.0.join("cache");
        let mut volumes = Vec::new();
        for (name, contents) in [("alpha", "123"), ("beta", "1234567")] {
            let source = fixture.0.join(name);
            std::fs::create_dir(&source).unwrap();
            std::fs::write(source.join(format!("{name}.txt")), contents).unwrap();
            volumes.push(space_scan::MountedVolume {
                label: "Same display label".into(),
                mount_path: NativePath::from_path(&source),
                aliases: vec![],
                identity: space_scan::inspect_root(&source).unwrap(),
                filesystem: "fixture".into(),
                removable: None,
                capacity: None,
            });
        }
        let listing = space_scan::VolumeListing {
            observed_at: timestamp().unwrap(),
            volumes,
            issues: vec![],
            complete: true,
        };
        let mut registry = DiskRegistry::default();
        registry.reconcile(&listing, &[]);
        let opened = open(&directory).unwrap();
        let mut coordinator = ScanBatch::default();
        let discover = coordinator.reduce(Event::ScanAll);
        let [Effect::Discover { batch }] = discover.as_slice() else {
            panic!("discover")
        };
        let mut effects = coordinator.reduce(Event::Discovered {
            batch: *batch,
            targets: registry.mounted_targets(),
        });
        let mut snapshots = std::collections::BTreeMap::new();
        let mut maps = std::collections::BTreeMap::new();
        while let [Effect::Start { token, target }] = effects.as_slice() {
            let token = *token;
            let result = scan(
                &opened.backend,
                ScanRequest {
                    path: target.path.to_path_buf().unwrap(),
                    cache_directory: directory.clone(),
                    expected_root: Some(target.expected_root.clone()),
                    refresh_root: target.refresh_root.clone(),
                    generation: token.request.0,
                },
                &Cancellation::new(),
                &Progress::default(),
            )
            .unwrap()
            .unwrap();
            assert_eq!(result.snapshot.root.identity, target.expected_root);
            assert!(coordinator.accepts(token));
            let map = preview(&opened.backend, &result.snapshot, Metric::Logical).unwrap();
            assert_eq!(map.page.entries.len(), 1);
            maps.insert(
                token.card_id,
                (
                    map.page.entries[0].entry.display_name.clone(),
                    map.total_bytes,
                ),
            );
            let scan = result.snapshot.scan;
            registry.reconcile(&listing, &result.roots);
            snapshots.insert(token.card_id, result.snapshot);
            effects = coordinator.reduce(Event::Finished {
                token,
                outcome: ScanOutcome::Published {
                    scan,
                    partial: false,
                },
            });
        }
        assert!(!coordinator.is_busy());
        assert_eq!(snapshots.len(), 2);
        let cards = registry.cards();
        assert_eq!(maps[&cards[0].id], ("alpha.txt".into(), 3));
        assert_eq!(maps[&cards[1].id], ("beta.txt".into(), 7));
        let first = &snapshots[&cards[0].id];
        let second = &snapshots[&cards[1].id];
        assert_ne!(first.scan, second.scan);
        assert!(open_scan(&opened.backend, first.root.clone(), second.scan).is_err());
        // Offline browsing reads only this immutable cached publication.
        std::fs::rename(
            fixture.0.join("alpha"),
            fixture.0.join("alpha-disconnected"),
        )
        .unwrap();
        let offline = open_scan(&opened.backend, first.root.clone(), first.scan).unwrap();
        assert_eq!(
            preview(&opened.backend, &offline, Metric::Logical)
                .unwrap()
                .total_bytes,
            3
        );
        assert!(
            validated_reveal_path(&opened.backend, &offline, offline.root_entry.entry.node)
                .is_err()
        );
        assert_eq!(
            std::fs::read(fixture.0.join("alpha-disconnected/alpha.txt")).unwrap(),
            b"123"
        );
        assert_eq!(
            std::fs::read(fixture.0.join("beta/beta.txt")).unwrap(),
            b"1234567"
        );
    }

    #[test]
    fn preview_keeps_other_totals_and_exact_reopen_does_not_follow_latest() {
        let fixture = Fixture::new();
        let source = fixture.0.join("source");
        let directory = fixture.0.join("cache");
        std::fs::create_dir(&source).unwrap();
        for index in 0..150 {
            std::fs::write(source.join(format!("file-{index:03}")), b"x").unwrap();
        }
        let opened = open(&directory).unwrap();
        let first = scan(
            &opened.backend,
            guarded_request(&source, &directory, 1),
            &Cancellation::new(),
            &Progress::default(),
        )
        .unwrap()
        .unwrap();
        let map = preview(&opened.backend, &first.snapshot, Metric::Logical).unwrap();
        assert_eq!(map.page.entries.len(), PAGE_SIZE);
        assert_eq!(map.total_count, 150);
        assert_eq!(map.total_bytes, 150);
        assert_eq!(map.other_count, 150 - PAGE_SIZE as u64);
        assert_eq!(map.other_bytes, 150 - PAGE_SIZE as u64);
        std::fs::write(source.join("new"), b"1234").unwrap();
        let second = scan(
            &opened.backend,
            guarded_request(&source, &directory, 2),
            &Cancellation::new(),
            &Progress::default(),
        )
        .unwrap()
        .unwrap();
        let exact = open_scan(
            &opened.backend,
            first.snapshot.root.clone(),
            first.snapshot.scan,
        )
        .unwrap();
        assert_eq!(
            preview(&opened.backend, &exact, Metric::Logical)
                .unwrap()
                .total_bytes,
            150
        );
        assert_eq!(
            preview(&opened.backend, &second.snapshot, Metric::Logical)
                .unwrap()
                .total_bytes,
            154
        );
    }

    #[test]
    fn observed_native_alias_reuses_root_but_same_volume_nested_folder_does_not() {
        let fixture = Fixture::new();
        let source = fixture.0.join("source");
        let nested = source.join("nested");
        let directory = fixture.0.join("cache");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("file"), b"keep").unwrap();
        let opened = open(&directory).unwrap();
        let first = scan(
            &opened.backend,
            guarded_request(&source, &directory, 1),
            &Cancellation::new(),
            &Progress::default(),
        )
        .unwrap()
        .unwrap();
        let alias = scan(
            &opened.backend,
            guarded_request(&source.join("."), &directory, 2),
            &Cancellation::new(),
            &Progress::default(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(first.snapshot.root.id, alias.snapshot.root.id);
        let child = scan(
            &opened.backend,
            guarded_request(&nested, &directory, 3),
            &Cancellation::new(),
            &Progress::default(),
        )
        .unwrap()
        .unwrap();
        assert_ne!(first.snapshot.root.id, child.snapshot.root.id);
        assert_eq!(child.roots.len(), 2);
    }
    #[test]
    fn batch_replacement_failure_advances_then_stop_preserves_published_scans_and_skips_queue() {
        use crate::disk_model::{CardId, ScanTarget};
        use crate::scan_batch::{Effect, Event, ScanBatch, ScanOutcome};
        let fixture = Fixture::new();
        let directory = fixture.0.join("cache");
        let opened = open(&directory).unwrap();
        let mut targets = Vec::new();
        for id in 1..=4 {
            let path = fixture.0.join(format!("disk-{id}"));
            std::fs::create_dir(&path).unwrap();
            std::fs::write(path.join("kept"), b"123").unwrap();
            targets.push(ScanTarget {
                card_id: CardId(id),
                expected_root: space_scan::inspect_root(&path).unwrap(),
                path: NativePath::from_path(&path),
                refresh_root: None,
            });
        }
        let prior = scan(
            &opened.backend,
            guarded_request(&fixture.0.join("disk-1"), &directory, 1),
            &Cancellation::new(),
            &Progress::default(),
        )
        .unwrap()
        .unwrap();
        targets[0].refresh_root = Some(prior.snapshot.root.clone());
        let mut batch = ScanBatch::default();
        let mut effects = batch.reduce(Event::ScanTargets(targets));
        // Replacement happens after membership was frozen and before the worker
        // begins, including the first-scan case guarded in the separate test.
        std::fs::rename(fixture.0.join("disk-1"), fixture.0.join("old-disk-1")).unwrap();
        std::fs::create_dir(fixture.0.join("disk-1")).unwrap();
        let execute = |effect: &Effect, cancel: &Cancellation| {
            let Effect::Start { token, target } = effect else {
                panic!("start")
            };
            scan(
                &opened.backend,
                ScanRequest {
                    path: target.path.to_path_buf().unwrap(),
                    cache_directory: directory.clone(),
                    expected_root: Some(target.expected_root.clone()),
                    refresh_root: target.refresh_root.clone(),
                    generation: token.request.0,
                },
                cancel,
                &Progress::default(),
            )
        };
        let token = batch.active().unwrap().token;
        let failure = execute(&effects[0], &Cancellation::new()).err().unwrap();
        effects = batch.reduce(Event::Finished {
            token,
            outcome: ScanOutcome::Failed(failure),
        });
        assert_eq!(batch.active().unwrap().token.card_id, CardId(2));
        let success = execute(&effects[0], &Cancellation::new()).unwrap().unwrap();
        assert_eq!(
            preview(&opened.backend, &success.snapshot, Metric::Logical)
                .unwrap()
                .total_bytes,
            3
        );
        let token = batch.active().unwrap().token;
        effects = batch.reduce(Event::Finished {
            token,
            outcome: ScanOutcome::Published {
                scan: success.snapshot.scan,
                partial: false,
            },
        });
        let token = batch.active().unwrap().token;
        assert_eq!(token.card_id, CardId(3));
        assert_eq!(batch.queued_total(), 1);
        assert!(matches!(
            batch.reduce(Event::Stop).as_slice(),
            [Effect::Cancel { .. }]
        ));
        let cancel = Cancellation::new();
        cancel.cancel();
        assert!(execute(&effects[0], &cancel).unwrap().is_none());
        assert!(batch.stopping());
        assert!(
            batch
                .reduce(Event::Finished {
                    token,
                    outcome: ScanOutcome::Cancelled
                })
                .is_empty()
        );
        assert!(!batch.is_busy());
        let cache = lock(&opened.backend).unwrap();
        assert_eq!(
            cache.latest(prior.snapshot.root.id).unwrap(),
            Some(prior.snapshot.scan)
        );
        assert_eq!(cache.scans(None, 100).unwrap().scans.len(), 2);
        assert_eq!(
            std::fs::read(fixture.0.join("old-disk-1/kept")).unwrap(),
            b"123"
        );
        assert_eq!(
            std::fs::read(fixture.0.join("disk-4/kept")).unwrap(),
            b"123"
        );
    }
}
