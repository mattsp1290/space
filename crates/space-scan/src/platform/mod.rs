use cap_std::fs::{Dir, Metadata};
use space_core::*;
use std::{ffi::OsStr, io};

pub(crate) struct Observation {
    pub metadata: Metadata,
    pub identity: Option<FileIdentity>,
    pub allocated: Option<u64>,
    pub link: LinkState,
}
#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub(crate) use unix::*;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::*;

pub(crate) fn root_identity(dir: &Dir) -> io::Result<RootIdentity> {
    let observation = directory(dir)?;
    let identity = observation
        .identity
        .ok_or_else(|| io::Error::other("Directory identity unavailable"))?;
    Ok(RootIdentity {
        volume: identity.volume,
        directory: identity.file,
    })
}

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub(crate) use macos::with_no_hydration;
#[cfg(not(target_os = "macos"))]
pub(crate) fn with_no_hydration<T>(work: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    work()
}

#[derive(Debug)]
pub(crate) struct ChangedObservation(&'static str);
impl std::fmt::Display for ChangedObservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for ChangedObservation {}
pub(crate) fn changed(detail: &'static str) -> io::Error {
    io::Error::other(ChangedObservation(detail))
}
