use super::*;
use crate::*;

fn spec(id: u64) -> ScanSpec {
    ScanSpec {
        key: ScanKey {
            id: ScanId::new(id).unwrap(),
            generation: id,
        },
        root_id: RootId::new(1).unwrap(),
        lineage_id: LineageId::new(1).unwrap(),
        expected_root: RootIdentity {
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
        },
        policy: ScanPolicy::default(),
    }
}
fn validate(a: &ScanSpec, b: &ScanSpec) -> Result<(), ComparisonRefusal> {
    validate_comparison(a, ScanStatus::Published, b, ScanStatus::Published)
}
#[test]
fn compatible_immutable_observations_and_normalized_policy() {
    let mut a = spec(1);
    let mut b = spec(2);
    let x = vec![NativeName::unix(b"x".to_vec()).unwrap()];
    let y = vec![NativeName::unix(b"y".to_vec()).unwrap()];
    a.policy.exclusions = vec![x.clone(), y.clone(), x.clone()];
    b.policy.exclusions = vec![y, x];
    assert_eq!(validate(&a, &b), Ok(()));
    assert_eq!(a.policy.exclusions.len(), 3); // validation does not mutate snapshots
}
#[test]
fn compatibility_refuses_each_invalid_pair() {
    let a = spec(1);
    assert_eq!(validate(&a, &a), Err(ComparisonRefusal::SameScan));
    for status in [
        ScanStatus::Scanning,
        ScanStatus::Finalizing,
        ScanStatus::Failed,
        ScanStatus::Cancelled,
        ScanStatus::Interrupted,
    ] {
        assert_eq!(
            validate_comparison(&a, status, &spec(2), ScanStatus::Published),
            Err(ComparisonRefusal::NotPublished)
        );
        assert_eq!(
            validate_comparison(&a, ScanStatus::Published, &spec(2), status),
            Err(ComparisonRefusal::NotPublished)
        );
    }
    let mut b = spec(2);
    b.root_id = RootId::new(2).unwrap();
    assert_eq!(validate(&a, &b), Err(ComparisonRefusal::DifferentRoot));
    b = spec(2);
    b.lineage_id = LineageId::new(2).unwrap();
    assert_eq!(validate(&a, &b), Err(ComparisonRefusal::DifferentLineage));
    b = spec(2);
    b.expected_root.volume.value = vec![9];
    assert_eq!(validate(&a, &b), Err(ComparisonRefusal::UnverifiedIdentity));
    b = spec(2);
    b.expected_root.directory.value = vec![9];
    assert_eq!(validate(&a, &b), Err(ComparisonRefusal::UnverifiedIdentity));
    b = spec(2);
    b.expected_root.directory.strength = IdentityStrength::Weak;
    assert_eq!(validate(&a, &b), Err(ComparisonRefusal::UnverifiedIdentity));
    b = spec(2);
    b.policy.exclude_nested_mounts = false;
    assert_eq!(validate(&a, &b), Err(ComparisonRefusal::DifferentPolicy));
    b = spec(2);
    b.policy.version += 1;
    assert_eq!(validate(&a, &b), Err(ComparisonRefusal::InvalidPolicy));
}
#[test]
fn raw_names_are_not_lossy_display_or_case_folded_matches() {
    let a = NativeName::unix(vec![255]).unwrap();
    let b = NativeName::unix(vec![254]).unwrap();
    assert_eq!(a.display(), b.display());
    assert_ne!(a, b);
    let mut old = spec(1);
    let mut new = spec(2);
    old.policy.exclusions = vec![vec![a]];
    new.policy.exclusions = vec![vec![b]];
    assert_eq!(
        validate(&old, &new),
        Err(ComparisonRefusal::DifferentPolicy)
    );
}
#[test]
fn complete_presence_truth_table() {
    use Presence::*;
    for (old, new, expected) in [
        (Present, Present, ChangeKind::Unchanged),
        (Present, ProvenAbsent, ChangeKind::Removed),
        (Present, Unobserved, ChangeKind::NoLongerObserved),
        (ProvenAbsent, Present, ChangeKind::Added),
        (ProvenAbsent, ProvenAbsent, ChangeKind::Unknown),
        (ProvenAbsent, Unobserved, ChangeKind::Unknown),
        (Unobserved, Present, ChangeKind::NewlyObserved),
        (Unobserved, ProvenAbsent, ChangeKind::Unknown),
        (Unobserved, Unobserved, ChangeKind::Unknown),
    ] {
        assert_eq!(classify_change(old, new, true, Some(false)), expected);
    }
    assert_eq!(
        classify_change(Present, Present, true, Some(true)),
        ChangeKind::Modified
    );
    assert_eq!(
        classify_change(Present, Present, true, None),
        ChangeKind::Unknown
    );
    assert_eq!(
        classify_change(Present, Present, false, None),
        ChangeKind::TypeChanged
    );
    // No identity input can turn these two independently matched paths into a rename.
    assert_eq!(
        classify_change(Present, ProvenAbsent, true, None),
        ChangeKind::Removed
    );
    assert_eq!(
        classify_change(ProvenAbsent, Present, true, None),
        ChangeKind::Added
    );
}
fn change(a: Option<u64>, b: Option<u64>) -> MetricChange {
    MetricChange::from_observations(a, b, AccountingState::Comparable).unwrap()
}
#[test]
fn gross_is_not_net_and_zero_identity_preserves_totals() {
    let removed = change(Some(10), Some(0));
    let added = change(Some(0), Some(10));
    let total = removed.checked_add(added).unwrap();
    assert_eq!(
        (total.net, total.gross_increase, total.gross_decrease),
        (Some(0), 10, 10)
    );
    assert_eq!(total.before.observed_bytes, 10);
    assert_eq!(total.after.observed_bytes, 10);
    assert_eq!(total.checked_add(MetricChange::zero()).unwrap(), total);
    assert_eq!(change(Some(2), Some(7)).net, Some(5));
    assert_eq!(change(Some(7), Some(2)).net, Some(-5));
}
#[test]
fn unknown_scope_or_metric_never_becomes_zero_or_exact_net() {
    let known = change(Some(2), Some(7));
    let unknown = change(None, Some(0));
    assert_eq!(unknown.before.unknown_count, 1);
    assert_eq!(unknown.net, None);
    let total = known.checked_add(unknown).unwrap();
    assert_eq!(total.net, None);
    assert_eq!(total.gross_increase, 5); // only the known comparable contribution
    assert_eq!(total.unknown_changes, 1);
    assert_eq!(change(Some(0), Some(0)).net, Some(0));
}
#[test]
fn owner_redistribution_is_not_growth_and_group_is_compared_once() {
    for state in [
        AccountingState::OwnershipRedistributed,
        AccountingState::Estimated,
        AccountingState::Unknown,
    ] {
        let moved = MetricChange::from_observations(Some(0), Some(20), state).unwrap();
        assert_eq!(
            (moved.net, moved.gross_increase, moved.gross_decrease),
            (None, 0, 0)
        );
        assert_eq!(moved.after.observed_bytes, 20);
        assert_eq!(moved.unknown_changes, 1);
        assert_eq!(MetricChange::zero().checked_add(moved).unwrap(), moved);
        assert_eq!(
            moved.before.estimated_count,
            u64::from(state == AccountingState::Estimated)
        );
    }
    let stable_group = change(Some(20), Some(20));
    assert_eq!(
        (
            stable_group.net,
            stable_group.gross_increase,
            stable_group.gross_decrease
        ),
        (Some(0), 0, 0)
    );
}
#[test]
fn sizes_and_aggregate_overflow_are_checked() {
    let max = i64::MAX as u64;
    assert!(
        MetricChange::from_observations(Some(max + 1), Some(0), AccountingState::Comparable)
            .is_err()
    );
    assert_eq!(change(Some(max), Some(0)).net, Some(-i64::MAX));
    assert_eq!(change(Some(0), Some(max)).net, Some(i64::MAX));
    assert!(
        change(Some(max), Some(0))
            .checked_add(change(Some(1), Some(0)))
            .is_err()
    );
    let mut gross = MetricChange::zero();
    gross.gross_increase = u64::MAX;
    assert!(gross.checked_add(change(Some(0), Some(1))).is_err());
    let mut unknown = change(None, None);
    unknown.unknown_changes = u64::MAX;
    assert!(unknown.checked_add(change(None, None)).is_err());
}
