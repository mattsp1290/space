use crate::*;
fn observed(scheme: IdentityScheme, value: u8) -> ObservedIdentity {
    ObservedIdentity {
        scheme,
        value: vec![value],
        strength: IdentityStrength::Observed,
    }
}
fn root() -> RootIdentity {
    RootIdentity {
        volume: observed(IdentityScheme::UnixDevice, 1),
        directory: observed(IdentityScheme::UnixInode, 2),
    }
}
fn entry() -> EntryRecord {
    EntryRecord {
        node: NodeId::new(1).unwrap(),
        parent: None,
        name: None,
        display_name: "root".into(),
        kind: EntryKind::Directory,
        identity: None,
        logical_bytes: None,
        allocated_bytes: None,
        modified: None,
        link_state: LinkState::None,
        metadata_state: MetadataState::Complete,
    }
}
#[test]
fn ids_enforce_positive_sqlite_range() {
    assert!(ScanId::new(0).is_err());
    assert!(ScanId::new(u64::MAX).is_err());
    assert!(RootId::try_from(-1).is_err());
    assert!(NodeId::try_from(0).is_err());
    assert!(LineageId::new(i64::MAX as u64).is_ok());
    assert_eq!(ScanId::new(4).unwrap().get(), 4);
}
#[test]
fn native_name_rejects_navigation_and_separators() {
    for value in [b"".as_slice(), b".", b"..", b"a/b", b"a\0b"] {
        assert!(NativeName::unix(value.to_vec()).is_err());
    }
    for value in ["", ".", "..", "a/b", "a\\b", "C:", "a\0b"] {
        assert!(NativeName::windows(value.encode_utf16().collect()).is_err());
    }
    assert!(NativeName::unix(b"a\\b:c".to_vec()).is_ok());
}
#[test]
fn native_names_preserve_raw_values() {
    let unix = NativeName::unix(vec![b'a', 255]).unwrap();
    assert_eq!(
        NativeName::from_bytes(unix.encoding(), &unix.to_bytes()).unwrap(),
        unix
    );
    assert!(unix.display().contains('\u{fffd}'));
    let win = NativeName::windows(vec![0xd800, 0x61]).unwrap();
    assert_eq!(
        NativeName::from_bytes(win.encoding(), &win.to_bytes()).unwrap(),
        win
    );
    assert!(win.display().contains('\u{fffd}'));
    assert!(NativeName::from_bytes(NativeEncoding::WindowsUtf16, &[1]).is_err());
}
#[test]
fn windows_sort_is_code_unit_order_not_little_endian_bytes() {
    let a = NativeName::windows(vec![0xff]).unwrap();
    let b = NativeName::windows(vec![0x100]).unwrap();
    assert!(a < b);
    assert!(a.sort_key() < b.sort_key());
    assert!(a.to_bytes() > b.to_bytes());
    let shorter = NativeName::windows(vec![65]).unwrap();
    let longer = NativeName::windows(vec![65, 66]).unwrap();
    assert!(shorter.sort_key() < longer.sort_key());
}
#[test]
fn names_are_case_sensitive_and_lossy_display_is_not_identity() {
    assert_ne!(
        NativeName::unix(b"A".to_vec()).unwrap(),
        NativeName::unix(b"a".to_vec()).unwrap()
    );
    let a = NativeName::unix(vec![255]).unwrap();
    let b = NativeName::unix(vec![254]).unwrap();
    assert_eq!(a.display(), b.display());
    assert_ne!(a, b);
}
#[test]
fn selected_path_roundtrips_on_current_platform() {
    let p = std::env::temp_dir().join("Space test");
    let native = NativePath::from_path(&p);
    assert_eq!(native.to_path_buf().unwrap(), p);
}
#[cfg(unix)]
#[test]
fn unix_os_conversion_and_foreign_path_rejection() {
    use std::os::unix::ffi::OsStringExt;
    let value = std::ffi::OsString::from_vec(vec![255, 97]);
    let name = NativeName::from_os(&value).unwrap();
    assert_eq!(name.to_os().unwrap(), value);
    assert!(NativeName::windows(vec![97]).unwrap().to_os().is_err());
    assert!(
        NativePath {
            encoding: NativeEncoding::WindowsUtf16,
            bytes: vec![97, 0]
        }
        .to_path_buf()
        .is_err()
    );
}
#[cfg(windows)]
#[test]
fn windows_os_conversion_and_foreign_path_rejection() {
    use std::os::windows::ffi::OsStringExt;
    let value = std::ffi::OsString::from_wide(&[0xd800, 97]);
    let name = NativeName::from_os(&value).unwrap();
    assert_eq!(name.to_os().unwrap(), value);
    assert!(NativeName::unix(vec![97]).unwrap().to_os().is_err());
}
#[test]
fn malformed_full_paths_rejected() {
    for bytes in [vec![], vec![97, 0]] {
        assert!(
            NativePath {
                encoding: NativeEncoding::UnixBytes,
                bytes
            }
            .validate()
            .is_err()
        );
    }
    assert!(
        NativePath {
            encoding: NativeEncoding::WindowsUtf16,
            bytes: vec![97]
        }
        .validate()
        .is_err()
    );
    assert!(
        NativePath {
            encoding: NativeEncoding::WindowsUtf16,
            bytes: vec![0, 0]
        }
        .validate()
        .is_err()
    );
}
#[test]
fn root_identity_requires_directory_and_volume() {
    let original = root();
    assert_eq!(
        original.validate_against(&original),
        IdentityValidation::MatchingObservation
    );
    let mut replacement = original.clone();
    replacement.directory.value = vec![3];
    assert_eq!(
        original.validate_against(&replacement),
        IdentityValidation::Changed
    );
    replacement = original.clone();
    replacement.volume.value = vec![3];
    assert_eq!(
        original.validate_against(&replacement),
        IdentityValidation::Changed
    );
    replacement = original.clone();
    replacement.directory.strength = IdentityStrength::Weak;
    assert_eq!(
        original.validate_against(&replacement),
        IdentityValidation::RequiresSelection
    );
    replacement = original.clone();
    replacement.volume.value.clear();
    assert_eq!(
        original.validate_against(&replacement),
        IdentityValidation::RequiresSelection
    );
}
#[test]
fn identities_are_scoped_and_only_observed_values_deduplicate() {
    let a = FileIdentity {
        volume: root().volume,
        file: root().directory,
    };
    assert!(a.is_deduplicable());
    let mut b = a.clone();
    b.volume.value = vec![9];
    assert_ne!(a, b);
    b.file.strength = IdentityStrength::Weak;
    assert!(!b.is_deduplicable());
}
#[test]
fn entry_parent_and_root_contract() {
    let mut e = entry();
    assert!(e.validate().is_ok());
    e.kind = EntryKind::RegularFile;
    assert!(e.validate().is_err());
    e.node = NodeId::new(2).unwrap();
    e.parent = Some(NodeId::new(1).unwrap());
    e.name = Some(NativeName::unix(vec![97]).unwrap());
    assert!(e.validate().is_ok());
    e.parent = Some(e.node);
    assert!(e.validate().is_err());
    e.parent = Some(NodeId::new(3).unwrap());
    assert!(e.validate().is_err());
}
#[test]
fn metadata_timestamps_validate_after_deserialization() {
    assert!(Timestamp::new(-2, 999_999_999).is_ok());
    assert!(Timestamp::new(0, 1_000_000_000).is_err());
    let mut e = entry();
    e.modified = Some(Timestamp {
        seconds: 0,
        nanos: u32::MAX,
    });
    assert!(e.validate().is_err());
}
#[test]
fn unknown_bytes_never_become_known_zero() {
    assert_ne!(
        MetricTotal::observation(None, false),
        MetricTotal::observation(Some(0), false)
    );
    let sum = MetricTotal::observation(Some(42), false)
        .checked_add(MetricTotal::observation(None, true))
        .unwrap();
    assert_eq!(
        sum,
        MetricTotal {
            observed_bytes: 42,
            unknown_count: 1,
            estimated_count: 1
        }
    );
}
#[test]
fn all_aggregate_fields_check_overflow() {
    assert!(
        MetricTotal {
            observed_bytes: u64::MAX,
            ..Default::default()
        }
        .checked_add(MetricTotal::observation(Some(1), false))
        .is_err()
    );
    assert!(
        MetricTotal {
            unknown_count: u64::MAX,
            ..Default::default()
        }
        .checked_add(MetricTotal::observation(None, false))
        .is_err()
    );
    assert!(
        MetricTotal {
            estimated_count: u64::MAX,
            ..Default::default()
        }
        .checked_add(MetricTotal::observation(Some(0), true))
        .is_err()
    );
    assert!(
        CoverageSummary {
            boundary_scopes: u64::MAX,
            ..Default::default()
        }
        .checked_add(CoverageSummary {
            boundary_scopes: 1,
            ..Default::default()
        })
        .is_err()
    );
    assert_eq!(sqlite_integer(i64::MAX as u64).unwrap(), i64::MAX);
    assert!(sqlite_integer(i64::MAX as u64 + 1).is_err());
}
#[test]
fn coverage_does_not_hide_intentional_skips() {
    assert!(CoverageSummary::default().is_complete());
    for c in [
        CoverageSummary {
            unknown_entries: 1,
            ..Default::default()
        },
        CoverageSummary {
            error_count: 1,
            ..Default::default()
        },
        CoverageSummary {
            excluded_scopes: 1,
            ..Default::default()
        },
        CoverageSummary {
            boundary_scopes: 1,
            ..Default::default()
        },
    ] {
        assert!(!c.is_complete());
    }
}
#[test]
fn policy_exclusions_normalize_and_validate() {
    let a = vec![NativeName::unix(vec![97]).unwrap()];
    let b = vec![NativeName::unix(vec![98]).unwrap()];
    let mut p = ScanPolicy {
        exclusions: vec![b.clone(), a.clone(), b.clone()],
        ..Default::default()
    };
    p.normalize();
    assert_eq!(p.exclusions, vec![a, b]);
    assert!(p.validate().is_ok());
    p.exclusions.push(vec![]);
    assert!(p.validate().is_err());
    p = ScanPolicy::default();
    p.version += 1;
    assert!(p.validate().is_err());
}
#[test]
fn cancellation_clones_share_only_the_same_job() {
    let a = Cancellation::new();
    let b = a.clone();
    let c = Cancellation::new();
    a.cancel();
    assert!(b.is_cancelled());
    assert!(!c.is_cancelled());
}
#[test]
fn bounded_error_detail_preserves_utf8() {
    let detail = "é".repeat(1000);
    let bounded = ScanIssue::bounded_detail(&detail);
    assert!(bounded.len() <= ScanIssue::MAX_DETAIL_BYTES);
    assert!(detail.starts_with(&bounded));
}

#[test]
fn deserialized_ids_cannot_bypass_range_checks() {
    use serde::Deserialize;
    use serde::de::value::{Error, I64Deserializer};
    assert!(ScanId::deserialize(I64Deserializer::<Error>::new(-1)).is_err());
    assert!(NodeId::deserialize(I64Deserializer::<Error>::new(0)).is_err());
    assert_eq!(
        RootId::deserialize(I64Deserializer::<Error>::new(42))
            .unwrap()
            .get(),
        42
    );
}

#[test]
fn direct_public_enum_construction_is_validated_at_boundary() {
    assert!(NativeName::Unix(vec![]).validate().is_err());
    let mut e = entry();
    e.node = NodeId::new(2).unwrap();
    e.parent = Some(NodeId::new(1).unwrap());
    e.name = Some(NativeName::Windows(vec![92, 92, 97]));
    assert!(e.validate().is_err());
}

#[test]
fn exclusions_require_the_selected_roots_encoding() {
    let unix = NativeName::unix(vec![97]).unwrap();
    let windows = NativeName::windows(vec![97]).unwrap();
    let mut policy = ScanPolicy {
        exclusions: vec![vec![windows.clone()]],
        ..Default::default()
    };
    assert!(policy.validate().is_ok());
    assert!(
        policy
            .validate_for_encoding(NativeEncoding::UnixBytes)
            .is_err()
    );
    assert!(
        policy
            .validate_for_encoding(NativeEncoding::WindowsUtf16)
            .is_ok()
    );
    policy.exclusions.push(vec![unix.clone()]);
    assert!(
        policy
            .validate_for_encoding(NativeEncoding::UnixBytes)
            .is_err()
    );
    assert!(
        policy
            .validate_for_encoding(NativeEncoding::WindowsUtf16)
            .is_err()
    );
    policy.exclusions = vec![vec![unix]];
    assert!(
        policy
            .validate_for_encoding(NativeEncoding::UnixBytes)
            .is_ok()
    );
}

#[test]
fn cancellation_winning_prevents_publication() {
    let cancel = Cancellation::new();
    cancel.cancel();
    assert_eq!(
        cancel.with_publication_guard(|| panic!("must not publish")),
        None::<()>
    );
}

#[test]
fn publication_is_serialized_before_later_cancel() {
    let cancel = Cancellation::new();
    let other = cancel.clone();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let publisher = std::thread::spawn(move || {
        other.with_publication_guard(|| {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            42
        })
    });
    started_rx.recv().unwrap();
    let other = cancel.clone();
    let canceller = std::thread::spawn(move || other.cancel());
    release_tx.send(()).unwrap();
    assert_eq!(publisher.join().unwrap(), Some(42));
    canceller.join().unwrap();
    assert!(cancel.is_cancelled());
    assert_eq!(cancel.with_publication_guard(|| 99), None);
}

#[test]
fn panicked_publication_guard_fails_closed() {
    let cancel = Cancellation::new();
    let other = cancel.clone();
    assert!(
        std::thread::spawn(
            move || other.with_publication_guard(|| panic!("simulated commit panic"))
        )
        .join()
        .is_err()
    );
    assert_eq!(cancel.with_publication_guard(|| 42), None);
    cancel.cancel();
    assert!(cancel.is_cancelled());
}

#[test]
fn directory_identity_exclusions_are_strong_and_normalized() {
    let excluded = root();
    let mut policy = ScanPolicy {
        excluded_directory_identities: vec![excluded.clone(), excluded.clone()],
        ..Default::default()
    };
    assert!(policy.validate().is_ok());
    policy.normalize();
    assert_eq!(policy.excluded_directory_identities, vec![excluded.clone()]);
    policy.excluded_directory_identities[0].directory.strength = IdentityStrength::Weak;
    assert!(policy.validate().is_err());
    policy.excluded_directory_identities[0] = excluded.clone();
    policy.excluded_directory_identities[0].volume.value.clear();
    assert!(policy.validate().is_err());
    policy.excluded_directory_identities[0] = excluded;
    policy.excluded_directory_identities[0].directory.scheme = IdentityScheme::UnixDevice;
    assert!(policy.validate().is_err());
}

#[test]
fn directory_identity_matching_ignores_paths_but_respects_volume() {
    let excluded = root();
    let mut different = excluded.clone();
    assert_eq!(
        excluded.validate_against(&different),
        IdentityValidation::MatchingObservation
    );
    different.volume.value = vec![42];
    assert_eq!(
        excluded.validate_against(&different),
        IdentityValidation::Changed
    );
    let windows = RootIdentity {
        volume: observed(IdentityScheme::WindowsVolume, 1),
        directory: observed(IdentityScheme::WindowsFileId, 2),
    };
    assert!(windows.is_valid_exclusion());
}
