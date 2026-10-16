use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

#[derive(
    serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
pub enum NativeEncoding {
    UnixBytes,
    WindowsUtf16,
}
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum NativePathError {
    EmptyName,
    InvalidComponent,
    OddUtf16Length,
    WrongPlatform,
}
impl std::fmt::Display for NativePathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for NativePathError {}

/// One nonempty filename component. UTF-16 may contain unpaired surrogates.
#[derive(
    serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
pub enum NativeName {
    Unix(Vec<u8>),
    Windows(Vec<u16>),
}
impl NativeName {
    pub fn unix(bytes: Vec<u8>) -> Result<Self, NativePathError> {
        let n = Self::Unix(bytes);
        n.validate()?;
        Ok(n)
    }
    pub fn windows(units: Vec<u16>) -> Result<Self, NativePathError> {
        let n = Self::Windows(units);
        n.validate()?;
        Ok(n)
    }
    /// Validate also at trust boundaries: enum variants are public for adapters.
    pub fn validate(&self) -> Result<(), NativePathError> {
        let units: Vec<u16> = match self {
            Self::Unix(v) => v.iter().map(|&b| u16::from(b)).collect(),
            Self::Windows(v) => v.clone(),
        };
        if units.is_empty() {
            return Err(NativePathError::EmptyName);
        }
        if units == [46]
            || units == [46, 46]
            || units.iter().any(|&c| {
                c == 0 || c == 47 || matches!(self, Self::Windows(_)) && (c == 92 || c == 58)
            })
        {
            return Err(NativePathError::InvalidComponent);
        }
        Ok(())
    }
    pub fn encoding(&self) -> NativeEncoding {
        match self {
            Self::Unix(_) => NativeEncoding::UnixBytes,
            Self::Windows(_) => NativeEncoding::WindowsUtf16,
        }
    }
    /// Storage representation; NOT the Windows ordering key.
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Self::Unix(v) => v.clone(),
            Self::Windows(v) => v.iter().flat_map(|c| c.to_le_bytes()).collect(),
        }
    }
    pub fn from_bytes(encoding: NativeEncoding, bytes: &[u8]) -> Result<Self, NativePathError> {
        match encoding {
            NativeEncoding::UnixBytes => Self::unix(bytes.to_vec()),
            NativeEncoding::WindowsUtf16 => Self::windows(decode_utf16(bytes)?),
        }
    }
    /// SQLite BLOB ordering equals native byte/code-unit lexicographic ordering.
    pub fn sort_key(&self) -> Vec<u8> {
        match self {
            Self::Unix(v) => v.clone(),
            Self::Windows(v) => v.iter().flat_map(|c| c.to_be_bytes()).collect(),
        }
    }
    pub fn display(&self) -> String {
        match self {
            Self::Unix(v) => String::from_utf8_lossy(v).into_owned(),
            Self::Windows(v) => String::from_utf16_lossy(v),
        }
    }
    pub fn from_os(value: &std::ffi::OsStr) -> Result<Self, NativePathError> {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            Self::unix(value.as_bytes().to_vec())
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            Self::windows(value.encode_wide().collect())
        }
    }
    pub fn to_os(&self) -> Result<OsString, NativePathError> {
        self.validate()?;
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            match self {
                Self::Unix(v) => Ok(OsString::from_vec(v.clone())),
                _ => Err(NativePathError::WrongPlatform),
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            match self {
                Self::Windows(v) => Ok(OsString::from_wide(v)),
                _ => Err(NativePathError::WrongPlatform),
            }
        }
    }
}
fn decode_utf16(bytes: &[u8]) -> Result<Vec<u16>, NativePathError> {
    if !bytes.len().is_multiple_of(2) {
        return Err(NativePathError::OddUtf16Length);
    }
    Ok(bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect())
}
/// Full selected path stored only once. Display text must never be used for actions.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct NativePath {
    pub encoding: NativeEncoding,
    pub bytes: Vec<u8>,
}
impl NativePath {
    pub fn from_path(path: &Path) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            Self {
                encoding: NativeEncoding::UnixBytes,
                bytes: path.as_os_str().as_bytes().to_vec(),
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            Self {
                encoding: NativeEncoding::WindowsUtf16,
                bytes: path
                    .as_os_str()
                    .encode_wide()
                    .flat_map(u16::to_le_bytes)
                    .collect(),
            }
        }
    }
    pub fn validate(&self) -> Result<(), NativePathError> {
        if self.bytes.is_empty() {
            return Err(NativePathError::EmptyName);
        }
        match self.encoding {
            NativeEncoding::UnixBytes if self.bytes.contains(&0) => {
                Err(NativePathError::InvalidComponent)
            }
            NativeEncoding::WindowsUtf16 => {
                if decode_utf16(&self.bytes)?.contains(&0) {
                    Err(NativePathError::InvalidComponent)
                } else {
                    Ok(())
                }
            }
            _ => Ok(()),
        }
    }
    pub fn to_path_buf(&self) -> Result<PathBuf, NativePathError> {
        self.validate()?;
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            if self.encoding != NativeEncoding::UnixBytes {
                return Err(NativePathError::WrongPlatform);
            }
            Ok(PathBuf::from(OsString::from_vec(self.bytes.clone())))
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            if self.encoding != NativeEncoding::WindowsUtf16 {
                return Err(NativePathError::WrongPlatform);
            }
            Ok(PathBuf::from(OsString::from_wide(&decode_utf16(
                &self.bytes,
            )?)))
        }
    }
}
