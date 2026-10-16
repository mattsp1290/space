use super::*;

#[derive(Clone, Debug)]
pub struct ScanInfo {
    pub spec: ScanSpec,
    pub status: ScanStatus,
    pub started: Timestamp,
    pub ended: Option<Timestamp>,
    pub coverage: Option<CoverageSummary>,
}
#[derive(Clone, Debug)]
pub struct ScanPage {
    pub scans: Vec<ScanInfo>,
    pub next: Option<ScanId>,
}
#[derive(Clone, Debug)]
pub struct IssuePage {
    pub issues: Vec<ScanIssue>,
    pub next: Option<i64>,
}
#[derive(Clone, Copy, Debug)]
pub struct CacheFootprint {
    pub database_bytes: u64,
    pub unused_database_bytes: u64,
    pub wal_bytes: u64,
}
impl Cache {
    /// Cache metadata only. Call only after the user's remove-scan confirmation;
    /// this cannot remove a pinned or active scan and never touches source files.
    pub fn remove_scan(&mut self, scan: ScanId) -> Result<()> {
        if matches!(
            self.status(scan)?,
            ScanStatus::Scanning | ScanStatus::Finalizing
        ) {
            return Err(StoreError::Invalid("cancel active scan before removing it"));
        }
        self.mark_retiring(scan)?;
        let result: Result<()> = (|| {
            let tx = self.conn.transaction()?;
            let root: i64 =
                tx.query_row("SELECT root FROM scans WHERE id=?", [scan.get()], |r| {
                    r.get(0)
                })?;
            tx.execute("DELETE FROM publication WHERE scan=?", [scan.get()])?;
            tx.execute("DELETE FROM scans WHERE id=?", [scan.get()])?;
            tx.execute("INSERT OR IGNORE INTO publication(root,scan) SELECT ?,id FROM scans WHERE root=? AND status='published' ORDER BY id DESC LIMIT 1",params![root,root])?;
            tx.commit()?;
            Ok(())
        })();
        self.pins
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&scan.get());
        result
    }
    fn mark_retiring(&self, scan: ScanId) -> Result<()> {
        let mut pins = self.pins.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(pins.get(&scan.get()), Some(PinState::Retiring))
            || pins
                .get(&scan.get())
                .is_some_and(|p| matches!(p,PinState::Active(w) if w.strong_count()>0))
        {
            return Err(StoreError::Invalid("scan is pinned by an open view"));
        }
        pins.insert(scan.get(), PinState::Retiring);
        Ok(())
    }
    /// Removes all cached scan/root metadata after explicit user confirmation.
    /// This is not secure erasure. Rescanning is the only recovery. Active views
    /// and working scans must be closed first. The cache lock remains held.
    pub fn clear_cache(&mut self) -> Result<()> {
        let active: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM scans WHERE status IN ('scanning','finalizing'))",
            [],
            |r| r.get(0),
        )?;
        if active {
            return Err(StoreError::Invalid(
                "cancel active scan before clearing cache",
            ));
        }
        {
            let mut pins = self.pins.lock().unwrap_or_else(|e| e.into_inner());
            if pins.values().any(|p| {
                matches!(p,PinState::Active(w) if w.strong_count()>0)
                    | matches!(p, PinState::Retiring)
            }) {
                return Err(StoreError::Invalid(
                    "close pinned views before clearing cache",
                ));
            }
            pins.insert(0, PinState::Retiring);
        }
        let result: Result<()> = (|| {
            let tx = self.conn.transaction()?;
            tx.execute("DELETE FROM publication", [])?;
            tx.execute("DELETE FROM scans", [])?;
            tx.execute("DELETE FROM roots", [])?;
            tx.commit()?;
            Ok(())
        })();
        {
            let mut pins = self.pins.lock().unwrap_or_else(|e| e.into_inner());
            if result.is_ok() {
                pins.clear();
            } else {
                pins.remove(&0);
            }
        }
        result?;
        self.maintenance_warning = self
            .conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")
            .err()
            .map(|e| e.to_string());
        Ok(())
    }
    pub fn scans(&self, before: Option<ScanId>, limit: usize) -> Result<ScanPage> {
        check_limit(limit)?;
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM scans WHERE (? IS NULL OR id<?) ORDER BY id DESC LIMIT ?")?;
        let ids = stmt
            .query_map(
                params![
                    before.map(ScanId::get),
                    before.map(ScanId::get),
                    limit as i64 + 1
                ],
                |r| r.get::<_, i64>(0),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let more = ids.len() > limit;
        let mut scans = Vec::new();
        for id in ids.into_iter().take(limit) {
            let id = ScanId::try_from(id).map_err(|_| StoreError::Corrupt("scan id".into()))?;
            let spec = self.spec(id)?;
            let status = self.status(id)?;
            let (started, ended) = self.interval(id)?;
            let coverage = if status == ScanStatus::Published {
                Some(self.root_entry(id)?.coverage)
            } else {
                None
            };
            scans.push(ScanInfo {
                spec,
                status,
                started,
                ended,
                coverage,
            });
        }
        let next = if more {
            scans.last().map(|s| s.spec.key.id)
        } else {
            None
        };
        Ok(ScanPage { scans, next })
    }
    pub fn issues(&self, scan: ScanId, after: Option<i64>, limit: usize) -> Result<IssuePage> {
        check_limit(limit)?;
        let mut stmt = self.conn.prepare(
            "SELECT rowid,data FROM errors WHERE scan=? AND rowid>? ORDER BY rowid LIMIT ?",
        )?;
        let rows = stmt
            .query_map(
                params![scan.get(), after.unwrap_or(0), limit as i64 + 1],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let more = rows.len() > limit;
        let mut issues = Vec::new();
        let mut last = 0;
        for (id, data) in rows.into_iter().take(limit) {
            let issue: ScanIssue = decode(data)?;
            if issue.detail.len() > ScanIssue::MAX_DETAIL_BYTES {
                return Err(StoreError::Corrupt("unbounded cached issue".into()));
            }
            issues.push(issue);
            last = id;
        }
        Ok(IssuePage {
            issues,
            next: more.then_some(last),
        })
    }
    pub fn footprint(&self) -> Result<CacheFootprint> {
        let pages: i64 = self.conn.query_row("PRAGMA page_count", [], |r| r.get(0))?;
        let size: i64 = self.conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
        let free: i64 = self
            .conn
            .query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
        let pages = u64::try_from(pages).map_err(|_| StoreError::Overflow)?;
        let size = u64::try_from(size).map_err(|_| StoreError::Overflow)?;
        let free = u64::try_from(free).map_err(|_| StoreError::Overflow)?;
        let wal = self
            .conn
            .path()
            .map(|p| std::fs::metadata(format!("{p}-wal")))
            .transpose();
        let wal_bytes = match wal {
            Ok(Some(m)) => m.len(),
            Ok(None) => 0,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
            Err(e) => return Err(e.into()),
        };
        Ok(CacheFootprint {
            database_bytes: pages.checked_mul(size).ok_or(StoreError::Overflow)?,
            unused_database_bytes: free.checked_mul(size).ok_or(StoreError::Overflow)?,
            wal_bytes,
        })
    }
}
impl CacheReader {
    pub fn scans(&self, before: Option<ScanId>, limit: usize) -> Result<ScanPage> {
        self.cache.scans(before, limit)
    }
    pub fn issues(&self, scan: ScanId, after: Option<i64>, limit: usize) -> Result<IssuePage> {
        self.cache.issues(scan, after, limit)
    }
    pub fn footprint(&self) -> Result<CacheFootprint> {
        self.cache.footprint()
    }
}
