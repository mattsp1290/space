use super::*;
use std::{ffi::OsString, fs::File, io::Read, os::unix::ffi::OsStringExt};
const MAX_MOUNT_BYTES: u64 = 8 * 1024 * 1024;

pub(super) fn candidates() -> io::Result<Candidates> {
    let mut bytes = Vec::new();
    File::open("/proc/self/mountinfo")?
        .take(MAX_MOUNT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_MOUNT_BYTES {
        return Err(io::Error::other(
            "Mount table exceeds discovery resource limit",
        ));
    }
    let mut result = parse_mountinfo(&bytes);
    for candidate in &mut result.values {
        // Linux's block-device removable flag is not universally available
        // (device mapper, containers, non-block filesystems), so keep None.
        if let Some(device) = candidate.device.as_deref() {
            candidate.candidate.removable = removable(device);
        }
    }
    Ok(Candidates {
        values: result
            .values
            .into_iter()
            .map(|entry| entry.candidate)
            .collect(),
        issues: result.issues,
        complete: result.complete,
    })
}
struct ParsedCandidate {
    candidate: Candidate,
    device: Option<String>,
}
struct Parsed {
    values: Vec<ParsedCandidate>,
    issues: Vec<VolumeIssue>,
    complete: bool,
}
fn parse_mountinfo(bytes: &[u8]) -> Parsed {
    let mut result = Parsed {
        values: Vec::new(),
        issues: vec![],
        complete: true,
    };
    for (index, line) in bytes
        .split(|&b| b == b'\n')
        .filter(|line| !line.is_empty())
        .enumerate()
    {
        if result.issues.len() >= MAX_ISSUES - 2 {
            result.complete = false;
            result.issues.push(issue(
                None,
                IssueClass::ResourceLimit,
                "Mount-table diagnostic limit reached",
            ));
            break;
        }
        if index >= 16_384 {
            result.complete = false;
            result.issues.push(issue(
                None,
                IssueClass::ResourceLimit,
                "Mount-table record limit reached",
            ));
            break;
        }
        if line.len() > 65_536 {
            result.complete = false;
            result.issues.push(issue(
                None,
                IssueClass::ResourceLimit,
                "Oversized mount-table record skipped",
            ));
            continue;
        }
        let fields: Vec<_> = line
            .split(|&b| b == b' ')
            .filter(|field| !field.is_empty())
            .collect();
        let separator = fields.iter().position(|&field| field == b"-");
        let Some(separator) =
            separator.filter(|&separator| separator >= 6 && separator + 3 < fields.len())
        else {
            result.complete = false;
            result.issues.push(issue(
                None,
                IssueClass::UnsupportedMetadata,
                "Malformed mount-table record",
            ));
            continue;
        };
        let filesystem = fields[separator + 1];
        // Filesystems supported by the scanner's opened-handle locality checks.
        let local = matches!(
            filesystem,
            b"ext2"
                | b"ext3"
                | b"ext4"
                | b"xfs"
                | b"btrfs"
                | b"zfs"
                | b"vfat"
                | b"msdos"
                | b"exfat"
                | b"ntfs"
                | b"ntfs3"
                | b"overlay"
        );
        if !local {
            let excluded_network_or_virtual = matches!(
                filesystem,
                b"proc"
                    | b"sysfs"
                    | b"tmpfs"
                    | b"devtmpfs"
                    | b"devpts"
                    | b"cgroup"
                    | b"cgroup2"
                    | b"securityfs"
                    | b"debugfs"
                    | b"tracefs"
                    | b"configfs"
                    | b"pstore"
                    | b"efivarfs"
                    | b"mqueue"
                    | b"hugetlbfs"
                    | b"bpf"
                    | b"fusectl"
                    | b"rpc_pipefs"
                    | b"autofs"
                    | b"nfs"
                    | b"nfs4"
                    | b"cifs"
                    | b"smb3"
                    | b"9p"
                    | b"ceph"
                    | b"fuse.sshfs"
            );
            if !excluded_network_or_virtual {
                result.complete = false;
                result.issues.push(issue(
                    None,
                    IssueClass::UnsupportedMetadata,
                    "A mounted filesystem is not recognized as a supported local target",
                ));
            }
            continue;
        }
        let (Ok(path), Ok(source)) = (unescape(fields[4]), unescape(fields[separator + 2])) else {
            result.complete = false;
            result.issues.push(issue(
                None,
                IssueClass::UnsupportedMetadata,
                "Invalid native mount-path encoding",
            ));
            continue;
        };
        let path = PathBuf::from(OsString::from_vec(path));
        if !path.is_absolute() {
            result.complete = false;
            result.issues.push(issue(
                Some(&path),
                IssueClass::UnsupportedMetadata,
                "Mount path is not absolute",
            ));
            continue;
        }
        if result.values.len() >= MAX_CANDIDATES {
            result.complete = false;
            result.issues.push(issue(
                None,
                IssueClass::ResourceLimit,
                "Mounted-volume discovery limit reached",
            ));
            break;
        }
        let device = std::str::from_utf8(fields[2])
            .ok()
            .filter(|value| {
                value.split_once(':').is_some_and(|(major, minor)| {
                    !major.is_empty()
                        && !minor.is_empty()
                        && major.bytes().all(|b| b.is_ascii_digit())
                        && minor.bytes().all(|b| b.is_ascii_digit())
                })
            })
            .map(str::to_owned);
        result.values.push(ParsedCandidate {
            candidate: Candidate {
                label: format!("{} ({})", String::from_utf8_lossy(&source), path.display()),
                path,
                filesystem: String::from_utf8_lossy(filesystem).into_owned(),
                removable: None,
            },
            device,
        });
    }
    result.values.sort_by(|a, b| {
        a.candidate
            .path
            .components()
            .count()
            .cmp(&b.candidate.path.components().count())
            .then_with(|| a.candidate.path.cmp(&b.candidate.path))
    });
    result
}
fn unescape(bytes: &[u8]) -> io::Result<Vec<u8>> {
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            let octal = bytes.get(index + 1..index + 4).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "Truncated mount escape")
            })?;
            if !octal.iter().all(|byte| (b'0'..=b'7').contains(byte)) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Invalid mount escape",
                ));
            }
            let value = u16::from(octal[0] - b'0') * 64
                + u16::from(octal[1] - b'0') * 8
                + u16::from(octal[2] - b'0');
            let value = u8::try_from(value).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Mount escape exceeds byte range",
                )
            })?;
            if value == 0 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "NUL mount path"));
            }
            output.push(value);
            index += 4;
        } else {
            if bytes[index] == 0 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "NUL mount path"));
            }
            output.push(bytes[index]);
            index += 1;
        }
    }
    Ok(output)
}
fn removable(device: &str) -> Option<bool> {
    let target = std::fs::canonicalize(Path::new("/sys/dev/block").join(device)).ok()?;
    if !target.starts_with("/sys/devices") {
        return None;
    }
    for directory in [Some(target.as_path()), target.parent()]
        .into_iter()
        .flatten()
    {
        let mut value = String::new();
        let Ok(file) = File::open(directory.join("removable")) else {
            continue;
        };
        if file.take(16).read_to_string(&mut value).is_ok() {
            match value.trim() {
                "1" => return Some(true),
                "0" => return Some(false),
                _ => {}
            }
        }
    }
    None
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mount_parser_preserves_native_bytes_and_escapes() {
        let table = b"24 1 8:1 / /media/a\\040b\\134c\xff rw - ext4 /dev/sda1 rw\n";
        let result = parse_mountinfo(table);
        assert!(result.complete);
        assert_eq!(result.values.len(), 1);
        assert_eq!(
            NativePath::from_path(&result.values[0].candidate.path).bytes,
            b"/media/a b\\c\xff"
        );
    }
    #[test]
    fn network_virtual_and_unknown_are_not_advertised() {
        let table=b"1 0 0:1 / /proc rw - proc proc rw\n2 0 0:2 / /net rw - nfs host:/ rw\n3 0 8:3 / /special rw - strange /dev/sda3 rw\n4 0 8:4 / /data rw - ext4 /dev/sda4 rw\n";
        let result = parse_mountinfo(table);
        assert_eq!(result.values.len(), 1);
        assert_eq!(result.values[0].candidate.path, Path::new("/data"));
        assert!(!result.complete);
    }
    #[test]
    fn unsupported_optical_and_unknown_sources_are_incomplete() {
        for line in [
            b"1 0 8:1 / /disc rw - iso9660 /dev/sr0 rw\n".as_slice(),
            b"1 0 0:1 / /unknown rw - mystery UUID=abc rw\n".as_slice(),
        ] {
            let result = parse_mountinfo(line);
            assert!(!result.complete);
            assert!(result.values.is_empty());
            assert_eq!(result.issues.len(), 1);
        }
    }
    #[test]
    fn malformed_record_work_and_diagnostics_are_bounded() {
        let result = parse_mountinfo(&b"broken\n".repeat(MAX_ISSUES * 2));
        assert!(!result.complete);
        assert!(result.issues.len() < MAX_ISSUES);
        assert_eq!(
            result.issues.last().unwrap().class,
            IssueClass::ResourceLimit
        );
    }
    #[test]
    fn malformed_tables_and_escapes_are_explicit() {
        assert!(!parse_mountinfo(b"broken\n").complete);
        assert!(unescape(b"bad\\40").is_err());
        assert!(unescape(b"bad\\000").is_err());
        assert!(unescape(b"bad\\777").is_err());
        assert_eq!(unescape(b"a\\011b\\012c").unwrap(), b"a\tb\nc");
    }
}
