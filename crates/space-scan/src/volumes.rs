//! Best-effort mounted local/removable targets. Discovery never enumerates a
//! source directory and never starts a scan. Capacity belongs to the filesystem,
//! not to the selected directory or the sum of scanned entries.
use crate::{open_root, platform};
use space_core::{IssueClass, NativePath, RootIdentity, ScanIssue, ScanPolicy, Timestamp};
use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolumeCapacity {
    /// Filesystem capacity reported to the calling account (quotas may apply).
    pub total_bytes: u64,
    /// Bytes currently available to the calling account, not reclaimable bytes.
    pub available_bytes: u64,
    pub observed_at: Timestamp,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MountedVolume {
    pub label: String,
    pub mount_path: NativePath,
    /// Other mounted paths to the same observed directory object, not lossy keys.
    pub aliases: Vec<NativePath>,
    pub identity: RootIdentity,
    pub filesystem: String,
    /// None means the operating system classification is unavailable.
    pub removable: Option<bool>,
    pub capacity: Option<VolumeCapacity>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolumeIssue {
    pub mount_path: Option<NativePath>,
    pub class: IssueClass,
    pub detail: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolumeListing {
    pub observed_at: Timestamp,
    pub volumes: Vec<MountedVolume>,
    pub issues: Vec<VolumeIssue>,
    /// False when discovery or validation may have omitted local mounted roots.
    /// A true value is still a changing observation interval, not an atomic list.
    pub complete: bool,
}
const MAX_CANDIDATES: usize = 4096;
const MAX_ISSUES: usize = 4096;
const MAX_PATH_BYTES: usize = 65_536;

struct Candidate {
    path: PathBuf,
    label: String,
    filesystem: String,
    removable: Option<bool>,
}
struct Candidates {
    values: Vec<Candidate>,
    issues: Vec<VolumeIssue>,
    complete: bool,
}
fn now() -> io::Result<Timestamp> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?;
    Ok(Timestamp {
        seconds: i64::try_from(duration.as_secs()).map_err(io::Error::other)?,
        nanos: duration.subsec_nanos(),
    })
}
fn issue(path: Option<&Path>, class: IssueClass, detail: impl AsRef<str>) -> VolumeIssue {
    VolumeIssue {
        mount_path: path
            .filter(|p| p.as_os_str().as_encoded_bytes().len() <= MAX_PATH_BYTES)
            .map(NativePath::from_path),
        class,
        detail: ScanIssue::bounded_detail(detail.as_ref()),
    }
}
/// Blocking metadata-only discovery. Run off the UI thread. Every chosen target
/// still requires inspect_root and scan's expected-root identity validation.
pub fn enumerate_volumes() -> io::Result<VolumeListing> {
    platform::with_no_hydration(|| {
        let observed_at = now()?;
        let candidates = candidates()?;
        let mut listing = VolumeListing {
            observed_at,
            volumes: Vec::new(),
            issues: candidates.issues,
            complete: candidates.complete,
        };
        let mut identities = BTreeMap::new();
        for candidate in candidates.values.into_iter().take(MAX_CANDIDATES) {
            if listing.issues.len() >= MAX_ISSUES - 2 {
                listing.complete = false;
                listing.issues.push(issue(
                    None,
                    IssueClass::ResourceLimit,
                    "Volume diagnostic limit reached",
                ));
                break;
            }
            if candidate.path.as_os_str().as_encoded_bytes().len() > MAX_PATH_BYTES {
                listing.complete = false;
                listing.issues.push(issue(
                    None,
                    IssueClass::ResourceLimit,
                    "Oversized native mount path skipped",
                ));
                continue;
            }
            #[cfg(windows)]
            let (candidate, aliases) = {
                let mut candidate = candidate;
                match platform::mounted_volume_root(&candidate.path) {
                    Ok(root) => {
                        let aliases = vec![NativePath::from_path(&candidate.path)];
                        candidate.path = root;
                        (candidate, aliases)
                    }
                    Err(error) => {
                        listing.complete = false;
                        listing.issues.push(issue(
                            Some(&candidate.path),
                            crate::io_class(&error),
                            error.to_string(),
                        ));
                        continue;
                    }
                }
            };
            #[cfg(not(windows))]
            let aliases = vec![];
            let dir = match open_root(&candidate.path) {
                Ok(dir) => dir,
                Err(error) if error.kind() == io::ErrorKind::NotADirectory => continue,
                Err(error) => {
                    listing.complete = false;
                    listing.issues.push(issue(
                        Some(&candidate.path),
                        crate::io_class(&error),
                        error.to_string(),
                    ));
                    continue;
                }
            };
            let result = (|| {
                platform::boundary(&dir, &ScanPolicy::default())?;
                let identity = platform::root_identity(&dir)
                    .map_err(|e| (IssueClass::UnsupportedMetadata, e.to_string()))?;
                let capacity = match capacity_from_dir(&dir) {
                    Ok(capacity) => Some(capacity),
                    Err(error) => {
                        listing.complete = false;
                        listing.issues.push(issue(
                            Some(&candidate.path),
                            IssueClass::UnsupportedMetadata,
                            format!("Capacity unavailable: {error}"),
                        ));
                        None
                    }
                };
                Ok::<_, (IssueClass, String)>((identity, capacity))
            })();
            let (identity, capacity) = match result {
                Ok(value) => value,
                Err((class, detail)) => {
                    listing.complete = false;
                    listing
                        .issues
                        .push(issue(Some(&candidate.path), class, detail));
                    continue;
                }
            };
            merge_mount(
                &mut listing.volumes,
                &mut identities,
                MountedVolume {
                    label: ScanIssue::bounded_detail(&candidate.label),
                    mount_path: NativePath::from_path(&candidate.path),
                    aliases,
                    identity,
                    filesystem: ScanIssue::bounded_detail(&candidate.filesystem),
                    removable: candidate.removable,
                    capacity,
                },
            );
        }
        Ok(listing)
    })
}
type IdentityIndex = BTreeMap<(space_core::ObservedIdentity, space_core::ObservedIdentity), usize>;
fn merge_mount(
    volumes: &mut Vec<MountedVolume>,
    identities: &mut IdentityIndex,
    incoming: MountedVolume,
) {
    let key = (
        incoming.identity.volume.clone(),
        incoming.identity.directory.clone(),
    );
    if let Some(&index) = identities.get(&key) {
        let volume = &mut volumes[index];
        for alias in std::iter::once(incoming.mount_path).chain(incoming.aliases) {
            if volume.mount_path != alias && !volume.aliases.contains(&alias) {
                volume.aliases.push(alias);
            }
        }
        if volume.capacity.is_none() {
            volume.capacity = incoming.capacity;
        }
        if volume.removable.is_none() {
            volume.removable = incoming.removable;
        }
    } else {
        identities.insert(key, volumes.len());
        volumes.push(incoming);
    }
}
/// OS capacity for the selected root's filesystem, obtained from an opened,
/// locality-validated directory. This does not recursively inspect the root.
pub fn volume_capacity(root: &Path) -> io::Result<VolumeCapacity> {
    platform::with_no_hydration(|| {
        let dir = open_root(root)?;
        platform::boundary(&dir, &ScanPolicy::default())
            .map_err(|(_, detail)| io::Error::new(io::ErrorKind::Unsupported, detail))?;
        capacity_from_dir(&dir)
    })
}
fn checked_capacity(total: u64, available: u64) -> io::Result<VolumeCapacity> {
    if available > total {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Available bytes exceed reported capacity",
        ));
    }
    Ok(VolumeCapacity {
        total_bytes: total,
        available_bytes: available,
        observed_at: now()?,
    })
}
#[cfg(unix)]
fn capacity_from_dir(dir: &cap_std::fs::Dir) -> io::Result<VolumeCapacity> {
    let stat = rustix::fs::fstatvfs(dir)?;
    let unit = stat.f_frsize;
    if unit == 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Filesystem did not report its allocation unit",
        ));
    }
    checked_capacity(
        stat.f_blocks
            .checked_mul(unit)
            .ok_or_else(|| io::Error::other("Capacity overflow"))?,
        stat.f_bavail
            .checked_mul(unit)
            .ok_or_else(|| io::Error::other("Available-space overflow"))?,
    )
}
#[cfg(windows)]
fn capacity_from_dir(dir: &cap_std::fs::Dir) -> io::Result<VolumeCapacity> {
    let (total, available) = platform::volume_capacity_bytes(dir)?;
    checked_capacity(total, available)
}

#[cfg(any(target_os = "macos", windows))]
fn candidates() -> io::Result<Candidates> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let mut values: Vec<_> = disks
        .list()
        .iter()
        .take(MAX_CANDIDATES)
        .map(|disk| Candidate {
            path: disk.mount_point().to_owned(),
            label: disk.name().to_string_lossy().into_owned(),
            filesystem: disk.file_system().to_string_lossy().into_owned(),
            removable: removable_classification(disk.is_removable()),
        })
        .collect();
    // Shorter mount aliases (drive roots) are preferred deterministically.
    values.sort_by(|a, b| {
        a.path
            .components()
            .count()
            .cmp(&b.path.components().count())
            .then_with(|| a.path.cmp(&b.path))
    });
    Ok(Candidates {
        values,
        issues: vec![issue(
            None,
            IssueClass::UnsupportedMetadata,
            "Volume discovery is best-effort; inaccessible or unrecognized mounted volumes may be omitted. Choose Folder remains available.",
        )],
        complete: false,
    })
}
#[cfg(any(target_os = "macos", windows))]
fn removable_classification(removable: bool) -> Option<bool> {
    // The macOS library collapses missing property values into false. Windows
    // candidates are positively classified FIXED/REMOVABLE by GetDriveType.
    #[cfg(target_os = "macos")]
    {
        removable.then_some(true)
    }
    #[cfg(windows)]
    {
        Some(removable)
    }
}
#[cfg(target_os = "linux")]
fn candidates() -> io::Result<Candidates> {
    linux::candidates()
}

#[cfg(target_os = "linux")]
mod linux;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn oversized_issue_path_is_omitted_and_detail_is_bounded() {
        let path = PathBuf::from("x".repeat(MAX_PATH_BYTES + 1));
        let issue = issue(Some(&path), IssueClass::ResourceLimit, "界".repeat(1024));
        assert!(issue.mount_path.is_none());
        assert!(issue.detail.len() <= 1024);
    }
    #[test]
    fn capacity_is_checked_and_timestamped() {
        assert!(checked_capacity(10, 11).is_err());
        let capacity = checked_capacity(10, 0).unwrap();
        assert_eq!(capacity.available_bytes, 0);
        assert!(capacity.observed_at.seconds > 0);
    }
    #[test]
    fn native_root_capacity_is_separate_from_scan_totals() {
        let path = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let capacity = volume_capacity(&path).unwrap();
        assert!(capacity.total_bytes > 0);
        assert!(capacity.available_bytes <= capacity.total_bytes);
    }
    #[test]
    fn native_discovery_returns_validated_paths_without_scanning() {
        let listing = enumerate_volumes().unwrap();
        assert!(
            !listing.volumes.is_empty(),
            "No usable mounted system volume: {:?}",
            listing.issues
        );
        for volume in listing.volumes {
            assert!(!volume.mount_path.bytes.is_empty());
            assert!(volume.identity.volume.is_usable());
            assert!(volume.identity.directory.is_usable());
        }
    }
}

#[cfg(test)]
mod alias_tests {
    use super::*;
    use space_core::{IdentityScheme, IdentityStrength, ObservedIdentity};
    fn volume(path: &str, inode: u8) -> MountedVolume {
        MountedVolume {
            label: path.into(),
            mount_path: NativePath::from_path(Path::new(path)),
            aliases: vec![],
            identity: RootIdentity {
                volume: ObservedIdentity {
                    scheme: IdentityScheme::UnixDevice,
                    value: vec![1],
                    strength: IdentityStrength::Observed,
                },
                directory: ObservedIdentity {
                    scheme: IdentityScheme::UnixInode,
                    value: vec![inode],
                    strength: IdentityStrength::Observed,
                },
            },
            filesystem: "fixture".into(),
            removable: None,
            capacity: None,
        }
    }
    #[test]
    fn aliases_deduplicate_only_the_same_directory_object() {
        let mut volumes = Vec::new();
        let mut identities = BTreeMap::new();
        merge_mount(&mut volumes, &mut identities, volume("/first", 1));
        let mut alias = volume("/alias", 1);
        alias.aliases = vec![
            NativePath::from_path(Path::new("/alias")),
            NativePath::from_path(Path::new("/first")),
        ];
        merge_mount(&mut volumes, &mut identities, alias);
        merge_mount(&mut volumes, &mut identities, volume("/alias", 1));
        merge_mount(
            &mut volumes,
            &mut identities,
            volume("/different-subtree", 2),
        );
        assert_eq!(volumes.len(), 2);
        assert_eq!(volumes[0].aliases.len(), 1);
        assert_eq!(
            volumes[0].aliases[0],
            NativePath::from_path(Path::new("/alias"))
        );
    }
}
