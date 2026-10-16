use super::*;

impl ComparisonView {
    pub(super) fn shared(&self, e: &StoredEntry) -> Result<bool> {
        let Some(identity) = e.entry.identity.as_ref().filter(|i| i.is_deduplicable()) else {
            return Ok(false);
        };
        if e.entry.kind != EntryKind::RegularFile {
            return Ok(false);
        }
        Ok(self.conn().query_row(
            "SELECT EXISTS(SELECT 1 FROM groups WHERE identity=?)",
            [encode(identity)?],
            |r| r.get(0),
        )?)
    }
    pub(super) fn own_change(
        &self,
        b: Option<&StoredEntry>,
        bp: Presence,
        a: Option<&StoredEntry>,
        ap: Presence,
        metric: Metric,
    ) -> Result<MetricChange> {
        let side = |e: Option<&StoredEntry>, presence: Presence| -> Result<(Option<u64>, bool)> {
            let Some(e) = e else {
                return Ok((
                    if presence == Presence::ProvenAbsent {
                        Some(0)
                    } else {
                        None
                    },
                    false,
                ));
            };
            if e.entry.kind != EntryKind::RegularFile || self.shared(e)? || !e.canonical {
                return Ok((Some(0), false));
            }
            let bytes = match metric {
                Metric::Logical => e.entry.logical_bytes,
                Metric::Allocated => e.entry.allocated_bytes,
            };
            let estimated = e.entry.kind == EntryKind::RegularFile
                && !e
                    .entry
                    .identity
                    .as_ref()
                    .is_some_and(FileIdentity::is_deduplicable);
            Ok((bytes, estimated))
        };
        let (bv, be) = side(b, bp)?;
        let (av, ae) = side(a, ap)?;
        let uncertain = [b, a].into_iter().flatten().any(|e| {
            e.entry.metadata_state != MetadataState::Complete
                || (e.entry.kind != EntryKind::Directory && e.coverage.error_count > 0)
        });
        Ok(MetricChange::from_observations(
            bv,
            av,
            if uncertain {
                AccountingState::Unknown
            } else if be || ae {
                AccountingState::Estimated
            } else {
                AccountingState::Comparable
            },
        )?)
    }
    pub(super) fn account_groups(
        &self,
        cancel: &Cancellation,
        progress: &ComparisonProgress,
    ) -> Result<()> {
        let c = self.conn();
        let count: i64 = c.query_row("SELECT count(*) FROM groups", [], |r| r.get(0))?;
        progress.set(2, count as u64);
        let mut stmt=c.prepare("SELECT g.before_node,g.after_node,b.id,a.id,b.selected,a.selected FROM groups g JOIN pairs b ON b.before_node=g.before_node JOIN pairs a ON a.after_node=g.after_node")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            Self::check(cancel)?;
            let bn: i64 = row.get(0)?;
            let an: i64 = row.get(1)?;
            let bi: i64 = row.get(2)?;
            let ai: i64 = row.get(3)?;
            if !row.get::<_, bool>(4)? && !row.get::<_, bool>(5)? {
                progress.advance();
                continue;
            }
            let b = self.endpoint(self.before, Some(bn))?.unwrap();
            let a = self.endpoint(self.after, Some(an))?.unwrap();
            let lca = self.common_ancestor(bi, ai)?;
            if bi != ai {
                self.mark_redistributed(bi, lca)?;
                self.mark_redistributed(ai, lca)?;
            }
            let state = if b.entry.metadata_state != MetadataState::Complete
                || a.entry.metadata_state != MetadataState::Complete
                || b.coverage.error_count > 0
                || a.coverage.error_count > 0
            {
                AccountingState::Unknown
            } else {
                AccountingState::Comparable
            };
            let group = Summary {
                logical: MetricChange::from_observations(
                    b.entry.logical_bytes,
                    a.entry.logical_bytes,
                    state,
                )?,
                allocated: MetricChange::from_observations(
                    b.entry.allocated_bytes,
                    a.entry.allocated_bytes,
                    state,
                )?,
                matching: 0,
                changed: 0,
            };
            self.put_summary(lca, self.summary(lca)?.add(group)?)?;
            c.execute("UPDATE pairs SET visible=1 WHERE id=?", [lca])?;
            progress.advance();
        }
        Ok(())
    }
    pub(super) fn common_ancestor(&self, mut b: i64, mut a: i64) -> Result<i64> {
        let mut bd = self.pair(b)?.depth;
        let mut ad = self.pair(a)?.depth;
        while b != a {
            if bd >= ad {
                b = self
                    .pair(b)?
                    .parent
                    .ok_or_else(|| StoreError::Corrupt("comparison ancestor".into()))?;
                bd -= 1;
            } else {
                a = self
                    .pair(a)?
                    .parent
                    .ok_or_else(|| StoreError::Corrupt("comparison ancestor".into()))?;
                ad -= 1;
            }
        }
        Ok(b)
    }
    pub(super) fn mark_redistributed(&self, mut id: i64, ancestor: i64) -> Result<()> {
        while id != ancestor {
            self.conn()
                .execute("UPDATE pairs SET redistributed=1 WHERE id=?", [id])?;
            id = self
                .pair(id)?
                .parent
                .ok_or_else(|| StoreError::Corrupt("comparison ancestor".into()))?;
        }
        Ok(())
    }
    pub(super) fn aggregate(
        &self,
        cancel: &Cancellation,
        progress: &ComparisonProgress,
    ) -> Result<()> {
        let c = self.conn();
        let count: i64 = c.query_row("SELECT count(*) FROM pairs", [], |r| r.get(0))?;
        progress.set(3, count as u64);
        let mut stmt = c.prepare("SELECT id,parent,visible FROM pairs ORDER BY id DESC")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            Self::check(cancel)?;
            let id: i64 = row.get(0)?;
            let parent: Option<i64> = row.get(1)?;
            let visible: bool = row.get(2)?;
            let s = self.summary(id)?;
            if let Some(parent) = parent {
                self.put_summary(parent, self.summary(parent)?.add(s)?)?;
                if visible {
                    c.execute("UPDATE pairs SET visible=1 WHERE id=?", [parent])?;
                }
            }
            c.execute(
                "UPDATE pairs SET linc=?,ldec=?,lrank=?,ainc=?,adec=?,arank=? WHERE id=?",
                params![
                    number(s.logical.gross_increase)?,
                    number(s.logical.gross_decrease)?,
                    number(s.logical.gross_increase.max(s.logical.gross_decrease))?,
                    number(s.allocated.gross_increase)?,
                    number(s.allocated.gross_decrease)?,
                    number(s.allocated.gross_increase.max(s.allocated.gross_decrease))?,
                    id
                ],
            )?;
            progress.advance();
        }
        Ok(())
    }
}
