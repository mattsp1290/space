/// IDs have a checked SQLite-compatible representation. Zero is never an ID.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidId;
impl std::fmt::Display for InvalidId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ID must be in 1..=i64::MAX")
    }
}
impl std::error::Error for InvalidId {}
macro_rules! id { ($($name:ident),+) => {$ (
    #[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    #[serde(try_from = "i64", into = "i64")]
    pub struct $name(i64);
    impl $name {
        pub fn new(value: u64) -> Result<Self, InvalidId> { if value == 0 || value > i64::MAX as u64 { Err(InvalidId) } else { Ok(Self(value as i64)) } }
        pub const fn get(self) -> i64 { self.0 }
    }
    impl From<$name> for i64 { fn from(v: $name) -> i64 { v.0 } }
    impl TryFrom<i64> for $name { type Error = InvalidId; fn try_from(v: i64) -> Result<Self, InvalidId> { if v <= 0 { Err(InvalidId) } else { Ok(Self(v)) } } }
)+}; }
id!(ScanId, RootId, LineageId, NodeId);
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ScanKey {
    pub id: ScanId,
    pub generation: u64,
}
#[derive(
    serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
pub enum IdentityScheme {
    UnixDevice,
    UnixInode,
    WindowsVolume,
    WindowsFileId,
    VolumeUuid,
}
#[derive(
    serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
pub enum IdentityStrength {
    Weak,
    Observed,
}
/// Observed IDs can be reused after removal/reformat. Equality does not prove
/// eternal continuity. Weak IDs require explicit re-selection on reconnect.
#[derive(
    serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
pub struct ObservedIdentity {
    pub scheme: IdentityScheme,
    pub value: Vec<u8>,
    pub strength: IdentityStrength,
}
impl ObservedIdentity {
    pub fn is_usable(&self) -> bool {
        !self.value.is_empty()
    }
    pub fn establishes_match(&self, other: &Self) -> bool {
        self.is_usable()
            && self.strength == IdentityStrength::Observed
            && other.strength == IdentityStrength::Observed
            && self == other
    }
}
#[derive(
    serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
pub struct FileIdentity {
    pub volume: ObservedIdentity,
    pub file: ObservedIdentity,
}
impl FileIdentity {
    pub fn is_deduplicable(&self) -> bool {
        self.volume.is_usable()
            && self.file.is_usable()
            && self.volume.strength == IdentityStrength::Observed
            && self.file.strength == IdentityStrength::Observed
    }
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RootIdentity {
    pub volume: ObservedIdentity,
    pub directory: ObservedIdentity,
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityValidation {
    MatchingObservation,
    Changed,
    RequiresSelection,
}
impl RootIdentity {
    /// Exclusions need a usable, observed directory object on an identified
    /// volume. Weak or wrongly typed identities cannot enforce self-exclusion.
    pub fn is_valid_exclusion(&self) -> bool {
        let valid_schemes = matches!(
            (self.volume.scheme, self.directory.scheme),
            (IdentityScheme::UnixDevice, IdentityScheme::UnixInode)
                | (IdentityScheme::WindowsVolume, IdentityScheme::WindowsFileId)
                | (IdentityScheme::VolumeUuid, IdentityScheme::UnixInode)
                | (IdentityScheme::VolumeUuid, IdentityScheme::WindowsFileId)
        );
        valid_schemes
            && self.volume.is_usable()
            && self.directory.is_usable()
            && self.volume.strength == IdentityStrength::Observed
            && self.directory.strength == IdentityStrength::Observed
    }

    pub fn validate_against(&self, current: &Self) -> IdentityValidation {
        if !self.volume.is_usable()
            || !self.directory.is_usable()
            || !current.volume.is_usable()
            || !current.directory.is_usable()
            || self.volume.strength == IdentityStrength::Weak
            || self.directory.strength == IdentityStrength::Weak
            || current.volume.strength == IdentityStrength::Weak
            || current.directory.strength == IdentityStrength::Weak
        {
            return IdentityValidation::RequiresSelection;
        }
        if self.volume.establishes_match(&current.volume)
            && self.directory.establishes_match(&current.directory)
        {
            IdentityValidation::MatchingObservation
        } else {
            IdentityValidation::Changed
        }
    }
}
