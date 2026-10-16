//! Bounded, blocking, metadata-only scanning. Run on one dedicated worker.
//!
//! A synchronous batch callback supplies backpressure all the way to directory
//! enumeration. No file content stream is opened. Cancellation is cooperative:
//! a blocked filesystem call or callback cannot be forcibly interrupted.
mod platform;
use cap_std::fs::Dir;
use space_core::*;
use std::{
    io,
    path::{Component, Path, PathBuf},
    time::UNIX_EPOCH,
};

/// Limits bound open handles, pending records, and total work. A limit creates
/// explicit incomplete coverage, rather than claiming an empty subtree.
#[derive(Clone, Copy, Debug)]
pub struct ScanLimits {
    pub max_depth: usize,
    pub batch_size: usize,
    pub max_entries: u64,
}
impl Default for ScanLimits {
    fn default() -> Self {
        Self {
            max_depth: 128,
            batch_size: 256,
            max_entries: 10_000_000,
        }
    }
}
impl ScanLimits {
    fn validate(self) -> io::Result<()> {
        if !(1..=256).contains(&self.max_depth)
            || !(1..=4096).contains(&self.batch_size)
            || self.max_entries == 0
            || self.max_entries > i64::MAX as u64
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid scan resource limits",
            ));
        }
        Ok(())
    }
}

/// Opens each selected-path component without following redirects. Selection
/// uses an absolute native path; parent traversal is deliberately rejected.
fn open_root(path: &Path) -> io::Result<Dir> {
    open_root_with_policy(path, None)
}
fn open_root_with_policy(path: &Path, policy: Option<&ScanPolicy>) -> io::Result<Dir> {
    let absolute = std::path::absolute(path)?;
    let mut anchor = PathBuf::new();
    let mut components = Vec::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => {
                #[cfg(windows)]
                if !matches!(
                    prefix.kind(),
                    std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)
                ) {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "Network and device roots are unsupported",
                    ));
                }
                anchor.push(prefix.as_os_str());
            }
            Component::RootDir => anchor.push(component.as_os_str()),
            Component::Normal(name) => components.push(name),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Select a path without parent components",
                ));
            }
        }
    }
    let mut dir = platform::open_anchor(&anchor)?;
    reject_excluded_ancestor(&dir, policy)?;
    for name in components {
        let next = platform::open_child(&dir, name)?;
        reject_redirect(&platform::directory(&next)?)?;
        reject_excluded_ancestor(&next, policy)?;
        dir = next;
    }
    reject_redirect(&platform::directory(&dir)?)?;
    Ok(dir)
}
fn reject_excluded_ancestor(dir: &Dir, policy: Option<&ScanPolicy>) -> io::Result<()> {
    if let Some(policy) = policy.filter(|policy| !policy.excluded_directory_identities.is_empty())
        && identity_excluded(&platform::root_identity(dir)?, policy)?
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Selected directory is inside an excluded directory",
        ));
    }
    Ok(())
}
fn reject_redirect(observation: &platform::Observation) -> io::Result<()> {
    if !observation.metadata.is_dir() || !matches!(observation.link, LinkState::None) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Directory redirect or placeholder is not traversed",
        ));
    }
    Ok(())
}
/// Observe selected directory and volume identity separately. Observed IDs may
/// be reused; a match is not an eternal or cryptographic continuity guarantee.
pub fn inspect_root(path: &Path) -> io::Result<RootIdentity> {
    platform::with_no_hydration(|| platform::root_identity(&open_root(path)?))
}
pub fn validate_root(path: &Path, expected: &RootIdentity) -> io::Result<IdentityValidation> {
    Ok(expected.validate_against(&inspect_root(path)?))
}

struct Frame {
    dir: Dir,
    entries: platform::Entries,
    node: NodeId,
    failed: bool,
}
struct Output<F> {
    key: ScanKey,
    sequence: u64,
    records: Vec<ScanRecord>,
    limit: usize,
    sink: F,
}
impl<F: FnMut(ScanBatch) -> io::Result<()>> Output<F> {
    fn emit(&mut self, record: ScanRecord) -> io::Result<()> {
        self.records.push(record);
        if self.records.len() >= self.limit {
            self.flush()?;
        }
        Ok(())
    }
    fn flush(&mut self) -> io::Result<()> {
        if self.records.is_empty() {
            return Ok(());
        }
        let batch = ScanBatch {
            key: self.key,
            sequence: self.sequence,
            records: std::mem::replace(&mut self.records, Vec::with_capacity(self.limit)),
        };
        (self.sink)(batch)?;
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| io::Error::other("Batch sequence overflow"))?;
        Ok(())
    }
    fn issue(
        &mut self,
        node: Option<NodeId>,
        parent: Option<NodeId>,
        name: Option<NativeName>,
        class: IssueClass,
        detail: impl AsRef<str>,
    ) -> io::Result<()> {
        self.emit(ScanRecord::Issue(ScanIssue {
            node,
            parent,
            name,
            class,
            detail: ScanIssue::bounded_detail(detail.as_ref()),
        }))
    }
    fn finish_dir(&mut self, node: NodeId, enumeration: EnumerationState) -> io::Result<()> {
        self.emit(ScanRecord::DirectoryFinished { node, enumeration })
    }
}
fn io_class(error: &io::Error) -> IssueClass {
    if error
        .get_ref()
        .is_some_and(|error| error.is::<platform::ChangedObservation>())
    {
        return IssueClass::Changed;
    }
    #[cfg(target_os = "macos")]
    if error.raw_os_error() == Some(11) {
        return IssueClass::UnsupportedMetadata;
    } // EDEADLK: dataless materialization refused.

    match error.kind() {
        io::ErrorKind::PermissionDenied => IssueClass::PermissionDenied,
        io::ErrorKind::NotFound => IssueClass::Vanished,
        io::ErrorKind::Unsupported => IssueClass::UnsupportedMetadata,
        io::ErrorKind::NotConnected => IssueClass::Disconnected,
        _ => IssueClass::Io,
    }
}
fn entry(
    node: NodeId,
    parent: Option<NodeId>,
    name: Option<NativeName>,
    observation: &platform::Observation,
) -> EntryRecord {
    let meta = &observation.metadata;
    let kind = if meta.is_symlink() {
        EntryKind::Symlink
    } else if meta.is_dir() {
        EntryKind::Directory
    } else if meta.is_file() {
        EntryKind::RegularFile
    } else {
        EntryKind::Other
    };
    let modified = meta.modified().ok().map(|t| t.into_std()).and_then(|t| {
        match t.duration_since(UNIX_EPOCH) {
            Ok(d) => i64::try_from(d.as_secs()).ok().map(|seconds| Timestamp {
                seconds,
                nanos: d.subsec_nanos(),
            }),
            Err(e) => {
                let d = e.duration();
                i64::try_from(d.as_secs()).ok().and_then(|seconds| {
                    if d.subsec_nanos() == 0 {
                        Some(Timestamp {
                            seconds: -seconds,
                            nanos: 0,
                        })
                    } else {
                        seconds.checked_add(1).map(|s| Timestamp {
                            seconds: -s,
                            nanos: 1_000_000_000 - d.subsec_nanos(),
                        })
                    }
                })
            }
        }
    });
    EntryRecord {
        node,
        parent,
        display_name: name
            .as_ref()
            .map(NativeName::display)
            .unwrap_or_else(|| "Selected folder".into()),
        name,
        kind,
        identity: observation.identity.clone(),
        logical_bytes: (kind == EntryKind::RegularFile).then(|| meta.len()),
        allocated_bytes: if kind == EntryKind::RegularFile {
            observation.allocated
        } else {
            None
        },
        modified,
        link_state: observation.link,
        metadata_state: MetadataState::Complete,
    }
}

/// Emits observations only; the store owns hardlink attribution and totals.
/// Sink errors stop immediately, preserving the previously published scan.
pub fn scan<F>(
    root: &Path,
    spec: &ScanSpec,
    cancel: &Cancellation,
    limits: ScanLimits,
    sink: F,
) -> io::Result<ScanFinish>
where
    F: FnMut(ScanBatch) -> io::Result<()>,
{
    scan_impl(root, spec, cancel, limits, sink, &mut |_| {})
}
fn scan_impl<F>(
    root: &Path,
    spec: &ScanSpec,
    cancel: &Cancellation,
    limits: ScanLimits,
    sink: F,
    before_descent: &mut dyn FnMut(&[NativeName]),
) -> io::Result<ScanFinish>
where
    F: FnMut(ScanBatch) -> io::Result<()>,
{
    platform::with_no_hydration(|| scan_inner(root, spec, cancel, limits, sink, before_descent))
}
fn scan_inner<F>(
    root: &Path,
    spec: &ScanSpec,
    cancel: &Cancellation,
    limits: ScanLimits,
    sink: F,
    before_descent: &mut dyn FnMut(&[NativeName]),
) -> io::Result<ScanFinish>
where
    F: FnMut(ScanBatch) -> io::Result<()>,
{
    limits.validate()?;
    spec.policy
        .validate_for_encoding(NativePath::from_path(root).encoding)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let root_dir = open_root_with_policy(root, Some(&spec.policy))?;
    let identity = platform::root_identity(&root_dir)?;
    if identity_excluded(&identity, &spec.policy)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Selected directory is excluded from scanning",
        ));
    }
    if spec.expected_root.validate_against(&identity) != IdentityValidation::MatchingObservation {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Selected root identity changed or requires explicit re-selection",
        ));
    }
    if cancel.is_cancelled() {
        return Ok(ScanFinish {
            key: spec.key,
            root_after: identity,
            outcome: TraversalOutcome::Cancelled,
        });
    }
    let mut out = Output {
        key: spec.key,
        sequence: 0,
        records: Vec::with_capacity(limits.batch_size),
        limit: limits.batch_size,
        sink,
    };
    let root_node = NodeId::new(1).expect("constant ID");
    out.emit(ScanRecord::Entry(entry(
        root_node,
        None,
        None,
        &platform::directory(&root_dir)?,
    )))?;
    if cancel.is_cancelled() {
        out.finish_dir(root_node, EnumerationState::Cancelled)?;
        out.flush()?;
        return Ok(ScanFinish {
            key: spec.key,
            root_after: identity,
            outcome: TraversalOutcome::Cancelled,
        });
    }
    let boundary = match platform::boundary(&root_dir, &spec.policy) {
        Ok(b) => b,
        Err((class, detail)) => {
            out.issue(Some(root_node), None, None, class, detail)?;
            out.finish_dir(root_node, EnumerationState::BoundarySkipped)?;
            out.flush()?;
            return finish_scan(
                root,
                spec.key,
                &identity,
                cancel,
                TraversalOutcome::Finished,
            );
        }
    };
    let entries = match platform::entries(&root_dir) {
        Ok(entries) => entries,
        Err(error) => {
            out.issue(
                Some(root_node),
                None,
                None,
                io_class(&error),
                error.to_string(),
            )?;
            out.finish_dir(root_node, EnumerationState::Failed)?;
            out.flush()?;
            return finish_scan(
                root,
                spec.key,
                &identity,
                cancel,
                TraversalOutcome::Finished,
            );
        }
    };
    let mut stack = vec![Frame {
        dir: root_dir,
        entries,
        node: root_node,
        failed: false,
    }];
    let mut relative = Vec::new();
    let mut count = 1u64;
    let mut visited = 1u64;
    let mut outcome = TraversalOutcome::Finished;
    while !stack.is_empty() {
        if cancel.is_cancelled() {
            outcome = TraversalOutcome::Cancelled;
            break;
        }
        let frame = stack.last_mut().expect("nonempty stack");
        let next = frame.entries.next();
        let Some(next) = next else {
            let frame = stack.pop().expect("nonempty stack");
            out.finish_dir(
                frame.node,
                if frame.failed {
                    EnumerationState::Failed
                } else {
                    EnumerationState::Complete
                },
            )?;
            relative.pop();
            continue;
        };
        if visited >= limits.max_entries {
            out.issue(
                Some(frame.node),
                None,
                None,
                IssueClass::ResourceLimit,
                "Entry limit reached; remaining scope was not enumerated",
            )?;
            for frame in &stack {
                out.finish_dir(frame.node, EnumerationState::ResourceLimit)?;
            }
            stack.clear();
            break;
        }
        visited += 1;
        let item = match next {
            Ok(item) => item,
            Err(error) => {
                frame.failed = true;
                out.issue(
                    Some(frame.node),
                    None,
                    None,
                    io_class(&error),
                    error.to_string(),
                )?;
                continue;
            }
        };
        let os_name = item;
        let name = NativeName::from_os(&os_name)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        relative.push(name.clone());
        let excluded = spec
            .policy
            .exclusions
            .iter()
            .any(|path| relative.starts_with(path));
        relative.pop();
        if excluded {
            out.issue(
                None,
                Some(frame.node),
                Some(name),
                IssueClass::Excluded,
                "Excluded by scan policy",
            )?;
            continue;
        }
        let observed = match platform::observe(&frame.dir, &os_name) {
            Ok(o) => o,
            Err(error) => {
                frame.failed = true;
                out.issue(
                    None,
                    Some(frame.node),
                    Some(name),
                    io_class(&error),
                    error.to_string(),
                )?;
                continue;
            }
        };
        count += 1;
        let node = NodeId::new(count).map_err(io::Error::other)?;
        let record = entry(node, Some(frame.node), Some(name.clone()), &observed);
        let is_dir = record.kind == EntryKind::Directory;
        out.emit(ScanRecord::Entry(record))?;
        if cancel.is_cancelled() {
            outcome = TraversalOutcome::Cancelled;
            break;
        }
        if !is_dir {
            continue;
        }
        relative.push(name);
        if stack.len() >= limits.max_depth {
            out.issue(
                Some(node),
                None,
                None,
                IssueClass::ResourceLimit,
                "Open-directory depth limit reached",
            )?;
            out.finish_dir(node, EnumerationState::ResourceLimit)?;
            relative.pop();
            continue;
        }
        if let Err(error) = reject_redirect(&observed) {
            out.issue(
                Some(node),
                None,
                None,
                IssueClass::UnsafeDescent,
                error.to_string(),
            )?;
            out.finish_dir(node, EnumerationState::BoundarySkipped)?;
            relative.pop();
            continue;
        }
        before_descent(&relative);
        let parent = &stack.last().expect("nonempty stack").dir;
        let opened = (|| {
            let dir = platform::open_child(parent, &os_name)?;
            let actual = platform::directory(&dir)?;
            reject_redirect(&actual)?;
            if observed.identity != actual.identity || actual.identity.is_none() {
                return Err(platform::changed(
                    "Directory object changed between observation and open",
                ));
            }
            Ok(dir)
        })();
        let dir = match opened {
            Ok(dir) => dir,
            Err(error) => {
                out.issue(
                    Some(node),
                    None,
                    None,
                    match error.kind() {
                        io::ErrorKind::NotFound => IssueClass::Vanished,
                        io::ErrorKind::PermissionDenied => IssueClass::PermissionDenied,
                        _ if io_class(&error) == IssueClass::Changed => IssueClass::Changed,
                        _ if io_class(&error) == IssueClass::UnsupportedMetadata => {
                            IssueClass::UnsupportedMetadata
                        }
                        _ => IssueClass::UnsafeDescent,
                    },
                    error.to_string(),
                )?;
                out.finish_dir(node, EnumerationState::Failed)?;
                relative.pop();
                continue;
            }
        };
        let excluded_identity = platform::root_identity(&dir)
            .and_then(|identity| identity_excluded(&identity, &spec.policy));
        match excluded_identity {
            Ok(false) => {}
            Ok(true) => {
                out.issue(
                    Some(node),
                    None,
                    None,
                    IssueClass::Excluded,
                    "Opened directory identity is excluded",
                )?;
                out.finish_dir(node, EnumerationState::Excluded)?;
                relative.pop();
                continue;
            }
            Err(error) => {
                out.issue(
                    Some(node),
                    None,
                    None,
                    IssueClass::UnsupportedMetadata,
                    error.to_string(),
                )?;
                out.finish_dir(node, EnumerationState::Failed)?;
                relative.pop();
                continue;
            }
        }
        match platform::boundary(&dir, &spec.policy) {
            Ok(current) if spec.policy.exclude_nested_mounts && current != boundary => {
                out.issue(
                    Some(node),
                    None,
                    None,
                    IssueClass::MountBoundary,
                    "Nested mount excluded",
                )?;
                out.finish_dir(node, EnumerationState::BoundarySkipped)?;
                relative.pop();
                continue;
            }
            Err((class, detail)) => {
                out.issue(Some(node), None, None, class, detail)?;
                out.finish_dir(node, EnumerationState::BoundarySkipped)?;
                relative.pop();
                continue;
            }
            _ => {}
        }
        match platform::entries(&dir) {
            Ok(entries) => stack.push(Frame {
                dir,
                entries,
                node,
                failed: false,
            }),
            Err(error) => {
                out.issue(Some(node), None, None, io_class(&error), error.to_string())?;
                out.finish_dir(node, EnumerationState::Failed)?;
                relative.pop();
            }
        }
    }
    if outcome == TraversalOutcome::Cancelled {
        for frame in &stack {
            out.finish_dir(frame.node, EnumerationState::Cancelled)?;
        }
    }
    out.flush()?;
    finish_scan(root, spec.key, &identity, cancel, outcome)
}
fn identity_excluded(identity: &RootIdentity, policy: &ScanPolicy) -> io::Result<bool> {
    for excluded in &policy.excluded_directory_identities {
        match excluded.validate_against(identity) {
            IdentityValidation::MatchingObservation => return Ok(true),
            IdentityValidation::Changed => {}
            IdentityValidation::RequiresSelection => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "Cannot establish directory identity against cache exclusions",
                ));
            }
        }
    }
    Ok(false)
}
fn finish_scan(
    root: &Path,
    key: ScanKey,
    identity: &RootIdentity,
    cancel: &Cancellation,
    mut outcome: TraversalOutcome,
) -> io::Result<ScanFinish> {
    if cancel.is_cancelled() {
        return Ok(ScanFinish {
            key,
            root_after: identity.clone(),
            outcome: TraversalOutcome::Cancelled,
        });
    }
    let root_after = match inspect_root(root) {
        Ok(current) => current,
        Err(_) => {
            outcome = TraversalOutcome::Failed;
            identity.clone()
        }
    };
    if identity.validate_against(&root_after) != IdentityValidation::MatchingObservation {
        outcome = TraversalOutcome::Failed;
    }
    if cancel.is_cancelled() {
        outcome = TraversalOutcome::Cancelled;
    }
    Ok(ScanFinish {
        key,
        root_after,
        outcome,
    })
}

#[cfg(test)]
mod tests;
