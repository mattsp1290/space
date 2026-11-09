use super::*;

pub(super) fn ingest(conn: &Connection, batch: &ScanBatch) -> Result<()> {
    let sid = batch.key.id.get();
    let mut deltas: BTreeMap<i64, Totals> = BTreeMap::new();
    let mut parents: BTreeMap<i64, Option<i64>> = BTreeMap::new();
    for record in &batch.records {
        let (node, delta) = match record {
            ScanRecord::Entry(entry) => {
                let node = entry.node.get();
                parents.insert(node, entry.parent.map(NodeId::get));
                let duplicate = if entry.kind == EntryKind::RegularFile {
                    let identity = entry.identity.as_ref().filter(|id| id.is_deduplicable());
                    if let Some(identity) = identity {
                        conn.query_row(
                            "SELECT EXISTS(SELECT 1 FROM entries WHERE scan=? AND identity=? AND kind=1 AND node<>? AND canonical=1)",
                            params![sid, encode(identity)?, node],
                            |row| row.get::<_, bool>(0),
                        )?
                    } else {
                        false
                    }
                } else {
                    false
                };
                let charged = entry.kind == EntryKind::RegularFile && !duplicate;
                let logical = if charged {
                    entry.logical_bytes
                } else {
                    Some(0)
                };
                let allocated = if charged {
                    entry.allocated_bytes
                } else {
                    Some(0)
                };
                conn.execute(
                    "UPDATE entries SET ordinal=node,canonical=?,charged_logical=?,charged_allocated=? WHERE scan=? AND node=?",
                    params![!duplicate, logical.map(number).transpose()?, allocated.map(number).transpose()?, sid, node],
                )?;
                (
                    node,
                    Totals {
                        logical: logical.map(number).transpose()?.unwrap_or(0),
                        allocated: allocated.map(number).transpose()?.unwrap_or(0),
                        unknown_logical: i64::from(logical.is_none()),
                        unknown_allocated: i64::from(allocated.is_none()),
                        estimated: i64::from(
                            charged
                                && !entry
                                    .identity
                                    .as_ref()
                                    .is_some_and(FileIdentity::is_deduplicable),
                        ),
                        unknown_entries: i64::from(
                            entry.kind == EntryKind::Directory
                                || entry.metadata_state != MetadataState::Complete,
                        ),
                        ..Totals::default()
                    },
                )
            }
            ScanRecord::Issue(issue) => {
                let node = match issue.node.or(issue.parent) {
                    Some(node) => node.get(),
                    None => conn.query_row(
                        "SELECT node FROM entries WHERE scan=? AND parent IS NULL",
                        [sid],
                        |row| row.get(0),
                    )?,
                };
                let mut delta = Totals::default();
                match issue.class {
                    IssueClass::Excluded => delta.excluded = 1,
                    IssueClass::MountBoundary
                    | IssueClass::VirtualFilesystem
                    | IssueClass::NetworkFilesystem => delta.boundary = 1,
                    _ => delta.errors = 1,
                }
                (node, delta)
            }
            ScanRecord::DirectoryFinished { .. } => continue,
        };
        let mut current = Some(node);
        while let Some(node) = current {
            let total = deltas.entry(node).or_default();
            *total = total.add(delta)?;
            current = match parents.get(&node) {
                Some(parent) => *parent,
                None => {
                    let parent = conn.query_row(
                        "SELECT parent FROM entries WHERE scan=? AND node=?",
                        params![sid, node],
                        |row| row.get(0),
                    )?;
                    parents.insert(node, parent);
                    parent
                }
            };
        }
    }
    for (node, delta) in deltas {
        let total = read_totals(conn, sid, node)?
            .unwrap_or_default()
            .add(delta)?;
        put_totals(conn, sid, node, total)?;
        conn.execute(
            "INSERT INTO child_sizes SELECT scan,parent,node,ordinal,?,? FROM entries WHERE scan=? AND node=? ON CONFLICT(scan,node) DO UPDATE SET logical=excluded.logical,allocated=excluded.allocated",
            params![total.logical, total.allocated, sid, node],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{PROCESS_GATE, begin, dir, entry, finished, publish, spec};

    #[test]
    fn live_reader_browses_growing_tree_without_publishing_and_survives_cancel() {
        let _gate = PROCESS_GATE.read().unwrap();
        let mut cache = Cache::open(dir()).unwrap();
        let scan = spec(1);
        begin(&mut cache, &scan);
        cache
            .ingest(&ScanBatch {
                key: scan.key,
                sequence: 0,
                records: vec![
                    entry(1, None, b"root", None, false),
                    entry(2, Some(1), b"folder", None, false),
                    entry(3, Some(2), b"first", Some(9), true),
                ],
            })
            .unwrap();
        let normal = cache.reader().unwrap();
        let live = normal.live_reader(scan.key.id).unwrap();
        let pin = live.pin(scan.key.id).unwrap();
        let root = NodeId::new(1).unwrap();
        let folder = NodeId::new(2).unwrap();
        assert!(normal.root_entry(scan.key.id).is_err());
        assert!(cache.latest(scan.root_id).unwrap().is_none());
        assert_eq!(
            live.root_entry(scan.key.id).unwrap().logical.observed_bytes,
            9
        );
        assert!(
            live.root_entry(scan.key.id)
                .unwrap()
                .coverage
                .unknown_entries
                > 0
        );
        let transaction = live.begin_read().unwrap();
        assert_eq!(
            live.entry(scan.key.id, folder)
                .unwrap()
                .logical
                .observed_bytes,
            9
        );
        cache
            .ingest(&ScanBatch {
                key: scan.key,
                sequence: 1,
                records: vec![entry(4, Some(2), b"second", Some(13), true)],
            })
            .unwrap();
        assert_eq!(
            live.children_summary(scan.key.id, folder, Metric::Logical, 100)
                .unwrap()
                .total_bytes,
            9
        );
        drop(transaction);
        let summary = live
            .children_summary(scan.key.id, folder, Metric::Logical, 1)
            .unwrap();
        assert_eq!(summary.total_bytes, 22);
        assert_eq!(
            summary.other_bytes
                + summary
                    .page
                    .entries
                    .iter()
                    .map(|row| row.logical.observed_bytes)
                    .sum::<u64>(),
            22
        );
        assert_eq!(
            live.entry(scan.key.id, root)
                .unwrap()
                .logical
                .observed_bytes,
            22
        );
        assert!(
            live.native_path(scan.key.id, NodeId::new(4).unwrap())
                .is_ok()
        );
        cache.cancel(scan.key).unwrap();
        assert!(normal.root_entry(scan.key.id).is_err());
        assert_eq!(
            live.entry(scan.key.id, folder)
                .unwrap()
                .logical
                .observed_bytes,
            22
        );
        assert_eq!(pin.scan_id(), scan.key.id);
    }

    #[test]
    fn publication_rebuilds_provisional_totals_and_canonical_owners() {
        let _gate = PROCESS_GATE.read().unwrap();
        let mut cache = Cache::open(dir()).unwrap();
        let scan = spec(1);
        begin(&mut cache, &scan);
        cache
            .ingest(&ScanBatch {
                key: scan.key,
                sequence: 0,
                records: vec![
                    entry(1, None, b"root", None, false),
                    entry(2, Some(1), b"z", Some(5), true),
                ],
            })
            .unwrap();
        let reader = cache.reader().unwrap();
        let live = reader.live_reader(scan.key.id).unwrap();
        cache
            .ingest(&ScanBatch {
                key: scan.key,
                sequence: 1,
                records: vec![entry(3, Some(1), b"a", Some(7), true), finished(1)],
            })
            .unwrap();
        assert_eq!(
            live.root_entry(scan.key.id).unwrap().logical.observed_bytes,
            12
        );
        publish(&mut cache, &scan);
        let root = reader.root_entry(scan.key.id).unwrap();
        assert_eq!(root.logical.observed_bytes, 12);
        assert_eq!(root.coverage.unknown_entries, 0);
        let page = reader
            .children(scan.key.id, root.entry.node, None, 100)
            .unwrap();
        assert_eq!(
            page.entries
                .iter()
                .map(|row| row.entry.display_name.as_str())
                .collect::<Vec<_>>(),
            ["a", "z"]
        );
        assert_eq!(
            live.root_entry(scan.key.id).unwrap().logical.observed_bytes,
            12
        );
        assert_eq!(reader.latest(scan.root_id).unwrap(), Some(scan.key.id));
    }
    #[test]
    fn provisional_hardlinks_charge_once_across_and_within_batches() {
        let _gate = PROCESS_GATE.read().unwrap();
        let mut cache = Cache::open(dir()).unwrap();
        let scan = spec(1);
        begin(&mut cache, &scan);
        let alias = |node, name: &[u8]| {
            let mut record = entry(node, Some(1), name, Some(10), true);
            if let ScanRecord::Entry(entry) = &mut record {
                entry.identity = Some(FileIdentity {
                    volume: crate::tests::identity().volume,
                    file: crate::tests::identity().directory,
                });
            }
            record
        };
        cache
            .ingest(&ScanBatch {
                key: scan.key,
                sequence: 0,
                records: vec![
                    entry(1, None, b"root", None, false),
                    alias(2, b"z"),
                    alias(3, b"y"),
                ],
            })
            .unwrap();
        let live = cache.reader().unwrap().live_reader(scan.key.id).unwrap();
        assert_eq!(
            live.root_entry(scan.key.id).unwrap().logical.observed_bytes,
            10
        );
        cache
            .ingest(&ScanBatch {
                key: scan.key,
                sequence: 1,
                records: vec![alias(4, b"a"), finished(1)],
            })
            .unwrap();
        let page = live
            .children(scan.key.id, NodeId::new(1).unwrap(), None, 100)
            .unwrap();
        assert_eq!(page.entries.iter().filter(|row| row.canonical).count(), 1);
        assert_eq!(
            live.root_entry(scan.key.id).unwrap().logical.observed_bytes,
            10
        );
        publish(&mut cache, &scan);
        let page = cache
            .children(scan.key.id, NodeId::new(1).unwrap(), None, 100)
            .unwrap();
        let owner = page.entries.iter().find(|row| row.canonical).unwrap();
        assert_eq!(owner.entry.display_name, "a");
        assert_eq!(
            cache
                .root_entry(scan.key.id)
                .unwrap()
                .logical
                .observed_bytes,
            10
        );
    }
}
