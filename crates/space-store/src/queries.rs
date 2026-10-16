use super::*;

impl Cache {
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
        let (raw,canonical)=self.conn.query_row("SELECT data,node,parent,native,kind,ordinal,logical,allocated,identity,canonical FROM entries WHERE scan=? AND node=?",params![scan.get(),node.get()],|r|Ok((read_record(r)?,r.get::<_,bool>(9)?)))?;
        let ordinal = raw.ordinal;
        let entry = verify_record(raw)?;
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

#[derive(Clone, Debug)]
pub struct AliasPage {
    pub entries: Vec<EntryRecord>,
    pub next: Option<PageCursor>,
}
#[derive(Clone, Debug)]
pub struct ChildrenSummary {
    pub page: Page,
    pub total_count: u64,
    pub total_bytes: u64,
    pub unknown_count: u64,
    pub zero_count: u64,
    pub other_bytes: u64,
    pub other_count: u64,
}
impl Cache {
    pub fn children_sorted(
        &self,
        scan: ScanId,
        parent: NodeId,
        metric: Metric,
        after: Option<PageCursor>,
        limit: usize,
    ) -> Result<Page> {
        self.ensure_published(scan)?;
        check_limit(limit)?;
        let column = match metric {
            Metric::Logical => "logical",
            Metric::Allocated => "allocated",
        };
        let cursor = after.unwrap_or(PageCursor {
            ordinal: 0,
            value: i64::MAX,
        });
        let sql = format!(
            "SELECT node,ordinal,{column} FROM child_sizes WHERE scan=? AND parent=? AND ({column}<? OR ({column}=? AND ordinal>?)) ORDER BY {column} DESC,ordinal LIMIT ?"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(
                params![
                    scan.get(),
                    parent.get(),
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
    pub fn largest_in(
        &self,
        scan: ScanId,
        subtree: Option<NodeId>,
        metric: Metric,
        after: Option<PageCursor>,
        limit: usize,
    ) -> Result<Page> {
        self.ensure_published(scan)?;
        check_limit(limit)?;
        let (low, high) = match subtree {
            Some(node) => self.subtree_range(scan, node)?,
            None => (1, i64::MAX),
        };
        let column = match metric {
            Metric::Logical => "charged_logical",
            Metric::Allocated => "charged_allocated",
        };
        let cursor = after.unwrap_or(PageCursor {
            ordinal: 0,
            value: i64::MAX,
        });
        let sql = format!(
            "SELECT node,ordinal,{column} FROM entries WHERE scan=? AND kind=1 AND canonical=1 AND ordinal BETWEEN ? AND ? AND {column} IS NOT NULL AND ({column}<? OR ({column}=? AND ordinal>?)) ORDER BY {column} DESC,ordinal,node LIMIT ?"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(
                params![
                    scan.get(),
                    low,
                    high,
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
    fn subtree_range(&self, scan: ScanId, node: NodeId) -> Result<(i64, i64)> {
        let first = self.entry(scan, node)?;
        let low = first.ordinal;
        let mut current = first.entry;
        while let Some(parent) = current.parent {
            let next:Option<i64>=self.conn.query_row("SELECT ordinal FROM main.entries WHERE scan=? AND parent=? AND ordinal>? ORDER BY ordinal LIMIT 1",params![scan.get(),parent.get(),low],|r|r.get(0)).optional()?;
            if let Some(next) = next {
                return Ok((low, next - 1));
            }
            current = self.entry(scan, parent)?.entry;
        }
        Ok((low, i64::MAX))
    }
    pub fn aliases(
        &self,
        scan: ScanId,
        node: NodeId,
        after: Option<PageCursor>,
        limit: usize,
    ) -> Result<AliasPage> {
        self.ensure_published(scan)?;
        check_limit(limit)?;
        let identity: Option<Vec<u8>> = self.conn.query_row(
            "SELECT identity FROM main.entries WHERE scan=? AND node=?",
            params![scan.get(), node.get()],
            |r| r.get(0),
        )?;
        let mut stmt=self.conn.prepare("SELECT data,node,parent,native,kind,ordinal,logical,allocated,identity FROM main.entries WHERE scan=? AND ((? IS NOT NULL AND identity=?) OR (? IS NULL AND node=?)) AND ordinal>? ORDER BY ordinal LIMIT ?")?;
        let rows = stmt
            .query_map(
                params![
                    scan.get(),
                    identity,
                    identity,
                    identity,
                    node.get(),
                    after.map_or(0, |a| a.ordinal),
                    limit as i64 + 1
                ],
                read_record,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let more = rows.len() > limit;
        let mut entries = Vec::new();
        let mut last = 0;
        for raw in rows.into_iter().take(limit) {
            last = raw.ordinal;
            entries.push(verify_record(raw)?);
        }
        Ok(AliasPage {
            entries,
            next: more.then_some(PageCursor {
                ordinal: last,
                value: 0,
            }),
        })
    }
    pub fn children_summary(
        &self,
        scan: ScanId,
        parent: NodeId,
        metric: Metric,
        limit: usize,
    ) -> Result<ChildrenSummary> {
        let page = self.children_sorted(scan, parent, metric, None, limit)?;
        let column = match metric {
            Metric::Logical => "logical",
            Metric::Allocated => "allocated",
        };
        let parent_entry = self.entry(scan, parent)?;
        if parent_entry.entry.kind != EntryKind::Directory {
            return Err(StoreError::Invalid("child scope must be a directory"));
        }
        let total = match metric {
            Metric::Logical => parent_entry.logical,
            Metric::Allocated => parent_entry.allocated,
        };
        let count: i64 = self.conn.query_row(
            "SELECT count(*) FROM child_sizes WHERE scan=? AND parent=?",
            params![scan.get(), parent.get()],
            |r| r.get(0),
        )?;
        let unknown = match metric {
            Metric::Logical => "unknown_logical",
            Metric::Allocated => "unknown_allocated",
        };
        let zero: i64 = self.conn.query_row(
            &format!("SELECT count(*) FROM child_sizes c JOIN totals t ON t.scan=c.scan AND t.node=c.node WHERE c.scan=? AND c.parent=? AND c.{column}=0 AND t.{unknown}=0 AND t.unknown_entries=0 AND t.errors=0 AND t.excluded=0 AND t.boundary=0"),
            params![scan.get(), parent.get()],
            |r| r.get(0),
        )?;
        let bytes = number(total.observed_bytes)?;
        let unknown = number(total.unknown_count)?;
        let shown = page.entries.iter().try_fold(0u64, |sum, e| {
            sum.checked_add(match metric {
                Metric::Logical => e.logical.observed_bytes,
                Metric::Allocated => e.allocated.observed_bytes,
            })
            .ok_or(StoreError::Overflow)
        })?;
        let count = u64::try_from(count).map_err(|_| StoreError::Overflow)?;
        let bytes = u64::try_from(bytes).map_err(|_| StoreError::Overflow)?;
        Ok(ChildrenSummary {
            other_bytes: bytes.checked_sub(shown).ok_or(StoreError::Overflow)?,
            other_count: count - page.entries.len() as u64,
            page,
            total_count: count,
            total_bytes: bytes,
            unknown_count: u64::try_from(unknown).map_err(|_| StoreError::Overflow)?,
            zero_count: u64::try_from(zero).map_err(|_| StoreError::Overflow)?,
        })
    }
}
impl CacheReader {
    pub fn children_sorted(
        &self,
        scan: ScanId,
        parent: NodeId,
        metric: Metric,
        after: Option<PageCursor>,
        limit: usize,
    ) -> Result<Page> {
        self.cache
            .children_sorted(scan, parent, metric, after, limit)
    }
    pub fn largest_in(
        &self,
        scan: ScanId,
        subtree: Option<NodeId>,
        metric: Metric,
        after: Option<PageCursor>,
        limit: usize,
    ) -> Result<Page> {
        self.cache.largest_in(scan, subtree, metric, after, limit)
    }
    pub fn aliases(
        &self,
        scan: ScanId,
        node: NodeId,
        after: Option<PageCursor>,
        limit: usize,
    ) -> Result<AliasPage> {
        self.cache.aliases(scan, node, after, limit)
    }
    pub fn children_summary(
        &self,
        scan: ScanId,
        parent: NodeId,
        metric: Metric,
        limit: usize,
    ) -> Result<ChildrenSummary> {
        self.cache.children_summary(scan, parent, metric, limit)
    }
}

// Verify duplicated lookup keys at every public read boundary before a decoded
// record can drive hierarchy, filtering, alias grouping or native actions.
pub(super) struct RawRecord {
    data: Vec<u8>,
    node: i64,
    parent: Option<i64>,
    native: Vec<u8>,
    kind: i64,
    pub ordinal: i64,
    logical: Option<i64>,
    allocated: Option<i64>,
    identity: Option<Vec<u8>>,
}
pub(super) fn read_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawRecord> {
    Ok(RawRecord {
        data: row.get(0)?,
        node: row.get(1)?,
        parent: row.get(2)?,
        native: row.get(3)?,
        kind: row.get(4)?,
        ordinal: row.get(5)?,
        logical: row.get(6)?,
        allocated: row.get(7)?,
        identity: row.get(8)?,
    })
}
pub(super) fn verify_record(raw: RawRecord) -> Result<EntryRecord> {
    let e: EntryRecord = decode(raw.data)?;
    e.validate()?;
    let identity = e
        .identity
        .as_ref()
        .filter(|i| i.is_deduplicable())
        .map(encode)
        .transpose()?;
    if e.node.get() != raw.node
        || e.parent.map(NodeId::get) != raw.parent
        || e.name
            .as_ref()
            .map(NativeName::sort_key)
            .unwrap_or_default()
            != raw.native
        || kind(e.kind) != raw.kind
        || raw.ordinal <= 0
        || e.logical_bytes.map(number).transpose()? != raw.logical
        || e.allocated_bytes.map(number).transpose()? != raw.allocated
        || identity != raw.identity
    {
        return Err(StoreError::Corrupt("entry key/payload mismatch".into()));
    }
    Ok(e)
}
