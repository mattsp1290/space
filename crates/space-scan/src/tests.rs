use super::*;
use std::{
    fs,
    sync::atomic::{AtomicU64, Ordering},
};
static SERIAL: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "space-scan-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }
    fn spec(&self) -> ScanSpec {
        ScanSpec {
            key: ScanKey {
                id: ScanId::new(1).unwrap(),
                generation: 17,
            },
            root_id: RootId::new(1).unwrap(),
            lineage_id: LineageId::new(1).unwrap(),
            expected_root: inspect_root(&self.0).unwrap(),
            policy: ScanPolicy::default(),
        }
    }
    fn collect(&self, spec: &ScanSpec, limits: ScanLimits) -> (ScanFinish, Vec<ScanRecord>) {
        let mut records = Vec::new();
        let mut sequence = 0;
        let finish = scan(&self.0, spec, &Cancellation::new(), limits, |batch| {
            assert_eq!(batch.key, spec.key);
            assert_eq!(batch.sequence, sequence);
            sequence += 1;
            assert!(batch.records.len() <= limits.batch_size);
            records.extend(batch.records);
            Ok(())
        })
        .unwrap();
        (finish, records)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn entries(records: &[ScanRecord]) -> Vec<&EntryRecord> {
    records
        .iter()
        .filter_map(|r| {
            if let ScanRecord::Entry(e) = r {
                Some(e)
            } else {
                None
            }
        })
        .collect()
}
#[test]
fn native_metadata_batches_and_hardlinks() {
    let f = Fixture::new();
    fs::create_dir(f.0.join("nested")).unwrap();
    fs::write(f.0.join("payload"), b"1234567").unwrap();
    fs::hard_link(f.0.join("payload"), f.0.join("nested/alias")).unwrap();
    let (finish, records) = f.collect(
        &f.spec(),
        ScanLimits {
            batch_size: 1,
            ..Default::default()
        },
    );
    assert_eq!(finish.outcome, TraversalOutcome::Finished);
    let files: Vec<_> = entries(&records)
        .into_iter()
        .filter(|e| e.kind == EntryKind::RegularFile)
        .collect();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].identity, files[1].identity);
    assert!(
        files
            .iter()
            .all(|e| e.logical_bytes == Some(7)
                && matches!(e.link_state, LinkState::Hardlink { .. }))
    );
    assert!(
        records.iter().all(|r| !matches!(r, ScanRecord::Issue(_))),
        "{records:?}"
    );
}
#[test]
fn sparse_metadata_has_independent_allocation() {
    let f = Fixture::new();
    let file = fs::File::create(f.0.join("sparse")).unwrap();
    file.set_len(8 * 1024 * 1024).unwrap();
    drop(file);
    let (_, records) = f.collect(&f.spec(), Default::default());
    let file = entries(&records)
        .into_iter()
        .find(|e| e.kind == EntryKind::RegularFile)
        .unwrap();
    assert_eq!(file.logical_bytes, Some(8 * 1024 * 1024));
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            file.allocated_bytes,
            fs::symlink_metadata(f.0.join("sparse"))
                .unwrap()
                .blocks()
                .checked_mul(512)
        );
    }
}
#[test]
fn exclusions_include_files_and_directories() {
    let f = Fixture::new();
    fs::create_dir(f.0.join("hidden")).unwrap();
    fs::write(f.0.join("hidden/secret"), b"a").unwrap();
    fs::write(f.0.join("excluded"), b"b").unwrap();
    let mut spec = f.spec();
    spec.policy.exclusions = ["hidden", "excluded"]
        .iter()
        .map(|s| vec![NativeName::from_os(s.as_ref()).unwrap()])
        .collect();
    let (_, records) = f.collect(&spec, Default::default());
    assert_eq!(entries(&records).len(), 1);
    assert_eq!(
        records
            .iter()
            .filter(|r| matches!(
                r,
                ScanRecord::Issue(ScanIssue {
                    class: IssueClass::Excluded,
                    ..
                })
            ))
            .count(),
        2
    );
}
#[test]
fn cancellation_in_callback_stops_traversal() {
    let f = Fixture::new();
    for i in 0..50 {
        fs::write(f.0.join(i.to_string()), []).unwrap();
    }
    let cancel = Cancellation::new();
    let mut batches = 0;
    let finish = scan(
        &f.0,
        &f.spec(),
        &cancel,
        ScanLimits {
            batch_size: 1,
            ..Default::default()
        },
        |_| {
            batches += 1;
            cancel.cancel();
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(finish.outcome, TraversalOutcome::Cancelled);
    assert!(batches <= 2);
}
#[test]
fn sink_failure_propagates_without_claiming_completion() {
    let f = Fixture::new();
    let error = scan(
        &f.0,
        &f.spec(),
        &Cancellation::new(),
        ScanLimits {
            batch_size: 1,
            ..Default::default()
        },
        |_| Err(io::Error::other("writer full")),
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "writer full");
}
#[test]
fn depth_and_entry_limits_have_explicit_coverage() {
    let f = Fixture::new();
    fs::create_dir_all(f.0.join("a/b/c")).unwrap();
    fs::write(f.0.join("a/b/c/data"), b"x").unwrap();
    let (_, records) = f.collect(
        &f.spec(),
        ScanLimits {
            max_depth: 1,
            ..Default::default()
        },
    );
    assert!(records.iter().any(|r| matches!(
        r,
        ScanRecord::DirectoryFinished {
            enumeration: EnumerationState::ResourceLimit,
            ..
        }
    )));
    assert!(!entries(&records).iter().any(|e| e.display_name == "data"));
    let (_, records) = f.collect(
        &f.spec(),
        ScanLimits {
            max_entries: 1,
            ..Default::default()
        },
    );
    assert_eq!(entries(&records).len(), 1);
    assert!(records.iter().any(|r| matches!(
        r,
        ScanRecord::Issue(ScanIssue {
            class: IssueClass::ResourceLimit,
            ..
        })
    )));
}
#[test]
fn selected_root_replacement_is_not_continuity() {
    let f = Fixture::new();
    fs::create_dir(f.0.join("root")).unwrap();
    let old = inspect_root(&f.0.join("root")).unwrap();
    fs::rename(f.0.join("root"), f.0.join("old")).unwrap();
    fs::create_dir(f.0.join("root")).unwrap();
    assert_eq!(
        validate_root(&f.0.join("root"), &old).unwrap(),
        IdentityValidation::Changed
    );
}
#[cfg(target_os = "linux")]
#[test]
fn raw_names_and_symlink_cycles_are_preserved_not_followed() {
    use std::os::unix::{ffi::OsStringExt, fs::symlink};
    let f = Fixture::new();
    let name = std::ffi::OsString::from_vec(vec![b'x', 0xff]);
    fs::write(f.0.join(&name), b"x").unwrap();
    symlink(&f.0, f.0.join("cycle")).unwrap();
    let (_, records) = f.collect(&f.spec(), Default::default());
    assert!(
        entries(&records)
            .iter()
            .any(|e| e.name == Some(NativeName::Unix(vec![b'x', 0xff])))
    );
    assert_eq!(entries(&records).len(), 3);
    assert!(
        entries(&records)
            .iter()
            .any(|e| e.kind == EntryKind::Symlink)
    );
}
#[cfg(unix)]
#[test]
fn directory_substitution_between_inspection_and_open_never_enumerates_target() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let target = Fixture::new();
    fs::write(target.0.join("must-not-see"), b"secret").unwrap();
    fs::create_dir(f.0.join("victim")).unwrap();
    let mut records = Vec::new();
    let mut changed = false;
    let finish = scan_impl(
        &f.0,
        &f.spec(),
        &Cancellation::new(),
        Default::default(),
        |batch| {
            records.extend(batch.records);
            Ok(())
        },
        &mut |path| {
            if !changed && path.last().unwrap().display() == "victim" {
                fs::remove_dir(f.0.join("victim")).unwrap();
                symlink(&target.0, f.0.join("victim")).unwrap();
                changed = true;
            }
        },
    )
    .unwrap();
    assert!(changed);
    assert_eq!(finish.outcome, TraversalOutcome::Finished);
    assert!(
        !entries(&records)
            .iter()
            .any(|e| e.display_name == "must-not-see")
    );
    assert!(records.iter().any(|r| matches!(
        r,
        ScanRecord::Issue(ScanIssue {
            class: IssueClass::UnsafeDescent,
            ..
        })
    )));
}
#[cfg(unix)]
#[test]
fn directory_replacement_with_directory_is_detected_before_enumeration() {
    let f = Fixture::new();
    fs::create_dir(f.0.join("victim")).unwrap();
    let mut records = Vec::new();
    let mut changed = false;
    scan_impl(
        &f.0,
        &f.spec(),
        &Cancellation::new(),
        Default::default(),
        |b| {
            records.extend(b.records);
            Ok(())
        },
        &mut |path| {
            if !changed && path.last().unwrap().display() == "victim" {
                fs::rename(f.0.join("victim"), f.0.join("old")).unwrap();
                fs::create_dir(f.0.join("victim")).unwrap();
                fs::write(f.0.join("victim/redirected"), b"x").unwrap();
                changed = true;
            }
        },
    )
    .unwrap();
    assert!(
        !entries(&records)
            .iter()
            .any(|e| e.display_name == "redirected")
    );
}
#[cfg(target_os = "linux")]
#[test]
fn virtual_root_is_explicitly_skipped() {
    let path = Path::new("/proc");
    let spec = ScanSpec {
        expected_root: inspect_root(path).unwrap(),
        ..Fixture::new().spec()
    };
    let mut records = Vec::new();
    scan(path, &spec, &Cancellation::new(), Default::default(), |b| {
        records.extend(b.records);
        Ok(())
    })
    .unwrap();
    assert!(records.iter().any(|r| matches!(
        r,
        ScanRecord::Issue(ScanIssue {
            class: IssueClass::VirtualFilesystem,
            ..
        })
    )));
    assert_eq!(entries(&records).len(), 1);
}

#[cfg(unix)]
#[test]
fn vanished_directory_is_typed_incomplete_coverage() {
    let f = Fixture::new();
    fs::create_dir(f.0.join("victim")).unwrap();
    let mut records = Vec::new();
    scan_impl(
        &f.0,
        &f.spec(),
        &Cancellation::new(),
        Default::default(),
        |b| {
            records.extend(b.records);
            Ok(())
        },
        &mut |_| {
            fs::remove_dir(f.0.join("victim")).unwrap();
        },
    )
    .unwrap();
    assert!(records.iter().any(|r| matches!(
        r,
        ScanRecord::Issue(ScanIssue {
            class: IssueClass::Vanished,
            ..
        })
    )));
}
#[cfg(unix)]
#[test]
fn permission_denial_is_coverage_not_empty_directory() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let denied = f.0.join("denied");
    fs::create_dir(&denied).unwrap();
    fs::set_permissions(&denied, fs::Permissions::from_mode(0o0)).unwrap();
    let unprivileged = fs::read_dir(&denied).is_err();
    let (_, records) = f.collect(&f.spec(), Default::default());
    fs::set_permissions(&denied, fs::Permissions::from_mode(0o700)).unwrap();
    if unprivileged {
        assert!(records.iter().any(|r| matches!(
            r,
            ScanRecord::Issue(ScanIssue {
                class: IssueClass::PermissionDenied,
                ..
            })
        )));
    } else {
        eprintln!(
            "Permission-denial assertion unavailable with this privileged account; native non-root CI is required"
        );
    }
}

#[test]
fn excluded_candidates_still_consume_work_budget() {
    let f = Fixture::new();
    let mut spec = f.spec();
    for i in 0..20 {
        let name = i.to_string();
        fs::write(f.0.join(&name), []).unwrap();
        spec.policy
            .exclusions
            .push(vec![NativeName::from_os(name.as_ref()).unwrap()]);
    }
    let (_, records) = f.collect(
        &spec,
        ScanLimits {
            max_entries: 3,
            ..Default::default()
        },
    );
    assert_eq!(entries(&records).len(), 1);
    assert_eq!(
        records
            .iter()
            .filter(|r| matches!(
                r,
                ScanRecord::Issue(ScanIssue {
                    class: IssueClass::Excluded,
                    ..
                })
            ))
            .count(),
        2
    );
    assert!(records.iter().any(|r| matches!(
        r,
        ScanRecord::Issue(ScanIssue {
            class: IssueClass::ResourceLimit,
            ..
        })
    )));
}

#[test]
fn identity_exclusion_survives_directory_rename() {
    let f = Fixture::new();
    fs::create_dir(f.0.join("cache")).unwrap();
    fs::write(f.0.join("cache/private"), b"metadata").unwrap();
    let cache = inspect_root(&f.0.join("cache")).unwrap();
    fs::rename(f.0.join("cache"), f.0.join("renamed-cache")).unwrap();
    let mut spec = f.spec();
    spec.policy.excluded_directory_identities.push(cache);
    let (_, records) = f.collect(&spec, Default::default());
    assert!(
        !entries(&records)
            .iter()
            .any(|entry| entry.display_name == "private")
    );
    assert!(records.iter().any(|record| matches!(
        record,
        ScanRecord::DirectoryFinished {
            enumeration: EnumerationState::Excluded,
            ..
        }
    )));
}
#[test]
fn selecting_identity_excluded_root_is_refused() {
    let f = Fixture::new();
    let mut spec = f.spec();
    spec.policy
        .excluded_directory_identities
        .push(spec.expected_root.clone());
    let mut emitted = false;
    let result = scan(
        &f.0,
        &spec,
        &Cancellation::new(),
        Default::default(),
        |_| {
            emitted = true;
            Ok(())
        },
    );
    assert!(result.is_err());
    assert!(!emitted);
}

#[test]
fn selecting_descendant_of_identity_excluded_root_is_refused() {
    let f = Fixture::new();
    fs::create_dir_all(f.0.join("cache/subfolder")).unwrap();
    let cache = inspect_root(&f.0.join("cache")).unwrap();
    let selected = f.0.join("cache/subfolder");
    let mut spec = f.spec();
    spec.expected_root = inspect_root(&selected).unwrap();
    spec.policy.excluded_directory_identities.push(cache);
    let mut emitted = false;
    let result = scan(
        &selected,
        &spec,
        &Cancellation::new(),
        Default::default(),
        |_| {
            emitted = true;
            Ok(())
        },
    );
    assert!(result.is_err());
    assert!(!emitted);
}

#[cfg(unix)]
#[test]
fn native_unicode_names_and_symlink_cycles_are_preserved_not_followed() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let name = std::ffi::OsStr::new("文件-MixedCase");
    fs::write(fixture.0.join(name), b"x").unwrap();
    symlink(&fixture.0, fixture.0.join("cycle")).unwrap();
    let (_, records) = fixture.collect(&fixture.spec(), Default::default());
    assert!(
        entries(&records)
            .iter()
            .any(|entry| entry.name == Some(NativeName::from_os(name).unwrap()))
    );
    assert_eq!(entries(&records).len(), 3);
    assert!(
        entries(&records)
            .iter()
            .any(|entry| entry.kind == EntryKind::Symlink)
    );
}
