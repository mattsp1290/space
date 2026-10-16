use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
// Subprocess fork/exec briefly inherits other tests' flock descriptors. Exclude
// those spawn fixtures from ordinary tests' last-close/immediate-reopen windows.
// This changes test scheduling only, never production locking or retries.
pub(super) static PROCESS_GATE: std::sync::RwLock<()> = std::sync::RwLock::new(());
static NEXT: AtomicU64 = AtomicU64::new(0);
pub(super) fn dir() -> std::path::PathBuf {
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
pub(super) fn identity() -> RootIdentity {
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
pub(super) fn spec(n: u64) -> ScanSpec {
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
pub(super) fn entry(
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
pub(super) fn finished(node: u64) -> ScanRecord {
    ScanRecord::DirectoryFinished {
        node: NodeId::new(node).unwrap(),
        enumeration: EnumerationState::Complete,
    }
}
pub(super) fn begin(c: &mut Cache, s: &ScanSpec) {
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
pub(super) fn ingest(c: &mut Cache, s: &ScanSpec, records: Vec<ScanRecord>) {
    c.ingest(&ScanBatch {
        key: s.key,
        sequence: 0,
        records,
    })
    .unwrap();
}
pub(super) fn publish(c: &mut Cache, s: &ScanSpec) {
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
pub(super) fn basic(c: &mut Cache, n: u64) {
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
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
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
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
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
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
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
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
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
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
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
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
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
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
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
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
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
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
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
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
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
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
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
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
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
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
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
    let _process_gate = crate::tests::PROCESS_GATE.write().unwrap();
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
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
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
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
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
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
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
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
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
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
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
    let _process_gate = crate::tests::PROCESS_GATE.write().unwrap();
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
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
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
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
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
    let _process_gate = crate::tests::PROCESS_GATE.read().unwrap();
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
