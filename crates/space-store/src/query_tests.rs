use super::*;
use crate::tests::*;
fn fixture() -> Cache {
    let mut c = Cache::open(dir()).unwrap();
    let s = spec(1);
    begin(&mut c, &s);
    let mut a = entry(3, Some(2), b"alpha.txt", Some(10), true);
    let mut alias = entry(7, Some(1), b"zlink.txt", Some(10), true);
    for e in [&mut a, &mut alias] {
        if let ScanRecord::Entry(e) = e {
            e.identity = Some(FileIdentity {
                volume: identity().volume,
                file: identity().directory,
            });
            e.modified = Some(Timestamp {
                seconds: 100,
                nanos: 0,
            });
            e.allocated_bytes = Some(8);
        }
    }
    let mut b = entry(6, Some(5), b"alpha.log", Some(30), true);
    if let ScanRecord::Entry(e) = &mut b {
        e.allocated_bytes = Some(40);
    }
    let mut unicode = entry(8, Some(5), b"unicode.txt", Some(40), true);
    if let ScanRecord::Entry(e) = &mut unicode {
        e.display_name = "Straße.TXT".into();
        e.allocated_bytes = Some(16);
    }
    let mut literal = entry(9, Some(5), b"%_[x].txt", Some(5), true);
    if let ScanRecord::Entry(e) = &mut literal {
        e.allocated_bytes = Some(4);
    }
    ingest(
        &mut c,
        &s,
        vec![
            entry(1, None, b"root", None, false),
            entry(2, Some(1), b"A", None, false),
            a,
            entry(4, Some(2), b"beta.bin", Some(20), true),
            finished(2),
            entry(5, Some(1), b"B", None, false),
            b,
            unicode,
            literal,
            finished(5),
            alias,
            finished(1),
        ],
    );
    publish(&mut c, &s);
    c
}
#[test]
fn size_sorted_children_subtree_largest_and_aliases() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let c = fixture();
    let scan = spec(1).key.id;
    let root = NodeId::new(1).unwrap();
    let p = c
        .children_sorted(scan, root, Metric::Logical, None, 1)
        .unwrap();
    assert_eq!(p.entries[0].entry.node.get(), 5);
    assert_eq!(p.entries[0].logical.observed_bytes, 75);
    let p = c
        .children_sorted(scan, root, Metric::Logical, p.next, 1)
        .unwrap();
    assert_eq!(p.entries[0].entry.node.get(), 2);
    let p = c
        .largest_in(
            scan,
            Some(NodeId::new(2).unwrap()),
            Metric::Logical,
            None,
            10,
        )
        .unwrap();
    assert_eq!(
        p.entries
            .iter()
            .map(|e| e.entry.node.get())
            .collect::<Vec<_>>(),
        vec![4, 3]
    );
    let p = c.aliases(scan, NodeId::new(3).unwrap(), None, 1).unwrap();
    assert_eq!(p.entries[0].node.get(), 3);
    let p = c.aliases(scan, NodeId::new(3).unwrap(), p.next, 1).unwrap();
    assert_eq!(p.entries[0].node.get(), 7);
    assert!(p.next.is_none());
}
#[test]
fn bounded_summary_accounts_for_other_zero_and_unknown() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let c = fixture();
    let s = c
        .children_summary(spec(1).key.id, NodeId::new(1).unwrap(), Metric::Logical, 1)
        .unwrap();
    assert_eq!(s.total_count, 3);
    assert_eq!(s.total_bytes, 105);
    assert_eq!(s.other_bytes, 30);
    assert_eq!(s.other_count, 2);
    assert_eq!(s.zero_count, 1);
    let s = c
        .children_summary(
            spec(1).key.id,
            NodeId::new(1).unwrap(),
            Metric::Allocated,
            1,
        )
        .unwrap();
    assert_eq!(s.total_bytes, 68);
    assert_eq!(s.unknown_count, 1);
}
#[test]
fn filtered_lists_and_tree_use_identical_charges() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let c = fixture();
    let reader = c.reader().unwrap();
    let view = reader
        .filtered(
            spec(1).key.id,
            Metric::Logical,
            &QueryFilter {
                name: Some("alpha".into()),
                ..Default::default()
            },
            &Cancellation::new(),
        )
        .unwrap();
    assert_eq!(view.matching_count().unwrap(), 2);
    assert_eq!(view.root_entry().unwrap().logical.observed_bytes, 40);
    let p = view.children(NodeId::new(1).unwrap(), None, 1).unwrap();
    assert_eq!(p.entries[0].logical.observed_bytes, 30);
    let p = view.children(NodeId::new(1).unwrap(), p.next, 1).unwrap();
    assert_eq!(p.entries[0].logical.observed_bytes, 10);
    assert!(p.next.is_none());
    assert_eq!(
        view.largest(Some(NodeId::new(2).unwrap()), None, 10)
            .unwrap()
            .entries
            .len(),
        1
    );
    let summary = view.children_summary(NodeId::new(1).unwrap(), 1).unwrap();
    assert_eq!(summary.other_bytes, 10);
    assert_eq!(summary.total_bytes, 40);
}
#[test]
fn path_extension_casefold_and_literal_search() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let c = fixture();
    let reader = c.reader().unwrap();
    for (filter, expected) in [
        (
            QueryFilter {
                path: Some("a/alpha".into()),
                ..Default::default()
            },
            10,
        ),
        (
            QueryFilter {
                name: Some("STRASSE".into()),
                ..Default::default()
            },
            40,
        ),
        (
            QueryFilter {
                name: Some("%_[x]".into()),
                ..Default::default()
            },
            5,
        ),
        (
            QueryFilter {
                extension: Some(".TXT".into()),
                ..Default::default()
            },
            55,
        ),
    ] {
        let view = reader
            .filtered(
                spec(1).key.id,
                Metric::Logical,
                &filter,
                &Cancellation::new(),
            )
            .unwrap();
        assert_eq!(view.root_entry().unwrap().logical.observed_bytes, expected);
    }
}
#[test]
fn size_time_unknown_rules_are_explicit() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let c = fixture();
    let reader = c.reader().unwrap();
    let scan = spec(1).key.id;
    let mut f = QueryFilter {
        min_size: Some(10),
        ..Default::default()
    };
    let v = reader
        .filtered(scan, Metric::Allocated, &f, &Cancellation::new())
        .unwrap();
    assert_eq!(v.root_entry().unwrap().allocated.observed_bytes, 56);
    assert_eq!(v.root_entry().unwrap().allocated.unknown_count, 0);
    f.include_unknown_size = true;
    let v = reader
        .filtered(scan, Metric::Allocated, &f, &Cancellation::new())
        .unwrap();
    assert_eq!(v.root_entry().unwrap().allocated.observed_bytes, 56);
    assert_eq!(v.root_entry().unwrap().allocated.unknown_count, 1);
    let mut f = QueryFilter {
        modified_after: Some(Timestamp {
            seconds: 100,
            nanos: 0,
        }),
        modified_before: Some(Timestamp {
            seconds: 100,
            nanos: 0,
        }),
        ..Default::default()
    };
    let v = reader
        .filtered(scan, Metric::Logical, &f, &Cancellation::new())
        .unwrap();
    assert_eq!(v.matching_count().unwrap(), 2);
    assert_eq!(v.root_entry().unwrap().logical.observed_bytes, 10);
    f.include_unknown_time = true;
    let v = reader
        .filtered(scan, Metric::Logical, &f, &Cancellation::new())
        .unwrap();
    assert_eq!(v.root_entry().unwrap().logical.observed_bytes, 105);
}
#[test]
fn matching_alias_does_not_move_canonical_charge() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let c = fixture();
    let reader = c.reader().unwrap();
    let v = reader
        .filtered(
            spec(1).key.id,
            Metric::Logical,
            &QueryFilter {
                name: Some("zlink".into()),
                ..Default::default()
            },
            &Cancellation::new(),
        )
        .unwrap();
    assert_eq!(v.matching_count().unwrap(), 1);
    assert_eq!(v.root_entry().unwrap().logical.observed_bytes, 0);
    assert!(v.largest(None, None, 10).unwrap().entries.is_empty());
    assert_eq!(
        v.aliases(NodeId::new(7).unwrap(), None, 10)
            .unwrap()
            .entries
            .len(),
        2
    );
    assert!(v.native_path(NodeId::new(3).unwrap()).is_ok());
}
#[test]
fn filter_cancel_and_empty_result_preserve_published() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let c = fixture();
    let reader = c.reader().unwrap();
    let cancel = Cancellation::new();
    cancel.cancel();
    assert!(
        reader
            .filtered(
                spec(1).key.id,
                Metric::Logical,
                &QueryFilter::default(),
                &cancel
            )
            .is_err()
    );
    let v = reader
        .filtered(
            spec(1).key.id,
            Metric::Logical,
            &QueryFilter {
                name: Some("nothing matches".into()),
                ..Default::default()
            },
            &Cancellation::new(),
        )
        .unwrap();
    assert_eq!(v.matching_count().unwrap(), 0);
    assert_eq!(v.root_entry().unwrap().logical.observed_bytes, 0);
    assert!(
        v.children(NodeId::new(1).unwrap(), None, 10)
            .unwrap()
            .entries
            .is_empty()
    );
    assert_eq!(
        reader
            .root_entry(spec(1).key.id)
            .unwrap()
            .logical
            .observed_bytes,
        105
    );
}
#[test]
fn schema_one_upgrade_preserves_published_scan() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let p = dir();
    {
        let mut c = Cache::open(&p).unwrap();
        basic(&mut c, 1);
        c.conn
            .execute_batch("DROP TABLE child_sizes; DROP TABLE id_highwater; PRAGMA user_version=1")
            .unwrap();
    }
    let c = Cache::open(p).unwrap();
    let p = c
        .children_sorted(
            spec(1).key.id,
            NodeId::new(1).unwrap(),
            Metric::Logical,
            None,
            10,
        )
        .unwrap();
    assert_eq!(p.entries[0].logical.observed_bytes, 7);
    assert_eq!(c.latest(spec(1).root_id).unwrap(), Some(spec(1).key.id));
}

#[test]
#[ignore = "requires SPACE_QUERY_BENCHMARK_CACHE pointing to million_entry_fixture output"]
fn million_query_benchmark() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let path = std::env::var_os("SPACE_QUERY_BENCHMARK_CACHE")
        .expect("set SPACE_QUERY_BENCHMARK_CACHE to million_entry_fixture cache directory");
    let start = std::time::Instant::now();
    let c = Cache::open(path).unwrap();
    println!("reopen_including_schema_upgrade={:?}", start.elapsed());
    let scan = spec(1).key.id;
    let root = NodeId::new(1).unwrap();
    let now = std::time::Instant::now();
    let summary = c
        .children_summary(scan, root, Metric::Logical, 128)
        .unwrap();
    println!(
        "million_children_summary={:?} total_count={} other_count={}",
        now.elapsed(),
        summary.total_count,
        summary.other_count
    );
    assert_eq!(summary.total_count, 1_000_000);
    let reader = c.reader().unwrap();
    let no_op = std::time::Instant::now();
    let empty = reader
        .filtered(
            scan,
            Metric::Logical,
            &QueryFilter::default(),
            &Cancellation::new(),
        )
        .unwrap();
    assert_eq!(
        empty.root_entry().unwrap().logical,
        c.root_entry(scan).unwrap().logical
    );
    println!("million_no_op_filter={:?}", no_op.elapsed());
    drop(empty);
    let progress = FilterProgress::default();
    let observer = progress.clone();
    let watch = std::thread::spawn(move || {
        let mut phase = FilterPhase::Waiting;
        let mut start = std::time::Instant::now();
        loop {
            let s = observer.snapshot();
            if s.phase != phase {
                println!("filter_phase_{phase:?}={:?}", start.elapsed());
                phase = s.phase;
                start = std::time::Instant::now();
            }
            if matches!(
                phase,
                FilterPhase::Finished | FilterPhase::Cancelled | FilterPhase::Failed
            ) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    });
    let now = std::time::Instant::now();
    let view = reader
        .filtered_with_progress(
            scan,
            Metric::Logical,
            &QueryFilter {
                min_size: Some(0),
                ..Default::default()
            },
            &Cancellation::new(),
            &progress,
        )
        .unwrap();
    watch.join().unwrap();
    println!(
        "million_match_filter_build={:?} matches={}",
        now.elapsed(),
        view.matching_count().unwrap()
    );
    assert_eq!(view.matching_count().unwrap(), 1_000_000);
    assert_eq!(
        view.root_entry().unwrap().logical,
        c.root_entry(scan).unwrap().logical
    );
    let mut samples = Vec::new();
    for _ in 0..20 {
        let now = std::time::Instant::now();
        let page = view.largest(None, None, 100).unwrap();
        assert_eq!(page.entries.len(), 100);
        samples.push(now.elapsed());
    }
    samples.sort();
    println!("filtered_top100_warm_p95={:?}", samples[18]);
    let now = std::time::Instant::now();
    let summary = view.children_summary(root, 128).unwrap();
    println!(
        "filtered_children_summary={:?} other_count={}",
        now.elapsed(),
        summary.other_count
    );
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
}

#[test]
fn filters_and_aliases_reject_payload_key_corruption() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let c = fixture();
    let mut wrong = c
        .entry(spec(1).key.id, NodeId::new(4).unwrap())
        .unwrap()
        .entry;
    wrong.display_name = "corrupt-only-match".into();
    c.conn
        .execute(
            "UPDATE entries SET data=? WHERE scan=1 AND node=3",
            [encode(&wrong).unwrap()],
        )
        .unwrap();
    let reader = c.reader().unwrap();
    assert!(matches!(
        reader.filtered(
            spec(1).key.id,
            Metric::Logical,
            &QueryFilter {
                name: Some("corrupt-only-match".into()),
                ..Default::default()
            },
            &Cancellation::new()
        ),
        Err(StoreError::Corrupt(_))
    ));
    assert!(matches!(
        reader.aliases(spec(1).key.id, NodeId::new(3).unwrap(), None, 10),
        Err(StoreError::Corrupt(_))
    ));
}
#[test]
fn unknown_bytes_are_not_known_zero_items() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let c = fixture();
    let p = c
        .children_summary(
            spec(1).key.id,
            NodeId::new(2).unwrap(),
            Metric::Allocated,
            10,
        )
        .unwrap();
    assert_eq!(p.unknown_count, 1);
    assert_eq!(p.zero_count, 0);
}
#[test]
fn cancelled_active_filter_releases_pin_and_preserves_base() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let mut c = Cache::open(dir()).unwrap();
    let s = spec(1);
    begin(&mut c, &s);
    c.ingest(&ScanBatch {
        key: s.key,
        sequence: 0,
        records: vec![entry(1, None, b"root", None, false)],
    })
    .unwrap();
    for sequence in 1..=20 {
        let records = (0..1000)
            .map(|n| {
                let id = 2 + (sequence - 1) * 1000 + n;
                entry(id, Some(1), format!("file{id}").as_bytes(), Some(id), true)
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
        sequence: 21,
        records: vec![finished(1)],
    })
    .unwrap();
    publish(&mut c, &s);
    let reader = c.reader().unwrap();
    let cancel = Cancellation::new();
    let signal = cancel.clone();
    let thread = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(10));
        signal.cancel();
    });
    let start = std::time::Instant::now();
    let result = reader.filtered(
        s.key.id,
        Metric::Logical,
        &QueryFilter {
            min_size: Some(0),
            ..Default::default()
        },
        &cancel,
    );
    thread.join().unwrap();
    assert!(result.is_err());
    assert!(start.elapsed() < std::time::Duration::from_secs(2));
    assert_eq!(reader.latest(s.root_id).unwrap(), Some(s.key.id));
    basic(&mut c, 2);
    basic(&mut c, 3);
    assert!(c.status(s.key.id).is_err());
}
#[test]
fn deep_sparse_subtree_and_directory_component_search() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let mut c = Cache::open(dir()).unwrap();
    let s = spec(1);
    begin(&mut c, &s);
    let mut records = vec![entry(1, None, b"root", None, false)];
    for node in 2..=100 {
        records.push(entry(
            node,
            Some(node - 1),
            format!("depth{node}").as_bytes(),
            None,
            false,
        ));
    }
    records.push(entry(101, Some(100), b"leaf", Some(9), true));
    for node in (2..=100).rev() {
        records.push(finished(node));
    }
    records.push(entry(102, Some(1), b"outside", Some(999), true));
    records.push(finished(1));
    ingest(&mut c, &s, records);
    publish(&mut c, &s);
    let now = std::time::Instant::now();
    let p = c
        .largest_in(
            s.key.id,
            Some(NodeId::new(80).unwrap()),
            Metric::Logical,
            None,
            10,
        )
        .unwrap();
    println!("deep_sparse_subtree={:?}", now.elapsed());
    assert_eq!(p.entries.len(), 1);
    assert_eq!(p.entries[0].entry.node.get(), 101);
    let reader = c.reader().unwrap();
    let now = std::time::Instant::now();
    let view = reader
        .filtered(
            s.key.id,
            Metric::Logical,
            &QueryFilter {
                path: Some("depth80/".into()),
                ..Default::default()
            },
            &Cancellation::new(),
        )
        .unwrap();
    println!("deep_path_filter={:?}", now.elapsed());
    assert_eq!(view.root_entry().unwrap().logical.observed_bytes, 9);
    assert_eq!(view.matching_count().unwrap(), 1);
}
#[test]
fn removal_rejects_pins_and_repoints_latest_without_source_io() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let mut c = fixture();
    let old = spec(1).key.id;
    let pin = c.pin(old).unwrap();
    assert!(c.remove_scan(old).is_err());
    assert!(c.clear_cache().is_err());
    drop(pin);
    basic(&mut c, 2);
    c.remove_scan(spec(2).key.id).unwrap();
    assert_eq!(c.latest(spec(1).root_id).unwrap(), Some(old));
    assert!(c.footprint().unwrap().database_bytes > 0);
    c.clear_cache().unwrap();
    assert!(c.roots().unwrap().is_empty());
    assert!(c.scans(None, 10).unwrap().scans.is_empty());
    assert!(c.latest(spec(1).root_id).unwrap().is_none());
}
#[test]
fn diagnostic_pages_have_stable_ids_and_bounded_issues() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let mut c = Cache::open(dir()).unwrap();
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
                class: IssueClass::Io,
                detail: "one".into(),
            }),
            ScanRecord::Issue(ScanIssue {
                node: None,
                parent: Some(NodeId::new(1).unwrap()),
                name: None,
                class: IssueClass::Io,
                detail: "two".into(),
            }),
            finished(1),
        ],
    );
    publish(&mut c, &s);
    basic(&mut c, 2);
    let p = c.scans(None, 1).unwrap();
    assert_eq!(p.scans[0].spec.key.id, spec(2).key.id);
    let p = c.scans(p.next, 1).unwrap();
    assert_eq!(p.scans[0].spec.key.id, s.key.id);
    let p = c.issues(s.key.id, None, 1).unwrap();
    assert_eq!(p.issues[0].detail, "one");
    let p = c.issues(s.key.id, p.next, 1).unwrap();
    assert_eq!(p.issues[0].detail, "two");
    assert!(p.next.is_none());
}

#[test]
fn removal_never_reuses_stale_scan_identity() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let mut c = fixture();
    let old = spec(1);
    c.clear_cache().unwrap();
    assert_eq!(c.next_scan_id().unwrap().get(), 2);
    assert_eq!(c.next_root_id().unwrap().get(), 2);
    assert!(
        c.begin(
            &old,
            &NativePath::from_path(Path::new("/offline/source")),
            Timestamp {
                seconds: 3,
                nanos: 0
            }
        )
        .is_err()
    );
    let mut newer = spec(2);
    newer.root_id = RootId::new(2).unwrap();
    newer.lineage_id = LineageId::new(2).unwrap();
    begin(&mut c, &newer);
    assert!(
        c.ingest(&ScanBatch {
            key: old.key,
            sequence: 0,
            records: vec![]
        })
        .is_err()
    );
}

#[test]
#[ignore = "requires SPACE_QUERY_BENCHMARK_CACHE pointing to million_entry_fixture output"]
fn million_sparse_subtree_benchmark() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let p = std::env::var_os("SPACE_QUERY_BENCHMARK_CACHE").unwrap();
    let c = Cache::open(p).unwrap();
    let mut times = Vec::new();
    for _ in 0..20 {
        let start = std::time::Instant::now();
        let page = c
            .largest_in(
                spec(1).key.id,
                Some(NodeId::new(2).unwrap()),
                Metric::Logical,
                None,
                100,
            )
            .unwrap();
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].entry.node.get(), 2);
        times.push(start.elapsed());
    }
    times.sort();
    println!("one_entry_subtree_in_million_warm_p95={:?}", times[18]);
}

#[test]
fn filtered_matching_leaf_without_totals_fails_closed() {
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
    let mut c = fixture();
    let reader = c.reader().unwrap();
    let previous = reader
        .filtered(
            spec(1).key.id,
            Metric::Logical,
            &QueryFilter {
                extension: Some("log".into()),
                ..Default::default()
            },
            &Cancellation::new(),
        )
        .unwrap();
    let previous_bytes = previous.root_entry().unwrap().logical.observed_bytes;
    c.conn
        .execute("DELETE FROM totals WHERE scan=1 AND node=3", [])
        .unwrap();
    assert!(
        reader
            .filtered(
                spec(1).key.id,
                Metric::Logical,
                &QueryFilter {
                    name: Some("alpha".into()),
                    ..Default::default()
                },
                &Cancellation::new()
            )
            .is_err()
    );
    assert_eq!(
        previous.root_entry().unwrap().logical.observed_bytes,
        previous_bytes
    );
    assert_eq!(c.latest(spec(1).root_id).unwrap(), Some(spec(1).key.id));
    drop(previous);
    c.remove_scan(spec(1).key.id).unwrap(); // Failed construction leaked no lifetime pin.
}
