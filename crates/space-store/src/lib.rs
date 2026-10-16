//! Exclusive, local SQLite cache. Only published snapshots are queryable.
//! APIs are synchronous: call them on a bounded background writer/query worker.
mod comparison;
pub use comparison::*;
#[cfg(test)]
mod comparison_tests;
mod filters;
mod maintenance;
mod queries;
pub use maintenance::{CacheFootprint, IssuePage, ScanInfo, ScanPage};
mod schema;
pub use filters::{FilterPhase, FilterProgress, FilterProgressSnapshot, FilteredView, QueryFilter};
pub use queries::{AliasPage, ChildrenSummary};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use space_core::*;
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    path::Path,
    sync::{Arc, Mutex, Weak},
};

pub const MAX_BATCH_RECORDS: usize = 1024;
pub const MAX_BATCH_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_PAGE_SIZE: usize = 500;
#[derive(Debug)]
pub enum StoreError {
    Sql(rusqlite::Error),
    Io(std::io::Error),
    Encoding(serde_json::Error),
    Contract(ContractError),
    ComparisonRefused(ComparisonRefusal),
    InUse,
    Schema(i64),
    Corrupt(String),
    Invalid(&'static str),
    Overflow,
}
impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Self::ComparisonRefused(reason) = self {
            return reason.fmt(f);
        }
        write!(f, "{self:?}")
    }
}
impl std::error::Error for StoreError {}
impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sql(e)
    }
}
impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<serde_json::Error> for StoreError {
    fn from(e: serde_json::Error) -> Self {
        Self::Encoding(e)
    }
}
impl From<ContractError> for StoreError {
    fn from(e: ContractError) -> Self {
        Self::Contract(e)
    }
}
pub type Result<T> = std::result::Result<T, StoreError>;
fn encode<T: Serialize>(v: &T) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(v)?)
}
fn decode<T: DeserializeOwned>(v: Vec<u8>) -> Result<T> {
    Ok(serde_json::from_slice(&v)?)
}
fn number(v: u64) -> Result<i64> {
    Ok(sqlite_integer(v)?)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RootRecord {
    pub id: RootId,
    pub lineage: LineageId,
    pub path: NativePath,
    pub identity: RootIdentity,
}
#[derive(Clone, Debug)]
pub struct StoredEntry {
    pub entry: EntryRecord,
    pub logical: MetricTotal,
    pub allocated: MetricTotal,
    pub coverage: CoverageSummary,
    pub canonical: bool,
    pub ordinal: i64,
}
#[derive(Clone, Copy, Debug)]
pub struct PageCursor {
    pub ordinal: i64,
    pub value: i64,
}
#[derive(Clone, Debug)]
pub struct Page {
    pub entries: Vec<StoredEntry>,
    pub next: Option<PageCursor>,
}
type FinalizeRow = (
    i64,
    Option<i64>,
    i64,
    Vec<u8>,
    Option<i64>,
    Option<i64>,
    bool,
);
enum PinState {
    Active(Weak<()>),
    Retiring,
}
type Pins = Arc<Mutex<BTreeMap<i64, PinState>>>;
/// A view owns this token until it releases the snapshot. It also keeps the OS
/// lock alive if the Cache is dropped before the view.
pub struct ScanPin {
    scan: ScanId,
    _token: Arc<()>,
    _lock: Arc<File>,
}
impl ScanPin {
    pub fn scan_id(&self) -> ScanId {
        self.scan
    }
}
pub struct Cache {
    maintenance_warning: Option<String>,
    conn: Connection,
    lock: Arc<File>,
    pins: Pins,
}
/// Independent read-only SQLite connection. Keeps the process lock and shared
/// retention pins alive; immutable published views remain readable during writes.
pub struct CacheReader {
    cache: Cache,
}
impl CacheReader {
    pub fn latest(&self, root: RootId) -> Result<Option<ScanId>> {
        self.cache.latest(root)
    }
    pub fn roots(&self) -> Result<Vec<RootRecord>> {
        self.cache.roots()
    }
    pub fn root(&self, id: RootId) -> Result<RootRecord> {
        self.cache.root(id)
    }
    pub fn spec(&self, id: ScanId) -> Result<ScanSpec> {
        self.cache.spec(id)
    }
    pub fn scan_path(&self, id: ScanId) -> Result<NativePath> {
        self.cache.scan_path(id)
    }
    pub fn interval(&self, id: ScanId) -> Result<(Timestamp, Option<Timestamp>)> {
        self.cache.interval(id)
    }
    pub fn pin(&self, id: ScanId) -> Result<ScanPin> {
        self.cache.pin(id)
    }
    pub fn root_entry(&self, id: ScanId) -> Result<StoredEntry> {
        self.cache.root_entry(id)
    }
    pub fn entry(&self, scan: ScanId, node: NodeId) -> Result<StoredEntry> {
        self.cache.entry(scan, node)
    }
    pub fn native_path(&self, scan: ScanId, node: NodeId) -> Result<NativePath> {
        self.cache.native_path(scan, node)
    }
    pub fn children(
        &self,
        scan: ScanId,
        parent: NodeId,
        after: Option<PageCursor>,
        limit: usize,
    ) -> Result<Page> {
        self.cache.children(scan, parent, after, limit)
    }
    pub fn largest(
        &self,
        scan: ScanId,
        metric: Metric,
        after: Option<PageCursor>,
        limit: usize,
    ) -> Result<Page> {
        self.cache.largest(scan, metric, after, limit)
    }
}
impl Cache {
    pub fn reader(&self) -> Result<CacheReader> {
        let path = self
            .conn
            .path()
            .ok_or(StoreError::Invalid("file-backed cache required"))?;
        let conn = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(std::time::Duration::from_secs(2))?;
        conn.execute_batch("PRAGMA query_only=ON; PRAGMA cache_size=-8192")?;
        Ok(CacheReader {
            cache: Cache {
                conn,
                lock: self.lock.clone(),
                pins: self.pins.clone(),
                maintenance_warning: None,
            },
        })
    }
    /// `directory` must be a dedicated application cache directory. Never pass a
    /// source folder. A corrupt/newer cache is reported and never replaced.
    pub fn open(directory: impl AsRef<Path>) -> Result<Self> {
        let directory = directory.as_ref();
        if !directory.exists() {
            let mut b = std::fs::DirBuilder::new();
            b.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                b.mode(0o700);
            }
            b.create(directory)?;
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options.open(directory.join("owner.lock"))?;
        lock.try_lock().map_err(|e| match e {
            std::fs::TryLockError::WouldBlock => StoreError::InUse,
            std::fs::TryLockError::Error(e) => StoreError::Io(e),
        })?;
        let path = directory.join("cache.sqlite3");
        let _ = options.open(&path)?;
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(2))?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version != 0 && version != 1 && version != 2 && version != schema::VERSION {
            return Err(StoreError::Schema(version));
        }
        let integrity: String = conn.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        if integrity != "ok" {
            return Err(StoreError::Corrupt(integrity));
        }
        conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA temp_store=FILE; PRAGMA cache_size=-8192;")?;
        if version == 0 {
            let tables: i64 = conn.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table'",
                [],
                |r| r.get(0),
            )?;
            if tables != 0 {
                return Err(StoreError::Schema(0));
            }
            conn.execute_batch(&format!(
                "BEGIN IMMEDIATE; {} {} {} COMMIT;",
                schema::CREATE,
                schema::QUERY_INDEX,
                schema::IDS
            ))?;
        }
        if version == 1 {
            conn.execute_batch(&format!(
                "BEGIN IMMEDIATE; {} {} COMMIT;",
                schema::QUERY_INDEX,
                schema::IDS
            ))?;
        }
        if version == 2 {
            conn.execute_batch(&format!("BEGIN IMMEDIATE; {} COMMIT;", schema::IDS))?;
        }
        conn.execute(
            "UPDATE scans SET status='interrupted' WHERE status IN ('scanning','finalizing')",
            [],
        )?;
        let mut cache = Self {
            maintenance_warning: None,
            conn,
            lock: Arc::new(lock),
            pins: Arc::new(Mutex::new(BTreeMap::new())),
        };
        cache.maintenance_warning = cache.retain().err().map(|e| e.to_string());
        Ok(cache)
    }
    pub fn root(&self, id: RootId) -> Result<RootRecord> {
        let root: RootRecord = decode(self.conn.query_row(
            "SELECT identity FROM roots WHERE id=?",
            [id.get()],
            |r| r.get(0),
        )?)?;
        if root.id != id {
            return Err(StoreError::Corrupt("root key mismatch".into()));
        }
        root.path
            .validate()
            .map_err(|_| StoreError::Corrupt("invalid root path".into()))?;
        Ok(root)
    }
    pub fn roots(&self) -> Result<Vec<RootRecord>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM roots ORDER BY id LIMIT 1000")?;
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
        rows.map(|r| {
            self.root(
                RootId::try_from(r?).map_err(|_| StoreError::Corrupt("invalid root id".into()))?,
            )
        })
        .collect()
    }
    pub fn latest(&self, root: RootId) -> Result<Option<ScanId>> {
        self.conn
            .query_row(
                "SELECT scan FROM publication WHERE root=?",
                [root.get()],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .map(|n| ScanId::try_from(n).map_err(|_| StoreError::Corrupt("invalid scan id".into())))
            .transpose()
    }
    pub fn spec(&self, scan: ScanId) -> Result<ScanSpec> {
        let (data, root, generation): (Vec<u8>, i64, i64) = self.conn.query_row(
            "SELECT metadata,root,generation FROM scans WHERE id=?",
            [scan.get()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let spec: ScanSpec = decode(data)?;
        if spec.key.id != scan
            || spec.root_id.get() != root
            || number(spec.key.generation)? != generation
        {
            return Err(StoreError::Corrupt("scan key mismatch".into()));
        }
        spec.policy
            .validate_for_encoding(self.root(spec.root_id)?.path.encoding)?;
        Ok(spec)
    }
    pub fn status(&self, scan: ScanId) -> Result<ScanStatus> {
        let s: String =
            self.conn
                .query_row("SELECT status FROM scans WHERE id=?", [scan.get()], |r| {
                    r.get(0)
                })?;
        match s.as_str() {
            "scanning" => Ok(ScanStatus::Scanning),
            "finalizing" => Ok(ScanStatus::Finalizing),
            "published" => Ok(ScanStatus::Published),
            "cancelled" => Ok(ScanStatus::Cancelled),
            "failed" => Ok(ScanStatus::Failed),
            "interrupted" => Ok(ScanStatus::Interrupted),
            _ => Err(StoreError::Corrupt(s)),
        }
    }
    pub fn begin(&mut self, spec: &ScanSpec, path: &NativePath, started: Timestamp) -> Result<()> {
        spec.policy.validate_for_encoding(path.encoding)?;
        path.validate()
            .map_err(|_| StoreError::Invalid("invalid native root"))?;
        Timestamp::new(started.seconds, started.nanos)?;
        let tx = self.conn.transaction()?;
        let highest: i64 =
            tx.query_row("SELECT scan FROM id_highwater WHERE singleton=1", [], |r| {
                r.get(0)
            })?;
        if spec.key.id.get() <= highest {
            return Err(StoreError::Invalid("scan ids must increase"));
        }
        let active: i64 = tx.query_row(
            "SELECT count(*) FROM scans WHERE status IN ('scanning','finalizing')",
            [],
            |r| r.get(0),
        )?;
        if active != 0 {
            return Err(StoreError::Invalid("writer already owns a working scan"));
        }
        let root = RootRecord {
            id: spec.root_id,
            lineage: spec.lineage_id,
            path: path.clone(),
            identity: spec.expected_root.clone(),
        };
        let old: Option<Vec<u8>> = tx
            .query_row(
                "SELECT identity FROM roots WHERE id=?",
                [spec.root_id.get()],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(old) = old {
            let old: RootRecord = decode(old)?;
            if old.lineage != root.lineage
                || old.identity.validate_against(&root.identity)
                    != IdentityValidation::MatchingObservation
            {
                return Err(StoreError::Invalid(
                    "root continuity requires explicit new root/lineage",
                ));
            }
        } else {
            let highest_root: i64 =
                tx.query_row("SELECT root FROM id_highwater WHERE singleton=1", [], |r| {
                    r.get(0)
                })?;
            if spec.root_id.get() <= highest_root {
                return Err(StoreError::Invalid("root ids must increase"));
            }
            tx.execute(
                "INSERT INTO roots(id,identity,policy) VALUES(?,?,?)",
                params![spec.root_id.get(), encode(&root)?, encode(&spec.policy)?],
            )?;
        }
        tx.execute("INSERT INTO scans(id,root,generation,status,started,started_nanos,selected_path,metadata) VALUES(?,?,?,'scanning',?,?,?,?)",params![spec.key.id.get(),spec.root_id.get(),number(spec.key.generation)?,started.seconds,started.nanos,encode(path)?,encode(spec)?])?;
        tx.execute(
            "UPDATE id_highwater SET scan=?,root=max(root,?) WHERE singleton=1",
            params![spec.key.id.get(), spec.root_id.get()],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn ingest(&mut self, batch: &ScanBatch) -> Result<()> {
        let result = self.ingest_inner(batch);
        if matches!(&result, Err(StoreError::Sql(_))) {
            self.fail(batch.key).ok();
        }
        result
    }
    fn ingest_inner(&mut self, batch: &ScanBatch) -> Result<()> {
        if batch.records.len() > MAX_BATCH_RECORDS {
            return Err(StoreError::Invalid("batch record bound"));
        }
        let mut bytes = 0usize;
        let mut encoded = Vec::with_capacity(batch.records.len());
        for r in &batch.records {
            let b = encode(r)?;
            bytes = bytes.checked_add(b.len()).ok_or(StoreError::Overflow)?;
            if bytes > MAX_BATCH_BYTES {
                return Err(StoreError::Invalid("batch byte bound"));
            }
            encoded.push(b);
        }
        let scan_spec = self.spec(batch.key.id)?;
        let root = self.root(scan_spec.root_id)?;
        let tx = self.conn.transaction()?;
        check_working(&tx, batch.key)?;
        let sequence: i64 = tx.query_row(
            "SELECT next_sequence FROM scans WHERE id=?",
            [batch.key.id.get()],
            |r| r.get(0),
        )?;
        if sequence != number(batch.sequence)? {
            return Err(StoreError::Invalid("out of order batch"));
        }
        for (record, _) in batch.records.iter().zip(encoded) {
            match record {
                ScanRecord::Entry(e) => {
                    e.validate()?;
                    if e.name
                        .as_ref()
                        .is_some_and(|n| n.encoding() != root.path.encoding)
                    {
                        return Err(StoreError::Invalid("mixed native encoding"));
                    }
                    if let Some(parent) = e.parent {
                        let k: Option<i64> = tx
                            .query_row(
                                "SELECT kind FROM entries WHERE scan=? AND node=?",
                                params![batch.key.id.get(), parent.get()],
                                |r| r.get(0),
                            )
                            .optional()?;
                        if k != Some(0) {
                            return Err(StoreError::Invalid("missing/non-directory parent"));
                        }
                        let closed: bool = tx.query_row(
                            "SELECT EXISTS(SELECT 1 FROM coverage WHERE scan=? AND node=?)",
                            params![batch.key.id.get(), parent.get()],
                            |r| r.get(0),
                        )?;
                        if closed {
                            return Err(StoreError::Invalid("child after directory finished"));
                        }
                    } else {
                        let count: i64 = tx.query_row(
                            "SELECT count(*) FROM entries WHERE scan=? AND parent IS NULL",
                            [batch.key.id.get()],
                            |r| r.get(0),
                        )?;
                        if count != 0 {
                            return Err(StoreError::Invalid("multiple roots"));
                        }
                    }
                    let identity = e
                        .identity
                        .as_ref()
                        .filter(|i| i.is_deduplicable())
                        .map(encode)
                        .transpose()?;
                    tx.execute("INSERT INTO entries(scan,node,parent,native,display,kind,identity,logical,allocated,data) VALUES(?,?,?,?,?,?,?,?,?,?)",params![batch.key.id.get(),e.node.get(),e.parent.map(NodeId::get),e.name.as_ref().map(NativeName::sort_key).unwrap_or_default(),e.display_name,kind(e.kind),identity,e.logical_bytes.map(number).transpose()?,e.allocated_bytes.map(number).transpose()?,encode(e)?])?;
                }
                ScanRecord::DirectoryFinished { node, enumeration } => {
                    let k: Option<i64> = tx
                        .query_row(
                            "SELECT kind FROM entries WHERE scan=? AND node=?",
                            params![batch.key.id.get(), node.get()],
                            |r| r.get(0),
                        )
                        .optional()?;
                    if k != Some(0) {
                        return Err(StoreError::Invalid("coverage for missing directory"));
                    }
                    tx.execute(
                        "INSERT INTO coverage(scan,node,data) VALUES(?,?,?)",
                        params![batch.key.id.get(), node.get(), encode(enumeration)?],
                    )?;
                }
                ScanRecord::Issue(issue) => {
                    if issue.detail.len() > ScanIssue::MAX_DETAIL_BYTES {
                        return Err(StoreError::Invalid("issue detail bound"));
                    }
                    if let Some(node) = issue.node.or(issue.parent) {
                        let exists: bool = tx.query_row(
                            "SELECT EXISTS(SELECT 1 FROM entries WHERE scan=? AND node=?)",
                            params![batch.key.id.get(), node.get()],
                            |r| r.get(0),
                        )?;
                        if !exists {
                            return Err(StoreError::Invalid("issue scope missing"));
                        }
                    }
                    if let Some(name) = &issue.name {
                        name.validate()
                            .map_err(|_| StoreError::Invalid("invalid issue name"))?;
                        if name.encoding() != root.path.encoding {
                            return Err(StoreError::Invalid("issue name encoding"));
                        }
                    }
                    tx.execute(
                        "INSERT INTO errors(scan,node,data) VALUES(?,?,?)",
                        params![
                            batch.key.id.get(),
                            issue.node.or(issue.parent).map(NodeId::get),
                            encode(issue)?
                        ],
                    )?;
                }
            }
        }
        tx.execute(
            "UPDATE scans SET next_sequence=? WHERE id=?",
            params![
                sequence.checked_add(1).ok_or(StoreError::Overflow)?,
                batch.key.id.get()
            ],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn cancel(&mut self, key: ScanKey) -> Result<()> {
        self.terminate(key, "cancelled")
    }
    pub fn fail(&mut self, key: ScanKey) -> Result<()> {
        self.terminate(key, "failed")
    }
    fn terminate(&mut self, key: ScanKey, status: &str) -> Result<()> {
        check_working(&self.conn, key)?;
        self.conn.execute(
            "UPDATE scans SET status=? WHERE id=?",
            params![status, key.id.get()],
        )?;
        self.maintenance_warning = self.retain().err().map(|e| e.to_string());
        Ok(())
    }
    pub fn publish(&mut self, finish: &ScanFinish, ended: Timestamp) -> Result<()> {
        self.publish_cancellable(finish, ended, &Cancellation::new())
    }
    pub fn publish_cancellable(
        &mut self,
        finish: &ScanFinish,
        ended: Timestamp,
        cancel: &Cancellation,
    ) -> Result<()> {
        if cancel.is_cancelled() {
            self.cancel(finish.key)?;
            return Err(StoreError::Invalid("publication cancelled"));
        }
        check_working(&self.conn, finish.key)?;
        if finish.outcome != TraversalOutcome::Finished {
            return self.terminate(
                finish.key,
                if finish.outcome == TraversalOutcome::Cancelled {
                    "cancelled"
                } else {
                    "failed"
                },
            );
        }
        let cancelled_scope: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM coverage WHERE scan=? AND data=?)",
            params![finish.key.id.get(), encode(&EnumerationState::Cancelled)?],
            |r| r.get(0),
        )?;
        if cancelled_scope {
            self.cancel(finish.key)?;
            return Err(StoreError::Invalid("cancelled traversal cannot publish"));
        }
        let spec = self.spec(finish.key.id)?;
        if spec.expected_root.validate_against(&finish.root_after)
            != IdentityValidation::MatchingObservation
        {
            self.fail(finish.key)?;
            return Err(StoreError::Invalid(
                "root changed or continuity unavailable",
            ));
        }
        Timestamp::new(ended.seconds, ended.nanos)?;
        self.conn.execute(
            "UPDATE scans SET status='finalizing' WHERE id=?",
            [finish.key.id.get()],
        )?;
        #[cfg(test)]
        crash_pause("finalizing");
        let result = self.finalize(finish.key.id, spec.root_id, ended, cancel);
        if result.is_err() {
            self.conn
                .execute(
                    "UPDATE scans SET status=? WHERE id=? AND status='finalizing'",
                    params![
                        if cancel.is_cancelled() {
                            "cancelled"
                        } else {
                            "failed"
                        },
                        finish.key.id.get()
                    ],
                )
                .ok();
        }
        result?;
        self.maintenance_warning = self.retain().err().map(|e| e.to_string());
        Ok(())
    }
    fn finalize(
        &mut self,
        scan: ScanId,
        root: RootId,
        ended: Timestamp,
        cancel: &Cancellation,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        let sid = scan.get();
        let root_node: i64 = tx.query_row(
            "SELECT node FROM entries WHERE scan=? AND parent IS NULL",
            [sid],
            |r| r.get(0),
        )?;
        // Iterative native-name DFS: memory scales with depth, never entry count.
        // Store ordinal rather than a repeated full path (deep trees stay linear).
        let mut stack = vec![(root_node, None::<(Vec<u8>, i64)>)];
        let mut ordinal = 1i64;
        tx.execute(
            "UPDATE entries SET ordinal=? WHERE scan=? AND node=?",
            params![ordinal, sid, root_node],
        )?;
        while let Some((parent, after)) = stack.last_mut() {
            if cancel.is_cancelled() {
                return Err(StoreError::Invalid("publication cancelled"));
            }
            let child:Option<(i64,Vec<u8>)>=match after {
                None=>tx.query_row("SELECT node,native FROM entries WHERE scan=? AND parent=? ORDER BY native,node LIMIT 1",params![sid,*parent],|r|Ok((r.get(0)?,r.get(1)?))).optional()?,
                Some((name,node))=>tx.query_row("SELECT node,native FROM entries WHERE scan=? AND parent=? AND (native,node)>(?,?) ORDER BY native,node LIMIT 1",params![sid,*parent,&*name,*node],|r|Ok((r.get(0)?,r.get(1)?))).optional()?
            };
            if let Some((node, name)) = child {
                *after = Some((name, node));
                ordinal = ordinal.checked_add(1).ok_or(StoreError::Overflow)?;
                tx.execute(
                    "UPDATE entries SET ordinal=? WHERE scan=? AND node=?",
                    params![ordinal, sid, node],
                )?;
                stack.push((node, None));
            } else {
                stack.pop();
            }
        }
        tx.execute("UPDATE entries SET canonical=0 WHERE scan=? AND kind=1 AND identity IS NOT NULL AND ordinal<>(SELECT min(b.ordinal) FROM entries b WHERE b.scan=entries.scan AND b.kind=1 AND b.identity=entries.identity)",[sid])?;
        tx.execute("UPDATE entries SET charged_logical=CASE WHEN kind=1 AND canonical=1 THEN logical ELSE 0 END,charged_allocated=CASE WHEN kind=1 AND canonical=1 THEN allocated ELSE 0 END WHERE scan=?",[sid])?;
        let mut before = i64::MAX;
        loop {
            if cancel.is_cancelled() {
                return Err(StoreError::Invalid("publication cancelled"));
            }
            let row:Option<FinalizeRow>=tx.query_row("SELECT node,parent,ordinal,data,charged_logical,charged_allocated,canonical FROM entries WHERE scan=? AND ordinal<? ORDER BY ordinal DESC LIMIT 1",params![sid,before],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).optional()?;
            let Some((node, parent, ord, data, logical, allocated, canonical)) = row else {
                break;
            };
            before = ord;
            let entry: EntryRecord = decode(data)?;
            let unknown = u64::from(entry.metadata_state != MetadataState::Complete);
            let mut errors = 0i64;
            let mut excluded = 0i64;
            let mut boundary = 0i64;
            let mut issues = tx.prepare(
                "SELECT data FROM errors WHERE scan=? AND (node=? OR (node IS NULL AND ?))",
            )?;
            let mut issue_rows = issues.query(params![sid, node, parent.is_none()])?;
            while let Some(row) = issue_rows.next()? {
                let issue: ScanIssue = decode(row.get(0)?)?;
                let count = match issue.class {
                    IssueClass::Excluded => &mut excluded,
                    IssueClass::MountBoundary
                    | IssueClass::VirtualFilesystem
                    | IssueClass::NetworkFilesystem => &mut boundary,
                    _ => &mut errors,
                };
                *count = count.checked_add(1).ok_or(StoreError::Overflow)?;
            }
            let conflicting: bool = if canonical
                && entry.kind == EntryKind::RegularFile
                && entry
                    .identity
                    .as_ref()
                    .is_some_and(FileIdentity::is_deduplicable)
            {
                tx.query_row("SELECT EXISTS(SELECT 1 FROM entries other JOIN entries owner ON owner.scan=other.scan AND owner.identity=other.identity WHERE owner.scan=? AND owner.node=? AND (other.logical IS NOT owner.logical OR other.allocated IS NOT owner.allocated))",params![sid,node],|r|r.get(0))?
            } else {
                false
            };
            errors = errors
                .checked_add(i64::from(conflicting))
                .ok_or(StoreError::Overflow)?;
            let mut total = Totals {
                logical: logical.unwrap_or(0),
                allocated: allocated.unwrap_or(0),
                unknown_logical: i64::from(logical.is_none()),
                unknown_allocated: i64::from(allocated.is_none()),
                errors,
                estimated: i64::from(
                    entry.kind == EntryKind::RegularFile
                        && canonical
                        && !entry
                            .identity
                            .as_ref()
                            .is_some_and(FileIdentity::is_deduplicable),
                ),
                unknown_entries: number(unknown)?
                    .checked_add(i64::from(conflicting))
                    .ok_or(StoreError::Overflow)?,
                excluded,
                boundary,
            };
            if entry.kind == EntryKind::Directory {
                let state: Option<Vec<u8>> = tx
                    .query_row(
                        "SELECT data FROM coverage WHERE scan=? AND node=?",
                        params![sid, node],
                        |r| r.get(0),
                    )
                    .optional()?;
                match state.map(decode::<EnumerationState>).transpose()? {
                    Some(EnumerationState::Complete) => {}
                    Some(EnumerationState::Excluded) => total.excluded = total.excluded.max(1),
                    Some(EnumerationState::BoundarySkipped) => {
                        total.boundary = total.boundary.max(1)
                    }
                    _ => {
                        total.unknown_entries = total
                            .unknown_entries
                            .checked_add(1)
                            .ok_or(StoreError::Overflow)?
                    }
                }
            }
            let descendants: Option<Totals> = read_totals(&tx, sid, node)?;
            if let Some(t) = descendants {
                total = total.add(t)?;
            }
            put_totals(&tx, sid, node, total)?;
            if let Some(parent) = parent {
                let mut inherited = read_totals(&tx, sid, parent)?.unwrap_or_default();
                inherited = inherited.add(total)?;
                put_totals(&tx, sid, parent, inherited)?;
            }
        }
        tx.execute("INSERT INTO child_sizes SELECT e.scan,e.parent,e.node,e.ordinal,t.logical,t.allocated FROM entries e JOIN totals t ON t.scan=e.scan AND t.node=e.node WHERE e.scan=?",[sid])?;
        tx.execute(
            "UPDATE scans SET status='published',ended=?,ended_nanos=? WHERE id=?",
            params![ended.seconds, ended.nanos, sid],
        )?;
        tx.execute("INSERT INTO publication(root,scan) VALUES(?,?) ON CONFLICT(root) DO UPDATE SET scan=excluded.scan",params![root.get(),sid])?;
        #[cfg(test)]
        crash_pause("before_commit");
        cancel
            .with_publication_guard(|| tx.commit())
            .ok_or(StoreError::Invalid("publication cancelled"))??;
        Ok(())
    }
    pub fn maintenance_warning(&self) -> Option<&str> {
        self.maintenance_warning.as_deref()
    }
    pub fn pin(&self, scan: ScanId) -> Result<ScanPin> {
        let mut pins = self.pins.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(pins.get(&scan.get()), Some(PinState::Retiring))
            || matches!(pins.get(&0), Some(PinState::Retiring))
        {
            return Err(StoreError::Invalid("snapshot is being retired"));
        }
        self.ensure_published(scan)?;
        let token = match pins.get(&scan.get()) {
            Some(PinState::Active(weak)) => weak.upgrade().unwrap_or_else(|| Arc::new(())),
            _ => Arc::new(()),
        };
        pins.insert(scan.get(), PinState::Active(Arc::downgrade(&token)));
        Ok(ScanPin {
            scan,
            _token: token,
            _lock: self.lock.clone(),
        })
    }
    fn ensure_published(&self, scan: ScanId) -> Result<()> {
        if self.status(scan)? != ScanStatus::Published {
            return Err(StoreError::Invalid("snapshot is not published"));
        }
        Ok(())
    }
    /// Keeps two newest publications per root and at most two diagnostic scans.
    /// Pinned snapshots survive until a subsequent retention pass after release.
    pub fn retain(&mut self) -> Result<()> {
        let mut cursor = 0i64;
        loop {
            let candidate:Option<i64>=self.conn.query_row("SELECT s.id FROM scans s WHERE s.id>? AND ((s.status='published' AND (SELECT count(*) FROM scans newer WHERE newer.root=s.root AND newer.status='published' AND newer.id>s.id)>=2) OR (s.status IN ('failed','cancelled','interrupted') AND (SELECT count(*) FROM scans newer WHERE newer.root=s.root AND newer.status IN ('failed','cancelled','interrupted') AND newer.id>s.id)>=2)) AND s.id NOT IN (SELECT scan FROM publication) ORDER BY s.id LIMIT 1",[cursor],|r|r.get(0)).optional()?;
            let Some(id) = candidate else { break };
            cursor = id;
            let retire = {
                let mut pins = self.pins.lock().unwrap_or_else(|e| e.into_inner());
                let in_use = match pins.get(&id) {
                    Some(PinState::Active(weak)) => weak.strong_count() > 0,
                    Some(PinState::Retiring) => true,
                    None => false,
                };
                if !in_use {
                    pins.insert(id, PinState::Retiring);
                }
                !in_use
            };
            if retire {
                // Drop is only an Arc decrement, and the registry lock is never
                // held across a cascading SQLite delete/commit.
                let result = self.conn.execute("DELETE FROM scans WHERE id=?", [id]);
                self.pins
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id);
                result?;
            }
        }
        Ok(())
    }
    pub fn scan_path(&self, scan: ScanId) -> Result<NativePath> {
        let path: NativePath = decode(self.conn.query_row(
            "SELECT selected_path FROM scans WHERE id=?",
            [scan.get()],
            |r| r.get(0),
        )?)?;
        path.validate()
            .map_err(|_| StoreError::Corrupt("invalid scan path".into()))?;
        Ok(path)
    }
    pub fn interval(&self, scan: ScanId) -> Result<(Timestamp, Option<Timestamp>)> {
        let (start, nanos, end, end_nanos): (i64, u32, Option<i64>, Option<u32>) =
            self.conn.query_row(
                "SELECT started,started_nanos,ended,ended_nanos FROM scans WHERE id=?",
                [scan.get()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
        Ok((
            Timestamp::new(start, nanos)?,
            end.map(|seconds| Timestamp::new(seconds, end_nanos.unwrap_or(0)))
                .transpose()?,
        ))
    }
    /// Reconstruct only the selected path from native parent components. No source I/O.
    pub fn native_path(&self, scan: ScanId, node: NodeId) -> Result<NativePath> {
        let selected_path = self.scan_path(scan)?;
        let mut names = Vec::new();
        let mut current = Some(node);
        while let Some(id) = current {
            if names.len() > 4096 {
                return Err(StoreError::Corrupt("path depth bound".into()));
            }
            let e = self.entry(scan, id)?.entry;
            if let Some(n) = e.name {
                names.push(n);
            }
            current = e.parent;
        }
        let mut path = selected_path
            .to_path_buf()
            .map_err(|_| StoreError::Invalid("native path belongs to another OS"))?;
        for name in names.into_iter().rev() {
            path.push(
                name.to_os()
                    .map_err(|_| StoreError::Invalid("native name belongs to another OS"))?,
            );
        }
        Ok(NativePath::from_path(&path))
    }
    pub fn next_scan_id(&self) -> Result<ScanId> {
        let id: i64 = self.conn.query_row(
            "SELECT scan+1 FROM id_highwater WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        ScanId::try_from(id).map_err(|_| StoreError::Overflow)
    }
    pub fn next_root_id(&self) -> Result<RootId> {
        let id: i64 = self.conn.query_row(
            "SELECT root+1 FROM id_highwater WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        RootId::try_from(id).map_err(|_| StoreError::Overflow)
    }
}
fn check_limit(limit: usize) -> Result<()> {
    if limit == 0 || limit > MAX_PAGE_SIZE {
        Err(StoreError::Invalid("page size must be 1..=500"))
    } else {
        Ok(())
    }
}
fn check_working(conn: &Connection, key: ScanKey) -> Result<()> {
    let current: Option<(i64, String)> = conn
        .query_row(
            "SELECT generation,status FROM scans WHERE id=?",
            [key.id.get()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if current != Some((number(key.generation)?, "scanning".into())) {
        Err(StoreError::Invalid("obsolete or terminal scan event"))
    } else {
        Ok(())
    }
}
fn kind(k: EntryKind) -> i64 {
    match k {
        EntryKind::Directory => 0,
        EntryKind::RegularFile => 1,
        EntryKind::Symlink => 2,
        EntryKind::Other => 3,
    }
}
#[derive(Default, Clone, Copy)]
struct Totals {
    logical: i64,
    allocated: i64,
    unknown_logical: i64,
    unknown_allocated: i64,
    errors: i64,
    estimated: i64,
    unknown_entries: i64,
    excluded: i64,
    boundary: i64,
}
impl Totals {
    fn add(self, r: Self) -> Result<Self> {
        macro_rules! a {
            ($f:ident) => {
                self.$f.checked_add(r.$f).ok_or(StoreError::Overflow)?
            };
        }
        Ok(Self {
            logical: a!(logical),
            allocated: a!(allocated),
            unknown_logical: a!(unknown_logical),
            unknown_allocated: a!(unknown_allocated),
            errors: a!(errors),
            estimated: a!(estimated),
            unknown_entries: a!(unknown_entries),
            excluded: a!(excluded),
            boundary: a!(boundary),
        })
    }
}
fn read_totals(c: &Connection, scan: i64, node: i64) -> Result<Option<Totals>> {
    read_totals_from(c, scan, node, "totals")
}
fn read_totals_from(c: &Connection, scan: i64, node: i64, table: &str) -> Result<Option<Totals>> {
    let result=c.query_row(&format!("SELECT logical,allocated,unknown_logical,unknown_allocated,errors,estimated,unknown_entries,excluded,boundary FROM {table} WHERE scan=? AND node=?"),params![scan,node],|r|Ok(Totals {logical:r.get(0)?,allocated:r.get(1)?,unknown_logical:r.get(2)?,unknown_allocated:r.get(3)?,errors:r.get(4)?,estimated:r.get(5)?,unknown_entries:r.get(6)?,excluded:r.get(7)?,boundary:r.get(8)?})).optional()?;
    if let Some(t) = result
        && [
            t.logical,
            t.allocated,
            t.unknown_logical,
            t.unknown_allocated,
            t.errors,
            t.estimated,
            t.unknown_entries,
            t.excluded,
            t.boundary,
        ]
        .iter()
        .any(|n| *n < 0)
    {
        return Err(StoreError::Corrupt("negative totals".into()));
    }
    Ok(result)
}
fn put_totals(c: &Connection, scan: i64, node: i64, t: Totals) -> Result<()> {
    c.execute("INSERT INTO totals VALUES(?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(scan,node) DO UPDATE SET logical=excluded.logical,allocated=excluded.allocated,unknown_logical=excluded.unknown_logical,unknown_allocated=excluded.unknown_allocated,errors=excluded.errors,estimated=excluded.estimated,unknown_entries=excluded.unknown_entries,excluded=excluded.excluded,boundary=excluded.boundary",params![scan,node,t.logical,t.allocated,t.unknown_logical,t.unknown_allocated,t.errors,t.estimated,t.unknown_entries,t.excluded,t.boundary])?;
    Ok(())
}

#[cfg(test)]
fn crash_pause(phase: &str) {
    if std::env::var("SPACE_STORE_CRASH_PHASE").ok().as_deref() == Some(phase) {
        let path = std::env::var_os("SPACE_STORE_CRASH_DIRECTORY").unwrap();
        std::fs::write(Path::new(&path).join("ready"), b"ready").unwrap();
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod query_tests;
