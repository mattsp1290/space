use crate::*;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

pub const POLICY_VERSION: u32 = 1;
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ScanPolicy {
    pub version: u32,
    /// Relative component sequences; empty sequences are invalid (would exclude root).
    pub exclusions: Vec<Vec<NativeName>>,
    /// Opened directory identities excluded irrespective of path aliases. Used
    /// for the cache directory. Missing serialized field is rejected, rather
    /// than silently disabling self-exclusion in an older working scan policy.
    pub excluded_directory_identities: Vec<RootIdentity>,
    pub exclude_nested_mounts: bool,
    pub exclude_virtual_filesystems: bool,
    pub exclude_network_filesystems: bool,
}
impl Default for ScanPolicy {
    fn default() -> Self {
        Self {
            version: POLICY_VERSION,
            exclusions: vec![],
            excluded_directory_identities: vec![],
            exclude_nested_mounts: true,
            exclude_virtual_filesystems: true,
            exclude_network_filesystems: true,
        }
    }
}
impl ScanPolicy {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.version != POLICY_VERSION {
            return Err(ContractError::UnsupportedPolicy);
        }
        for identity in &self.excluded_directory_identities {
            if !identity.is_valid_exclusion() {
                return Err(ContractError::InvalidEntry);
            }
        }
        for path in &self.exclusions {
            if path.is_empty() {
                return Err(ContractError::InvalidEntry);
            }
            for name in path {
                name.validate().map_err(|_| ContractError::InvalidEntry)?;
            }
            if path.windows(2).any(|p| p[0].encoding() != p[1].encoding()) {
                return Err(ContractError::InvalidEntry);
            }
        }
        Ok(())
    }
    /// Validate exclusions against the selected root, including distinct paths
    /// with different native encodings. Call at scan/store ingress.
    pub fn validate_for_encoding(&self, encoding: NativeEncoding) -> Result<(), ContractError> {
        self.validate()?;
        if self
            .exclusions
            .iter()
            .flatten()
            .any(|name| name.encoding() != encoding)
        {
            return Err(ContractError::InvalidEntry);
        }
        Ok(())
    }
    pub fn normalize(&mut self) {
        self.exclusions.sort();
        self.exclusions.dedup();
        self.excluded_directory_identities.sort();
        self.excluded_directory_identities.dedup();
    }
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ScanSpec {
    pub key: ScanKey,
    pub root_id: RootId,
    pub lineage_id: LineageId,
    pub expected_root: RootIdentity,
    pub policy: ScanPolicy,
}
/// Signed Unix seconds plus nanoseconds, supporting pre-epoch observations.
#[derive(
    serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord,
)]
pub struct Timestamp {
    pub seconds: i64,
    pub nanos: u32,
}
impl Timestamp {
    pub fn new(seconds: i64, nanos: u32) -> Result<Self, ContractError> {
        if nanos >= 1_000_000_000 {
            Err(ContractError::InvalidTimestamp)
        } else {
            Ok(Self { seconds, nanos })
        }
    }
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Directory,
    RegularFile,
    Symlink,
    Other,
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkState {
    None,
    Hardlink { observed_links: u64 },
    Symlink,
    RedirectingReparse,
    Placeholder,
    UnsupportedReparse,
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetadataState {
    Complete,
    Unavailable,
    Changed,
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnumerationState {
    Complete,
    Failed,
    Excluded,
    BoundarySkipped,
    Cancelled,
    ResourceLimit,
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum IssueClass {
    PermissionDenied,
    Vanished,
    Changed,
    Disconnected,
    UnsupportedMetadata,
    UnsafeDescent,
    Excluded,
    MountBoundary,
    VirtualFilesystem,
    NetworkFilesystem,
    ResourceLimit,
    Io,
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ScanIssue {
    pub node: Option<NodeId>,
    pub parent: Option<NodeId>,
    pub name: Option<NativeName>,
    pub class: IssueClass,
    pub detail: String,
}
impl ScanIssue {
    pub const MAX_DETAIL_BYTES: usize = 1024;
    pub fn bounded_detail(value: &str) -> String {
        let mut end = value.len().min(Self::MAX_DETAIL_BYTES);
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        value[..end].into()
    }
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct EntryRecord {
    pub node: NodeId,
    pub parent: Option<NodeId>,
    /// None only for root; root path belongs in the roots table.
    pub name: Option<NativeName>,
    pub display_name: String,
    pub kind: EntryKind,
    pub identity: Option<FileIdentity>,
    pub logical_bytes: Option<u64>,
    pub allocated_bytes: Option<u64>,
    pub modified: Option<Timestamp>,
    pub link_state: LinkState,
    pub metadata_state: MetadataState,
}
impl EntryRecord {
    pub fn validate(&self) -> Result<(), ContractError> {
        match (&self.parent, &self.name) {
            (None, None) if self.kind == EntryKind::Directory => {}
            (Some(parent), Some(name)) if parent.get() < self.node.get() => {
                name.validate().map_err(|_| ContractError::InvalidEntry)?;
            }
            _ => return Err(ContractError::InvalidEntry),
        }
        if self.modified.is_some_and(|t| t.nanos >= 1_000_000_000) {
            return Err(ContractError::InvalidTimestamp);
        }
        Ok(())
    }
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum ScanRecord {
    Entry(EntryRecord),
    DirectoryFinished {
        node: NodeId,
        enumeration: EnumerationState,
    },
    Issue(ScanIssue),
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ScanBatch {
    pub key: ScanKey,
    pub sequence: u64,
    pub records: Vec<ScanRecord>,
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraversalOutcome {
    Finished,
    Cancelled,
    Failed,
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ScanFinish {
    pub key: ScanKey,
    pub root_after: RootIdentity,
    pub outcome: TraversalOutcome,
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanStatus {
    Scanning,
    Finalizing,
    Published,
    Cancelled,
    Failed,
    Interrupted,
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Metric {
    Logical,
    Allocated,
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CoverageSummary {
    pub unknown_entries: u64,
    pub error_count: u64,
    pub excluded_scopes: u64,
    pub boundary_scopes: u64,
}
impl CoverageSummary {
    pub fn is_complete(self) -> bool {
        self.unknown_entries == 0
            && self.error_count == 0
            && self.excluded_scopes == 0
            && self.boundary_scopes == 0
    }
    pub fn checked_add(self, other: Self) -> Result<Self, ContractError> {
        Ok(Self {
            unknown_entries: add(self.unknown_entries, other.unknown_entries)?,
            error_count: add(self.error_count, other.error_count)?,
            excluded_scopes: add(self.excluded_scopes, other.excluded_scopes)?,
            boundary_scopes: add(self.boundary_scopes, other.boundary_scopes)?,
        })
    }
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MetricTotal {
    pub observed_bytes: u64,
    pub unknown_count: u64,
    pub estimated_count: u64,
}
impl MetricTotal {
    pub fn observation(bytes: Option<u64>, estimated: bool) -> Self {
        Self {
            observed_bytes: bytes.unwrap_or(0),
            unknown_count: u64::from(bytes.is_none()),
            estimated_count: u64::from(estimated),
        }
    }
    pub fn checked_add(self, other: Self) -> Result<Self, ContractError> {
        Ok(Self {
            observed_bytes: add(self.observed_bytes, other.observed_bytes)?,
            unknown_count: add(self.unknown_count, other.unknown_count)?,
            estimated_count: add(self.estimated_count, other.estimated_count)?,
        })
    }
}
pub fn sqlite_integer(value: u64) -> Result<i64, ContractError> {
    i64::try_from(value).map_err(|_| ContractError::SizeOverflow)
}
fn add(a: u64, b: u64) -> Result<u64, ContractError> {
    a.checked_add(b).ok_or(ContractError::SizeOverflow)
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContractError {
    InvalidEntry,
    InvalidTimestamp,
    UnsupportedPolicy,
    SizeOverflow,
}
impl std::fmt::Display for ContractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ContractError {}
/// Cooperative only: this cannot interrupt a blocked OS metadata operation.
/// Cancellation and the final publication commit have one serialized decision.
#[derive(Clone, Debug, Default)]
pub struct Cancellation(Arc<CancellationState>);
#[derive(Debug, Default)]
struct CancellationState {
    cancelled: AtomicBool,
    publication: Mutex<()>,
}
impl Cancellation {
    pub fn new() -> Self {
        Self::default()
    }
    /// May wait for an already-started publication commit. Do not call on the
    /// UI thread: acknowledge stopping there, then cancel on a worker.
    pub fn cancel(&self) {
        let _guard = self.0.publication.lock().unwrap_or_else(|e| e.into_inner());
        self.0.cancelled.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }
    /// Run only the final publication transaction commit under this guard.
    /// None means cancellation won; after a panic, publication is refused.
    /// The closure must not call cancel or recursively acquire this guard.
    pub fn with_publication_guard<T>(&self, f: impl FnOnce() -> T) -> Option<T> {
        let Ok(_guard) = self.0.publication.lock() else {
            return None;
        };
        if self.is_cancelled() { None } else { Some(f()) }
    }
}
