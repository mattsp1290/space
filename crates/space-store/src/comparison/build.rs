use super::*;

impl ComparisonView {
    pub(super) fn check(cancel: &Cancellation) -> Result<()> {
        if cancel.is_cancelled() {
            Err(StoreError::Invalid("comparison cancelled"))
        } else {
            Ok(())
        }
    }
    pub(super) fn enumeration(
        &self,
        scan: ScanId,
        e: Option<&StoredEntry>,
        missing: Presence,
    ) -> Result<Presence> {
        let Some(e) = e else { return Ok(missing) };
        if e.entry.kind != EntryKind::Directory {
            return Ok(if e.entry.metadata_state == MetadataState::Complete {
                Presence::ProvenAbsent
            } else {
                Presence::Unobserved
            });
        }
        let data: Option<Vec<u8>> = self
            .conn()
            .query_row(
                "SELECT data FROM main.coverage WHERE scan=? AND node=?",
                params![scan.get(), e.entry.node.get()],
                |r| r.get(0),
            )
            .optional()?;
        let state: EnumerationState =
            decode(data.ok_or_else(|| StoreError::Corrupt("missing directory coverage".into()))?)?;
        let issues: bool = self.conn().query_row(
            "SELECT EXISTS(SELECT 1 FROM blockers WHERE scan=? AND parent=? AND native IS NULL)",
            params![scan.get(), e.entry.node.get()],
            |r| r.get(0),
        )?;
        Ok(
            if state == EnumerationState::Complete
                && !issues
                && e.entry.metadata_state == MetadataState::Complete
            {
                Presence::ProvenAbsent
            } else {
                Presence::Unobserved
            },
        )
    }
    pub(super) fn build_blockers(
        &self,
        scan: ScanId,
        root: i64,
        cancel: &Cancellation,
    ) -> Result<()> {
        let mut stmt = self
            .conn()
            .prepare("SELECT node,data FROM main.errors WHERE scan=?")?;
        let mut rows = stmt.query([scan.get()])?;
        while let Some(row) = rows.next()? {
            Self::check(cancel)?;
            let key: Option<i64> = row.get(0)?;
            let issue: ScanIssue = decode(row.get(1)?)?;
            if key != issue.node.or(issue.parent).map(NodeId::get)
                || issue.detail.len() > ScanIssue::MAX_DETAIL_BYTES
            {
                return Err(StoreError::Corrupt("comparison issue scope".into()));
            }
            if let Some(name) = &issue.name {
                name.validate()
                    .map_err(|_| StoreError::Corrupt("comparison issue name".into()))?;
                if name.encoding() != self.reader.scan_path(scan)?.encoding {
                    return Err(StoreError::Corrupt("comparison issue encoding".into()));
                }
            }
            for node in [issue.node, issue.parent].into_iter().flatten() {
                self.reader.entry(scan, node)?;
            }
            let parent = if issue.name.is_some() {
                issue.parent.map(NodeId::get).unwrap_or(root)
            } else {
                issue.node.or(issue.parent).map(NodeId::get).unwrap_or(root)
            };
            self.conn().execute(
                "INSERT INTO blockers VALUES(?,?,?)",
                params![
                    scan.get(),
                    parent,
                    issue.name.as_ref().map(NativeName::sort_key)
                ],
            )?;
        }
        Ok(())
    }
    pub(super) fn has_blockers(&self, scan: ScanId, node: Option<i64>) -> Result<bool> {
        Ok(self.conn().query_row(
            "SELECT EXISTS(SELECT 1 FROM blockers WHERE scan=? AND parent=?)",
            params![scan.get(), node],
            |r| r.get(0),
        )?)
    }
    pub(super) fn absence(
        &self,
        scan: ScanId,
        parent: Option<i64>,
        native: &[u8],
        fallback: Presence,
    ) -> Result<Presence> {
        if fallback != Presence::ProvenAbsent {
            return Ok(fallback);
        }
        let blocked:bool=self.conn().query_row("SELECT EXISTS(SELECT 1 FROM blockers WHERE scan=? AND parent=? AND (native=? OR native IS NULL))",params![scan.get(),parent,native],|r|r.get(0))?;
        Ok(if blocked {
            Presence::Unobserved
        } else {
            fallback
        })
    }
    pub(super) fn build(
        &self,
        filter: &QueryFilter,
        cancel: &Cancellation,
        progress: &ComparisonProgress,
    ) -> Result<()> {
        Self::check(cancel)?;
        let c = self.conn();
        let token = cancel.clone();
        c.progress_handler(1000, Some(move || token.is_cancelled()))?;
        c.execute_batch("PRAGMA query_only=OFF; PRAGMA temp_store=FILE; BEGIN;
   CREATE TEMP TABLE pairs(id INTEGER PRIMARY KEY,neg_id INTEGER GENERATED ALWAYS AS (-id) STORED,parent INTEGER,before_node INTEGER UNIQUE,after_node INTEGER UNIQUE,bp INTEGER NOT NULL,ap INTEGER NOT NULL,depth INTEGER NOT NULL,display TEXT NOT NULL,witness BLOB,directory INTEGER NOT NULL,summary BLOB NOT NULL,change BLOB NOT NULL,visible INTEGER NOT NULL DEFAULT 0,selected INTEGER NOT NULL DEFAULT 0,redistributed INTEGER NOT NULL DEFAULT 0,lo INTEGER NOT NULL DEFAULT 0,hi INTEGER NOT NULL DEFAULT 0,linc INTEGER NOT NULL DEFAULT 0,ldec INTEGER NOT NULL DEFAULT 0,lrank INTEGER NOT NULL DEFAULT 0,ainc INTEGER NOT NULL DEFAULT 0,adec INTEGER NOT NULL DEFAULT 0,arank INTEGER NOT NULL DEFAULT 0);
   CREATE INDEX pair_parent ON pairs(parent,id); CREATE INDEX pair_witness ON pairs(witness);
   CREATE TEMP TABLE groups(identity BLOB PRIMARY KEY,before_node INTEGER NOT NULL,after_node INTEGER NOT NULL); CREATE TEMP TABLE blockers(scan INTEGER NOT NULL,parent INTEGER NOT NULL,native BLOB); CREATE INDEX blocked_scope ON blockers(scan,parent,native);")?;
        // Without the identity index SQLite may prefer the three-column
        // largest-file prefix and scan every canonical owner for every group.
        progress.set(2, 0);
        c.execute(
            OWNER_GROUPS_SQL,
            params![self.before.get(), self.after.get()],
        )?;
        let b = self.reader.root_entry(self.before)?;
        let a = self.reader.root_entry(self.after)?;
        self.build_blockers(self.before, b.entry.node.get(), cancel)?;
        self.build_blockers(self.after, a.entry.node.get(), cancel)?;
        self.insert(
            None,
            Some(b.entry.node.get()),
            Some(a.entry.node.get()),
            Presence::Present,
            Presence::Present,
            0,
        )?;
        let count: i64 = c.query_row(
            "SELECT count(*) FROM main.entries WHERE scan IN (?1,?2)",
            params![self.before.get(), self.after.get()],
            |r| r.get(0),
        )?;
        progress.set(1, count as u64);
        let mut id = 1;
        loop {
            Self::check(cancel)?;
            if !c.query_row("SELECT EXISTS(SELECT 1 FROM pairs WHERE id=?)", [id], |r| {
                r.get::<_, bool>(0)
            })? {
                break;
            }
            let p = self.pair(id)?;
            let b = self.endpoint(self.before, p.before)?;
            let a = self.endpoint(self.after, p.after)?;
            let display = a
                .as_ref()
                .or(b.as_ref())
                .ok_or_else(|| StoreError::Corrupt("empty comparison pair".into()))?
                .entry
                .display_name
                .clone();
            let witness = b
                .as_ref()
                .zip(a.as_ref())
                .and_then(|(b, a)| {
                    if b.entry.kind == EntryKind::RegularFile
                        && a.entry.kind == EntryKind::RegularFile
                        && b.entry.metadata_state == MetadataState::Complete
                        && a.entry.metadata_state == MetadataState::Complete
                        && b.entry.identity == a.entry.identity
                    {
                        b.entry.identity.as_ref().filter(|i| i.is_deduplicable())
                    } else {
                        None
                    }
                })
                .map(encode)
                .transpose()?;
            c.execute(
                "UPDATE pairs SET display=?,witness=? WHERE id=?",
                params![display, witness, id],
            )?;
            let directory = b
                .as_ref()
                .is_some_and(|e| e.entry.kind == EntryKind::Directory)
                || a.as_ref()
                    .is_some_and(|e| e.entry.kind == EntryKind::Directory);
            let bscope = self.enumeration(self.before, b.as_ref(), p.bp)?;
            let ascope = self.enumeration(self.after, a.as_ref(), p.ap)?;
            let path = if filter.path.is_some() {
                self.relative_path(&p)?
            } else {
                String::new()
            };
            let selected = filter.is_empty()
                || [b.as_ref(), a.as_ref()].into_iter().flatten().any(|e| {
                    e.entry.kind != EntryKind::Directory
                        && filter.matches(&e.entry, &path, self.metric)
                });
            let change = classify_change(
                p.bp,
                p.ap,
                b.as_ref()
                    .zip(a.as_ref())
                    .is_none_or(|(b, a)| b.entry.kind == a.entry.kind),
                changed(b.as_ref(), a.as_ref()),
            );
            let mut summary = Summary::zero();
            if selected {
                summary.matching = 1;
                summary.changed = u64::from(change != ChangeKind::Unchanged);
            }
            // Missing scopes cannot be evaluated by a name/size filter. Preserve their
            // uncertainty even when there are no selected observed descendants.
            let unknown_b = b
                .as_ref()
                .is_some_and(|e| e.entry.kind == EntryKind::Directory)
                && (bscope == Presence::Unobserved || self.has_blockers(self.before, p.before)?);
            let unknown_a = a
                .as_ref()
                .is_some_and(|e| e.entry.kind == EntryKind::Directory)
                && (ascope == Presence::Unobserved || self.has_blockers(self.after, p.after)?);
            if unknown_b || unknown_a {
                let unknown = MetricChange::from_observations(
                    if unknown_b { None } else { Some(0) },
                    if unknown_a { None } else { Some(0) },
                    AccountingState::Unknown,
                )?;
                summary.logical = summary.logical.checked_add(unknown)?;
                summary.allocated = summary.allocated.checked_add(unknown)?;
            }
            c.execute(
                "UPDATE pairs SET summary=?,change=?,directory=?,visible=?,selected=? WHERE id=?",
                params![
                    encode(&summary)?,
                    encode(&change)?,
                    directory,
                    selected || unknown_b || unknown_a || id == 1,
                    selected,
                    id
                ],
            )?;
            if directory {
                let bn = b
                    .as_ref()
                    .filter(|e| e.entry.kind == EntryKind::Directory)
                    .map(|e| e.entry.node.get());
                let an = a
                    .as_ref()
                    .filter(|e| e.entry.kind == EntryKind::Directory)
                    .map(|e| e.entry.node.get());
                let mut stmt=c.prepare("SELECT b.node,a.node,b.native FROM main.entries b LEFT JOIN main.entries a ON a.scan=?2 AND a.parent=?4 AND a.native=b.native WHERE b.scan=?1 AND b.parent=?3 UNION ALL SELECT NULL,a.node,a.native FROM main.entries a WHERE a.scan=?2 AND a.parent=?4 AND NOT EXISTS(SELECT 1 FROM main.entries b WHERE b.scan=?1 AND b.parent=?3 AND b.native=a.native) ORDER BY 3")?;
                let mut rows = stmt.query(params![self.before.get(), self.after.get(), bn, an])?;
                while let Some(row) = rows.next()? {
                    Self::check(cancel)?;
                    let bn: Option<i64> = row.get(0)?;
                    let an: Option<i64> = row.get(1)?;
                    let native: Vec<u8> = row.get(2)?;
                    self.insert(
                        Some(id),
                        bn,
                        an,
                        if bn.is_some() {
                            Presence::Present
                        } else {
                            self.absence(self.before, p.before, &native, bscope)?
                        },
                        if an.is_some() {
                            Presence::Present
                        } else {
                            self.absence(self.after, p.after, &native, ascope)?
                        },
                        p.depth + 1,
                    )?;
                }
            }
            if p.before.is_some() {
                progress.advance();
            }
            if p.after.is_some() {
                progress.advance();
            }
            id += 1;
        }
        let seen: i64 = c.query_row(
            "SELECT count(before_node)+count(after_node) FROM pairs",
            [],
            |r| r.get(0),
        )?;
        if seen != count {
            return Err(StoreError::Corrupt("unreachable comparison entries".into()));
        }
        // Identity equality alone does not establish continuity. Require a raw
        // path member with the same identity in both observations before
        // reconciling canonical-owner movement; pure renames retain gross churn.
        c.execute("DELETE FROM groups WHERE NOT EXISTS(SELECT 1 FROM pairs p WHERE p.witness=groups.identity)",[])?;
        {
            let mut stmt = c.prepare("SELECT id FROM pairs WHERE selected=1 ORDER BY id")?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                Self::check(cancel)?;
                let id: i64 = row.get(0)?;
                let p = self.pair(id)?;
                let b = self.endpoint(self.before, p.before)?;
                let a = self.endpoint(self.after, p.after)?;
                let own = Summary {
                    logical: self.own_change(
                        b.as_ref(),
                        p.bp,
                        a.as_ref(),
                        p.ap,
                        Metric::Logical,
                    )?,
                    allocated: self.own_change(
                        b.as_ref(),
                        p.bp,
                        a.as_ref(),
                        p.ap,
                        Metric::Allocated,
                    )?,
                    matching: 0,
                    changed: 0,
                };
                self.put_summary(id, self.summary(id)?.add(own)?)?;
            }
        }
        self.account_groups(cancel, progress)?;
        self.aggregate(cancel, progress)?;
        self.index(cancel, progress)?;
        c.execute_batch("COMMIT")?;
        Ok(())
    }
    pub(super) fn insert(
        &self,
        parent: Option<i64>,
        before: Option<i64>,
        after: Option<i64>,
        bp: Presence,
        ap: Presence,
        depth: i64,
    ) -> Result<()> {
        if depth > MAX_DEPTH {
            return Err(StoreError::Invalid("comparison depth bound"));
        }
        let display: String = if let Some(n) = after {
            self.conn().query_row(
                "SELECT display FROM main.entries WHERE scan=? AND node=?",
                params![self.after.get(), n],
                |r| r.get(0),
            )?
        } else {
            self.conn().query_row(
                "SELECT display FROM main.entries WHERE scan=? AND node=?",
                params![self.before.get(), before],
                |r| r.get(0),
            )?
        };
        self.conn().execute("INSERT INTO pairs(parent,before_node,after_node,bp,ap,depth,display,directory,summary,change) VALUES(?,?,?,?,?,?,?,0,?,?)",params![parent,before,after,presence_code(bp),presence_code(ap),depth,display,encode(&Summary::zero())?,encode(&ChangeKind::Unknown)?])?;
        Ok(())
    }
    pub(super) fn relative_path(&self, p: &Pair) -> Result<String> {
        let mut parts = Vec::new();
        let mut bytes = 0usize;
        let mut current = Some(p.id);
        while let Some(id) = current {
            let (parent, display): (Option<i64>, String) = self.conn().query_row(
                "SELECT parent,display FROM pairs WHERE id=?",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            if parent.is_some() {
                bytes = bytes
                    .checked_add(display.len() + 1)
                    .ok_or(StoreError::Overflow)?;
                if bytes > MAX_BATCH_BYTES {
                    return Err(StoreError::Invalid("comparison display path byte bound"));
                }
                parts.push(display)
            }
            current = parent;
            if parts.len() > MAX_DEPTH as usize {
                return Err(StoreError::Corrupt("comparison hierarchy cycle".into()));
            }
        }
        parts.reverse();
        Ok(parts.join("/"))
    }
    pub(super) fn index(&self, cancel: &Cancellation, progress: &ComparisonProgress) -> Result<()> {
        let c = self.conn();
        progress.set(4, 0);
        let mut ordinal = 1;
        let mut stack = vec![(1i64, 0i64)];
        c.execute("UPDATE pairs SET lo=1 WHERE id=1", [])?;
        while let Some((id, last)) = stack.last_mut() {
            Self::check(cancel)?;
            let child: Option<i64> = c
                .query_row(
                    "SELECT id FROM pairs WHERE parent=? AND id>? ORDER BY id LIMIT 1",
                    params![*id, *last],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(child) = child {
                *last = child;
                ordinal += 1;
                c.execute("UPDATE pairs SET lo=? WHERE id=?", params![ordinal, child])?;
                stack.push((child, 0));
            } else {
                c.execute("UPDATE pairs SET hi=? WHERE id=?", params![ordinal, *id])?;
                stack.pop();
            }
        }
        c.execute_batch("CREATE INDEX comparison_lchildren ON pairs(parent,visible,lrank DESC,neg_id DESC); CREATE INDEX comparison_achildren ON pairs(parent,visible,arank DESC,neg_id DESC); CREATE INDEX comparison_lincrease ON pairs(visible,linc DESC,neg_id DESC); CREATE INDEX comparison_ldecrease ON pairs(visible,ldec DESC,neg_id DESC); CREATE INDEX comparison_aincrease ON pairs(visible,ainc DESC,neg_id DESC); CREATE INDEX comparison_adecrease ON pairs(visible,adec DESC,neg_id DESC); CREATE INDEX comparison_subtree ON pairs(lo,hi);")?;
        Ok(())
    }
}
