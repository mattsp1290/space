//! Stable dashboard identities and frozen scan targets. Native paths are never
//! used as identity keys, and this model owns no database connections or pins.
use space_core::{IdentityValidation, NativePath, RootIdentity};
use space_scan::{MountedVolume, VolumeListing};
use space_store::RootRecord;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CardId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardKind {
    Disk,
    Folder,
}

#[derive(Clone, Debug)]
pub struct ScanTarget {
    pub card_id: CardId,
    pub path: NativePath,
    pub expected_root: RootIdentity,
    pub refresh_root: Option<RootRecord>,
}

#[derive(Clone, Debug)]
pub struct DiskCard {
    pub id: CardId,
    pub identity: RootIdentity,
    pub kind: CardKind,
    pub label: String,
    pub path: NativePath,
    pub aliases: Vec<NativePath>,
    pub connected: bool,
    pub volume: Option<MountedVolume>,
    pub root: Option<RootRecord>,
}

#[derive(Default)]
pub struct DiskRegistry {
    cards: Vec<DiskCard>,
    next_id: u64,
}

pub fn same_root(a: &RootIdentity, b: &RootIdentity) -> bool {
    a.validate_against(b) == IdentityValidation::MatchingObservation
}

impl DiskRegistry {
    pub fn cards(&self) -> &[DiskCard] {
        &self.cards
    }

    pub fn card(&self, id: CardId) -> Option<&DiskCard> {
        self.cards.iter().find(|card| card.id == id)
    }

    fn allocate(&mut self) -> CardId {
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("card identity exhausted");
        CardId(self.next_id)
    }

    /// Reconcile metadata-only discovery and *published* roots. An unmatched
    /// saved folder is unverified/offline until an explicit observation arrives;
    /// a previously inspected folder retains that observation for this session.
    /// Merely sharing a volume with a mounted disk does not verify a folder.
    /// Existing cards stay stable while a frozen batch may still reference them.
    pub fn reconcile(&mut self, listing: &VolumeListing, roots: &[RootRecord]) {
        for card in &mut self.cards {
            // Disk discovery does not include arbitrary nested folders. Absence
            // from that list cannot revoke a positive folder inspection. A
            // conflicting object at the exact native path *does* revoke it.
            let replaced = listing.volumes.iter().any(|volume| {
                (volume.mount_path == card.path || volume.aliases.contains(&card.path))
                    && !same_root(&card.identity, &volume.identity)
            });
            card.connected = card.kind == CardKind::Folder && card.connected && !replaced;
            card.volume = None;
            card.root = card.root.as_ref().and_then(|previous| {
                roots
                    .iter()
                    .find(|root| root.id == previous.id && root.identity == card.identity)
                    .cloned()
            });
        }
        for volume in &listing.volumes {
            // Weak observations must never automatically combine two cards.
            if !same_root(&volume.identity, &volume.identity) {
                continue;
            }
            let index = match self
                .cards
                .iter()
                .position(|card| same_root(&card.identity, &volume.identity))
            {
                Some(index) => index,
                None => {
                    let id = self.allocate();
                    self.cards.push(DiskCard {
                        id,
                        identity: volume.identity.clone(),
                        kind: CardKind::Disk,
                        label: volume.label.clone(),
                        path: volume.mount_path.clone(),
                        aliases: Vec::new(),
                        connected: true,
                        volume: None,
                        root: None,
                    });
                    self.cards.len() - 1
                }
            };
            let card = &mut self.cards[index];
            card.kind = CardKind::Disk;
            card.connected = true;
            if card.volume.is_none() {
                card.label = volume.label.clone();
                card.path = volume.mount_path.clone();
                card.volume = Some(volume.clone());
            }
            for alias in std::iter::once(&volume.mount_path).chain(volume.aliases.iter()) {
                if alias != &card.path && !card.aliases.contains(alias) {
                    card.aliases.push(alias.clone());
                }
            }
        }
        for root in roots {
            let index = self.cards.iter().position(|card| {
                card.root.as_ref().is_some_and(|saved| saved.id == root.id)
                    || same_root(&card.identity, &root.identity)
            });
            if let Some(index) = index {
                let card = &mut self.cards[index];
                // Prefer the already-associated lineage if several historical
                // path aliases published the same object as separate roots.
                if card.root.is_none() {
                    card.root = Some(root.clone());
                }
                if root.path != card.path && !card.aliases.contains(&root.path) {
                    card.aliases.push(root.path.clone());
                }
            } else {
                let id = self.allocate();
                self.cards.push(DiskCard {
                    id,
                    identity: root.identity.clone(),
                    kind: CardKind::Folder,
                    label: folder_label(&root.path),
                    path: root.path.clone(),
                    aliases: Vec::new(),
                    connected: false,
                    volume: None,
                    root: Some(root.clone()),
                });
            }
        }
        self.cards.sort_by_key(|card| {
            (
                !card.connected,
                matches!(card.kind, CardKind::Folder),
                card.id,
            )
        });
    }

    /// The caller inspected this exact native path off-thread. This is also how
    /// a cached folder is positively marked connected without volume-only guesses.
    pub fn register_folder(
        &mut self,
        path: NativePath,
        identity: RootIdentity,
        refresh_root: Option<RootRecord>,
    ) -> CardId {
        // A saved-root rescan does not authorize adopting a replacement at the
        // same path. Keep the historical card, but make it unscannable until the
        // caller explicitly selects a new source without a refresh guard.
        let connected = refresh_root
            .as_ref()
            .is_none_or(|root| same_root(&identity, &root.identity));
        let identity = if connected {
            identity
        } else {
            refresh_root.as_ref().unwrap().identity.clone()
        };
        if let Some(card) = self
            .cards
            .iter_mut()
            .find(|card| same_root(&card.identity, &identity))
        {
            if card.path != path && !card.aliases.contains(&path) {
                card.aliases.push(path.clone());
            }
            if card.kind == CardKind::Folder {
                card.path = path;
            }
            card.connected = connected;
            if refresh_root.is_some() {
                card.root = refresh_root;
            }
            return card.id;
        }
        let id = self.allocate();
        self.cards.push(DiskCard {
            id,
            identity,
            kind: CardKind::Folder,
            label: folder_label(&path),
            path,
            aliases: Vec::new(),
            connected,
            volume: None,
            root: refresh_root,
        });
        id
    }

    pub fn target(&self, id: CardId) -> Option<ScanTarget> {
        let card = self.card(id)?;
        (card.connected && same_root(&card.identity, &card.identity)).then(|| ScanTarget {
            card_id: id,
            path: card.path.clone(),
            expected_root: card.identity.clone(),
            refresh_root: card.root.clone(),
        })
    }

    /// Only currently supported mounted targets participate in Scan all disks.
    /// Saved folders never become additional recursive scans of the same disk.
    pub fn mounted_targets(&self) -> Vec<ScanTarget> {
        self.cards
            .iter()
            .filter(|card| card.volume.is_some())
            .filter_map(|card| self.target(card.id))
            .collect()
    }
}

fn folder_label(path: &NativePath) -> String {
    path.to_path_buf()
        .ok()
        .map(|path| {
            path.file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy()
                .into_owned()
        })
        .unwrap_or_else(|| "Saved folder".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use space_core::{
        IdentityScheme, IdentityStrength, LineageId, ObservedIdentity, RootId, Timestamp,
    };
    use std::path::Path;

    pub fn identity(volume: u8, directory: u8) -> RootIdentity {
        RootIdentity {
            volume: ObservedIdentity {
                scheme: IdentityScheme::UnixDevice,
                value: vec![volume],
                strength: IdentityStrength::Observed,
            },
            directory: ObservedIdentity {
                scheme: IdentityScheme::UnixInode,
                value: vec![directory],
                strength: IdentityStrength::Observed,
            },
        }
    }
    fn volume(path: &str, volume: u8, directory: u8) -> MountedVolume {
        MountedVolume {
            label: "Identical labels".into(),
            mount_path: NativePath::from_path(Path::new(path)),
            aliases: vec![],
            identity: identity(volume, directory),
            filesystem: "fixture".into(),
            removable: None,
            capacity: None,
        }
    }
    fn listing(volumes: Vec<MountedVolume>) -> VolumeListing {
        VolumeListing {
            observed_at: Timestamp::new(1, 0).unwrap(),
            volumes,
            issues: vec![],
            complete: true,
        }
    }
    fn root(id: u64, path: &str, volume: u8, directory: u8) -> RootRecord {
        RootRecord {
            id: RootId::new(id).unwrap(),
            lineage: LineageId::new(id).unwrap(),
            path: NativePath::from_path(Path::new(path)),
            identity: identity(volume, directory),
        }
    }

    #[test]
    fn aliases_merge_only_full_strong_identity_and_keep_native_paths() {
        let mut registry = DiskRegistry::default();
        registry.reconcile(
            &listing(vec![
                volume("/first", 1, 1),
                volume("/alias", 1, 1),
                volume("/nested", 1, 2),
                volume("/other", 2, 1),
            ]),
            &[],
        );
        assert_eq!(registry.cards().len(), 3);
        assert_eq!(registry.mounted_targets().len(), 3);
        assert_eq!(registry.cards()[0].aliases.len(), 1);
        assert_eq!(
            registry.cards()[0].aliases[0],
            NativePath::from_path(Path::new("/alias"))
        );
        let first = registry.cards()[0].id;
        registry.reconcile(&listing(vec![volume("/renamed-mount", 1, 1)]), &[]);
        assert_eq!(registry.mounted_targets()[0].card_id, first);
    }

    #[test]
    fn replacement_at_same_path_does_not_inherit_saved_snapshot_or_card() {
        let saved = root(1, "/disk", 1, 1);
        let mut registry = DiskRegistry::default();
        registry.reconcile(
            &listing(vec![volume("/disk", 1, 1)]),
            std::slice::from_ref(&saved),
        );
        let old = registry.cards()[0].id;
        registry.reconcile(&listing(vec![volume("/disk", 2, 2)]), &[saved]);
        let targets = registry.mounted_targets();
        assert_eq!(targets.len(), 1);
        assert_ne!(targets[0].card_id, old);
        assert!(targets[0].refresh_root.is_none());
        let offline = registry.card(old).unwrap();
        assert!(!offline.connected);
        assert!(offline.root.is_some());
    }

    #[test]
    fn folders_remain_separate_and_offline_cached_roots_keep_stable_ids() {
        let saved = root(7, "/disk/folder", 1, 2);
        let mut registry = DiskRegistry::default();
        registry.reconcile(
            &listing(vec![volume("/disk", 1, 1)]),
            std::slice::from_ref(&saved),
        );
        let folder = registry
            .cards()
            .iter()
            .find(|c| c.root.is_some())
            .unwrap()
            .id;
        assert_eq!(registry.card(folder).unwrap().kind, CardKind::Folder);
        assert_eq!(registry.mounted_targets().len(), 1);
        assert_eq!(
            registry.register_folder(
                saved.path.clone(),
                saved.identity.clone(),
                Some(saved.clone())
            ),
            folder
        );
        assert!(registry.target(folder).is_some());
        registry.reconcile(&listing(vec![]), std::slice::from_ref(&saved));
        assert!(registry.card(folder).unwrap().connected);
        assert!(registry.card(folder).unwrap().root.is_some());
        let mut cold = DiskRegistry::default();
        cold.reconcile(&listing(vec![]), &[saved]);
        assert!(!cold.cards()[0].connected);
        assert!(registry.mounted_targets().is_empty());
    }

    #[test]
    fn weak_identity_never_attaches_a_cached_root_to_a_mount() {
        let mut saved = root(1, "/disk", 1, 1);
        saved.identity.directory.strength = IdentityStrength::Weak;
        let mut registry = DiskRegistry::default();
        registry.reconcile(
            &listing(vec![volume("/disk", 1, 1)]),
            std::slice::from_ref(&saved),
        );
        assert_eq!(registry.cards().len(), 2);
        assert!(registry.mounted_targets()[0].refresh_root.is_none());
        let weak_id = registry
            .cards()
            .iter()
            .find(|c| c.root.is_some())
            .unwrap()
            .id;
        registry.reconcile(&listing(vec![]), &[saved]);
        assert!(registry.card(weak_id).unwrap().root.is_some());
    }

    #[test]
    fn removal_invalidates_saved_root_without_recycling_card_identity() {
        let saved = root(1, "/disk", 1, 1);
        let mut registry = DiskRegistry::default();
        registry.reconcile(&listing(vec![volume("/disk", 1, 1)]), &[saved]);
        let id = registry.cards()[0].id;
        registry.reconcile(&listing(vec![volume("/disk", 1, 1)]), &[]);
        assert_eq!(registry.cards()[0].id, id);
        assert!(registry.card(id).unwrap().root.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn lossy_path_collisions_are_not_identity_keys() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};
        let a = std::path::PathBuf::from(OsString::from_vec(vec![b'/', 0xff]));
        let b = std::path::PathBuf::from(OsString::from_vec(vec![b'/', 0xfe]));
        assert_eq!(a.to_string_lossy(), b.to_string_lossy());
        let mut registry = DiskRegistry::default();
        let a_id = registry.register_folder(NativePath::from_path(&a), identity(1, 1), None);
        let b_id = registry.register_folder(NativePath::from_path(&b), identity(1, 2), None);
        assert_ne!(a_id, b_id);
        assert_eq!(
            registry.target(a_id).unwrap().path.to_path_buf().unwrap(),
            a
        );
        assert_eq!(
            registry.target(b_id).unwrap().path.to_path_buf().unwrap(),
            b
        );
    }
    #[test]
    fn rescan_inspection_cannot_bless_a_replacement_without_explicit_selection() {
        let saved = root(1, "/disk", 1, 1);
        let mut registry = DiskRegistry::default();
        registry.reconcile(&listing(vec![]), std::slice::from_ref(&saved));
        let old = registry.cards()[0].id;
        let id = registry.register_folder(saved.path.clone(), identity(2, 2), Some(saved));
        assert_eq!(id, old);
        assert!(registry.target(id).is_none());
        let fresh = registry.register_folder(
            NativePath::from_path(Path::new("/disk")),
            identity(2, 2),
            None,
        );
        assert_ne!(id, fresh);
        assert!(registry.target(fresh).is_some());
    }
    #[test]
    fn positively_inspected_folder_stays_connected_until_a_native_path_conflict() {
        let saved = root(1, "/disk", 1, 1);
        let mut registry = DiskRegistry::default();
        let id = registry.register_folder(
            saved.path.clone(),
            saved.identity.clone(),
            Some(saved.clone()),
        );
        registry.reconcile(&listing(vec![]), std::slice::from_ref(&saved));
        assert!(registry.card(id).unwrap().connected);
        assert!(registry.mounted_targets().is_empty());
        registry.reconcile(&listing(vec![volume("/disk", 2, 2)]), &[saved]);
        assert!(!registry.card(id).unwrap().connected);
        assert!(registry.target(id).is_none());
        assert_eq!(registry.mounted_targets().len(), 1);
    }
}
