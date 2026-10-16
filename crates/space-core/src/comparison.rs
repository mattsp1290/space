//! Comparison of immutable observations, never filesystem mutation or rename inference.
//! Matching uses raw relative native components in the store. File identity may
//! reconcile accounting, but never proves historical continuity at a path.
use crate::{ContractError, IdentityValidation, MetricTotal, ScanSpec, ScanStatus, sqlite_integer};

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComparisonRefusal {
    NotPublished,
    SameScan,
    DifferentRoot,
    DifferentLineage,
    UnverifiedIdentity,
    InvalidPolicy,
    DifferentPolicy,
}
impl std::fmt::Display for ComparisonRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotPublished => "Both scans must be published observations.",
            Self::SameScan => "Choose two different scans.",
            Self::DifferentRoot => "Choose scans of the same selected root.",
            Self::DifferentLineage => "Choose scans from the same verified comparison lineage.",
            Self::UnverifiedIdentity => "Volume or selected-directory identity differs or cannot be verified; rescan the selected root.",
            Self::InvalidPolicy => "A scan policy is invalid or unsupported; rescan with the current policy.",
            Self::DifferentPolicy => "Scan policies differ; rescan with identical exclusions and boundary settings.",
        })
    }
}
impl std::error::Error for ComparisonRefusal {}

/// Compare immutable per-scan specs, not mutable root records or mount paths.
/// The store additionally validates native encoding and stored metric semantics.
pub fn validate_comparison(
    before: &ScanSpec,
    before_status: ScanStatus,
    after: &ScanSpec,
    after_status: ScanStatus,
) -> Result<(), ComparisonRefusal> {
    if before_status != ScanStatus::Published || after_status != ScanStatus::Published {
        return Err(ComparisonRefusal::NotPublished);
    }
    if before.key.id == after.key.id {
        return Err(ComparisonRefusal::SameScan);
    }
    if before.root_id != after.root_id {
        return Err(ComparisonRefusal::DifferentRoot);
    }
    if before.lineage_id != after.lineage_id {
        return Err(ComparisonRefusal::DifferentLineage);
    }
    if !before.expected_root.is_valid_exclusion()
        || !after.expected_root.is_valid_exclusion()
        || before.expected_root.validate_against(&after.expected_root)
            != IdentityValidation::MatchingObservation
    {
        return Err(ComparisonRefusal::UnverifiedIdentity);
    }
    before
        .policy
        .validate()
        .map_err(|_| ComparisonRefusal::InvalidPolicy)?;
    after
        .policy
        .validate()
        .map_err(|_| ComparisonRefusal::InvalidPolicy)?;
    let mut old_policy = before.policy.clone();
    let mut new_policy = after.policy.clone();
    old_policy.normalize();
    new_policy.normalize();
    if old_policy != new_policy {
        return Err(ComparisonRefusal::DifferentPolicy);
    }
    Ok(())
}

/// ProvenAbsent requires successful enumeration of the opposite parent, or
/// inherited proof that its ancestor directory is absent. Unobserved never
/// means zero bytes and never establishes deletion or creation.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presence {
    Present,
    ProvenAbsent,
    Unobserved,
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Removed,
    Modified,
    Unchanged,
    NewlyObserved,
    NoLongerObserved,
    TypeChanged,
    Unknown,
}
/// `changed` is None when observations cannot establish equality/difference.
/// It describes metadata observations, not file-content equality. Identity is
/// deliberately absent: a rename is two independently classified raw paths.
pub fn classify_change(
    before: Presence,
    after: Presence,
    same_kind: bool,
    changed: Option<bool>,
) -> ChangeKind {
    use Presence::*;
    match (before, after) {
        (ProvenAbsent, Present) => ChangeKind::Added,
        (Present, ProvenAbsent) => ChangeKind::Removed,
        (Unobserved, Present) => ChangeKind::NewlyObserved,
        (Present, Unobserved) => ChangeKind::NoLongerObserved,
        (Present, Present) if !same_kind => ChangeKind::TypeChanged,
        (Present, Present) => match changed {
            Some(true) => ChangeKind::Modified,
            Some(false) => ChangeKind::Unchanged,
            None => ChangeKind::Unknown,
        },
        _ => ChangeKind::Unknown,
    }
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountingState {
    Comparable,
    /// A canonical hardlink owner moved. Path attribution is not growth.
    OwnershipRedistributed,
    Estimated,
    Unknown,
}
/// Gross values are known comparable changes, not a claim that unknown scopes
/// contribute zero. Net is exact only when every contribution is comparable.
/// Coverage uncertainty is separate: an unseen subtree can have no known rows,
/// so callers must add an unknown contribution and preserve coverage labels.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct MetricChange {
    /// Contribution on the before side of this accounting comparison. For
    /// ancestor-attributed identity groups this is NOT the directory's raw
    /// snapshot total. Keep raw endpoint totals separately and label both.
    pub before: MetricTotal,
    /// Contribution on the after side; the same attribution caveat applies.
    pub after: MetricTotal,
    pub net: Option<i64>,
    pub gross_increase: u64,
    pub gross_decrease: u64,
    pub unknown_changes: u64,
    pub accounting: AccountingState,
}
impl MetricChange {
    pub fn zero() -> Self {
        Self {
            before: MetricTotal::default(),
            after: MetricTotal::default(),
            net: Some(0),
            gross_increase: 0,
            gross_decrease: 0,
            unknown_changes: 0,
            accounting: AccountingState::Comparable,
        }
    }
    /// Use Some(0) for proven absence; None means unavailable observation.
    /// Ownership redistribution retains the raw endpoint amounts but asserts no
    /// growth/shrinkage. Compare the group once separately; never replace those
    /// raw path values with the group's ancestor-attributed comparison totals.
    pub fn from_observations(
        before: Option<u64>,
        after: Option<u64>,
        accounting: AccountingState,
    ) -> Result<Self, ContractError> {
        let old = before.map(sqlite_integer).transpose()?;
        let new = after.map(sqlite_integer).transpose()?;
        let net = if accounting == AccountingState::Comparable {
            old.zip(new).map(|(a, b)| b - a)
        } else {
            None
        };
        Ok(Self {
            before: MetricTotal::observation(before, accounting == AccountingState::Estimated),
            after: MetricTotal::observation(after, accounting == AccountingState::Estimated),
            net,
            gross_increase: net.filter(|n| *n > 0).unwrap_or(0) as u64,
            gross_decrease: net.filter(|n| *n < 0).map_or(0, i64::unsigned_abs),
            unknown_changes: u64::from(net.is_none()),
            accounting,
        })
    }
    pub fn checked_add(self, other: Self) -> Result<Self, ContractError> {
        let before = self.before.checked_add(other.before)?;
        let after = self.after.checked_add(other.after)?;
        sqlite_integer(before.observed_bytes)?;
        sqlite_integer(after.observed_bytes)?;
        let net = self
            .net
            .zip(other.net)
            .map(|(a, b)| a.checked_add(b).ok_or(ContractError::SizeOverflow))
            .transpose()?;
        let add = |a: u64, b: u64| a.checked_add(b).ok_or(ContractError::SizeOverflow);
        let accounting = if self.accounting == other.accounting {
            self.accounting
        } else if self.accounting == AccountingState::Comparable {
            other.accounting
        } else if other.accounting == AccountingState::Comparable {
            self.accounting
        } else {
            AccountingState::Unknown
        };
        Ok(Self {
            before,
            after,
            net,
            gross_increase: add(self.gross_increase, other.gross_increase)?,
            gross_decrease: add(self.gross_decrease, other.gross_decrease)?,
            unknown_changes: add(self.unknown_changes, other.unknown_changes)?,
            accounting,
        })
    }
}

#[cfg(test)]
mod tests;
