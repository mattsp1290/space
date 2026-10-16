//! Raw-name observation comparisons. Identity groups reconcile accounting only;
//! they never infer a rename or historical continuity. All working indexes live
//! in disk-backed TEMP tables on an independently owned read-only connection.
mod accounting;
mod build;
use super::*;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
static NEXT_VIEW: AtomicU64 = AtomicU64::new(1);
const MAX_DEPTH: i64 = 4096;
pub(super) const OWNER_GROUPS_SQL: &str = "INSERT INTO groups SELECT b.identity,b.node,a.node FROM main.entries b JOIN main.entries a INDEXED BY hardlinks ON a.identity=b.identity AND a.scan=?2 AND a.kind=1 AND a.canonical=1 WHERE b.scan=?1 AND b.kind=1 AND b.identity IS NOT NULL AND b.canonical=1";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComparisonNodeId(i64);
impl ComparisonNodeId {
    pub fn get(self) -> i64 {
        self.0
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComparisonEndpoint {
    Before,
    After,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeDirection {
    Increase,
    Decrease,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComparisonPhase {
    Waiting,
    Matching,
    Accounting,
    Aggregating,
    Indexing,
    Finished,
    Cancelled,
    Failed,
}
#[derive(Default)]
struct Progress {
    phase: AtomicU8,
    processed: AtomicU64,
    total: AtomicU64,
}
#[derive(Clone, Default)]
pub struct ComparisonProgress(Arc<Progress>);
#[derive(Clone, Copy, Debug)]
pub struct ComparisonProgressSnapshot {
    pub phase: ComparisonPhase,
    pub processed: u64,
    pub total: u64,
}
impl ComparisonProgress {
    pub fn snapshot(&self) -> ComparisonProgressSnapshot {
        ComparisonProgressSnapshot {
            phase: match self.0.phase.load(Ordering::Acquire) {
                1 => ComparisonPhase::Matching,
                2 => ComparisonPhase::Accounting,
                3 => ComparisonPhase::Aggregating,
                4 => ComparisonPhase::Indexing,
                5 => ComparisonPhase::Finished,
                6 => ComparisonPhase::Cancelled,
                7 => ComparisonPhase::Failed,
                _ => ComparisonPhase::Waiting,
            },
            processed: self.0.processed.load(Ordering::Relaxed),
            total: self.0.total.load(Ordering::Relaxed),
        }
    }
    fn set(&self, phase: u8, total: u64) {
        self.0.processed.store(0, Ordering::Relaxed);
        self.0.total.store(total, Ordering::Relaxed);
        self.0.phase.store(phase, Ordering::Release);
    }
    fn advance(&self) {
        self.0.processed.fetch_add(1, Ordering::Relaxed);
    }
}
#[derive(Clone, Copy, Debug)]
pub struct ComparisonCursor {
    view: u64,
    scope: i64,
    mode: u8,
    metric: u8,
    value: i64,
    id: i64,
}
#[derive(Debug)]
pub struct ComparisonPage {
    pub entries: Vec<ComparisonEntry>,
    pub next: Option<ComparisonCursor>,
}
#[derive(Debug)]
pub struct ComparisonEntry {
    pub id: ComparisonNodeId,
    pub parent: Option<ComparisonNodeId>,
    /// Original snapshot observations, including original canonical attribution.
    pub before: Option<StoredEntry>,
    pub after: Option<StoredEntry>,
    pub before_presence: Presence,
    pub after_presence: Presence,
    pub change: ChangeKind,
    /// Comparison-accounting attribution; may differ from raw snapshot sizes when
    /// a hardlink owner moves. Shared identity groups are charged once at their LCA.
    pub logical: MetricChange,
    pub allocated: MetricChange,
    pub matching_paths: u64,
    pub changed_paths: u64,
}
pub struct ComparisonView {
    pub(super) reader: CacheReader,
    before: ScanId,
    after: ScanId,
    metric: Metric,
    view: u64,
    _before_pin: ScanPin,
    _after_pin: ScanPin,
}
#[derive(Serialize, Deserialize, Clone, Copy)]
struct Summary {
    logical: MetricChange,
    allocated: MetricChange,
    matching: u64,
    changed: u64,
}
impl Summary {
    fn zero() -> Self {
        Self {
            logical: MetricChange::zero(),
            allocated: MetricChange::zero(),
            matching: 0,
            changed: 0,
        }
    }
    fn add(self, b: Self) -> Result<Self> {
        Ok(Self {
            logical: self.logical.checked_add(b.logical)?,
            allocated: self.allocated.checked_add(b.allocated)?,
            matching: self
                .matching
                .checked_add(b.matching)
                .ok_or(StoreError::Overflow)?,
            changed: self
                .changed
                .checked_add(b.changed)
                .ok_or(StoreError::Overflow)?,
        })
    }
}
#[derive(Clone)]
struct Pair {
    id: i64,
    parent: Option<i64>,
    before: Option<i64>,
    after: Option<i64>,
    bp: Presence,
    ap: Presence,
    depth: i64,
}
impl CacheReader {
    pub fn compare(
        &self,
        before: ScanId,
        after: ScanId,
        filter_metric: Metric,
        filter: &QueryFilter,
        cancel: &Cancellation,
        progress: &ComparisonProgress,
    ) -> Result<ComparisonView> {
        filter.validate()?;
        let bp = self.pin(before)?;
        let ap = self.pin(after)?;
        validate_comparison(
            &self.spec(before)?,
            self.cache.status(before)?,
            &self.spec(after)?,
            self.cache.status(after)?,
        )
        .map_err(StoreError::ComparisonRefused)?;
        if self.scan_path(before)?.encoding != self.scan_path(after)?.encoding {
            return Err(StoreError::Invalid("comparison native encoding differs"));
        }
        let view = ComparisonView {
            reader: self.cache.reader()?,
            before,
            after,
            metric: filter_metric,
            view: NEXT_VIEW.fetch_add(1, Ordering::Relaxed),
            _before_pin: bp,
            _after_pin: ap,
        };
        let result = view.build(&filter.folded(), cancel, progress);
        view.reader
            .cache
            .conn
            .progress_handler(0, None::<fn() -> bool>)?;
        if let Err(e) = result {
            progress.set(if cancel.is_cancelled() { 6 } else { 7 }, 0);
            return Err(e);
        }
        progress.set(5, 0);
        Ok(view)
    }
}
impl ComparisonView {
    pub fn before_scan(&self) -> ScanId {
        self.before
    }
    pub fn after_scan(&self) -> ScanId {
        self.after
    }
    pub fn filter_metric(&self) -> Metric {
        self.metric
    }
    pub fn root(&self) -> Result<ComparisonEntry> {
        self.entry(ComparisonNodeId(1))
    }
    fn conn(&self) -> &Connection {
        &self.reader.cache.conn
    }
    fn endpoint(&self, scan: ScanId, node: Option<i64>) -> Result<Option<StoredEntry>> {
        node.map(|n| {
            self.reader.entry(
                scan,
                NodeId::try_from(n)
                    .map_err(|_| StoreError::Corrupt("comparison endpoint node".into()))?,
            )
        })
        .transpose()
    }
    fn pair(&self, id: i64) -> Result<Pair> {
        self.conn()
            .query_row(
                "SELECT parent,before_node,after_node,bp,ap,depth FROM pairs WHERE id=?",
                [id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .map_err(StoreError::from)
            .map(|(parent, before, after, bp, ap, depth)| Pair {
                id,
                parent,
                before,
                after,
                bp: presence(bp),
                ap: presence(ap),
                depth,
            })
    }
    pub fn entry(&self, id: ComparisonNodeId) -> Result<ComparisonEntry> {
        let p = self.pair(id.0)?;
        let (data, change, redist): (Vec<u8>, Vec<u8>, bool) = self.conn().query_row(
            "SELECT summary,change,redistributed FROM pairs WHERE id=?",
            [id.0],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let mut summary: Summary = decode(data)?;
        if redist {
            for m in [&mut summary.logical, &mut summary.allocated] {
                m.accounting = AccountingState::OwnershipRedistributed;
                m.net = None;
                m.unknown_changes = m.unknown_changes.max(1);
            }
        }
        let mut change: ChangeKind = decode(change)?;
        if change == ChangeKind::Unchanged {
            if summary.changed > 0
                || summary.logical.gross_increase > 0
                || summary.logical.gross_decrease > 0
                || summary.allocated.gross_increase > 0
                || summary.allocated.gross_decrease > 0
            {
                change = ChangeKind::Modified;
            } else if summary.logical.unknown_changes > 0 || summary.allocated.unknown_changes > 0 {
                change = ChangeKind::Unknown;
            }
        }
        Ok(ComparisonEntry {
            id,
            parent: p.parent.map(ComparisonNodeId),
            before: self.endpoint(self.before, p.before)?,
            after: self.endpoint(self.after, p.after)?,
            before_presence: p.bp,
            after_presence: p.ap,
            change,
            logical: summary.logical,
            allocated: summary.allocated,
            matching_paths: summary.matching,
            changed_paths: summary.changed,
        })
    }
    pub fn native_path(
        &self,
        id: ComparisonNodeId,
        endpoint: ComparisonEndpoint,
    ) -> Result<Option<NativePath>> {
        let p = self.pair(id.0)?;
        let (scan, node) = match endpoint {
            ComparisonEndpoint::Before => (self.before, p.before),
            ComparisonEndpoint::After => (self.after, p.after),
        };
        node.map(|n| {
            self.reader.native_path(
                scan,
                NodeId::try_from(n)
                    .map_err(|_| StoreError::Corrupt("comparison endpoint node".into()))?,
            )
        })
        .transpose()
    }
    pub fn children(
        &self,
        parent: ComparisonNodeId,
        metric: Metric,
        after: Option<ComparisonCursor>,
        limit: usize,
    ) -> Result<ComparisonPage> {
        self.page(Some(parent), metric, 0, after, limit)
    }
    pub fn largest(
        &self,
        subtree: Option<ComparisonNodeId>,
        metric: Metric,
        direction: ChangeDirection,
        after: Option<ComparisonCursor>,
        limit: usize,
    ) -> Result<ComparisonPage> {
        self.page(
            subtree,
            metric,
            if direction == ChangeDirection::Increase {
                1
            } else {
                2
            },
            after,
            limit,
        )
    }
    pub fn growing_directories(
        &self,
        subtree: Option<ComparisonNodeId>,
        metric: Metric,
        after: Option<ComparisonCursor>,
        limit: usize,
    ) -> Result<ComparisonPage> {
        self.page(subtree, metric, 3, after, limit)
    }
    fn page(
        &self,
        scope: Option<ComparisonNodeId>,
        metric: Metric,
        mode: u8,
        after: Option<ComparisonCursor>,
        limit: usize,
    ) -> Result<ComparisonPage> {
        check_limit(limit)?;
        let sid = scope.map_or(0, |n| n.0);
        let met = if metric == Metric::Logical { 0 } else { 1 };
        let cursor = after.unwrap_or(ComparisonCursor {
            view: self.view,
            scope: sid,
            mode,
            metric: met,
            value: i64::MAX,
            id: 0,
        });
        if cursor.view != self.view
            || cursor.scope != sid
            || cursor.mode != mode
            || cursor.metric != met
        {
            return Err(StoreError::Invalid(
                "comparison cursor belongs to another query",
            ));
        }
        let prefix = if met == 0 { "l" } else { "a" };
        let column = format!(
            "{prefix}{}",
            if mode == 0 {
                "rank"
            } else if mode == 2 {
                "dec"
            } else {
                "inc"
            }
        );
        let (lo, hi) = if let Some(id) = scope {
            self.conn()
                .query_row("SELECT lo,hi FROM pairs WHERE id=?", [id.0], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
                })?
        } else {
            (0, i64::MAX)
        };
        let condition = if mode == 0 {
            "parent=?1"
        } else if mode == 3 {
            "lo BETWEEN ?2 AND ?3 AND directory=1"
        } else {
            "lo BETWEEN ?2 AND ?3"
        };
        let positive = if mode == 0 {
            String::new()
        } else {
            format!("AND {column}>0")
        };
        let sql = format!(
            "SELECT id,{column} FROM pairs WHERE visible=1 AND {condition} {positive} AND ({column}<?4 OR ({column}=?4 AND id>?5)) ORDER BY {column} DESC,id LIMIT ?6"
        );
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt
            .query_map(
                params![sid, lo, hi, cursor.value, cursor.id, limit as i64 + 1],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let next = if rows.len() > limit {
            let (id, value) = rows[limit - 1];
            Some(ComparisonCursor {
                value,
                id,
                ..cursor
            })
        } else {
            None
        };
        Ok(ComparisonPage {
            entries: rows
                .into_iter()
                .take(limit)
                .map(|(id, _)| self.entry(ComparisonNodeId(id)))
                .collect::<Result<_>>()?,
            next,
        })
    }

    fn summary(&self, id: i64) -> Result<Summary> {
        decode(
            self.conn()
                .query_row("SELECT summary FROM pairs WHERE id=?", [id], |r| r.get(0))?,
        )
    }
    fn put_summary(&self, id: i64, s: Summary) -> Result<()> {
        self.conn().execute(
            "UPDATE pairs SET summary=? WHERE id=?",
            params![encode(&s)?, id],
        )?;
        Ok(())
    }
}
fn presence(code: i64) -> Presence {
    match code {
        0 => Presence::Present,
        1 => Presence::ProvenAbsent,
        _ => Presence::Unobserved,
    }
}
fn presence_code(p: Presence) -> i64 {
    match p {
        Presence::Present => 0,
        Presence::ProvenAbsent => 1,
        Presence::Unobserved => 2,
    }
}
fn changed(b: Option<&StoredEntry>, a: Option<&StoredEntry>) -> Option<bool> {
    let (b, a) = b.zip(a)?;
    let b = &b.entry;
    let a = &a.entry;
    if b.metadata_state != MetadataState::Complete || a.metadata_state != MetadataState::Complete {
        return None;
    }
    if b.kind != a.kind
        || b.logical_bytes != a.logical_bytes
        || b.allocated_bytes != a.allocated_bytes
        || b.modified != a.modified
        || b.link_state != a.link_state
    {
        return Some(true);
    }
    if b.kind == EntryKind::Directory && a.kind == EntryKind::Directory {
        return Some(false);
    }
    if b.logical_bytes.is_none() {
        None
    } else {
        Some(false)
    }
}
