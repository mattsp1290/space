use super::*;
use unicode_casefold::UnicodeCaseFold;

use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterPhase {
    Waiting,
    Matching,
    Aggregating,
    Indexing,
    Finished,
    Cancelled,
    Failed,
}
#[derive(Clone, Copy, Debug)]
pub struct FilterProgressSnapshot {
    pub phase: FilterPhase,
    pub processed: u64,
    pub total: u64,
}
#[derive(Default)]
struct ProgressState {
    phase: AtomicU8,
    processed: AtomicU64,
    total: AtomicU64,
}
#[derive(Clone, Default)]
pub struct FilterProgress(Arc<ProgressState>);
impl FilterProgress {
    pub fn snapshot(&self) -> FilterProgressSnapshot {
        let phase = match self.0.phase.load(Ordering::Acquire) {
            1 => FilterPhase::Matching,
            2 => FilterPhase::Aggregating,
            3 => FilterPhase::Indexing,
            4 => FilterPhase::Finished,
            5 => FilterPhase::Cancelled,
            6 => FilterPhase::Failed,
            _ => FilterPhase::Waiting,
        };
        FilterProgressSnapshot {
            phase,
            processed: self.0.processed.load(Ordering::Relaxed),
            total: self.0.total.load(Ordering::Relaxed),
        }
    }
    fn phase(&self, phase: u8, total: u64) {
        self.0.processed.store(0, Ordering::Relaxed);
        self.0.total.store(total, Ordering::Relaxed);
        self.0.phase.store(phase, Ordering::Release);
    }
    fn advance(&self) {
        self.0.processed.fetch_add(1, Ordering::Relaxed);
    }
}

/// Literal Unicode full/default case-folded display-text matching, without
/// Unicode normalization. Native keys remain authoritative for actions.
/// Size/time ranges are inclusive. Unknowns pass a constrained range only when
/// explicitly enabled. An absent range does not exclude unknown observations.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QueryFilter {
    pub name: Option<String>,
    pub path: Option<String>,
    pub extension: Option<String>,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    pub modified_after: Option<Timestamp>,
    pub modified_before: Option<Timestamp>,
    pub include_unknown_size: bool,
    pub include_unknown_time: bool,
}
impl QueryFilter {
    pub fn is_empty(&self) -> bool {
        self.name.as_deref().is_none_or(str::is_empty)
            && self.path.as_deref().is_none_or(str::is_empty)
            && self.extension.is_none()
            && self.min_size.is_none()
            && self.max_size.is_none()
            && self.modified_after.is_none()
            && self.modified_before.is_none()
    }
    pub(super) fn validate(&self) -> Result<()> {
        if self
            .name
            .iter()
            .chain(self.path.iter())
            .chain(self.extension.iter())
            .any(|s| s.len() > 4096)
        {
            return Err(StoreError::Invalid("filter text bound"));
        }
        if self.min_size.zip(self.max_size).is_some_and(|(a, b)| a > b)
            || self
                .modified_after
                .zip(self.modified_before)
                .is_some_and(|(a, b)| a > b)
        {
            return Err(StoreError::Invalid("inverted filter range"));
        }
        for t in [self.modified_after, self.modified_before]
            .into_iter()
            .flatten()
        {
            Timestamp::new(t.seconds, t.nanos)?;
        }
        Ok(())
    }
    pub(super) fn folded(&self) -> Self {
        let mut result = self.clone();
        result.name = self.name.as_deref().map(fold);
        result.path = self.path.as_deref().map(fold);
        result.extension = self
            .extension
            .as_deref()
            .map(|s| fold(s.trim_start_matches('.')));
        result
    }
    pub(super) fn matches(&self, entry: &EntryRecord, path: &str, metric: Metric) -> bool {
        let name = fold(&entry.display_name);
        if self.name.as_ref().is_some_and(|n| !name.contains(n))
            || self.path.as_ref().is_some_and(|n| !fold(path).contains(n))
        {
            return false;
        }
        if let Some(wanted) = &self.extension {
            let extension = name
                .rsplit_once('.')
                .filter(|(base, _)| !base.is_empty())
                .map_or("", |(_, extension)| extension);
            if extension != wanted {
                return false;
            }
        }
        if self.min_size.is_some() || self.max_size.is_some() {
            match match metric {
                Metric::Logical => entry.logical_bytes,
                Metric::Allocated => entry.allocated_bytes,
            } {
                Some(n) => {
                    if self.min_size.is_some_and(|min| n < min)
                        || self.max_size.is_some_and(|max| n > max)
                    {
                        return false;
                    }
                }
                None => {
                    if !self.include_unknown_size {
                        return false;
                    }
                }
            }
        }
        if self.modified_after.is_some() || self.modified_before.is_some() {
            match entry.modified {
                Some(t) => {
                    if self.modified_after.is_some_and(|min| t < min)
                        || self.modified_before.is_some_and(|max| t > max)
                    {
                        return false;
                    }
                }
                None => {
                    if !self.include_unknown_time {
                        return false;
                    }
                }
            }
        }
        true
    }
}
fn fold(value: &str) -> String {
    value.case_fold().collect()
}
/// A pinned, independently connected immutable filtered view. Build on a worker.
/// Temporary SQLite tables hold the match/aggregate index on disk, not in a UI
/// tree. Directory coverage stays conservative: filtering cannot erase an unseen
/// scope. Hardlink ownership is unchanged; matching non-owner aliases charge zero.
pub struct FilteredView {
    reader: CacheReader,
    scan: ScanId,
    metric: Metric,
    _pin: ScanPin,
    materialized: bool,
}
impl CacheReader {
    pub fn filtered(
        &self,
        scan: ScanId,
        metric: Metric,
        filter: &QueryFilter,
        cancel: &Cancellation,
    ) -> Result<FilteredView> {
        self.filtered_with_progress(scan, metric, filter, cancel, &FilterProgress::default())
    }
    pub fn filtered_with_progress(
        &self,
        scan: ScanId,
        metric: Metric,
        filter: &QueryFilter,
        cancel: &Cancellation,
        progress: &FilterProgress,
    ) -> Result<FilteredView> {
        filter.validate()?;
        let pin = self.pin(scan)?;
        let reader = self.cache.reader()?;
        if cancel.is_cancelled() {
            progress.phase(5, 0);
            return Err(StoreError::Invalid("filter cancelled"));
        }
        if filter.is_empty() {
            progress.phase(4, 0);
            return Ok(FilteredView {
                reader,
                scan,
                metric,
                _pin: pin,
                materialized: false,
            });
        }
        reader.cache.conn.execute_batch("PRAGMA query_only=OFF; PRAGMA temp_store=FILE; CREATE TEMP TABLE matches(node INTEGER PRIMARY KEY); CREATE TEMP TABLE counts(node INTEGER PRIMARY KEY,n INTEGER NOT NULL); CREATE TEMP TABLE totals(scan INTEGER NOT NULL,node INTEGER NOT NULL,logical INTEGER NOT NULL,allocated INTEGER NOT NULL,unknown_logical INTEGER NOT NULL,unknown_allocated INTEGER NOT NULL,errors INTEGER NOT NULL,estimated INTEGER NOT NULL,unknown_entries INTEGER NOT NULL,excluded INTEGER NOT NULL,boundary INTEGER NOT NULL,CHECK(logical>=0 AND allocated>=0 AND unknown_logical>=0 AND unknown_allocated>=0 AND errors>=0 AND estimated>=0 AND unknown_entries>=0 AND excluded>=0 AND boundary>=0),PRIMARY KEY(scan,node));")?;
        let view = FilteredView {
            reader,
            scan,
            metric,
            _pin: pin,
            materialized: true,
        };
        if let Err(error) = view.build(&filter.folded(), cancel, progress) {
            progress.phase(if cancel.is_cancelled() { 5 } else { 6 }, 0);
            return Err(error);
        }
        Ok(view)
    }
}
impl FilteredView {
    pub fn scan_id(&self) -> ScanId {
        self.scan
    }
    pub fn metric(&self) -> Metric {
        self.metric
    }
    pub fn matching_count(&self) -> Result<u64> {
        if !self.materialized {
            let n: i64 = self.reader.cache.conn.query_row(
                "SELECT count(*) FROM main.entries WHERE scan=? AND kind<>0",
                [self.scan.get()],
                |r| r.get(0),
            )?;
            return u64::try_from(n).map_err(|_| StoreError::Overflow);
        }
        let count:i64=self.reader.cache.conn.query_row("SELECT n FROM counts WHERE node=(SELECT node FROM main.entries WHERE scan=? AND parent IS NULL)",[self.scan.get()],|r|r.get(0))?;
        u64::try_from(count).map_err(|_| StoreError::Overflow)
    }
    pub fn root_entry(&self) -> Result<StoredEntry> {
        self.reader.root_entry(self.scan)
    }
    pub fn entry(&self, node: NodeId) -> Result<StoredEntry> {
        self.reader.entry(self.scan, node)
    }
    pub fn children(
        &self,
        parent: NodeId,
        after: Option<PageCursor>,
        limit: usize,
    ) -> Result<Page> {
        self.reader
            .children_sorted(self.scan, parent, self.metric, after, limit)
    }
    pub fn largest(
        &self,
        subtree: Option<NodeId>,
        after: Option<PageCursor>,
        limit: usize,
    ) -> Result<Page> {
        self.reader
            .largest_in(self.scan, subtree, self.metric, after, limit)
    }
    pub fn aliases(
        &self,
        node: NodeId,
        after: Option<PageCursor>,
        limit: usize,
    ) -> Result<AliasPage> {
        self.reader.aliases(self.scan, node, after, limit)
    }
    pub fn native_path(&self, node: NodeId) -> Result<NativePath> {
        self.reader.cache.reader()?.native_path(self.scan, node)
    }
    pub fn children_summary(&self, parent: NodeId, limit: usize) -> Result<ChildrenSummary> {
        self.reader
            .children_summary(self.scan, parent, self.metric, limit)
    }
    fn build(
        &self,
        filter: &QueryFilter,
        cancel: &Cancellation,
        progress: &FilterProgress,
    ) -> Result<()> {
        let c = &self.reader.cache.conn;
        let token = cancel.clone();
        c.progress_handler(1000, Some(move || token.is_cancelled()))?;
        let sid = self.scan.get();
        let total: i64 = c.query_row(
            "SELECT count(*) FROM main.entries WHERE scan=?",
            [sid],
            |r| r.get(0),
        )?;
        progress.phase(1, u64::try_from(total).map_err(|_| StoreError::Overflow)?);
        c.execute_batch("BEGIN")?;
        let mut stack: Vec<(NodeId, String)> = Vec::new();
        {
            let mut stmt =
                c.prepare("SELECT data,node,parent,native,kind,ordinal,logical,allocated,identity FROM main.entries WHERE scan=? ORDER BY ordinal")?;
            let mut rows = stmt.query([sid])?;
            let mut insert_match = c.prepare_cached("INSERT INTO matches VALUES(?)")?;
            while let Some(row) = rows.next()? {
                check_cancel(cancel)?;
                progress.advance();
                let e = crate::queries::verify_record(crate::queries::read_record(row)?)?;
                while stack
                    .last()
                    .is_some_and(|(node, _)| Some(*node) != e.parent)
                {
                    stack.pop();
                }
                if e.parent.is_some() && stack.last().map(|s| s.0) != e.parent {
                    return Err(StoreError::Corrupt("broken native traversal".into()));
                }
                if e.kind == EntryKind::Directory {
                    stack.push((
                        e.node,
                        if e.parent.is_none() {
                            String::new()
                        } else {
                            e.display_name.clone()
                        },
                    ));
                    continue;
                }
                let mut path = String::new();
                for (_, name) in &stack {
                    if !name.is_empty() {
                        if !path.is_empty() {
                            path.push('/');
                        }
                        path.push_str(name);
                    }
                }
                if !path.is_empty() {
                    path.push('/');
                }
                path.push_str(&e.display_name);
                if filter.matches(&e, &path, self.metric) {
                    insert_match.execute([e.node.get()])?;
                }
            }
        }
        progress.phase(3, 0);
        // Bulk SQL groups direct file contributions once. Only directories need
        // bottom-up propagation; there is no per-file ancestor SQL round trip.
        c.execute("INSERT INTO temp.totals SELECT t.* FROM matches m LEFT JOIN main.totals t ON t.node=m.node AND t.scan=?",[sid])?;
        c.execute("INSERT INTO counts SELECT e.parent,count(*) FROM main.entries e JOIN matches m ON m.node=e.node WHERE e.scan=? GROUP BY e.parent",[sid])?;
        c.execute("INSERT INTO temp.totals SELECT e.scan,e.parent,sum(t.logical),sum(t.allocated),sum(t.unknown_logical),sum(t.unknown_allocated),sum(t.errors),sum(t.estimated),sum(t.unknown_entries),sum(t.excluded),sum(t.boundary) FROM main.entries e JOIN temp.totals t ON t.scan=e.scan AND t.node=e.node WHERE e.scan=? AND e.kind<>0 GROUP BY e.parent",[sid])?;
        let directories: i64 = c.query_row(
            "SELECT count(*) FROM main.entries WHERE scan=? AND kind=0",
            [sid],
            |r| r.get(0),
        )?;
        progress.phase(
            2,
            u64::try_from(directories).map_err(|_| StoreError::Overflow)?,
        );
        {
            let mut stmt = c.prepare(
                "SELECT node,parent,kind FROM main.entries WHERE scan=? AND kind=0 ORDER BY ordinal DESC",
            )?;
            let mut rows = stmt.query([sid])?;
            while let Some(row) = rows.next()? {
                check_cancel(cancel)?;
                progress.advance();
                let node: i64 = row.get(0)?;
                let parent: Option<i64> = row.get(1)?;
                let kind: i64 = row.get(2)?;
                let own = read_totals(c, sid, node)?;
                if kind != 0 && own.is_none() {
                    continue;
                }
                let total = own.unwrap_or_default();
                put_totals(c, sid, node, total)?;
                let count: i64 = c
                    .query_row("SELECT n FROM counts WHERE node=?", [node], |r| r.get(0))
                    .optional()?
                    .unwrap_or(0);
                c.execute(
                    "INSERT OR IGNORE INTO counts VALUES(?,?)",
                    params![node, count],
                )?;
                if let Some(parent) = parent {
                    let inherited = read_totals(c, sid, parent)?
                        .unwrap_or_default()
                        .add(total)?;
                    put_totals(c, sid, parent, inherited)?;
                    let n: i64 = c
                        .query_row("SELECT n FROM counts WHERE node=?", [parent], |r| r.get(0))
                        .optional()?
                        .unwrap_or(0);
                    let n = n.checked_add(count).ok_or(StoreError::Overflow)?;
                    c.execute("INSERT INTO counts VALUES(?,?) ON CONFLICT(node) DO UPDATE SET n=excluded.n",params![parent,n])?;
                }
            }
        }
        check_cancel(cancel)?;
        progress.phase(3, 0);
        // Preserve full-scope coverage in each directory. Matching-file metric
        // unknown counts remain filtered; these are distinct uncertainty axes.
        c.execute("UPDATE temp.totals SET errors=(SELECT errors FROM main.totals m WHERE m.scan=totals.scan AND m.node=totals.node),unknown_entries=(SELECT unknown_entries FROM main.totals m WHERE m.scan=totals.scan AND m.node=totals.node),excluded=(SELECT excluded FROM main.totals m WHERE m.scan=totals.scan AND m.node=totals.node),boundary=(SELECT boundary FROM main.totals m WHERE m.scan=totals.scan AND m.node=totals.node) WHERE node IN (SELECT node FROM main.entries WHERE scan=? AND kind=0)",[sid])?;
        c.execute_batch("CREATE TEMP VIEW entries AS SELECT e.* FROM main.entries e WHERE e.kind=0 OR e.node IN (SELECT node FROM matches); CREATE TEMP TABLE child_sizes(scan INTEGER NOT NULL,parent INTEGER,node INTEGER NOT NULL,ordinal INTEGER NOT NULL,logical INTEGER NOT NULL,allocated INTEGER NOT NULL,PRIMARY KEY(scan,node)); CREATE INDEX temp.filtered_children_logical ON child_sizes(scan,parent,logical DESC,ordinal); CREATE INDEX temp.filtered_children_allocated ON child_sizes(scan,parent,allocated DESC,ordinal);")?;
        c.execute("INSERT INTO temp.child_sizes SELECT e.scan,e.parent,e.node,e.ordinal,t.logical,t.allocated FROM main.entries e JOIN temp.totals t ON t.scan=e.scan AND t.node=e.node LEFT JOIN counts n ON n.node=e.node WHERE e.scan=? AND (e.kind<>0 OR n.n>0 OR e.parent IS NULL)",[sid])?;
        check_cancel(cancel)?;
        c.execute_batch("COMMIT")?;
        c.progress_handler(0, None::<fn() -> bool>)?;
        progress.phase(4, 0);
        Ok(())
    }
}
fn check_cancel(cancel: &Cancellation) -> Result<()> {
    if cancel.is_cancelled() {
        Err(StoreError::Invalid("filter cancelled"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn temp_materialization_never_makes_main_database_writable() {
        let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
        let mut c = Cache::open(crate::tests::dir()).unwrap();
        crate::tests::basic(&mut c, 1);
        let r = c.reader().unwrap();
        let v = r
            .filtered(
                crate::tests::spec(1).key.id,
                Metric::Logical,
                &QueryFilter {
                    min_size: Some(0),
                    ..Default::default()
                },
                &Cancellation::new(),
            )
            .unwrap();
        assert!(v.materialized);
        v.reader
            .cache
            .conn
            .execute("CREATE TEMP TABLE isolation_probe(value INTEGER)", [])
            .unwrap();
        v.reader
            .cache
            .conn
            .execute("INSERT INTO isolation_probe VALUES(1)", [])
            .unwrap();
        assert!(
            v.reader
                .cache
                .conn
                .execute("UPDATE main.scans SET status='failed'", [])
                .is_err()
        );
        assert_eq!(
            r.root_entry(crate::tests::spec(1).key.id)
                .unwrap()
                .logical
                .observed_bytes,
            7
        );
    }
    #[test]
    fn no_op_filter_uses_indexed_base_without_temp_tables() {
        let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
        let mut c = Cache::open(crate::tests::dir()).unwrap();
        crate::tests::basic(&mut c, 1);
        let r = c.reader().unwrap();
        let progress = FilterProgress::default();
        let v = r
            .filtered_with_progress(
                crate::tests::spec(1).key.id,
                Metric::Logical,
                &QueryFilter::default(),
                &Cancellation::new(),
                &progress,
            )
            .unwrap();
        assert!(!v.materialized);
        let tables: i64 = v
            .reader
            .cache
            .conn
            .query_row("SELECT count(*) FROM sqlite_temp_master", [], |r| r.get(0))
            .unwrap();
        assert_eq!(tables, 0);
        assert_eq!(progress.snapshot().phase, FilterPhase::Finished);
        assert_eq!(v.root_entry().unwrap().logical.observed_bytes, 7);
    }
}
