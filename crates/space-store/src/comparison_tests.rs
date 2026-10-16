//! Independent before/after fixtures: expected accounting is written explicitly,
//! rather than computed using the comparison implementation under test.
use super::*;
use crate::tests::{begin, dir, entry, finished, identity, ingest, publish, spec};

fn observed_file(
    node: u64,
    parent: u64,
    name: &[u8],
    size: Option<u64>,
    object: u64,
) -> ScanRecord {
    let mut record = entry(node, Some(parent), name, size, true);
    if let ScanRecord::Entry(e) = &mut record {
        e.identity = Some(FileIdentity {
            volume: identity().volume,
            file: ObservedIdentity {
                scheme: IdentityScheme::UnixInode,
                value: object.to_be_bytes().to_vec(),
                strength: IdentityStrength::Observed,
            },
        });
        e.allocated_bytes = size;
        e.modified = Some(Timestamp {
            seconds: 100,
            nanos: 0,
        });
    }
    record
}

fn enumeration(node: u64, state: EnumerationState) -> ScanRecord {
    ScanRecord::DirectoryFinished {
        node: NodeId::new(node).unwrap(),
        enumeration: state,
    }
}

fn snapshot(c: &mut Cache, n: u64, records: Vec<ScanRecord>) {
    let s = spec(n);
    begin(c, &s);
    ingest(c, &s, records);
    publish(c, &s);
}

fn comparison(c: &Cache) -> ComparisonView {
    compare_filtered(c, Metric::Logical, &QueryFilter::default())
}

fn compare_filtered(c: &Cache, metric: Metric, filter: &QueryFilter) -> ComparisonView {
    c.reader()
        .unwrap()
        .compare(
            spec(1).key.id,
            spec(2).key.id,
            metric,
            filter,
            &Cancellation::new(),
            &ComparisonProgress::default(),
        )
        .unwrap()
}

fn display(e: &ComparisonEntry) -> &str {
    &e.after
        .as_ref()
        .or(e.before.as_ref())
        .unwrap()
        .entry
        .display_name
}

fn child(view: &ComparisonView, parent: ComparisonNodeId, name: &str) -> ComparisonEntry {
    view.children(parent, Metric::Logical, None, 500)
        .unwrap()
        .entries
        .into_iter()
        .find(|e| display(e) == name)
        .unwrap_or_else(|| panic!("missing child {name}"))
}

fn all_rows(view: &ComparisonView) -> Vec<ComparisonEntry> {
    let mut rows = vec![view.root().unwrap()];
    let mut index = 0;
    while index < rows.len() {
        let mut cursor = None;
        loop {
            let page = view
                .children(rows[index].id, Metric::Logical, cursor, 2)
                .unwrap();
            rows.extend(page.entries);
            cursor = page.next;
            if cursor.is_none() {
                break;
            }
        }
        index += 1;
    }
    rows
}

fn assert_change(change: MetricChange, before: u64, after: u64, net: i64, up: u64, down: u64) {
    assert_eq!(change.before.observed_bytes, before);
    assert_eq!(change.after.observed_bytes, after);
    assert_eq!(change.net, Some(net));
    assert_eq!(change.gross_increase, up);
    assert_eq!(change.gross_decrease, down);
    assert_eq!(change.unknown_changes, 0);
}

fn arithmetic_fixture() -> Cache {
    let mut c = Cache::open(dir()).unwrap();
    snapshot(
        &mut c,
        1,
        vec![
            entry(1, None, b"root", None, false),
            observed_file(2, 1, b"stable", Some(100), 10),
            observed_file(3, 1, b"removed", Some(20), 20),
            observed_file(4, 1, b"grows", Some(10), 30),
            observed_file(5, 1, b"shrinks", Some(30), 40),
            finished(1),
        ],
    );
    // Deliberately change all per-scan node IDs; matching is by native path.
    snapshot(
        &mut c,
        2,
        vec![
            entry(11, None, b"root", None, false),
            observed_file(12, 11, b"added", Some(35), 50),
            observed_file(13, 11, b"shrinks", Some(5), 40),
            observed_file(14, 11, b"grows", Some(25), 30),
            observed_file(15, 11, b"stable", Some(100), 10),
            finished(11),
        ],
    );
    c
}

#[test]
fn comparison_create_remove_grow_shrink_have_independent_net_and_gross() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let c = arithmetic_fixture();
    let v = comparison(&c);
    assert_eq!(v.before_scan(), spec(1).key.id);
    assert_eq!(v.after_scan(), spec(2).key.id);
    assert_eq!(v.filter_metric(), Metric::Logical);
    let root = v.root().unwrap();
    assert_change(root.logical, 160, 165, 5, 50, 45);
    assert_change(root.allocated, 160, 165, 5, 50, 45);
    for (name, kind, old, new, delta, increase, decrease) in [
        ("added", ChangeKind::Added, 0, 35, 35, 35, 0),
        ("removed", ChangeKind::Removed, 20, 0, -20, 0, 20),
        ("grows", ChangeKind::Modified, 10, 25, 15, 15, 0),
        ("shrinks", ChangeKind::Modified, 30, 5, -25, 0, 25),
        ("stable", ChangeKind::Unchanged, 100, 100, 0, 0, 0),
    ] {
        let row = child(&v, root.id, name);
        assert_eq!(row.change, kind, "{name}");
        assert_change(row.logical, old, new, delta, increase, decrease);
        assert_eq!(v.entry(row.id).unwrap().id, row.id);
    }
    assert!(root.before.as_ref().unwrap().coverage.is_complete());
    assert!(root.after.as_ref().unwrap().coverage.is_complete());
}

#[test]
fn comparison_same_size_offset_is_not_zero_gross_change() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let mut c = Cache::open(dir()).unwrap();
    snapshot(
        &mut c,
        1,
        vec![
            entry(1, None, b"root", None, false),
            observed_file(2, 1, b"old", Some(40), 10),
            finished(1),
        ],
    );
    snapshot(
        &mut c,
        2,
        vec![
            entry(1, None, b"root", None, false),
            observed_file(2, 1, b"new", Some(40), 20),
            finished(1),
        ],
    );
    let v = comparison(&c);
    assert_change(v.root().unwrap().logical, 40, 40, 0, 40, 40);
    let rows = all_rows(&v);
    assert_eq!(
        rows.iter()
            .filter(|r| r.change == ChangeKind::Added)
            .count(),
        1
    );
    assert_eq!(
        rows.iter()
            .filter(|r| r.change == ChangeKind::Removed)
            .count(),
        1
    );
}

#[test]
fn comparison_rename_stays_two_raw_paths_even_with_equal_identity() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let mut c = Cache::open(dir()).unwrap();
    snapshot(
        &mut c,
        1,
        vec![
            entry(1, None, b"root", None, false),
            observed_file(2, 1, b"before.txt", Some(40), 10),
            finished(1),
        ],
    );
    snapshot(
        &mut c,
        2,
        vec![
            entry(1, None, b"root", None, false),
            observed_file(2, 1, b"after.txt", Some(40), 10),
            finished(1),
        ],
    );
    let v = comparison(&c);
    let root = v.root().unwrap();
    assert_change(root.logical, 40, 40, 0, 40, 40);
    let old = child(&v, root.id, "before.txt");
    let new = child(&v, root.id, "after.txt");
    assert_eq!(old.change, ChangeKind::Removed);
    assert_eq!(new.change, ChangeKind::Added);
    assert_eq!(
        (old.before_presence, old.after_presence),
        (Presence::Present, Presence::ProvenAbsent)
    );
    assert_eq!(
        (new.before_presence, new.after_presence),
        (Presence::ProvenAbsent, Presence::Present)
    );
    assert!(
        v.native_path(old.id, ComparisonEndpoint::After)
            .unwrap()
            .is_none()
    );
    assert!(
        v.native_path(new.id, ComparisonEndpoint::Before)
            .unwrap()
            .is_none()
    );
    assert!(
        v.native_path(old.id, ComparisonEndpoint::Before)
            .unwrap()
            .unwrap()
            .to_path_buf()
            .unwrap()
            .ends_with("before.txt")
    );
    assert!(
        v.native_path(new.id, ComparisonEndpoint::After)
            .unwrap()
            .unwrap()
            .to_path_buf()
            .unwrap()
            .ends_with("after.txt")
    );
}

#[test]
fn comparison_reused_identity_at_unrelated_path_does_not_prove_continuity() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let mut c = Cache::open(dir()).unwrap();
    snapshot(
        &mut c,
        1,
        vec![
            entry(1, None, b"root", None, false),
            observed_file(2, 1, b"removed-object", Some(10), 777),
            finished(1),
        ],
    );
    // A later object may reuse the same inode. No stable raw-path observation
    // anchors this group, so identity equality cannot turn +70/-10 into +60.
    snapshot(
        &mut c,
        2,
        vec![
            entry(1, None, b"root", None, false),
            observed_file(2, 1, b"unrelated-object", Some(70), 777),
            finished(1),
        ],
    );
    let v = comparison(&c);
    let root = v.root().unwrap();
    assert_change(root.logical, 10, 70, 60, 70, 10);
    assert_eq!(
        child(&v, root.id, "removed-object").change,
        ChangeKind::Removed
    );
    assert_eq!(
        child(&v, root.id, "unrelated-object").change,
        ChangeKind::Added
    );
}

#[test]
fn comparison_absent_whole_directory_propagates_to_every_observed_descendant() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let mut c = Cache::open(dir()).unwrap();
    snapshot(
        &mut c,
        1,
        vec![
            entry(1, None, b"root", None, false),
            entry(2, Some(1), b"gone", None, false),
            observed_file(3, 2, b"a", Some(10), 10),
            entry(4, Some(2), b"nested", None, false),
            observed_file(5, 4, b"b", Some(20), 20),
            finished(4),
            finished(2),
            finished(1),
        ],
    );
    snapshot(
        &mut c,
        2,
        vec![entry(1, None, b"root", None, false), finished(1)],
    );
    let v = comparison(&c);
    assert_change(v.root().unwrap().logical, 30, 0, -30, 0, 30);
    let rows = all_rows(&v);
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0].matching_paths, 5);
    assert_eq!(rows[0].changed_paths, 4);
    let removed: Vec<_> = rows
        .iter()
        .filter(|r| r.change == ChangeKind::Removed)
        .collect();
    assert_eq!(
        removed.len(),
        4,
        "both directories and both files remain inspectable"
    );
    assert!(
        removed
            .iter()
            .all(|r| r.after_presence == Presence::ProvenAbsent)
    );
    let gone = child(&v, rows[0].id, "gone");
    assert_change(gone.logical, 30, 0, -30, 0, 30);
    assert_eq!(gone.matching_paths, 4);
    assert_eq!(gone.changed_paths, 4);
    let nested = child(&v, gone.id, "nested");
    assert_change(nested.logical, 20, 0, -20, 0, 20);
}

#[test]
fn comparison_denied_scope_is_not_removed_and_reopened_scope_is_newly_observed() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    for newly_visible in [false, true] {
        let mut c = Cache::open(dir()).unwrap();
        for n in 1..=2 {
            let observed = (n == 2) == newly_visible;
            let mut records = vec![
                entry(1, None, b"root", None, false),
                entry(2, Some(1), b"private", None, false),
            ];
            if observed {
                records.push(observed_file(3, 2, b"secret", Some(20), 10));
            }
            records.push(enumeration(
                2,
                if observed {
                    EnumerationState::Complete
                } else {
                    EnumerationState::Failed
                },
            ));
            records.push(finished(1));
            snapshot(&mut c, n, records);
        }
        let v = comparison(&c);
        let root = v.root().unwrap();
        let directory = child(&v, root.id, "private");
        let file = child(&v, directory.id, "secret");
        assert_eq!(
            file.change,
            if newly_visible {
                ChangeKind::NewlyObserved
            } else {
                ChangeKind::NoLongerObserved
            }
        );
        assert_eq!(
            if newly_visible {
                file.before_presence
            } else {
                file.after_presence
            },
            Presence::Unobserved
        );
        assert_eq!(root.logical.net, None);
        assert_eq!(
            (root.logical.gross_increase, root.logical.gross_decrease),
            (0, 0)
        );
        assert!(root.logical.unknown_changes > 0);
        assert_eq!(
            (file.logical.gross_increase, file.logical.gross_decrease),
            (0, 0)
        );
        assert!(
            !if newly_visible {
                root.before.unwrap()
            } else {
                root.after.unwrap()
            }
            .coverage
            .is_complete()
        );
    }
}

#[test]
fn comparison_removed_directory_preserves_unknown_old_descendants() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let mut c = Cache::open(dir()).unwrap();
    snapshot(
        &mut c,
        1,
        vec![
            entry(1, None, b"root", None, false),
            entry(2, Some(1), b"gone", None, false),
            observed_file(3, 2, b"known", Some(10), 10),
            entry(4, Some(2), b"unread", None, false),
            enumeration(4, EnumerationState::Failed),
            finished(2),
            finished(1),
        ],
    );
    snapshot(
        &mut c,
        2,
        vec![entry(1, None, b"root", None, false), finished(1)],
    );
    let v = comparison(&c);
    let root = v.root().unwrap();
    assert_eq!(root.logical.before.observed_bytes, 10);
    assert_eq!(root.logical.after.observed_bytes, 0);
    assert_eq!(root.logical.gross_decrease, 10);
    assert_eq!(root.logical.net, None);
    assert!(root.logical.unknown_changes > 0);
    let gone = child(&v, root.id, "gone");
    assert_eq!(gone.change, ChangeKind::Removed);
    assert_eq!(gone.logical.net, None);
    assert!(!gone.before.unwrap().coverage.is_complete());
}

#[test]
fn comparison_directory_file_replacement_keeps_descendant_absence_proof() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    for directory_before in [true, false] {
        let mut c = Cache::open(dir()).unwrap();
        for n in 1..=2 {
            let directory = (n == 1) == directory_before;
            let records = if directory {
                vec![
                    entry(1, None, b"root", None, false),
                    entry(2, Some(1), b"replacement", None, false),
                    observed_file(3, 2, b"leaf", Some(10), 10),
                    finished(2),
                    finished(1),
                ]
            } else {
                vec![
                    entry(1, None, b"root", None, false),
                    observed_file(2, 1, b"replacement", Some(20), 20),
                    finished(1),
                ]
            };
            snapshot(&mut c, n, records);
        }
        let v = comparison(&c);
        let root = v.root().unwrap();
        if directory_before {
            assert_change(root.logical, 10, 20, 10, 20, 10);
        } else {
            assert_change(root.logical, 20, 10, -10, 10, 20);
        }
        let replacement = child(&v, root.id, "replacement");
        assert_eq!(replacement.change, ChangeKind::TypeChanged);
        let leaf = child(&v, replacement.id, "leaf");
        assert_eq!(
            leaf.change,
            if directory_before {
                ChangeKind::Removed
            } else {
                ChangeKind::Added
            }
        );
        assert_eq!(
            if directory_before {
                leaf.after_presence
            } else {
                leaf.before_presence
            },
            Presence::ProvenAbsent
        );
    }
}

#[test]
fn comparison_hardlink_owner_crosses_siblings_without_manufacturing_growth() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    for growth in [0, 5] {
        let mut c = Cache::open(dir()).unwrap();
        snapshot(
            &mut c,
            1,
            vec![
                entry(1, None, b"root", None, false),
                entry(2, Some(1), b"a", None, false),
                observed_file(3, 2, b"owner", Some(20), 10),
                finished(2),
                entry(4, Some(1), b"z", None, false),
                observed_file(5, 4, b"alias", Some(20), 10),
                finished(4),
                finished(1),
            ],
        );
        snapshot(
            &mut c,
            2,
            vec![
                entry(1, None, b"root", None, false),
                entry(2, Some(1), b"a", None, false),
                finished(2),
                entry(4, Some(1), b"z", None, false),
                observed_file(5, 4, b"alias", Some(20 + growth), 10),
                finished(4),
                finished(1),
            ],
        );
        let v = comparison(&c);
        let root = v.root().unwrap();
        assert_change(root.logical, 20, 20 + growth, growth as i64, growth, 0);
        assert_change(root.allocated, 20, 20 + growth, growth as i64, growth, 0);
        let a = child(&v, root.id, "a");
        let z = child(&v, root.id, "z");
        // The +20 at z and -20 at a are charge movement, not disk growth.
        assert_eq!((a.logical.gross_increase, a.logical.gross_decrease), (0, 0));
        assert_eq!((z.logical.gross_increase, z.logical.gross_decrease), (0, 0));
        assert_eq!(a.logical.net, None);
        assert_eq!(z.logical.net, None);
        assert_eq!(child(&v, a.id, "owner").change, ChangeKind::Removed);
        if growth == 0 {
            assert!(
                v.growing_directories(None, Metric::Logical, None, 50)
                    .unwrap()
                    .entries
                    .is_empty()
            );
            assert!(
                v.largest(None, Metric::Logical, ChangeDirection::Increase, None, 50)
                    .unwrap()
                    .entries
                    .is_empty()
            );
            assert!(
                v.largest(None, Metric::Logical, ChangeDirection::Decrease, None, 50)
                    .unwrap()
                    .entries
                    .is_empty()
            );
        } else {
            for entries in [
                v.growing_directories(None, Metric::Logical, None, 50)
                    .unwrap()
                    .entries,
                v.largest(None, Metric::Logical, ChangeDirection::Increase, None, 50)
                    .unwrap()
                    .entries,
            ] {
                assert!(
                    entries
                        .iter()
                        .any(|row| row.id == root.id && row.logical.gross_increase == 5)
                );
                assert!(entries.iter().all(|row| row.id != a.id && row.id != z.id));
            }
        }
    }
}

#[test]
fn comparison_unknown_metric_never_becomes_known_zero() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let mut c = Cache::open(dir()).unwrap();
    let mut old = observed_file(2, 1, b"file", Some(10), 10);
    if let ScanRecord::Entry(e) = &mut old {
        e.allocated_bytes = None;
    }
    snapshot(
        &mut c,
        1,
        vec![entry(1, None, b"root", None, false), old, finished(1)],
    );
    snapshot(
        &mut c,
        2,
        vec![
            entry(1, None, b"root", None, false),
            observed_file(2, 1, b"file", Some(15), 10),
            finished(1),
        ],
    );
    let v = comparison(&c);
    let root = v.root().unwrap();
    assert_change(root.logical, 10, 15, 5, 5, 0);
    assert_eq!(root.allocated.before.unknown_count, 1);
    assert_eq!(root.allocated.after.observed_bytes, 15);
    assert_eq!(root.allocated.net, None);
    assert_eq!(
        (root.allocated.gross_increase, root.allocated.gross_decrease),
        (0, 0)
    );
    assert!(root.allocated.unknown_changes > 0);
}

#[test]
fn comparison_changed_metadata_alias_cannot_anchor_owner_continuity() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let mut c = Cache::open(dir()).unwrap();
    let mut alias = observed_file(5, 4, b"alias", Some(20), 10);
    if let ScanRecord::Entry(e) = &mut alias {
        e.metadata_state = MetadataState::Changed;
    }
    snapshot(
        &mut c,
        1,
        vec![
            entry(1, None, b"root", None, false),
            entry(2, Some(1), b"a", None, false),
            observed_file(3, 2, b"owner", Some(20), 10),
            finished(2),
            entry(4, Some(1), b"z", None, false),
            alias,
            finished(4),
            finished(1),
        ],
    );
    snapshot(
        &mut c,
        2,
        vec![
            entry(1, None, b"root", None, false),
            entry(2, Some(1), b"a", None, false),
            finished(2),
            entry(4, Some(1), b"z", None, false),
            observed_file(5, 4, b"alias", Some(20), 10),
            finished(4),
            finished(1),
        ],
    );
    let v = comparison(&c);
    let root = v.root().unwrap();
    assert_eq!(
        root.logical.net, None,
        "an unstable alias cannot prove an unchanged identity group"
    );
    assert!(root.logical.unknown_changes > 0);
    assert_eq!(
        root.logical.gross_increase, 0,
        "owner movement cannot be presented as +20 growth"
    );
    let z = child(&v, root.id, "z");
    assert_eq!(z.logical.net, None);
    assert_eq!(z.logical.gross_increase, 0);
}

#[test]
fn comparison_real_growth_remains_ranked_in_partly_redistributed_directory() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let mut c = Cache::open(dir()).unwrap();
    snapshot(
        &mut c,
        1,
        vec![
            entry(1, None, b"root", None, false),
            entry(2, Some(1), b"a", None, false),
            observed_file(3, 2, b"owner", Some(20), 10),
            observed_file(4, 2, b"grow", Some(10), 20),
            finished(2),
            entry(5, Some(1), b"z", None, false),
            observed_file(6, 5, b"alias", Some(20), 10),
            finished(5),
            finished(1),
        ],
    );
    snapshot(
        &mut c,
        2,
        vec![
            entry(1, None, b"root", None, false),
            entry(2, Some(1), b"a", None, false),
            observed_file(4, 2, b"grow", Some(30), 20),
            finished(2),
            entry(5, Some(1), b"z", None, false),
            observed_file(6, 5, b"alias", Some(20), 10),
            finished(5),
            finished(1),
        ],
    );
    let v = comparison(&c);
    let root = v.root().unwrap();
    assert_change(root.logical, 30, 50, 20, 20, 0);
    let a = child(&v, root.id, "a");
    assert_eq!(
        a.logical.accounting,
        AccountingState::OwnershipRedistributed
    );
    assert_eq!(a.logical.net, None);
    assert_eq!(a.logical.gross_increase, 20);
    for entries in [
        v.growing_directories(None, Metric::Logical, None, 50)
            .unwrap()
            .entries,
        v.largest(None, Metric::Logical, ChangeDirection::Increase, None, 50)
            .unwrap()
            .entries,
    ] {
        assert!(
            entries
                .iter()
                .any(|e| e.id == a.id && e.logical.gross_increase == 20)
        );
    }
}

#[test]
fn comparison_unavailable_replacement_metadata_does_not_prove_descendant_absence() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let mut c = Cache::open(dir()).unwrap();
    snapshot(
        &mut c,
        1,
        vec![
            entry(1, None, b"root", None, false),
            entry(2, Some(1), b"replacement", None, false),
            observed_file(3, 2, b"leaf", Some(10), 10),
            finished(2),
            finished(1),
        ],
    );
    let mut replacement = observed_file(2, 1, b"replacement", Some(20), 20);
    if let ScanRecord::Entry(e) = &mut replacement {
        e.metadata_state = MetadataState::Unavailable;
    }
    snapshot(
        &mut c,
        2,
        vec![
            entry(1, None, b"root", None, false),
            replacement,
            finished(1),
        ],
    );
    let v = comparison(&c);
    let root = v.root().unwrap();
    assert_eq!(root.logical.net, None);
    assert_eq!(root.logical.gross_decrease, 0);
    let replacement = child(&v, root.id, "replacement");
    let leaf = child(&v, replacement.id, "leaf");
    assert_eq!(leaf.change, ChangeKind::NoLongerObserved);
    assert_eq!(leaf.after_presence, Presence::Unobserved);
    assert_eq!(leaf.logical.gross_decrease, 0);
}

#[test]
fn comparison_missing_identity_does_not_claim_unique_byte_growth() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let mut c = Cache::open(dir()).unwrap();
    for n in 1..=2 {
        snapshot(
            &mut c,
            n,
            vec![
                entry(1, None, b"root", None, false),
                entry(2, Some(1), b"unidentified", Some(n * 10), true),
                finished(1),
            ],
        );
    }
    let root = comparison(&c).root().unwrap();
    assert_eq!(
        (
            root.logical.before.observed_bytes,
            root.logical.after.observed_bytes
        ),
        (10, 20)
    );
    assert_eq!(root.logical.net, None);
    assert_eq!(
        (root.logical.gross_increase, root.logical.gross_decrease),
        (0, 0)
    );
    assert!(root.logical.unknown_changes > 0);
}

#[test]
fn comparison_filter_keeps_both_endpoints_when_either_matches() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let c = arithmetic_fixture();
    // 'grows' crosses into the range; 'shrinks' crosses out. Both changes must
    // retain their full endpoints instead of becoming +25 and -30.
    let v = compare_filtered(
        &c,
        Metric::Logical,
        &QueryFilter {
            min_size: Some(25),
            max_size: Some(30),
            ..Default::default()
        },
    );
    let root = v.root().unwrap();
    assert_change(root.logical, 40, 30, -10, 15, 25);
    let children = v
        .children(root.id, Metric::Logical, None, 50)
        .unwrap()
        .entries;
    assert_eq!(children.len(), 2);
    assert_change(child(&v, root.id, "grows").logical, 10, 25, 15, 15, 0);
    assert_change(child(&v, root.id, "shrinks").logical, 30, 5, -25, 0, 25);
}

#[test]
fn comparison_filter_metric_and_compound_predicates_use_complete_endpoints() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let mut c = Cache::open(dir()).unwrap();
    for n in 1..=2 {
        let mut file = observed_file(2, 1, b"file", Some(n * 100), 10);
        if let ScanRecord::Entry(e) = &mut file {
            e.allocated_bytes = Some(n * 10);
            e.modified = Some(Timestamp {
                seconds: if n == 1 { 1 } else { 200 },
                nanos: 0,
            });
        }
        snapshot(
            &mut c,
            n,
            vec![entry(1, None, b"root", None, false), file, finished(1)],
        );
    }
    let size = QueryFilter {
        min_size: Some(150),
        ..Default::default()
    };
    let logical = compare_filtered(&c, Metric::Logical, &size).root().unwrap();
    assert_change(logical.logical, 100, 200, 100, 100, 0);
    assert_change(logical.allocated, 10, 20, 10, 10, 0);
    let allocated = compare_filtered(&c, Metric::Allocated, &size)
        .root()
        .unwrap();
    assert_change(allocated.logical, 0, 0, 0, 0, 0);
    assert_eq!(allocated.matching_paths, 0);
    // Neither endpoint satisfies both predicates; matching size before and time
    // after is not a qualifying observation.
    let split = QueryFilter {
        max_size: Some(150),
        modified_after: Some(Timestamp {
            seconds: 100,
            nanos: 0,
        }),
        ..Default::default()
    };
    let split = compare_filtered(&c, Metric::Logical, &split)
        .root()
        .unwrap();
    assert_change(split.logical, 0, 0, 0, 0, 0);
    assert_eq!(split.matching_paths, 0);
    let inclusive = QueryFilter {
        max_size: Some(100),
        modified_before: Some(Timestamp {
            seconds: 1,
            nanos: 0,
        }),
        ..Default::default()
    };
    let inclusive = compare_filtered(&c, Metric::Logical, &inclusive)
        .root()
        .unwrap();
    assert_change(inclusive.logical, 100, 200, 100, 100, 0);
    assert_eq!(inclusive.matching_paths, 1);
}

#[test]
fn comparison_pages_are_stable_bounded_and_lossless_with_tied_values() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let mut c = Cache::open(dir()).unwrap();
    for n in 1..=2 {
        let mut records = vec![entry(1, None, b"root", None, false)];
        for i in 0..17 {
            records.push(observed_file(
                2 + i,
                1,
                format!("file-{i:02}").as_bytes(),
                Some(n * 10),
                100 + i,
            ));
        }
        records.push(finished(1));
        snapshot(&mut c, n, records);
    }
    let v = comparison(&c);
    let root = v.root().unwrap();
    for metric in [Metric::Logical, Metric::Allocated] {
        let expected: Vec<_> = v
            .children(root.id, metric, None, 50)
            .unwrap()
            .entries
            .into_iter()
            .map(|e| e.id.get())
            .collect();
        assert_eq!(expected.len(), 17);
        for size in [1, 3, 7] {
            let mut actual = Vec::new();
            let mut cursor = None;
            loop {
                let page = v.children(root.id, metric, cursor, size).unwrap();
                assert!(page.entries.len() <= size);
                actual.extend(page.entries.into_iter().map(|e| e.id.get()));
                cursor = page.next;
                if cursor.is_none() {
                    break;
                }
                assert!(actual.len() <= 17, "cursor repeated rows");
            }
            assert_eq!(actual, expected);
        }
        let expected: Vec<_> = v
            .largest(None, metric, ChangeDirection::Increase, None, 50)
            .unwrap()
            .entries
            .into_iter()
            .map(|e| e.id.get())
            .collect();
        let mut actual = Vec::new();
        let mut cursor = None;
        loop {
            let page = v
                .largest(None, metric, ChangeDirection::Increase, cursor, 3)
                .unwrap();
            actual.extend(page.entries.into_iter().map(|e| e.id.get()));
            cursor = page.next;
            if cursor.is_none() {
                break;
            }
            assert!(actual.len() <= 18);
        }
        assert_eq!(actual, expected);
        assert!(v.children(root.id, metric, None, 0).is_err());
        assert!(
            v.children(root.id, metric, None, MAX_PAGE_SIZE + 1)
                .is_err()
        );
        assert!(
            v.largest(None, metric, ChangeDirection::Increase, None, 0)
                .is_err()
        );
        assert!(
            v.growing_directories(None, metric, None, MAX_PAGE_SIZE + 1)
                .is_err()
        );
    }
}

#[test]
fn comparison_tied_pages_cover_both_directions_directories_and_subtrees() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let mut c = Cache::open(dir()).unwrap();
    for n in 1..=2 {
        let mut records = vec![entry(1, None, b"root", None, false)];
        for directory in 0..3 {
            let parent = 2 + directory * 5;
            records.push(entry(
                parent,
                Some(1),
                format!("dir-{directory}").as_bytes(),
                None,
                false,
            ));
            for file in 0..4 {
                let growing = file < 2;
                let size = if (n == 2) == growing { 20 } else { 10 };
                records.push(observed_file(
                    parent + file + 1,
                    parent,
                    format!(
                        "{}-{directory}-{file}",
                        if growing { "grow" } else { "shrink" }
                    )
                    .as_bytes(),
                    Some(size),
                    parent + file + 1,
                ));
            }
            records.push(finished(parent));
        }
        records.push(finished(1));
        snapshot(&mut c, n, records);
    }
    let view = comparison(&c);
    let root = view.root().unwrap();
    assert_change(root.logical, 180, 180, 0, 60, 60);
    let first_directory = child(&view, root.id, "dir-0");
    for metric in [Metric::Logical, Metric::Allocated] {
        for mode in 0..4 {
            for subtree in [false, true] {
                let scope = if subtree {
                    Some(first_directory.id)
                } else {
                    None
                };
                let query = |cursor, limit| match mode {
                    0 => view.children(scope.unwrap_or(root.id), metric, cursor, limit),
                    1 => view.largest(scope, metric, ChangeDirection::Increase, cursor, limit),
                    2 => view.largest(scope, metric, ChangeDirection::Decrease, cursor, limit),
                    _ => view.growing_directories(scope, metric, cursor, limit),
                };
                let expected = query(None, 50).unwrap().entries;
                let expected_names: std::collections::BTreeSet<_> =
                    expected.iter().map(|row| display(row).to_owned()).collect();
                let mut names = std::collections::BTreeSet::new();
                if mode == 0 {
                    if subtree {
                        for file in 0..4 {
                            names.insert(format!(
                                "{}-0-{file}",
                                if file < 2 { "grow" } else { "shrink" }
                            ));
                        }
                    } else {
                        for directory in 0..3 {
                            names.insert(format!("dir-{directory}"));
                        }
                    }
                } else {
                    if !subtree {
                        names.insert("root".to_owned());
                    }
                    for directory in 0..if subtree { 1 } else { 3 } {
                        names.insert(format!("dir-{directory}"));
                        if mode != 3 {
                            for file in if mode == 1 { 0..2 } else { 2..4 } {
                                names.insert(format!(
                                    "{}-{directory}-{file}",
                                    if mode == 1 { "grow" } else { "shrink" }
                                ));
                            }
                        }
                    }
                }
                assert_eq!(
                    expected_names, names,
                    "mode {mode}, subtree {subtree}, metric {metric:?}"
                );
                let expected: Vec<_> = expected.into_iter().map(|row| row.id.get()).collect();
                for size in [1, 2, 3, 7] {
                    let mut actual = Vec::new();
                    let mut cursor = None;
                    loop {
                        let page = query(cursor, size).unwrap();
                        actual.extend(page.entries.into_iter().map(|row| row.id.get()));
                        cursor = page.next;
                        if cursor.is_none() {
                            break;
                        }
                        assert!(actual.len() <= expected.len(), "cursor repeated tied rows");
                    }
                    assert_eq!(
                        actual, expected,
                        "mode {mode}, subtree {subtree}, page size {size}"
                    );
                }
            }
        }
    }
}

#[test]
fn comparison_pins_both_scans_and_keeps_main_database_read_only() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let mut c = arithmetic_fixture();
    let v = comparison(&c);
    assert!(c.remove_scan(spec(1).key.id).is_err());
    assert!(c.remove_scan(spec(2).key.id).is_err());
    assert!(
        v.reader
            .cache
            .conn
            .execute("DELETE FROM main.entries WHERE scan=1", [])
            .is_err()
    );
    for n in 3..=4 {
        snapshot(
            &mut c,
            n,
            vec![entry(1, None, b"root", None, false), finished(1)],
        );
    }
    assert_eq!(c.status(spec(1).key.id).unwrap(), ScanStatus::Published);
    assert_eq!(c.status(spec(2).key.id).unwrap(), ScanStatus::Published);
    assert_change(v.root().unwrap().logical, 160, 165, 5, 50, 45);
    drop(v);
    c.retain().unwrap();
    assert!(c.status(spec(1).key.id).is_err());
    assert!(c.status(spec(2).key.id).is_err());
}

#[test]
fn comparison_precancelled_build_releases_both_pins_without_mutation() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let mut c = arithmetic_fixture();
    let cancel = Cancellation::new();
    cancel.cancel();
    let reader = c.reader().unwrap();
    assert!(
        reader
            .compare(
                spec(1).key.id,
                spec(2).key.id,
                Metric::Logical,
                &QueryFilter::default(),
                &cancel,
                &ComparisonProgress::default()
            )
            .is_err()
    );
    assert_eq!(
        reader
            .root_entry(spec(1).key.id)
            .unwrap()
            .logical
            .observed_bytes,
        160
    );
    assert_eq!(
        reader
            .root_entry(spec(2).key.id)
            .unwrap()
            .logical
            .observed_bytes,
        165
    );
    c.remove_scan(spec(1).key.id).unwrap();
    c.remove_scan(spec(2).key.id).unwrap();
}

#[test]
fn comparison_refuses_unpublished_same_scan_and_incompatible_immutable_specs() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let c = arithmetic_fixture();
    let reader = c.reader().unwrap();
    assert!(
        reader
            .compare(
                spec(1).key.id,
                spec(1).key.id,
                Metric::Logical,
                &QueryFilter::default(),
                &Cancellation::new(),
                &ComparisonProgress::default()
            )
            .is_err()
    );
    for variant in 0..6 {
        let mut changed = spec(2);
        match variant {
            0 => changed.policy.exclude_nested_mounts = false,
            1 => changed.lineage_id = LineageId::new(2).unwrap(),
            2 => changed.expected_root.volume.value = vec![99],
            3 => changed.expected_root.directory.value = vec![99],
            4 => changed.root_id = RootId::new(2).unwrap(),
            _ => {
                let ScanRecord::Entry(named) = entry(2, Some(1), b"excluded", None, false) else {
                    unreachable!()
                };
                changed.policy.exclusions.push(vec![named.name.unwrap()]);
            }
        }
        c.conn
            .execute(
                "UPDATE scans SET metadata=? WHERE id=2",
                [encode(&changed).unwrap()],
            )
            .unwrap();
        assert!(
            reader
                .compare(
                    spec(1).key.id,
                    spec(2).key.id,
                    Metric::Logical,
                    &QueryFilter::default(),
                    &Cancellation::new(),
                    &ComparisonProgress::default()
                )
                .is_err(),
            "variant {variant}"
        );
    }
    c.conn
        .execute(
            "UPDATE scans SET metadata=? WHERE id=2",
            [encode(&spec(2)).unwrap()],
        )
        .unwrap();
    c.conn
        .execute("UPDATE scans SET status='interrupted' WHERE id=2", [])
        .unwrap();
    assert!(
        reader
            .compare(
                spec(1).key.id,
                spec(2).key.id,
                Metric::Logical,
                &QueryFilter::default(),
                &Cancellation::new(),
                &ComparisonProgress::default()
            )
            .is_err()
    );
}

#[test]
fn comparison_missing_or_malformed_coverage_never_proves_absence() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    for malformed in [false, true] {
        let mut c = Cache::open(dir()).unwrap();
        snapshot(
            &mut c,
            1,
            vec![
                entry(1, None, b"root", None, false),
                observed_file(2, 1, b"old", Some(10), 10),
                finished(1),
            ],
        );
        snapshot(
            &mut c,
            2,
            vec![entry(1, None, b"root", None, false), finished(1)],
        );
        if malformed {
            c.conn
                .execute("UPDATE coverage SET data=x'00' WHERE scan=2 AND node=1", [])
                .unwrap();
        } else {
            c.conn
                .execute("DELETE FROM coverage WHERE scan=2 AND node=1", [])
                .unwrap();
        }
        let result = c.reader().unwrap().compare(
            spec(1).key.id,
            spec(2).key.id,
            Metric::Logical,
            &QueryFilter::default(),
            &Cancellation::new(),
            &ComparisonProgress::default(),
        );
        match result {
            Err(_) => {}
            Ok(v) => {
                let root = v.root().unwrap();
                assert_eq!(root.logical.net, None);
                assert_eq!(root.logical.gross_decrease, 0);
                let old = child(&v, root.id, "old");
                assert_eq!(old.after_presence, Presence::Unobserved);
                assert_eq!(old.change, ChangeKind::NoLongerObserved);
            }
        }
    }
}

#[test]
fn comparison_rejects_payload_key_corruption_before_filtering() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let c = arithmetic_fixture();
    let mut wrong = c
        .entry(spec(1).key.id, NodeId::new(4).unwrap())
        .unwrap()
        .entry;
    wrong.display_name = "filter-me-out".into();
    c.conn
        .execute(
            "UPDATE entries SET data=? WHERE scan=1 AND node=3",
            [encode(&wrong).unwrap()],
        )
        .unwrap();
    assert!(
        c.reader()
            .unwrap()
            .compare(
                spec(1).key.id,
                spec(2).key.id,
                Metric::Logical,
                &QueryFilter {
                    name: Some("stable".into()),
                    ..Default::default()
                },
                &Cancellation::new(),
                &ComparisonProgress::default()
            )
            .is_err()
    );
}

#[cfg(unix)]
#[test]
fn comparison_raw_names_with_identical_lossy_display_are_distinct() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let mut c = Cache::open(dir()).unwrap();
    snapshot(
        &mut c,
        1,
        vec![
            entry(1, None, b"root", None, false),
            observed_file(2, 1, &[0xff], Some(10), 10),
            finished(1),
        ],
    );
    snapshot(
        &mut c,
        2,
        vec![
            entry(1, None, b"root", None, false),
            observed_file(2, 1, &[0xfe], Some(10), 20),
            finished(1),
        ],
    );
    let v = comparison(&c);
    let root = v.root().unwrap();
    assert_change(root.logical, 10, 10, 0, 10, 10);
    let children = v
        .children(root.id, Metric::Logical, None, 10)
        .unwrap()
        .entries;
    assert_eq!(children.len(), 2);
    assert_eq!(display(&children[0]), display(&children[1]));
    assert_eq!(
        children
            .iter()
            .filter(|e| e.change == ChangeKind::Added)
            .count(),
        1
    );
    assert_eq!(
        children
            .iter()
            .filter(|e| e.change == ChangeKind::Removed)
            .count(),
        1
    );
}

#[test]
fn comparison_empty_unseen_scope_keeps_totals_incomplete_even_with_filter() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let mut c = Cache::open(dir()).unwrap();
    for n in 1..=2 {
        snapshot(
            &mut c,
            n,
            vec![
                entry(1, None, b"root", None, false),
                entry(2, Some(1), b"unseen", None, false),
                enumeration(2, EnumerationState::Failed),
                finished(1),
            ],
        );
    }
    for filter in [
        QueryFilter::default(),
        QueryFilter {
            name: Some("no-such-file".into()),
            ..Default::default()
        },
    ] {
        let root = compare_filtered(&c, Metric::Logical, &filter)
            .root()
            .unwrap();
        assert_eq!(root.logical.net, None);
        assert!(root.logical.unknown_changes > 0);
        assert_eq!(
            (root.logical.gross_increase, root.logical.gross_decrease),
            (0, 0)
        );
        assert!(!root.before.unwrap().coverage.is_complete());
        assert!(!root.after.unwrap().coverage.is_complete());
    }
}

#[test]
fn comparison_unscoped_issue_cannot_leave_an_exact_deletion_total() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let mut c = Cache::open(dir()).unwrap();
    snapshot(
        &mut c,
        1,
        vec![
            entry(1, None, b"root", None, false),
            observed_file(2, 1, b"old", Some(10), 10),
            finished(1),
        ],
    );
    snapshot(
        &mut c,
        2,
        vec![
            entry(1, None, b"root", None, false),
            ScanRecord::Issue(ScanIssue {
                node: None,
                parent: Some(NodeId::new(1).unwrap()),
                name: None,
                class: IssueClass::PermissionDenied,
                detail: "A child could not be observed".into(),
            }),
            finished(1),
        ],
    );
    let v = comparison(&c);
    let root = v.root().unwrap();
    assert!(!root.after.as_ref().unwrap().coverage.is_complete());
    assert_eq!(root.logical.net, None);
    assert_eq!(root.logical.gross_decrease, 0);
    assert_eq!(
        child(&v, root.id, "old").after_presence,
        Presence::Unobserved
    );
}

#[test]
fn comparison_named_unseen_child_does_not_hide_proven_absent_sibling() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    for class in [
        IssueClass::PermissionDenied,
        IssueClass::UnsupportedMetadata,
        IssueClass::Excluded,
        IssueClass::MountBoundary,
    ] {
        let mut c = Cache::open(dir()).unwrap();
        snapshot(
            &mut c,
            1,
            vec![
                entry(1, None, b"root", None, false),
                observed_file(2, 1, b"blocked", Some(10), 10),
                observed_file(3, 1, b"absent", Some(20), 20),
                finished(1),
            ],
        );
        let ScanRecord::Entry(named) = entry(2, Some(1), b"blocked", None, true) else {
            unreachable!()
        };
        snapshot(
            &mut c,
            2,
            vec![
                entry(1, None, b"root", None, false),
                ScanRecord::Issue(ScanIssue {
                    node: None,
                    parent: Some(NodeId::new(1).unwrap()),
                    name: named.name,
                    class,
                    detail: "The named child was seen but could not be inspected".into(),
                }),
                finished(1),
            ],
        );
        let v = comparison(&c);
        let root = v.root().unwrap();
        let blocked = child(&v, root.id, "blocked");
        assert_eq!(blocked.change, ChangeKind::NoLongerObserved, "{class:?}");
        assert_eq!(blocked.after_presence, Presence::Unobserved, "{class:?}");
        assert_eq!(blocked.logical.gross_decrease, 0, "{class:?}");
        let absent = child(&v, root.id, "absent");
        assert_eq!(absent.change, ChangeKind::Removed, "{class:?}");
        assert_eq!(absent.after_presence, Presence::ProvenAbsent, "{class:?}");
        assert_change(absent.logical, 20, 0, -20, 0, 20);
        assert_eq!(root.logical.net, None);
        assert_eq!(root.logical.gross_decrease, 20, "{class:?}");
        assert!(root.logical.unknown_changes > 0);
    }
}

#[test]
fn comparison_cursors_cannot_cross_views_metrics_scopes_or_directions() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let c = arithmetic_fixture();
    let v = comparison(&c);
    let other = comparison(&c);
    let root = v.root().unwrap();
    let cursor = v.children(root.id, Metric::Logical, None, 1).unwrap().next;
    assert!(cursor.is_some());
    assert!(
        other
            .children(other.root().unwrap().id, Metric::Logical, cursor, 1)
            .is_err()
    );
    assert!(v.children(root.id, Metric::Allocated, cursor, 1).is_err());
    let first = v
        .children(root.id, Metric::Logical, None, 1)
        .unwrap()
        .entries
        .remove(0);
    assert!(v.children(first.id, Metric::Logical, cursor, 1).is_err());
    assert!(
        v.largest(None, Metric::Logical, ChangeDirection::Increase, cursor, 1)
            .is_err()
    );
    let cursor = v
        .largest(None, Metric::Logical, ChangeDirection::Increase, None, 1)
        .unwrap()
        .next;
    assert!(cursor.is_some());
    assert!(
        v.largest(None, Metric::Logical, ChangeDirection::Decrease, cursor, 1)
            .is_err()
    );
}

#[test]
fn comparison_owner_join_uses_identity_lookup_in_actual_query_plan() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let c = arithmetic_fixture();
    c.conn.execute_batch("CREATE TEMP TABLE groups(identity BLOB PRIMARY KEY,before_node INTEGER NOT NULL,after_node INTEGER NOT NULL)").unwrap();
    let sql = format!("EXPLAIN QUERY PLAN {}", crate::comparison::OWNER_GROUPS_SQL);
    let details = c
        .conn
        .prepare(&sql)
        .unwrap()
        .query_map(params![1, 2], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    assert!(
        details
            .iter()
            .any(|line| line.contains("SEARCH a USING INDEX hardlinks")
                && line.contains("scan=? AND identity=?")),
        "owner join must perform a keyed identity lookup: {details:?}"
    );
    assert!(
        !details
            .iter()
            .any(|line| line.contains("SEARCH a USING INDEX largest_")),
        "owner join must not rescan every canonical file: {details:?}"
    );
}

#[test]
fn comparison_actual_page_queries_seek_full_tuple_with_bounded_late_tie_work() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    use std::sync::atomic::{AtomicUsize, Ordering};
    let cache = arithmetic_fixture();
    let view = comparison(&cache);
    let connection = &view.reader.cache.conn;
    let (summary, change): (Vec<u8>, Vec<u8>) = connection
        .query_row("SELECT summary,change FROM pairs WHERE id=1", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    // Populate only the view's disposable TEMP table, using its actual schema
    // and indexes. Main snapshots remain read-only and unchanged.
    connection.execute("DELETE FROM pairs", []).unwrap();
    connection.execute_batch("BEGIN").unwrap();
    {
        let mut insert = connection.prepare("INSERT INTO pairs(id,parent,bp,ap,depth,display,directory,summary,change,visible,selected,lo,hi,linc,ldec,lrank,ainc,adec,arank) VALUES(?1,1,0,0,1,'tie',1,?2,?3,1,1,?1,?1,1,1,1,1,1,1)").unwrap();
        for id in 1..=20_000 {
            insert.execute(params![id, &summary, &change]).unwrap();
        }
    }
    connection.execute_batch("COMMIT").unwrap();
    for rank in [1, i64::MAX] {
        connection
            .execute(
                "UPDATE pairs SET linc=?1,ldec=?1,lrank=?1,ainc=?1,adec=?1,arank=?1",
                [rank],
            )
            .unwrap();
        for metric in [Metric::Logical, Metric::Allocated] {
            for mode in 0..4 {
                let sql = crate::comparison::page_sql(metric, mode);
                let details = connection
                    .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                    .unwrap()
                    .query_map(params![1, 1, 20_000, rank, 19_000, 501], |row| {
                        row.get::<_, String>(3)
                    })
                    .unwrap()
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .unwrap();
                assert!(
                    details
                        .iter()
                        .any(|line| line.contains("neg_id") && line.contains("<(?,?)")),
                    "page must seek the full rank/id tuple: {details:?}"
                );
                assert!(
                    !details.iter().any(|line| line.contains("TEMP B-TREE")),
                    "page order must use its index: {details:?}"
                );
                let mut samples = Vec::new();
                for cursor in [0, 501, 19_000] {
                    let steps = Arc::new(AtomicUsize::new(0));
                    let count = steps.clone();
                    connection
                        .progress_handler(
                            100,
                            Some(move || {
                                count.fetch_add(100, Ordering::Relaxed);
                                false
                            }),
                        )
                        .unwrap();
                    let rows = connection
                        .prepare(&sql)
                        .unwrap()
                        .query_map(params![1, 1, 20_000, rank, cursor, 501], |row| {
                            row.get::<_, i64>(0)
                        })
                        .unwrap()
                        .collect::<std::result::Result<Vec<_>, _>>()
                        .unwrap();
                    connection
                        .progress_handler(0, None::<fn() -> bool>)
                        .unwrap();
                    assert_eq!(rows, ((cursor + 1)..=(cursor + 501)).collect::<Vec<_>>());
                    samples.push(steps.load(Ordering::Relaxed));
                }
                assert!(
                    samples[2] < 20_000,
                    "late tied page rescanned its prefix: {samples:?}, mode {mode}, metric {metric:?}"
                );
                assert!(
                    samples[2] <= samples[0] + 2_000,
                    "late tied page did substantially more work than first page: {samples:?}"
                );
            }
        }
    }
    assert_eq!(
        cache
            .root_entry(spec(1).key.id)
            .unwrap()
            .logical
            .observed_bytes,
        160
    );
    assert_eq!(
        cache
            .root_entry(spec(2).key.id)
            .unwrap()
            .logical
            .observed_bytes,
        165
    );
}

#[test]
fn comparison_active_cancellation_releases_both_pins_and_reports_terminal_phase() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let mut c = Cache::open(dir()).unwrap();
    for n in 1..=2 {
        let s = spec(n);
        begin(&mut c, &s);
        c.ingest(&ScanBatch {
            key: s.key,
            sequence: 0,
            records: vec![entry(1, None, b"root", None, false)],
        })
        .unwrap();
        for sequence in 1..=4 {
            let records = (0..1000)
                .map(|i| {
                    let node = 2 + (sequence - 1) * 1000 + i;
                    observed_file(
                        node,
                        1,
                        format!("file-{node}").as_bytes(),
                        Some(n * 10),
                        node,
                    )
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
            sequence: 5,
            records: vec![finished(1)],
        })
        .unwrap();
        publish(&mut c, &s);
    }
    let cancel = Cancellation::new();
    let signal = cancel.clone();
    let progress = ComparisonProgress::default();
    let observer = progress.clone();
    let watcher = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let phase = observer.snapshot().phase;
            if matches!(
                phase,
                ComparisonPhase::Matching
                    | ComparisonPhase::Accounting
                    | ComparisonPhase::Aggregating
                    | ComparisonPhase::Indexing
            ) {
                signal.cancel();
                return true;
            }
            if matches!(
                phase,
                ComparisonPhase::Finished | ComparisonPhase::Cancelled | ComparisonPhase::Failed
            ) || std::time::Instant::now() >= deadline
            {
                signal.cancel();
                return false;
            }
            std::thread::yield_now();
        }
    });
    let result = c.reader().unwrap().compare(
        spec(1).key.id,
        spec(2).key.id,
        Metric::Logical,
        &QueryFilter::default(),
        &cancel,
        &progress,
    );
    assert!(
        watcher.join().unwrap(),
        "cancellation must occur during an active build"
    );
    assert!(result.is_err());
    assert_eq!(progress.snapshot().phase, ComparisonPhase::Cancelled);
    assert_eq!(
        c.root_entry(spec(1).key.id).unwrap().logical.observed_bytes,
        40_000
    );
    assert_eq!(
        c.root_entry(spec(2).key.id).unwrap().logical.observed_bytes,
        80_000
    );
    c.remove_scan(spec(1).key.id).unwrap();
    c.remove_scan(spec(2).key.id).unwrap();
}

#[test]
#[ignore = "release comparison build/page benchmark; creates two 100,000-file snapshots"]
fn comparison_wide_release_benchmark() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let files: u64 = std::env::var("SPACE_COMPARISON_BENCHMARK_FILES")
        .map(|value| {
            value
                .parse()
                .expect("benchmark file count must be an integer")
        })
        .unwrap_or(100_000);
    assert!(
        files > 0 && files.is_multiple_of(1000),
        "benchmark file count must be a positive multiple of 1,000"
    );
    let retained = std::env::var_os("SPACE_COMPARISON_BENCHMARK_CACHE");
    let mut c = Cache::open(
        retained
            .as_ref()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(dir),
    )
    .unwrap();
    for n in 1..=if retained.is_some() { 0 } else { 2 } {
        let s = spec(n);
        begin(&mut c, &s);
        c.ingest(&ScanBatch {
            key: s.key,
            sequence: 0,
            records: vec![entry(1, None, b"root", None, false)],
        })
        .unwrap();
        for sequence in 1..=files / 1000 {
            let records = (0..1000)
                .map(|i| {
                    let node = 2 + (sequence - 1) * 1000 + i;
                    observed_file(node, 1, format!("file-{node:08}").as_bytes(), Some(n), node)
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
            sequence: files / 1000 + 1,
            records: vec![finished(1)],
        })
        .unwrap();
        publish(&mut c, &s);
    }
    let started = std::time::Instant::now();
    let progress = ComparisonProgress::default();
    let observer = progress.clone();
    let watcher = std::thread::spawn(move || {
        let mut phase = ComparisonPhase::Waiting;
        let mut reported = std::time::Instant::now();
        loop {
            let snapshot = observer.snapshot();
            if snapshot.phase != phase || reported.elapsed() >= std::time::Duration::from_secs(10) {
                println!(
                    "comparison_wide_phase={:?} processed={} total={} elapsed={:?}",
                    snapshot.phase,
                    snapshot.processed,
                    snapshot.total,
                    started.elapsed()
                );
                phase = snapshot.phase;
                reported = std::time::Instant::now();
            }
            if matches!(
                phase,
                ComparisonPhase::Finished | ComparisonPhase::Cancelled | ComparisonPhase::Failed
            ) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    });
    let view = c.reader().unwrap().compare(
        spec(1).key.id,
        spec(2).key.id,
        Metric::Logical,
        &QueryFilter::default(),
        &Cancellation::new(),
        &progress,
    );
    watcher.join().unwrap();
    let view = view.unwrap();
    println!(
        "comparison_wide_files={files} build={:?}",
        started.elapsed()
    );
    let root = view.root().unwrap();
    assert_change(root.logical, files, files * 2, files as i64, files, 0);
    assert_eq!(root.matching_paths, files + 1);
    let mut samples = Vec::new();
    for _ in 0..20 {
        let start = std::time::Instant::now();
        let page = view
            .largest(None, Metric::Logical, ChangeDirection::Increase, None, 100)
            .unwrap();
        assert_eq!(page.entries.len(), 100);
        samples.push(start.elapsed());
    }
    samples.sort();
    println!("comparison_wide_top100_warm_p95={:?}", samples[18]);
    let paging_started = std::time::Instant::now();
    let mut late_page_samples = Vec::new();
    let mut cursor = None;
    let mut children = 0;
    loop {
        let page_started = std::time::Instant::now();
        let page = view
            .children(root.id, Metric::Logical, cursor, MAX_PAGE_SIZE)
            .unwrap();
        children += page.entries.len();
        if children as u64 > files.saturating_sub(10_000) {
            late_page_samples.push(page_started.elapsed());
        }
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(children, files as usize);
    println!(
        "comparison_wide_all_children={:?} rows={children}",
        paging_started.elapsed()
    );
    late_page_samples.sort();
    println!(
        "comparison_wide_last500_warm_p95={:?}",
        late_page_samples[(late_page_samples.len() * 95 / 100).saturating_sub(1)]
    );
    #[cfg(target_os = "linux")]
    for line in std::fs::read_to_string("/proc/self/status")
        .unwrap()
        .lines()
        .filter(|l| l.starts_with("VmHWM:") || l.starts_with("VmRSS:"))
    {
        println!("{line}");
    }
}

#[test]
#[ignore = "release comparison build/subtree benchmark; creates two 500-level snapshots"]
fn comparison_deep_release_benchmark() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    const DEPTH: u64 = 500;
    let mut c = Cache::open(dir()).unwrap();
    for n in 1..=2 {
        let s = spec(n);
        begin(&mut c, &s);
        let mut records = vec![entry(1, None, b"root", None, false)];
        for node in 2..=DEPTH + 1 {
            records.push(entry(
                node,
                Some(node - 1),
                format!("depth-{node}").as_bytes(),
                None,
                false,
            ));
        }
        records.push(observed_file(
            DEPTH + 2,
            DEPTH + 1,
            b"leaf",
            Some(100 + 50 * (n - 1)),
            900,
        ));
        for node in (1..=DEPTH + 1).rev() {
            records.push(finished(node));
        }
        for (sequence, chunk) in records.chunks(MAX_BATCH_RECORDS).enumerate() {
            c.ingest(&ScanBatch {
                key: s.key,
                sequence: sequence as u64,
                records: chunk.to_vec(),
            })
            .unwrap();
        }
        publish(&mut c, &s);
    }
    let started = std::time::Instant::now();
    let view = comparison(&c);
    println!("comparison_deep_build_500={:?}", started.elapsed());
    let root = view.root().unwrap();
    assert_change(root.logical, 100, 150, 50, 50, 0);
    assert_eq!(root.matching_paths, DEPTH + 2);
    let mut current = root.id;
    for _ in 0..DEPTH {
        let page = view.children(current, Metric::Logical, None, 1).unwrap();
        assert_eq!(page.entries.len(), 1);
        assert!(page.next.is_none());
        current = page.entries[0].id;
    }
    let started = std::time::Instant::now();
    let page = view
        .largest(
            Some(current),
            Metric::Logical,
            ChangeDirection::Increase,
            None,
            100,
        )
        .unwrap();
    println!("comparison_deep_subtree_query={:?}", started.elapsed());
    assert_eq!(
        page.entries.iter().filter(|e| display(e) == "leaf").count(),
        1
    );
    assert!(page.entries.len() <= 2);
    let rows = all_rows(&view);
    assert_eq!(rows.len(), (DEPTH + 2) as usize);
}
