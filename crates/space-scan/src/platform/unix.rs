use super::*;
use cap_std::fs::MetadataExt;

fn observation(metadata: Metadata) -> Observation {
    let identity = FileIdentity {
        volume: ObservedIdentity {
            scheme: IdentityScheme::UnixDevice,
            value: metadata.dev().to_le_bytes().to_vec(),
            strength: IdentityStrength::Observed,
        },
        file: ObservedIdentity {
            scheme: IdentityScheme::UnixInode,
            value: metadata.ino().to_le_bytes().to_vec(),
            strength: IdentityStrength::Observed,
        },
    };
    let link = if metadata.is_symlink() {
        LinkState::Symlink
    } else if metadata.is_file() && metadata.nlink() > 1 {
        LinkState::Hardlink {
            observed_links: metadata.nlink(),
        }
    } else {
        LinkState::None
    };
    let allocated = metadata.blocks().checked_mul(512);
    Observation {
        metadata,
        identity: Some(identity),
        allocated,
        link,
    }
}
pub(crate) fn observe(parent: &Dir, name: &OsStr) -> io::Result<Observation> {
    let observed = observation(parent.symlink_metadata(name)?);
    #[cfg(target_os = "macos")]
    {
        let stat = rustix::fs::statat(parent, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)?;
        mac_flags(observed, stat)
    }
    #[cfg(not(target_os = "macos"))]
    Ok(observed)
}
pub(crate) fn directory(dir: &Dir) -> io::Result<Observation> {
    let observed = observation(dir.dir_metadata()?);
    #[cfg(target_os = "macos")]
    {
        mac_flags(observed, rustix::fs::fstat(dir)?)
    }
    #[cfg(not(target_os = "macos"))]
    Ok(observed)
}
#[cfg(target_os = "macos")]
fn mac_flags(mut observed: Observation, stat: rustix::fs::Stat) -> io::Result<Observation> {
    if observed.metadata.ino() != stat.st_ino || observed.metadata.dev() != stat.st_dev as u64 {
        return Err(super::changed(
            "Filesystem object changed while reading placeholder flags",
        ));
    }
    // SF_DATALESS, Apple sys/stat.h. The enclosing thread guard prevents
    // materialization even if this flag changes immediately after inspection.
    if stat.st_flags & 0x4000_0000 != 0 {
        observed.link = LinkState::Placeholder;
    }
    Ok(observed)
}

/// Filesystem identity is checked on the opened object, never on a stale path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Boundary {
    device: u64,
    mount: u64,
    #[cfg(target_os = "macos")]
    mount_path: [u8; 1024],
}
pub(crate) fn boundary(dir: &Dir, policy: &ScanPolicy) -> Result<Boundary, (IssueClass, String)> {
    let meta = dir
        .dir_metadata()
        .map_err(|e| (IssueClass::Io, e.to_string()))?;
    let fs =
        rustix::fs::fstatfs(dir).map_err(|e| (IssueClass::UnsupportedMetadata, e.to_string()))?;
    #[cfg(target_os = "linux")]
    {
        let kind = fs.f_type as u64;
        // Conservative allow-list. Unknown/FUSE filesystems may be remote or
        // synthetic; explicit unsupported coverage is safer than guessing.
        let local = matches!(
            kind,
            0xef53
                | 0x58465342
                | 0x9123683e
                | 0x794c7630
                | 0x01021994
                | 0x2fc12fc1
                | 0x4d44
                | 0x2011bab0
                | 0x5346544e
        );
        let network = matches!(kind, 0x6969 | 0xff534d42 | 0xfe534d42 | 0x517b);
        let virtual_fs = matches!(
            kind,
            0x9fa0 | 0x62656572 | 0x64626720 | 0x1cd1 | 0x63677270 | 0x27e0eb
        );
        if policy.exclude_network_filesystems && network {
            return Err((
                IssueClass::NetworkFilesystem,
                "Network filesystem excluded".into(),
            ));
        }
        if policy.exclude_virtual_filesystems && virtual_fs {
            return Err((
                IssueClass::VirtualFilesystem,
                "Virtual filesystem excluded".into(),
            ));
        }
        if !local
            && !network
            && !virtual_fs
            && (policy.exclude_network_filesystems || policy.exclude_virtual_filesystems)
        {
            return Err((
                IssueClass::UnsupportedMetadata,
                "Filesystem locality is not established".into(),
            ));
        }
        let mount = if policy.exclude_nested_mounts {
            let stat = rustix::fs::statx(
                dir,
                "",
                rustix::fs::AtFlags::EMPTY_PATH,
                rustix::fs::StatxFlags::MNT_ID,
            )
            .map_err(|e| {
                (
                    IssueClass::UnsupportedMetadata,
                    format!("Mount identity unavailable: {e}"),
                )
            })?;
            if stat.stx_mask & rustix::fs::StatxFlags::MNT_ID.bits() == 0 {
                return Err((
                    IssueClass::UnsupportedMetadata,
                    "Kernel did not report mount identity".into(),
                ));
            }
            stat.stx_mnt_id
        } else {
            0
        };
        Ok(Boundary {
            device: meta.dev(),
            mount,
        })
    }
    #[cfg(target_os = "macos")]
    {
        let name: Vec<u8> = fs
            .f_fstypename
            .iter()
            .take_while(|&&b| b != 0)
            .map(|&b| b as u8)
            .collect();
        let local = matches!(
            name.as_slice(),
            b"apfs" | b"hfs" | b"msdos" | b"exfat" | b"ufs" | b"tmpfs"
        );
        if !local && (policy.exclude_network_filesystems || policy.exclude_virtual_filesystems) {
            return Err((
                IssueClass::UnsupportedMetadata,
                "Filesystem is not a recognized local filesystem".into(),
            ));
        }
        let mut mount_path = [0; 1024];
        for (index, &byte) in fs
            .f_mntonname
            .iter()
            .take_while(|&&byte| byte != 0)
            .enumerate()
        {
            mount_path[index] = byte as u8;
        }
        Ok(Boundary {
            device: meta.dev(),
            mount: 0,
            mount_path,
        })
    }
}

pub(crate) fn open_child(parent: &Dir, name: &OsStr) -> io::Result<Dir> {
    use cap_fs_ext::DirExt;
    parent.open_dir_nofollow(name)
}

pub(crate) struct Entries(cap_std::fs::ReadDir);
impl Iterator for Entries {
    type Item = io::Result<std::ffi::OsString>;
    fn next(&mut self) -> Option<Self::Item> {
        self.0
            .next()
            .map(|entry| entry.map(|entry| entry.file_name()))
    }
}
pub(crate) fn entries(dir: &Dir) -> io::Result<Entries> {
    dir.entries().map(Entries)
}

pub(crate) fn open_anchor(path: &std::path::Path) -> io::Result<Dir> {
    Dir::open_ambient_dir(path, cap_std::ambient_authority())
}
