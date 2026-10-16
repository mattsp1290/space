//! Exclusive, local SQLite cache. Only published snapshots are queryable.
//! APIs are synchronous: call them on a bounded background writer/query worker.
mod schema;
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
    InUse,
    Schema(i64),
    Corrupt(String),
    Invalid(&'static str),
    Overflow,
}
impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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
        if version != 0 && version != schema::VERSION {
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
            conn.execute_batch(&format!("BEGIN IMMEDIATE; {} COMMIT;", schema::CREATE))?;
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
            tx.query_row("SELECT coalesce(max(id),0) FROM scans", [], |r| r.get(0))?;
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
            tx.execute(
                "INSERT INTO roots(id,identity,policy) VALUES(?,?,?)",
                params![spec.root_id.get(), encode(&root)?, encode(&spec.policy)?],
            )?;
        }
        tx.execute("INSERT INTO scans(id,root,generation,status,started,started_nanos,selected_path,metadata) VALUES(?,?,?,'scanning',?,?,?,?)",params![spec.key.id.get(),spec.root_id.get(),number(spec.key.generation)?,started.seconds,started.nanos,encode(path)?,encode(spec)?])?;
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
        if matches!(pins.get(&scan.get()), Some(PinState::Retiring)) {
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
        let id: i64 = self
            .conn
            .query_row("SELECT coalesce(max(id),0)+1 FROM scans", [], |r| r.get(0))?;
        ScanId::try_from(id).map_err(|_| StoreError::Overflow)
    }
    pub fn next_root_id(&self) -> Result<RootId> {
        let id: i64 = self
            .conn
            .query_row("SELECT coalesce(max(id),0)+1 FROM roots", [], |r| r.get(0))?;
        RootId::try_from(id).map_err(|_| StoreError::Overflow)
    }
    pub fn root_entry(&self, scan: ScanId) -> Result<StoredEntry> {
        self.ensure_published(scan)?;
        let node: i64 = self.conn.query_row(
            "SELECT node FROM entries WHERE scan=? AND parent IS NULL",
            [scan.get()],
            |r| r.get(0),
        )?;
        self.entry(
            scan,
            NodeId::try_from(node).map_err(|_| StoreError::Corrupt("node".into()))?,
        )
    }
    pub fn entry(&self, scan: ScanId, node: NodeId) -> Result<StoredEntry> {
        self.ensure_published(scan)?;
        let (data, canonical, ordinal, parent, native, k): (
            Vec<u8>,
            bool,
            i64,
            Option<i64>,
            Vec<u8>,
            i64,
        ) = self.conn.query_row(
            "SELECT data,canonical,ordinal,parent,native,kind FROM entries WHERE scan=? AND node=?",
            params![scan.get(), node.get()],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )?;
        let entry: EntryRecord = decode(data)?;
        entry.validate()?;
        if entry.node != node
            || entry.parent.map(NodeId::get) != parent
            || entry
                .name
                .as_ref()
                .map(NativeName::sort_key)
                .unwrap_or_default()
                != native
            || kind(entry.kind) != k
            || ordinal <= 0
        {
            return Err(StoreError::Corrupt("entry key/payload mismatch".into()));
        }
        let t = read_totals(&self.conn, scan.get(), node.get())?
            .ok_or(StoreError::Corrupt("missing totals".into()))?;
        Ok(StoredEntry {
            entry,
            canonical,
            ordinal,
            logical: MetricTotal {
                observed_bytes: t.logical as u64,
                unknown_count: t.unknown_logical as u64,
                estimated_count: t.estimated as u64,
            },
            allocated: MetricTotal {
                observed_bytes: t.allocated as u64,
                unknown_count: t.unknown_allocated as u64,
                estimated_count: t.estimated as u64,
            },
            coverage: CoverageSummary {
                unknown_entries: t.unknown_entries as u64,
                error_count: t.errors as u64,
                excluded_scopes: t.excluded as u64,
                boundary_scopes: t.boundary as u64,
            },
        })
    }
    /// Native-name ordered child pages. Cursor belongs to this scan and parent.
    pub fn children(
        &self,
        scan: ScanId,
        parent: NodeId,
        after: Option<PageCursor>,
        limit: usize,
    ) -> Result<Page> {
        self.ensure_published(scan)?;
        check_limit(limit)?;
        let mut stmt=self.conn.prepare("SELECT node,ordinal FROM entries WHERE scan=? AND parent=? AND ordinal>? ORDER BY ordinal LIMIT ?")?;
        let rows = stmt
            .query_map(
                params![
                    scan.get(),
                    parent.get(),
                    after.map_or(0, |a| a.ordinal),
                    limit as i64 + 1
                ],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        self.page(
            scan,
            rows.into_iter().map(|(n, o)| (n, o, 0)).collect(),
            limit,
        )
    }
    /// Known charged regular-file sizes descending, deterministic native path ties.
    /// Unknown metric sizes are excluded, never substituted with another metric.
    pub fn largest(
        &self,
        scan: ScanId,
        metric: Metric,
        after: Option<PageCursor>,
        limit: usize,
    ) -> Result<Page> {
        self.ensure_published(scan)?;
        check_limit(limit)?;
        let column = match metric {
            Metric::Logical => "charged_logical",
            Metric::Allocated => "charged_allocated",
        };
        let sql = format!(
            "SELECT node,ordinal,{column} FROM entries WHERE scan=? AND kind=1 AND canonical=1 AND {column} IS NOT NULL AND ({column}<? OR ({column}=? AND ordinal>?)) ORDER BY {column} DESC,ordinal,node LIMIT ?"
        );
        let cursor = after.unwrap_or(PageCursor {
            ordinal: 0,
            value: i64::MAX,
        });
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(
                params![
                    scan.get(),
                    cursor.value,
                    cursor.value,
                    cursor.ordinal,
                    limit as i64 + 1
                ],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        self.page(scan, rows, limit)
    }
    fn page(&self, scan: ScanId, mut rows: Vec<(i64, i64, i64)>, limit: usize) -> Result<Page> {
        let more = rows.len() > limit;
        rows.truncate(limit);
        let next = if more {
            rows.last().map(|r| PageCursor {
                ordinal: r.1,
                value: r.2,
            })
        } else {
            None
        };
        let entries = rows
            .into_iter()
            .map(|r| {
                self.entry(
                    scan,
                    NodeId::try_from(r.0).map_err(|_| StoreError::Corrupt("node".into()))?,
                )
            })
            .collect::<Result<_>>()?;
        Ok(Page { entries, next })
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
    let result=c.query_row("SELECT logical,allocated,unknown_logical,unknown_allocated,errors,estimated,unknown_entries,excluded,boundary FROM totals WHERE scan=? AND node=?",params![scan,node],|r|Ok(Totals {logical:r.get(0)?,allocated:r.get(1)?,unknown_logical:r.get(2)?,unknown_allocated:r.get(3)?,errors:r.get(4)?,estimated:r.get(5)?,unknown_entries:r.get(6)?,excluded:r.get(7)?,boundary:r.get(8)?})).optional()?;
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
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    fn dir() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "space-store-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
    fn identity() -> RootIdentity {
        RootIdentity {
            volume: ObservedIdentity {
                scheme: IdentityScheme::UnixDevice,
                value: vec![1],
                strength: IdentityStrength::Observed,
            },
            directory: ObservedIdentity {
                scheme: IdentityScheme::UnixInode,
                value: vec![2],
                strength: IdentityStrength::Observed,
            },
        }
    }
    fn spec(n: u64) -> ScanSpec {
        ScanSpec {
            key: ScanKey {
                id: ScanId::new(n).unwrap(),
                generation: n,
            },
            root_id: RootId::new(1).unwrap(),
            lineage_id: LineageId::new(1).unwrap(),
            expected_root: identity(),
            policy: ScanPolicy::default(),
        }
    }
    fn entry(
        node: u64,
        parent: Option<u64>,
        name: &[u8],
        size: Option<u64>,
        file: bool,
    ) -> ScanRecord {
        ScanRecord::Entry(EntryRecord {
            node: NodeId::new(node).unwrap(),
            parent: parent.map(|n| NodeId::new(n).unwrap()),
            name: parent.map(|_| {
                #[cfg(unix)]
                {
                    NativeName::Unix(name.to_vec())
                }
                #[cfg(windows)]
                {
                    NativeName::Windows(name.iter().map(|b| u16::from(*b)).collect())
                }
            }),
            display_name: String::from_utf8_lossy(name).into(),
            kind: if file {
                EntryKind::RegularFile
            } else {
                EntryKind::Directory
            },
            identity: None,
            logical_bytes: size,
            allocated_bytes: None,
            modified: None,
            link_state: LinkState::None,
            metadata_state: MetadataState::Complete,
        })
    }
    fn finished(node: u64) -> ScanRecord {
        ScanRecord::DirectoryFinished {
            node: NodeId::new(node).unwrap(),
            enumeration: EnumerationState::Complete,
        }
    }
    fn begin(c: &mut Cache, s: &ScanSpec) {
        c.begin(
            s,
            &NativePath::from_path(Path::new("/offline/source")),
            Timestamp {
                seconds: 1,
                nanos: 0,
            },
        )
        .unwrap();
    }
    fn ingest(c: &mut Cache, s: &ScanSpec, records: Vec<ScanRecord>) {
        c.ingest(&ScanBatch {
            key: s.key,
            sequence: 0,
            records,
        })
        .unwrap();
    }
    fn publish(c: &mut Cache, s: &ScanSpec) {
        c.publish(
            &ScanFinish {
                key: s.key,
                root_after: identity(),
                outcome: TraversalOutcome::Finished,
            },
            Timestamp {
                seconds: 2,
                nanos: 0,
            },
        )
        .unwrap();
    }
    fn basic(c: &mut Cache, n: u64) {
        let s = spec(n);
        begin(c, &s);
        ingest(
            c,
            &s,
            vec![
                entry(1, None, b"root", None, false),
                entry(2, Some(1), b"a", Some(7), true),
                finished(1),
            ],
        );
        publish(c, &s);
    }
    #[cfg(unix)]
    #[test]
    fn offline_reopen_raw_names_and_nullable_allocation() {
        let p = dir();
        {
            let mut c = Cache::open(&p).unwrap();
            let s = spec(1);
            begin(&mut c, &s);
            ingest(
                &mut c,
                &s,
                vec![
                    entry(1, None, b"root", None, false),
                    entry(2, Some(1), b"\xff", Some(9), true),
                    finished(1),
                ],
            );
            publish(&mut c, &s);
        }
        let c = Cache::open(&p).unwrap();
        let s = spec(1);
        let root = c.root_entry(s.key.id).unwrap();
        assert_eq!(root.logical.observed_bytes, 9);
        assert_eq!(root.allocated.unknown_count, 1);
        assert!(root.coverage.is_complete());
        let page = c
            .children(s.key.id, NodeId::new(1).unwrap(), None, 10)
            .unwrap();
        assert_eq!(
            page.entries[0].entry.name,
            Some(NativeName::Unix(vec![255]))
        );
        assert!(
            c.largest(s.key.id, Metric::Allocated, None, 10)
                .unwrap()
                .entries
                .is_empty()
        );
    }
    #[test]
    fn cancelled_late_batches_and_finish_preserve_publication() {
        let p = dir();
        let mut c = Cache::open(p).unwrap();
        basic(&mut c, 1);
        let s = spec(2);
        begin(&mut c, &s);
        c.cancel(s.key).unwrap();
        assert!(
            c.ingest(&ScanBatch {
                key: s.key,
                sequence: 0,
                records: vec![]
            })
            .is_err()
        );
        assert!(
            c.publish(
                &ScanFinish {
                    key: s.key,
                    root_after: identity(),
                    outcome: TraversalOutcome::Finished
                },
                Timestamp {
                    seconds: 3,
                    nanos: 0
                }
            )
            .is_err()
        );
        assert_eq!(c.latest(s.root_id).unwrap(), Some(spec(1).key.id));
    }
    #[test]
    fn exclusive_lock_and_pin_outlives_cache() {
        let p = dir();
        let mut c = Cache::open(&p).unwrap();
        basic(&mut c, 1);
        let pin = c.pin(spec(1).key.id).unwrap();
        assert!(matches!(Cache::open(&p), Err(StoreError::InUse)));
        drop(c);
        assert!(matches!(Cache::open(&p), Err(StoreError::InUse)));
        drop(pin);
        assert!(Cache::open(p).is_ok());
    }
    #[test]
    fn recovery_keeps_previous_and_interrupts_working() {
        let p = dir();
        {
            let mut c = Cache::open(&p).unwrap();
            basic(&mut c, 1);
            begin(&mut c, &spec(2));
        }
        let c = Cache::open(p).unwrap();
        assert_eq!(c.status(spec(2).key.id).unwrap(), ScanStatus::Interrupted);
        assert_eq!(c.latest(spec(1).root_id).unwrap(), Some(spec(1).key.id));
    }
    #[test]
    fn deterministic_hardlinks_across_siblings() {
        let p = dir();
        let mut c = Cache::open(p).unwrap();
        let s = spec(1);
        begin(&mut c, &s);
        let mut z = entry(3, Some(2), b"file", Some(10), true);
        let mut a = entry(5, Some(4), b"file", Some(10), true);
        for r in [&mut z, &mut a] {
            if let ScanRecord::Entry(e) = r {
                e.identity = Some(FileIdentity {
                    volume: identity().volume,
                    file: identity().directory,
                });
            }
        }
        ingest(
            &mut c,
            &s,
            vec![
                entry(1, None, b"root", None, false),
                entry(2, Some(1), b"z", None, false),
                z,
                entry(4, Some(1), b"a", None, false),
                a,
                finished(1),
                finished(2),
                finished(4),
            ],
        );
        publish(&mut c, &s);
        assert_eq!(c.root_entry(s.key.id).unwrap().logical.observed_bytes, 10);
        assert_eq!(
            c.entry(s.key.id, NodeId::new(2).unwrap())
                .unwrap()
                .logical
                .observed_bytes,
            0
        );
        assert_eq!(
            c.entry(s.key.id, NodeId::new(4).unwrap())
                .unwrap()
                .logical
                .observed_bytes,
            10
        );
        assert_eq!(
            c.largest(s.key.id, Metric::Logical, None, 10)
                .unwrap()
                .entries
                .len(),
            1
        );
    }
    #[test]
    fn checked_aggregate_overflow_never_publishes() {
        let p = dir();
        let mut c = Cache::open(p).unwrap();
        basic(&mut c, 1);
        let s = spec(2);
        begin(&mut c, &s);
        ingest(
            &mut c,
            &s,
            vec![
                entry(1, None, b"root", None, false),
                entry(2, Some(1), b"a", Some(i64::MAX as u64), true),
                entry(3, Some(1), b"b", Some(1), true),
                finished(1),
            ],
        );
        assert!(
            c.publish(
                &ScanFinish {
                    key: s.key,
                    root_after: identity(),
                    outcome: TraversalOutcome::Finished
                },
                Timestamp {
                    seconds: 3,
                    nanos: 0
                }
            )
            .is_err()
        );
        assert_eq!(c.latest(s.root_id).unwrap(), Some(spec(1).key.id));
        assert_eq!(c.status(s.key.id).unwrap(), ScanStatus::Failed);
    }
    #[test]
    fn retention_respects_lifetime_pin() {
        let p = dir();
        let mut c = Cache::open(p).unwrap();
        basic(&mut c, 1);
        let pin = c.pin(spec(1).key.id).unwrap();
        basic(&mut c, 2);
        basic(&mut c, 3);
        assert!(c.entry(spec(1).key.id, NodeId::new(1).unwrap()).is_ok());
        drop(pin);
        c.retain().unwrap();
        assert!(c.status(spec(1).key.id).is_err());
        assert_eq!(c.status(spec(2).key.id).unwrap(), ScanStatus::Published);
    }
    #[test]
    fn partial_scope_cannot_claim_complete() {
        let p = dir();
        let mut c = Cache::open(p).unwrap();
        let s = spec(1);
        begin(&mut c, &s);
        ingest(
            &mut c,
            &s,
            vec![
                entry(1, None, b"root", None, false),
                entry(2, Some(1), b"denied", None, false),
                finished(1),
                ScanRecord::Issue(ScanIssue {
                    node: Some(NodeId::new(2).unwrap()),
                    parent: None,
                    name: None,
                    class: IssueClass::PermissionDenied,
                    detail: "denied".into(),
                }),
            ],
        );
        publish(&mut c, &s);
        let t = c.root_entry(s.key.id).unwrap();
        assert!(!t.coverage.is_complete());
        assert_eq!(t.coverage.unknown_entries, 1);
        assert_eq!(t.coverage.error_count, 1);
    }
    #[test]
    fn generation_sequence_and_record_bound() {
        let p = dir();
        let mut c = Cache::open(p).unwrap();
        let s = spec(1);
        begin(&mut c, &s);
        assert!(
            c.ingest(&ScanBatch {
                key: s.key,
                sequence: 1,
                records: vec![]
            })
            .is_err()
        );
        assert!(
            c.ingest(&ScanBatch {
                key: ScanKey {
                    generation: 9,
                    ..s.key
                },
                sequence: 0,
                records: vec![]
            })
            .is_err()
        );
        assert!(
            c.ingest(&ScanBatch {
                key: s.key,
                sequence: 0,
                records: vec![finished(1); MAX_BATCH_RECORDS + 1]
            })
            .is_err()
        );
    }
    #[test]
    fn corrupt_and_newer_schema_never_wiped() {
        let p = dir();
        std::fs::write(p.join("cache.sqlite3"), b"not a database").unwrap();
        assert!(Cache::open(&p).is_err());
        assert_eq!(
            std::fs::read(p.join("cache.sqlite3")).unwrap(),
            b"not a database"
        );
        let p = dir();
        let c = Cache::open(&p).unwrap();
        c.conn.execute_batch("PRAGMA user_version=99").unwrap();
        drop(c);
        assert!(matches!(Cache::open(p), Err(StoreError::Schema(99))));
    }
    #[test]
    fn pages_have_no_duplicates_and_cancellation_blocks_publish() {
        let p = dir();
        let mut c = Cache::open(p).unwrap();
        let s = spec(1);
        begin(&mut c, &s);
        let mut records = vec![entry(1, None, b"root", None, false)];
        for n in 2..102 {
            records.push(entry(
                n,
                Some(1),
                format!("f{n:03}").as_bytes(),
                Some(1),
                true,
            ));
        }
        records.push(finished(1));
        ingest(&mut c, &s, records);
        publish(&mut c, &s);
        let mut cursor = None;
        let mut count = 0;
        loop {
            let p = c.largest(s.key.id, Metric::Logical, cursor, 7).unwrap();
            count += p.entries.len();
            cursor = p.next;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(count, 100);
        let s = spec(2);
        begin(&mut c, &s);
        let cancel = Cancellation::new();
        cancel.cancel();
        assert!(
            c.publish_cancellable(
                &ScanFinish {
                    key: s.key,
                    root_after: identity(),
                    outcome: TraversalOutcome::Finished
                },
                Timestamp {
                    seconds: 3,
                    nanos: 0
                },
                &cancel
            )
            .is_err()
        );
        assert_eq!(c.latest(s.root_id).unwrap(), Some(spec(1).key.id));
    }
    #[test]
    fn disk_full_rolls_back_batch_and_keeps_published() {
        let p = dir();
        let mut c = Cache::open(p).unwrap();
        basic(&mut c, 1);
        let s = spec(2);
        begin(&mut c, &s);
        c.conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .unwrap();
        let pages: i64 = c
            .conn
            .query_row("PRAGMA page_count", [], |r| r.get(0))
            .unwrap();
        c.conn
            .execute_batch(&format!("PRAGMA max_page_count={pages}"))
            .unwrap();
        let mut records = vec![entry(1, None, b"root", None, false)];
        for n in 2..502 {
            let mut record = entry(n, Some(1), format!("file{n}").as_bytes(), Some(1), true);
            if let ScanRecord::Entry(e) = &mut record {
                e.display_name = "large-display".repeat(100);
            }
            records.push(record);
        }
        let result = c.ingest(&ScanBatch {
            key: s.key,
            sequence: 0,
            records,
        });
        assert!(result.is_err(), "expected full database: {result:?}");
        assert_eq!(c.latest(s.root_id).unwrap(), Some(spec(1).key.id));
        let count: i64 = c
            .conn
            .query_row("SELECT count(*) FROM entries WHERE scan=2", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }
    #[test]
    fn malformed_payload_and_negative_totals_are_corruption() {
        let p = dir();
        let mut c = Cache::open(p).unwrap();
        basic(&mut c, 1);
        let mut e = c
            .entry(spec(1).key.id, NodeId::new(2).unwrap())
            .unwrap()
            .entry;
        e.node = NodeId::new(3).unwrap();
        c.conn
            .execute(
                "UPDATE entries SET data=? WHERE node=2",
                [encode(&e).unwrap()],
            )
            .unwrap();
        assert!(matches!(
            c.entry(spec(1).key.id, NodeId::new(2).unwrap()),
            Err(StoreError::Corrupt(_))
        ));
        c.conn
            .execute("UPDATE totals SET logical=-1 WHERE node=1", [])
            .unwrap();
        assert!(matches!(
            c.root_entry(spec(1).key.id),
            Err(StoreError::Corrupt(_))
        ));
    }
    // Spawn this exact test in a child process, then kill it while it owns an
    // unfinished scan. This exercises kernel lock release, not a stale PID file.
    #[test]
    fn process_crash_releases_lock_and_recovers() {
        const ENV: &str = "SPACE_STORE_CRASH_FIXTURE";
        if let Some(p) = std::env::var_os(ENV) {
            let mut c = Cache::open(&p).unwrap();
            begin(&mut c, &spec(2));
            std::fs::write(Path::new(&p).join("ready"), b"ready").unwrap();
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
        let p = dir();
        {
            let mut c = Cache::open(&p).unwrap();
            basic(&mut c, 1);
        }
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::process_crash_releases_lock_and_recovers",
                "--nocapture",
            ])
            .env(ENV, &p)
            .spawn()
            .unwrap();
        let start = std::time::Instant::now();
        while !p.join("ready").exists() {
            if start.elapsed() > std::time::Duration::from_secs(15) {
                child.kill().ok();
                panic!("child readiness timeout");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(matches!(Cache::open(&p), Err(StoreError::InUse)));
        child.kill().unwrap();
        child.wait().unwrap();
        let c = Cache::open(p).unwrap();
        assert_eq!(c.status(spec(2).key.id).unwrap(), ScanStatus::Interrupted);
        assert_eq!(c.latest(spec(1).root_id).unwrap(), Some(spec(1).key.id));
    }
    #[test]
    #[ignore = "million-row benchmark; run explicitly with --ignored --nocapture"]
    fn million_entry_fixture() {
        let p = dir();
        let mut c = Cache::open(&p).unwrap();
        let s = spec(1);
        begin(&mut c, &s);
        let start = std::time::Instant::now();
        let mut sequence = 0;
        c.ingest(&ScanBatch {
            key: s.key,
            sequence,
            records: vec![entry(1, None, b"root", None, false)],
        })
        .unwrap();
        sequence += 1;
        for chunk in 0..1000 {
            let records = (0..1000)
                .map(|n| {
                    let id = 2 + chunk * 1000 + n;
                    entry(
                        id,
                        Some(1),
                        format!("file{id:09}").as_bytes(),
                        Some(id),
                        true,
                    )
                })
                .collect();
            c.ingest(&ScanBatch {
                key: s.key,
                sequence,
                records,
            })
            .unwrap();
            sequence += 1;
        }
        c.ingest(&ScanBatch {
            key: s.key,
            sequence,
            records: vec![finished(1)],
        })
        .unwrap();
        let ingest_time = start.elapsed();
        let now = std::time::Instant::now();
        publish(&mut c, &s);
        let finalize = now.elapsed();
        drop(c);
        let now = std::time::Instant::now();
        let c = Cache::open(&p).unwrap();
        let page = c.largest(s.key.id, Metric::Logical, None, 100).unwrap();
        let reopen = now.elapsed();
        let mut samples = Vec::new();
        for _ in 0..20 {
            let now = std::time::Instant::now();
            let page = c.largest(s.key.id, Metric::Logical, None, 100).unwrap();
            assert_eq!(page.entries.len(), 100);
            samples.push(now.elapsed());
        }
        samples.sort();
        println!("top100_warm_p95={:?}", samples[18]);
        #[cfg(target_os = "linux")]
        {
            let status = std::fs::read_to_string("/proc/self/status").unwrap();
            for line in status
                .lines()
                .filter(|l| l.starts_with("VmHWM:") || l.starts_with("VmRSS:"))
            {
                println!("{line}");
            }
        }
        assert_eq!(page.entries.len(), 100);
        println!(
            "fixture entries=1000000 ingest={ingest_time:?} finalize={finalize:?} reopen_plus_top100={reopen:?} cache_bytes={} path={}",
            std::fs::metadata(p.join("cache.sqlite3")).unwrap().len(),
            p.display()
        );
    }
    #[test]
    fn same_object_new_path_keeps_historical_paths() {
        let p = dir();
        let mut c = Cache::open(p).unwrap();
        basic(&mut c, 1);
        let s = spec(2);
        let moved = NativePath::from_path(Path::new("/offline/moved"));
        c.begin(
            &s,
            &moved,
            Timestamp {
                seconds: 4,
                nanos: 123,
            },
        )
        .unwrap();
        ingest(
            &mut c,
            &s,
            vec![
                entry(1, None, b"root", None, false),
                entry(2, Some(1), b"a", Some(7), true),
                finished(1),
            ],
        );
        publish(&mut c, &s);
        assert_eq!(c.scan_path(s.key.id).unwrap(), moved);
        assert_eq!(
            c.native_path(s.key.id, NodeId::new(2).unwrap()).unwrap(),
            NativePath::from_path(&Path::new("/offline/moved").join("a"))
        );
        assert_eq!(
            c.native_path(spec(1).key.id, NodeId::new(2).unwrap())
                .unwrap(),
            NativePath::from_path(&Path::new("/offline/source").join("a"))
        );
        assert_eq!(c.interval(s.key.id).unwrap().0.nanos, 123);
    }
    #[test]
    fn exclusion_issue_is_scope_not_error() {
        let p = dir();
        let mut c = Cache::open(p).unwrap();
        let s = spec(1);
        begin(&mut c, &s);
        ingest(
            &mut c,
            &s,
            vec![
                entry(1, None, b"root", None, false),
                ScanRecord::Issue(ScanIssue {
                    node: None,
                    parent: Some(NodeId::new(1).unwrap()),
                    name: None,
                    class: IssueClass::Excluded,
                    detail: "excluded".into(),
                }),
                finished(1),
            ],
        );
        publish(&mut c, &s);
        let summary = c.root_entry(s.key.id).unwrap().coverage;
        assert_eq!(summary.excluded_scopes, 1);
        assert_eq!(summary.error_count, 0);
        assert!(!summary.is_complete());
    }
    #[test]
    fn orphan_issue_and_child_after_close_rejected() {
        let p = dir();
        let mut c = Cache::open(p).unwrap();
        let s = spec(1);
        begin(&mut c, &s);
        assert!(
            c.ingest(&ScanBatch {
                key: s.key,
                sequence: 0,
                records: vec![
                    entry(1, None, b"root", None, false),
                    ScanRecord::Issue(ScanIssue {
                        node: Some(NodeId::new(999).unwrap()),
                        parent: None,
                        name: None,
                        class: IssueClass::Io,
                        detail: "missing".into()
                    })
                ]
            })
            .is_err()
        );
        assert!(
            c.ingest(&ScanBatch {
                key: s.key,
                sequence: 0,
                records: vec![
                    entry(1, None, b"root", None, false),
                    finished(1),
                    entry(2, Some(1), b"late", Some(1), true)
                ]
            })
            .is_err()
        );
    }
    #[test]
    fn deep_tree_uses_linear_native_components() {
        let p = dir();
        let mut c = Cache::open(p).unwrap();
        let s = spec(1);
        begin(&mut c, &s);
        let mut records = vec![entry(1, None, b"root", None, false)];
        for node in 2..201 {
            records.push(entry(node, Some(node - 1), b"d", None, false));
        }
        records.push(entry(201, Some(200), b"file", Some(19), true));
        for node in (1..201).rev() {
            records.push(finished(node));
        }
        ingest(&mut c, &s, records);
        publish(&mut c, &s);
        assert_eq!(c.root_entry(s.key.id).unwrap().logical.observed_bytes, 19);
        assert!(c.native_path(s.key.id, NodeId::new(201).unwrap()).is_ok());
        let names: i64 = c
            .conn
            .query_row("SELECT sum(length(native)) FROM entries", [], |r| r.get(0))
            .unwrap();
        assert_eq!(names, if cfg!(windows) { 406 } else { 203 });
    }
    #[test]
    fn crash_at_finalizing_and_before_commit_is_atomic() {
        if let Some(p) = std::env::var_os("SPACE_STORE_CRASH_DIRECTORY") {
            let mut c = Cache::open(p).unwrap();
            basic(&mut c, 2);
            return;
        }
        for phase in ["finalizing", "before_commit"] {
            let p = dir();
            {
                let mut c = Cache::open(&p).unwrap();
                basic(&mut c, 1);
            }
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tests::crash_at_finalizing_and_before_commit_is_atomic",
                    "--nocapture",
                ])
                .env("SPACE_STORE_CRASH_DIRECTORY", &p)
                .env("SPACE_STORE_CRASH_PHASE", phase)
                .spawn()
                .unwrap();
            let start = std::time::Instant::now();
            while !p.join("ready").exists() {
                if start.elapsed() > std::time::Duration::from_secs(15) {
                    child.kill().ok();
                    panic!("child readiness timeout");
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            child.kill().unwrap();
            child.wait().unwrap();
            let c = Cache::open(p).unwrap();
            assert_eq!(c.status(spec(2).key.id).unwrap(), ScanStatus::Interrupted);
            assert_eq!(c.latest(spec(1).root_id).unwrap(), Some(spec(1).key.id));
            assert_eq!(
                c.root_entry(spec(1).key.id).unwrap().logical.observed_bytes,
                7
            );
        }
    }
    #[test]
    fn independent_reader_works_while_writer_transaction_open() {
        let p = dir();
        let mut c = Cache::open(p).unwrap();
        basic(&mut c, 1);
        let reader = c.reader().unwrap();
        let pin = reader.pin(spec(1).key.id).unwrap();
        begin(&mut c, &spec(2));
        let tx = c.conn.transaction().unwrap();
        tx.execute("UPDATE scans SET status='finalizing' WHERE id=2", [])
            .unwrap();
        assert_eq!(
            reader
                .root_entry(spec(1).key.id)
                .unwrap()
                .logical
                .observed_bytes,
            7
        );
        assert_eq!(
            reader.latest(spec(1).root_id).unwrap(),
            Some(spec(1).key.id)
        );
        drop(tx);
        drop(c);
        assert!(reader.root_entry(pin.scan_id()).is_ok());
    }
    #[test]
    fn pin_drop_never_waits_for_retention_registry() {
        let p = dir();
        let mut c = Cache::open(p).unwrap();
        basic(&mut c, 1);
        let pin = c.pin(spec(1).key.id).unwrap();
        let registry = c.pins.lock().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            drop(pin);
            sender.send(()).unwrap();
        });
        receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("pin drop blocked on registry");
        drop(registry);
        thread.join().unwrap();
        c.pins.lock().unwrap().insert(1, PinState::Retiring);
        assert!(c.pin(spec(1).key.id).is_err());
        c.pins.lock().unwrap().remove(&1);
        assert!(c.pin(spec(1).key.id).is_ok());
    }
    #[test]
    fn cancellation_during_finalization_is_terminal_cancelled() {
        let p = dir();
        let mut c = Cache::open(&p).unwrap();
        basic(&mut c, 1);
        let s = spec(2);
        begin(&mut c, &s);
        c.ingest(&ScanBatch {
            key: s.key,
            sequence: 0,
            records: vec![entry(1, None, b"root", None, false)],
        })
        .unwrap();
        for sequence in 1..=10 {
            let records = (0..1000)
                .map(|n| {
                    let id = 2 + (sequence - 1) * 1000 + n;
                    entry(id, Some(1), format!("f{id}").as_bytes(), Some(id), true)
                })
                .collect();
            c.ingest(&ScanBatch {
                key: s.key,
                sequence,
                records,
            })
            .unwrap();
        }
        c.ingest(&ScanBatch {
            key: s.key,
            sequence: 11,
            records: vec![finished(1)],
        })
        .unwrap();
        let cancel = Cancellation::new();
        let signal = cancel.clone();
        let thread = std::thread::spawn(move || {
            let conn = Connection::open_with_flags(
                p.join("cache.sqlite3"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap();
            let start = std::time::Instant::now();
            loop {
                let status: String = conn
                    .query_row("SELECT status FROM scans WHERE id=2", [], |r| r.get(0))
                    .unwrap();
                if status == "finalizing" {
                    signal.cancel();
                    break;
                }
                assert!(start.elapsed() < std::time::Duration::from_secs(10));
                std::thread::yield_now();
            }
        });
        assert!(
            c.publish_cancellable(
                &ScanFinish {
                    key: s.key,
                    root_after: identity(),
                    outcome: TraversalOutcome::Finished
                },
                Timestamp {
                    seconds: 3,
                    nanos: 0
                },
                &cancel
            )
            .is_err()
        );
        thread.join().unwrap();
        assert_eq!(c.status(s.key.id).unwrap(), ScanStatus::Cancelled);
        assert_eq!(c.latest(s.root_id).unwrap(), Some(spec(1).key.id));
    }
}
