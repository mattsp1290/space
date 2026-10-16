//! The Windows adapter only opens metadata handles. It never requests file data,
//! reads reparse payloads, enables privileges, or follows a reparse point.
//!
//! Unsafe code is contained in this platform adapter. SDK queries borrow live handles and
//! write to initialized, correctly sized output buffers. Directory iteration
//! reopens the held object with list-directory rights, never an ambient path.
#![allow(unsafe_code)]

use super::*;
use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{OpenOptions, OpenOptionsExt};
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
use windows_sys::Win32::Foundation::{ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_OFFLINE, FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS, FILE_ATTRIBUTE_RECALL_ON_OPEN,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_NO_RECALL, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_BOTH_DIR_INFO, FILE_ID_INFO,
    FILE_LIST_DIRECTORY, FILE_NAME_OPENED, FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FILE_STANDARD_INFO, FileAttributeTagInfo, FileIdBothDirectoryInfo,
    FileIdBothDirectoryRestartInfo, FileIdInfo, FileStandardInfo, GetFileInformationByHandleEx,
    GetFinalPathNameByHandleW, ReOpenFile, VOLUME_NAME_DOS, VOLUME_NAME_GUID,
};
use windows_sys::Win32::System::SystemServices::{
    IO_REPARSE_TAG_CLOUD, IO_REPARSE_TAG_CLOUD_MASK, IO_REPARSE_TAG_FILE_PLACEHOLDER,
    IO_REPARSE_TAG_MOUNT_POINT, IO_REPARSE_TAG_ONEDRIVE, IO_REPARSE_TAG_STORAGE_SYNC,
    IO_REPARSE_TAG_SYMLINK,
};

const METADATA_OPEN_FLAGS: u32 =
    FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_NO_RECALL;

fn metadata_file(parent: &Dir, name: &OsStr) -> io::Result<std::fs::File> {
    // A single native component prevents intermediate path components from
    // bypassing the caller's component-by-component reparse checks.
    NativeName::from_os(name).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let mut options = OpenOptions::new();
    options
        .access_mode(0)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(METADATA_OPEN_FLAGS)
        .follow(FollowSymlinks::No);
    // cap-primitives 4.0.3 preserves an explicitly supplied OPEN_REPARSE_POINT
    // with follow(No), opening the reparse object rather than its target.
    // Its relative CreateFileAtW also supplies FILE_READ_ATTRIBUTES internally;
    // requesting zero here excludes data access, not that required metadata right.
    parent.open_with(name, &options).map(|file| file.into_std())
}

pub(crate) fn observe(parent: &Dir, name: &OsStr) -> io::Result<Observation> {
    observation(&metadata_file(parent, name)?)
}

pub(crate) fn directory(dir: &Dir) -> io::Result<Observation> {
    // A cloned owner makes a safe std::fs::File view without manufacturing or
    // transferring a borrowed raw handle. No path is reopened.
    let file = dir.try_clone()?.into_std_file();
    require_directory(observation(&file)?)
}

pub(crate) fn open_child(parent: &Dir, name: &OsStr) -> io::Result<Dir> {
    let file = metadata_file(parent, name)?;
    require_directory(observation(&file)?)?;
    // metadata_file omits FILE_SHARE_DELETE, as required by cap-std's Windows
    // Dir::from_std_file contract to pin the directory against rename/delete.
    Ok(Dir::from_std_file(file))
}

pub(crate) fn open_anchor(path: &std::path::Path) -> io::Result<Dir> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::path::{Component, Prefix};
    let mut components = path.components();
    let drive = matches!(components.next(), Some(Component::Prefix(prefix))
        if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)));
    if !drive || components.next() != Some(Component::RootDir) || components.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Windows scan anchor must be an absolute drive root",
        ));
    }
    let file = std::fs::OpenOptions::new()
        // Unlike cap's relative open, this std open needs an explicit metadata
        // right for FileAttributeTagInfo. FILE_READ_ATTRIBUTES is not file data.
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(METADATA_OPEN_FLAGS)
        .open(path)?;
    require_directory(observation(&file)?)?;
    Ok(Dir::from_std_file(file))
}

// Microsoft requires an eight-byte-aligned directory information buffer. Keeping
// bytes initialized makes even malformed kernel/provider output safe to parse.
const DIRECTORY_BUFFER_BYTES: usize = 64 * 1024;
#[repr(align(8))]
struct DirectoryBuffer([u8; DIRECTORY_BUFFER_BYTES]);

pub(crate) struct Entries {
    file: std::fs::File,
    buffer: Box<DirectoryBuffer>,
    next_offset: Option<usize>,
    restart: bool,
    done: bool,
}

pub(crate) fn entries(dir: &Dir) -> io::Result<Entries> {
    let original = dir.try_clone()?.into_std_file();
    let before = require_directory(observation(&original)?)?;
    // SAFETY: original owns a live directory handle throughout this synchronous
    // call. ReOpenFile operates on that held object, not its current path. It
    // returns a new owning handle or INVALID_HANDLE_VALUE. These flags request
    // directory listing and metadata, retain no-delete sharing, and suppress reparse
    // processing. No privilege is enabled and no regular-file data is requested.
    let raw = unsafe {
        ReOpenFile(
            original.as_raw_handle(),
            // ReOpenFile replaces the access mask; it does not inherit the
            // metadata rights needed by our post-open and refill tag checks.
            FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            METADATA_OPEN_FLAGS,
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(sdk_error("ReOpenFile(directory listing)"));
    }
    // SAFETY: the successful ReOpenFile result is a new, valid owned handle.
    // Ownership transfers exactly once into OwnedHandle, which closes it on
    // every exit path; original remains separately owned and is not consumed.
    let file = std::fs::File::from(unsafe { OwnedHandle::from_raw_handle(raw) });
    let after = require_directory(observation(&file)?)?;
    if before.identity != after.identity {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Directory identity changed while opening enumeration handle",
        ));
    }
    Ok(Entries {
        file,
        buffer: Box::new(DirectoryBuffer([0; DIRECTORY_BUFFER_BYTES])),
        next_offset: None,
        restart: true,
        done: false,
    })
}

impl Entries {
    fn refill(&mut self) -> io::Result<bool> {
        if classify_link(attribute_info(self.file.as_handle())?, None, false) != LinkState::None {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Directory became a reparse point or offline during enumeration",
            ));
        }
        self.buffer.0.fill(0);
        let class = if self.restart {
            FileIdBothDirectoryRestartInfo
        } else {
            FileIdBothDirectoryInfo
        };
        // SAFETY: file owns the live directory. The initialized, eight-byte-
        // aligned buffer is writable for the exact advertised fixed byte count.
        // Both information classes return FILE_ID_BOTH_DIR_INFO records. The
        // call is synchronous and retains neither handle nor buffer pointer.
        let success = unsafe {
            GetFileInformationByHandleEx(
                self.file.as_raw_handle(),
                class,
                self.buffer.0.as_mut_ptr().cast(),
                DIRECTORY_BUFFER_BYTES as u32,
            )
        };
        if success == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                self.done = true;
                return Ok(false);
            }
            return Err(io::Error::new(
                error.kind(),
                format!("GetFileInformationByHandleEx(directory entries): {error}"),
            ));
        }
        self.restart = false;
        self.next_offset = Some(0);
        Ok(true)
    }
}

impl Iterator for Entries {
    type Item = io::Result<OsString>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.done {
                return None;
            }
            if self.next_offset.is_none() {
                match self.refill() {
                    Ok(true) => {}
                    Ok(false) => return None,
                    Err(error) => {
                        self.done = true;
                        return Some(Err(error));
                    }
                }
            }
            let offset = self.next_offset?;
            match parse_directory_entry(&self.buffer.0, offset) {
                Ok((name, next_offset)) => {
                    self.next_offset = next_offset;
                    if let Some(name) = name {
                        return Some(Ok(name));
                    }
                }
                Err(error) => {
                    self.done = true;
                    return Some(Err(error));
                }
            }
        }
    }
}

fn invalid_directory_record() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Malformed Windows directory record",
    )
}

fn parse_directory_entry(
    buffer: &[u8],
    offset: usize,
) -> io::Result<(Option<OsString>, Option<usize>)> {
    const NAME_OFFSET: usize = std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileName);
    const LENGTH_OFFSET: usize = std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileNameLength);
    const NEXT_OFFSET: usize = std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, NextEntryOffset);
    if !offset.is_multiple_of(8) {
        return Err(invalid_directory_record());
    }
    let record = buffer.get(offset..).ok_or_else(invalid_directory_record)?;
    if record.len() < NAME_OFFSET {
        return Err(invalid_directory_record());
    }
    let read_u32 = |position| -> io::Result<u32> {
        let bytes: [u8; 4] = record
            .get(position..position + 4)
            .ok_or_else(invalid_directory_record)?
            .try_into()
            .map_err(|_| invalid_directory_record())?;
        Ok(u32::from_le_bytes(bytes))
    };
    let name_length = read_u32(LENGTH_OFFSET)? as usize;
    if name_length == 0 || !name_length.is_multiple_of(2) {
        return Err(invalid_directory_record());
    }
    let name_end = NAME_OFFSET
        .checked_add(name_length)
        .ok_or_else(invalid_directory_record)?;
    let name_bytes = record
        .get(NAME_OFFSET..name_end)
        .ok_or_else(invalid_directory_record)?;
    let next_relative = read_u32(NEXT_OFFSET)? as usize;
    let next = if next_relative == 0 {
        None
    } else {
        if !next_relative.is_multiple_of(8) || next_relative < name_end {
            return Err(invalid_directory_record());
        }
        let next = offset
            .checked_add(next_relative)
            .ok_or_else(invalid_directory_record)?;
        let minimum_end = next
            .checked_add(NAME_OFFSET + 2)
            .ok_or_else(invalid_directory_record)?;
        if minimum_end > buffer.len() {
            return Err(invalid_directory_record());
        }
        Some(next)
    };
    let name: Vec<u16> = name_bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
        .collect();
    if name == [u16::from(b'.')] || name == [u16::from(b'.'), u16::from(b'.')] {
        return Ok((None, next));
    }
    // NativeName checks separators, NUL, colon, and empty names. Unpaired UTF-16
    // surrogates are legal native names and must survive without lossy decoding.
    NativeName::windows(name.clone()).map_err(|_| invalid_directory_record())?;
    Ok((Some(OsString::from_wide(&name)), next))
}

fn require_directory(observed: Observation) -> io::Result<Observation> {
    if observed.link != LinkState::None {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Refusing to descend into a reparse point or offline directory",
        ));
    }
    if !observed.metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "Opened object is not a directory",
        ));
    }
    if observed.identity.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Full Windows directory identity is unavailable",
        ));
    }
    Ok(observed)
}

fn observation(file: &std::fs::File) -> io::Result<Observation> {
    let handle = file.as_handle();
    // Attribute failure is an error, since otherwise a redirecting or offline
    // object could be mistaken for an ordinary directory.
    let attributes = attribute_info(handle)?;
    let identity = file_id_info(handle);
    let standard = standard_info(handle);
    // Do not rely on cap-std's legacy 64-bit file index, including as a fallback
    // when the full 128-bit ID is unsupported (notably on some network shares).
    let metadata = Metadata::from_just_metadata(file.metadata()?);
    Ok(assemble_observation(
        metadata, identity, standard, attributes,
    ))
}

fn assemble_observation(
    metadata: Metadata,
    identity: io::Result<FILE_ID_INFO>,
    standard: io::Result<FILE_STANDARD_INFO>,
    attributes: FILE_ATTRIBUTE_TAG_INFO,
) -> Observation {
    let identity = identity.ok().map(|info| FileIdentity {
        volume: ObservedIdentity {
            scheme: IdentityScheme::WindowsVolume,
            value: info.VolumeSerialNumber.to_le_bytes().to_vec(),
            strength: IdentityStrength::Observed,
        },
        file: ObservedIdentity {
            scheme: IdentityScheme::WindowsFileId,
            value: info.FileId.Identifier.to_vec(),
            strength: IdentityStrength::Observed,
        },
    });
    let standard = standard.ok();
    let allocated = standard
        .as_ref()
        .and_then(|info| u64::try_from(info.AllocationSize).ok());
    let link = classify_link(attributes, standard.as_ref(), metadata.is_file());
    Observation {
        metadata,
        identity,
        allocated,
        link,
    }
}

fn classify_link(
    attributes: FILE_ATTRIBUTE_TAG_INFO,
    standard: Option<&FILE_STANDARD_INFO>,
    is_file: bool,
) -> LinkState {
    let flags = attributes.FileAttributes;
    let tag = attributes.ReparseTag;
    if flags & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        // A provider may use a name-surrogate reparse tag other than the two
        // common tags. Every such tag redirects name resolution.
        if tag == IO_REPARSE_TAG_SYMLINK {
            return LinkState::Symlink;
        }
        if tag == IO_REPARSE_TAG_MOUNT_POINT || tag & 0x2000_0000 != 0 {
            return LinkState::RedirectingReparse;
        }
        if tag & !IO_REPARSE_TAG_CLOUD_MASK == IO_REPARSE_TAG_CLOUD
            || matches!(
                tag,
                IO_REPARSE_TAG_FILE_PLACEHOLDER
                    | IO_REPARSE_TAG_ONEDRIVE
                    | IO_REPARSE_TAG_STORAGE_SYNC
            )
        {
            return LinkState::Placeholder;
        }
        return LinkState::UnsupportedReparse;
    }
    if flags
        & (FILE_ATTRIBUTE_OFFLINE
            | FILE_ATTRIBUTE_RECALL_ON_OPEN
            | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS)
        != 0
    {
        return LinkState::Placeholder;
    }
    if is_file
        && let Some(info) = standard
        && info.NumberOfLinks > 1
    {
        return LinkState::Hardlink {
            observed_links: u64::from(info.NumberOfLinks),
        };
    }
    LinkState::None
}

fn file_id_info(handle: BorrowedHandle<'_>) -> io::Result<FILE_ID_INFO> {
    let mut info = FILE_ID_INFO::default();
    // SAFETY: handle borrows a live file for this synchronous call; info is an
    // initialized, aligned FILE_ID_INFO, matching FileIdInfo and its exact size.
    // The API neither retains the buffer nor takes ownership of the handle.
    let success = unsafe {
        GetFileInformationByHandleEx(
            handle.as_raw_handle(),
            FileIdInfo,
            (&mut info as *mut FILE_ID_INFO).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if success == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(info)
    }
}

fn standard_info(handle: BorrowedHandle<'_>) -> io::Result<FILE_STANDARD_INFO> {
    let mut info = FILE_STANDARD_INFO::default();
    // SAFETY: the borrowed live handle and initialized FILE_STANDARD_INFO stay
    // valid for the call. FileStandardInfo writes this SDK structure, including
    // its SDK BOOLEAN fields, within the exact supplied buffer size.
    let success = unsafe {
        GetFileInformationByHandleEx(
            handle.as_raw_handle(),
            FileStandardInfo,
            (&mut info as *mut FILE_STANDARD_INFO).cast(),
            std::mem::size_of::<FILE_STANDARD_INFO>() as u32,
        )
    };
    if success == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(info)
    }
}

fn attribute_info(handle: BorrowedHandle<'_>) -> io::Result<FILE_ATTRIBUTE_TAG_INFO> {
    let mut info = FILE_ATTRIBUTE_TAG_INFO::default();
    // SAFETY: handle remains borrowed and live. The initialized output is the
    // SDK type required by FileAttributeTagInfo, with its full exact byte size.
    let success = unsafe {
        GetFileInformationByHandleEx(
            handle.as_raw_handle(),
            FileAttributeTagInfo,
            (&mut info as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
            std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    };
    if success == 0 {
        Err(sdk_error(
            "GetFileInformationByHandleEx(FileAttributeTagInfo)",
        ))
    } else {
        Ok(info)
    }
}

fn sdk_error(operation: &str) -> io::Error {
    let error = io::Error::last_os_error();
    io::Error::new(error.kind(), format!("{operation}: {error}"))
}

/// The entire 64-bit volume serial returned with the 128-bit file ID is kept.
/// Reparse mount points/junctions are independently rejected before descent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Boundary {
    volume: u64,
}

pub(crate) fn boundary(dir: &Dir, policy: &ScanPolicy) -> Result<Boundary, (IssueClass, String)> {
    let handle = dir.as_handle();
    let identity = file_id_info(handle).map_err(|e| {
        (
            IssueClass::UnsupportedMetadata,
            format!("Windows volume identity is unavailable: {e}"),
        )
    })?;
    if policy.exclude_network_filesystems || policy.exclude_virtual_filesystems {
        classify_locality(
            final_path(handle, VOLUME_NAME_GUID),
            || final_path(handle, VOLUME_NAME_DOS),
            policy,
        )?;
    }
    Ok(Boundary {
        volume: identity.VolumeSerialNumber,
    })
}

fn classify_locality(
    guid_path: io::Result<Vec<u16>>,
    dos_path: impl FnOnce() -> io::Result<Vec<u16>>,
    policy: &ScanPolicy,
) -> Result<(), (IssueClass, String)> {
    match guid_path {
        Ok(path) if starts_with_ascii_ignore_case(&path, br"\\?\Volume{") => Ok(()),
        Ok(_) => Err((
            IssueClass::UnsupportedMetadata,
            "Windows returned an unrecognized volume GUID path".into(),
        )),
        Err(guid_error) => {
            // Microsoft documents that network shares have no volume GUID.
            // Other failures also occur, so an error alone is not evidence of
            // a network filesystem. Query the same handle for a UNC path.
            match dos_path() {
                Ok(path) if starts_with_ascii_ignore_case(&path, br"\\?\UNC\") => {
                    if policy.exclude_network_filesystems {
                        Err((
                            IssueClass::NetworkFilesystem,
                            "Network filesystem excluded".into(),
                        ))
                    } else {
                        Ok(())
                    }
                }
                Ok(_) => Err((
                    IssueClass::UnsupportedMetadata,
                    format!("Filesystem locality is not established: {guid_error}"),
                )),
                Err(dos_error) => Err((
                    IssueClass::UnsupportedMetadata,
                    format!(
                        "Filesystem locality is unavailable: GUID query: {guid_error}; DOS query: {dos_error}"
                    ),
                )),
            }
        }
    }
}

fn starts_with_ascii_ignore_case(path: &[u16], prefix: &[u8]) -> bool {
    path.len() >= prefix.len()
        && path.iter().zip(prefix).all(|(&unit, &byte)| {
            u8::try_from(unit).is_ok_and(|unit| unit.eq_ignore_ascii_case(&byte))
        })
}

fn final_path(handle: BorrowedHandle<'_>, volume_flag: u32) -> io::Result<Vec<u16>> {
    // Include the terminator and bound allocation under concurrent path changes.
    const MAX_PATH_UNITS: usize = 65_536;
    let mut buffer = vec![0_u16; 512];
    loop {
        // SAFETY: handle is live and borrowed; buffer is initialized and writable
        // for exactly buffer.len() UTF-16 units (bounded below u32::MAX). The API
        // does not retain either pointer. FILE_NAME_OPENED avoids normalization
        // queries through ancestor paths, especially on inaccessible SMB paths.
        let length = unsafe {
            GetFinalPathNameByHandleW(
                handle.as_raw_handle(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                volume_flag | FILE_NAME_OPENED,
            )
        };
        if length == 0 {
            return Err(io::Error::last_os_error());
        }
        let length = length as usize;
        if length < buffer.len() {
            buffer.truncate(length);
            return Ok(buffer);
        }
        // On insufficient space Windows returns required units including NUL.
        // Grow strictly even if a concurrently changing path returns equality.
        let capacity = length
            .checked_add(1)
            .filter(|n| *n <= MAX_PATH_UNITS)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Windows final path exceeds the bounded metadata buffer",
                )
            })?;
        buffer.resize(capacity, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "space-windows-metadata-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn dir(&self) -> Dir {
            Dir::open_ambient_dir(&self.0, cap_std::ambient_authority()).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn native_identity_allocation_hardlink_and_local_root() {
        let fixture = Fixture::new();
        std::fs::write(fixture.0.join("original"), [7_u8; 8193]).unwrap();
        std::fs::hard_link(fixture.0.join("original"), fixture.0.join("alias")).unwrap();
        let root = fixture.dir();
        let original = observe(&root, OsStr::new("original")).unwrap();
        let alias = observe(&root, OsStr::new("alias")).unwrap();
        let identity = original
            .identity
            .as_ref()
            .expect("native full Windows file ID");
        assert_eq!(identity.file.value.len(), 16);
        assert_eq!(identity.volume.value.len(), 8);
        assert_eq!(original.identity, alias.identity);
        assert_eq!(original.allocated, alias.allocated);
        assert!(original.allocated.is_some_and(|bytes| bytes > 0));
        assert_eq!(original.link, LinkState::Hardlink { observed_links: 2 });
        assert!(directory(&root).unwrap().identity.is_some());
        assert!(boundary(&root, &ScanPolicy::default()).is_ok());
        std::fs::create_dir(fixture.0.join("child")).unwrap();
        let child = open_child(&root, OsStr::new("child")).unwrap();
        assert_eq!(
            boundary(&root, &ScanPolicy::default()).unwrap(),
            boundary(&child, &ScanPolicy::default()).unwrap()
        );
        assert!(entries(&child).unwrap().next().is_none());
        assert!(open_child(&root, OsStr::new("original")).is_err());
        assert!(observe(&root, OsStr::new("child\\anything")).is_err());
    }

    #[test]
    fn native_observation_does_not_request_read_access() {
        use std::os::windows::fs::OpenOptionsExt as _;
        let fixture = Fixture::new();
        let path = fixture.0.join("locked");
        let _locked = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(0)
            .open(path)
            .unwrap();
        // A data-reading open conflicts with share_mode(0), while a zero-access
        // metadata open is permitted by Windows sharing rules.
        assert!(std::fs::File::open(fixture.0.join("locked")).is_err());
        assert!(observe(&fixture.dir(), OsStr::new("locked")).is_ok());
    }

    #[test]
    fn native_sharing_failure_is_an_error_not_zero_size() {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::{DELETE, FILE_SHARE_DELETE};
        let fixture = Fixture::new();
        let path = fixture.0.join("locked");
        std::fs::write(&path, [1_u8; 12]).unwrap();
        // A live DELETE-access handle conflicts with our no-delete-sharing
        // policy, even though the scanner asks for no data access at all.
        let _locked = std::fs::OpenOptions::new()
            .access_mode(DELETE)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(path)
            .unwrap();
        assert!(observe(&fixture.dir(), OsStr::new("locked")).is_err());
    }

    #[test]
    fn native_reparse_is_observed_without_following() {
        let fixture = Fixture::new();
        std::fs::create_dir(fixture.0.join("target")).unwrap();
        let link = fixture.0.join("link");
        if let Err(error) = std::os::windows::fs::symlink_dir(fixture.0.join("target"), &link) {
            if error.raw_os_error() == Some(1314) {
                eprintln!(
                    "Native symlink fixture requires Developer Mode or symlink privilege: {error}"
                );
                return;
            }
            panic!("Could not create native reparse fixture: {error}");
        }
        let root = fixture.dir();
        let observed = observe(&root, OsStr::new("link")).unwrap();
        assert_eq!(observed.link, LinkState::Symlink);
        assert_ne!(
            observed.identity,
            observe(&root, OsStr::new("target")).unwrap().identity
        );
        assert!(open_child(&root, OsStr::new("link")).is_err());
        std::fs::remove_dir(link).unwrap();
    }

    #[test]
    fn reparse_tags_and_offline_attributes_are_conservative() {
        let classify = |tag| {
            classify_link(
                FILE_ATTRIBUTE_TAG_INFO {
                    FileAttributes: FILE_ATTRIBUTE_REPARSE_POINT,
                    ReparseTag: tag,
                },
                None,
                true,
            )
        };
        assert_eq!(classify(IO_REPARSE_TAG_SYMLINK), LinkState::Symlink);
        assert_eq!(
            classify(IO_REPARSE_TAG_MOUNT_POINT),
            LinkState::RedirectingReparse
        );
        assert_eq!(classify(0x2000_0042), LinkState::RedirectingReparse);
        for variant in 0..=15 {
            assert_eq!(
                classify(IO_REPARSE_TAG_CLOUD | (variant << 12)),
                LinkState::Placeholder
            );
        }
        for tag in [
            IO_REPARSE_TAG_ONEDRIVE,
            IO_REPARSE_TAG_FILE_PLACEHOLDER,
            IO_REPARSE_TAG_STORAGE_SYNC,
        ] {
            assert_eq!(classify(tag), LinkState::Placeholder);
        }
        assert_eq!(classify(0x8000_0042), LinkState::UnsupportedReparse);
        for flag in [
            FILE_ATTRIBUTE_OFFLINE,
            FILE_ATTRIBUTE_RECALL_ON_OPEN,
            FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS,
        ] {
            assert_eq!(
                classify_link(
                    FILE_ATTRIBUTE_TAG_INFO {
                        FileAttributes: flag,
                        ReparseTag: 0
                    },
                    None,
                    false
                ),
                LinkState::Placeholder
            );
        }
    }

    #[test]
    fn failed_queries_preserve_unknown_and_full_width_identity() {
        let fixture = Fixture::new();
        let metadata = Metadata::from_just_metadata(std::fs::metadata(&fixture.0).unwrap());
        let unavailable =
            || io::Error::new(io::ErrorKind::Unsupported, "fixture unsupported query");
        let observed = assemble_observation(
            metadata.clone(),
            Err(unavailable()),
            Err(unavailable()),
            FILE_ATTRIBUTE_TAG_INFO::default(),
        );
        assert!(observed.identity.is_none());
        assert!(observed.allocated.is_none());
        assert!(require_directory(observed).is_err());
        let mut identity = FILE_ID_INFO {
            VolumeSerialNumber: 0xfedc_ba98_7654_3210,
            ..Default::default()
        };
        identity.FileId.Identifier = [0x42; 16];
        identity.FileId.Identifier[15] = 0x93;
        let observed = assemble_observation(
            metadata,
            Ok(identity),
            Ok(FILE_STANDARD_INFO {
                AllocationSize: -1,
                ..Default::default()
            }),
            FILE_ATTRIBUTE_TAG_INFO::default(),
        );
        assert!(observed.allocated.is_none());
        let identity = observed.identity.unwrap();
        assert_eq!(
            identity.volume.value,
            0xfedc_ba98_7654_3210_u64.to_le_bytes()
        );
        assert_eq!(identity.file.value.len(), 16);
        assert_eq!(identity.file.value[15], 0x93);
    }

    #[test]
    fn locality_failures_do_not_masquerade_as_local_or_network() {
        let wide = |s: &str| s.encode_utf16().collect::<Vec<_>>();
        let unavailable =
            || io::Error::new(io::ErrorKind::Unsupported, "fixture unsupported path query");
        let policy = ScanPolicy::default();
        assert!(
            classify_locality(
                Ok(wide(
                    r"\\?\Volume{01234567-0123-0123-0123-012345678901}\root"
                )),
                || panic!("no DOS fallback for known GUID"),
                &policy
            )
            .is_ok()
        );
        let network = classify_locality(
            Err(unavailable()),
            || Ok(wide(r"\\?\UNC\server\share\root")),
            &policy,
        )
        .unwrap_err();
        assert_eq!(network.0, IssueClass::NetworkFilesystem);
        let unknown = classify_locality(Err(unavailable()), || Ok(wide(r"\\?\C:\root")), &policy)
            .unwrap_err();
        assert_eq!(unknown.0, IssueClass::UnsupportedMetadata);
        assert_eq!(
            classify_locality(Err(unavailable()), || Err(unavailable()), &policy)
                .unwrap_err()
                .0,
            IssueClass::UnsupportedMetadata
        );
        assert_eq!(
            classify_locality(
                Ok(wide("unexpected")),
                || panic!("invalid successful GUID result"),
                &policy
            )
            .unwrap_err()
            .0,
            IssueClass::UnsupportedMetadata
        );
    }

    fn encoded_record(name: &[u16]) -> Vec<u8> {
        let mut record = vec![0_u8; std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileName)];
        let length_offset = std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileNameLength);
        record[length_offset..length_offset + 4]
            .copy_from_slice(&((name.len() * 2) as u32).to_le_bytes());
        record.extend(name.iter().flat_map(|unit| unit.to_le_bytes()));
        record
    }

    #[test]
    fn directory_parser_preserves_native_names_and_skips_dot_entries() {
        use std::os::windows::ffi::OsStrExt as _;
        let name = [0x0061, 0xd800, 0x0062];
        let first = encoded_record(&name);
        let (observed, next) = parse_directory_entry(&first, 0).unwrap();
        assert_eq!(observed.unwrap().encode_wide().collect::<Vec<_>>(), name);
        assert!(next.is_none());
        let mut first = first;
        first.resize(first.len().next_multiple_of(8), 0);
        let next = first.len();
        first[..4].copy_from_slice(&(next as u32).to_le_bytes());
        first.extend(encoded_record(&[b'z' as u16]));
        assert_eq!(parse_directory_entry(&first, 0).unwrap().1, Some(next));
        assert_eq!(
            parse_directory_entry(&first, next).unwrap().0.unwrap(),
            OsString::from("z")
        );
        assert!(
            parse_directory_entry(&encoded_record(&[b'.' as u16]), 0)
                .unwrap()
                .0
                .is_none()
        );
        assert!(
            parse_directory_entry(&encoded_record(&[b'.' as u16; 2]), 0)
                .unwrap()
                .0
                .is_none()
        );
    }

    #[test]
    fn directory_parser_rejects_malformed_bounds_lengths_offsets_and_names() {
        let valid = encoded_record(&[b'a' as u16]);
        for length in 0..valid.len() {
            assert!(parse_directory_entry(&valid[..length], 0).is_err());
        }
        assert!(parse_directory_entry(&valid, 1).is_err());
        assert!(parse_directory_entry(&valid, usize::MAX).is_err());
        let length_offset = std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileNameLength);
        for length in [0_u32, 1, 3, u32::MAX] {
            let mut record = valid.clone();
            record[length_offset..length_offset + 4].copy_from_slice(&length.to_le_bytes());
            assert!(parse_directory_entry(&record, 0).is_err());
        }
        for offset in [1_u32, 8, 103, 104, 108, 112, u32::MAX] {
            let mut record = valid.clone();
            record[..4].copy_from_slice(&offset.to_le_bytes());
            assert!(parse_directory_entry(&record, 0).is_err());
        }
        for name in [
            vec![0],
            vec![b'/' as u16],
            vec![b'\\' as u16],
            vec![b':' as u16],
        ] {
            assert!(parse_directory_entry(&encoded_record(&name), 0).is_err());
        }
    }

    #[test]
    fn native_enumeration_is_bounded_and_continues_across_batches() {
        let fixture = Fixture::new();
        let mut expected = std::collections::BTreeSet::new();
        for index in 0..700 {
            let name = format!("entry-{index:04}-{}", "x".repeat(120));
            std::fs::write(fixture.0.join(&name), []).unwrap();
            expected.insert(OsString::from(name));
        }
        let mut iterator = entries(&fixture.dir()).unwrap();
        assert_eq!(iterator.buffer.0.len(), DIRECTORY_BUFFER_BYTES);
        let mut actual = std::collections::BTreeSet::new();
        for entry in iterator.by_ref() {
            assert!(
                actual.insert(entry.unwrap()),
                "directory entry repeated across batches"
            );
        }
        assert_eq!(actual, expected);
        assert!(iterator.next().is_none());
    }

    // This test-only mutator creates a junction in a disposable fixture. It is
    // never reachable from scanner code and never enables symlink privileges.
    fn set_fixture_junction(path: &std::path::Path, target: &std::path::Path) -> io::Result<()> {
        use std::os::windows::ffi::OsStrExt as _;
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::System::{IO::DeviceIoControl, Ioctl::FSCTL_SET_REPARSE_POINT};
        let target = std::fs::canonicalize(target)?;
        let target: Vec<u16> = target.as_os_str().encode_wide().collect();
        let target = target.strip_prefix(&[92, 92, 63, 92]).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "fixture requires a local extended Windows path",
            )
        })?;
        let substitute: Vec<u16> = [92, 63, 63, 92]
            .into_iter()
            .chain(target.iter().copied())
            .collect();
        let substitute_bytes = u16::try_from(substitute.len() * 2).unwrap();
        let print_bytes = u16::try_from(target.len() * 2).unwrap();
        let data_length = 8 + substitute_bytes + 2 + print_bytes + 2;
        let mut buffer = Vec::with_capacity(usize::from(data_length) + 8);
        buffer.extend(IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
        buffer.extend(data_length.to_le_bytes());
        buffer.extend(0_u16.to_le_bytes());
        buffer.extend(0_u16.to_le_bytes());
        buffer.extend(substitute_bytes.to_le_bytes());
        buffer.extend((substitute_bytes + 2).to_le_bytes());
        buffer.extend(print_bytes.to_le_bytes());
        buffer.extend(substitute.iter().flat_map(|unit| unit.to_le_bytes()));
        buffer.extend(0_u16.to_le_bytes());
        buffer.extend(target.iter().flat_map(|unit| unit.to_le_bytes()));
        buffer.extend(0_u16.to_le_bytes());
        let file = std::fs::OpenOptions::new()
            .write(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        let mut returned = 0_u32;
        // SAFETY: file owns a live disposable fixture directory; buffer contains
        // the checked-length mount-point reparse layout and remains live for the
        // synchronous call. There is no output buffer; returned is initialized
        // and writable. No pointer/handle ownership is transferred or retained.
        let success = unsafe {
            DeviceIoControl(
                file.as_raw_handle(),
                FSCTL_SET_REPARSE_POINT,
                buffer.as_ptr().cast(),
                buffer.len() as u32,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if success == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    #[test]
    fn native_junction_is_observed_and_refused() {
        let fixture = Fixture::new();
        let target = fixture.0.join("target");
        let junction = fixture.0.join("junction");
        std::fs::create_dir(&target).unwrap();
        std::fs::create_dir(&junction).unwrap();
        set_fixture_junction(&junction, &target).unwrap();
        let root = fixture.dir();
        assert_eq!(
            observe(&root, OsStr::new("junction")).unwrap().link,
            LinkState::RedirectingReparse
        );
        assert!(open_child(&root, OsStr::new("junction")).is_err());
        std::fs::remove_dir(&junction).unwrap();
    }

    #[test]
    fn native_reparse_substitution_never_enumerates_outside_held_directory() {
        let fixture = Fixture::new();
        let outside = fixture.0.join("outside");
        let victim = fixture.0.join("victim");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("outside-marker"), []).unwrap();
        std::fs::create_dir(&victim).unwrap();
        let root = fixture.dir();
        let child = open_child(&root, OsStr::new("victim")).unwrap();
        let mut iterator = entries(&child).unwrap();
        match set_fixture_junction(&victim, &outside) {
            Ok(()) => assert!(iterator.next().is_some_and(|entry| entry.is_err())),
            Err(error) if matches!(error.raw_os_error(), Some(5 | 32)) => {
                // Some Windows versions/filesystems reject the mutation while
                // no-delete-share directory handles are held. This is safe too.
                assert!(iterator.next().is_none());
            }
            Err(error) => panic!("Unexpected fixture substitution error: {error}"),
        }
        assert!(iterator.next().is_none());
        drop(iterator);
        drop(child);
        std::fs::remove_dir(victim).unwrap();
    }
}
