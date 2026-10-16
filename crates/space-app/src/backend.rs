//! Synchronous service boundary. Every function is called off the GPUI thread.
use space_core::*;
use space_store::{Cache, CacheReader, Page, PageCursor, RootRecord, ScanPin, StoredEntry};
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
    pub snapshot: Option<Snapshot>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Folders,
    Largest,
}
#[derive(Clone, Copy)]
pub struct Query {
    pub scan: ScanId,
    pub parent: NodeId,
    pub view: View,
    pub metric: Metric,
    pub cursor: Option<PageCursor>,
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
pub fn query(backend: &Backend, query: Query) -> Result<Page> {
    let cache = read(backend)?;
    match query.view {
        View::Folders => cache.children(query.scan, query.parent, query.cursor, PAGE_SIZE),
        View::Largest => cache.largest(query.scan, query.metric, query.cursor, PAGE_SIZE),
    }
    .map_err(error)
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
        assert_eq!(completed.snapshot.root_entry.logical.observed_bytes, 12);
        let page = query(
            &opened.backend,
            Query {
                scan: completed.snapshot.scan,
                parent: completed.snapshot.root_entry.entry.node,
                view: View::Largest,
                metric: Metric::Logical,
                cursor: None,
            },
        )
        .unwrap();
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
