//! Disk discovery and serial scan orchestration. All source and store I/O runs on workers.
use super::*;
use disk_model::{CardId, DiskRegistry, ScanTarget};
use scan_batch::{
    BatchId, CardStatus, Effect, Event as BatchEvent, ScanBatch, ScanOutcome, ScanToken,
};
use space_core::{ScanId, Timestamp};
use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    rc::Rc,
};
#[derive(Default)]
pub(super) struct CardData {
    pub(super) scan: Option<ScanId>,
    pub(super) snapshot: Option<Arc<Snapshot>>,
    preview: Option<Arc<tree::Tree>>,
    pub(super) epoch: u64,
    metric: Option<Metric>,
    dated: String,
    total: Option<DisplayTotals>,
    partial: bool,
    error: Option<String>,
    pub(super) opening: bool,
    geometry: Rc<RefCell<Option<treemap_view::GeometryCache>>>,
}
#[derive(Clone)]
pub(super) struct Bookmark {
    pub scan: ScanId,
    pub crumbs: Vec<(NodeId, String, DisplayTotals)>,
    pub selected: Option<NodeId>,
    pub list_mode: explore::ListMode,
}
#[derive(Clone)]
struct PreviewJob {
    card: CardId,
    epoch: u64,
    revision: u64,
    scan: Option<ScanId>,
    root: RootRecord,
    metric: Metric,
}
#[cfg(test)]
pub type DiscoveryOverride =
    Arc<dyn Fn() -> backend::Result<(space_scan::VolumeListing, Vec<RootRecord>)> + Send + Sync>;
#[derive(Default)]
pub(super) struct State {
    pub registry: DiskRegistry,
    pub batch: ScanBatch,
    pub(super) cards: BTreeMap<CardId, CardData>,
    pub selected: Option<CardId>,
    pub discovering: bool,
    discovery_generation: u64,
    pub folder_preflight: bool,
    folder_cancelled: bool,
    pub preview_busy: bool,
    metadata_busy: bool,
    preview_queue: VecDeque<PreviewJob>,
    preview_active: Option<PreviewJob>,
    revision: u64,
    pub(super) open_request: u64,
    pub initializing: bool,
    pub notice: Option<String>,
    pub issues_open: bool,
    pub(super) bookmarks: BTreeMap<CardId, Bookmark>,
    #[cfg(test)]
    pub discovery_override: Option<DiscoveryOverride>,
}
fn date(t: Timestamp) -> String {
    chrono::DateTime::from_timestamp(t.seconds, t.nanos)
        .map(|time| time.format("%b %-d, %H:%M UTC").to_string())
        .unwrap_or_else(|| "Date unavailable".into())
}
fn partial(snapshot: &Snapshot) -> bool {
    let c = &snapshot.root_entry.coverage;
    c.error_count > 0 || c.unknown_entries > 0
}
impl Space {
    pub(super) fn reconcile_disks(&mut self, cx: &mut Context<Self>) {
        let listing = self.volumes.clone().unwrap_or(space_scan::VolumeListing {
            observed_at: Timestamp {
                seconds: 0,
                nanos: 0,
            },
            volumes: vec![],
            issues: vec![],
            complete: false,
        });
        self.disks.registry.reconcile(&listing, &self.roots);
        self.load_saved_card_metadata(cx);
        self.queue_visible_previews(cx);
    }
    // Saved publications outside the mini-map cap still need scan tickets. Load
    // only their metadata here and release every temporary snapshot on a worker.
    fn load_saved_card_metadata(&mut self, cx: &mut Context<Self>) {
        if self.disks.metadata_busy {
            return;
        }
        let Some(backend) = self.backend.clone() else {
            return;
        };
        let jobs = self
            .disks
            .registry
            .cards()
            .iter()
            .filter(|c| self.disks.cards.get(&c.id).is_none_or(|d| d.scan.is_none()))
            .filter_map(|c| c.root.clone().map(|r| (c.id, r)))
            .collect::<Vec<_>>();
        if jobs.is_empty() {
            return;
        }
        self.disks.metadata_busy = true;
        let revision = self.disks.revision;
        let task = cx.background_executor().spawn(async move {
            jobs.into_iter()
                .map(|(id, root)| {
                    let result = backend::open_root(&backend, root).map(|snapshot| {
                        (
                            snapshot.scan,
                            date(snapshot.interval.1.unwrap_or(snapshot.interval.0)),
                            DisplayTotals::from_entry(&snapshot.root_entry),
                            partial(&snapshot),
                        )
                    });
                    (id, result)
                })
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let results = task.await;
            let _ = this.update(cx, |this, cx| {
                this.disks.metadata_busy = false;
                if this.disks.revision != revision {
                    return;
                }
                for (id, result) in results {
                    let data = this.disks.cards.entry(id).or_default();
                    if data.scan.is_some() {
                        continue;
                    }
                    match result {
                        Ok((scan, dated, total, partial)) => {
                            data.scan = Some(scan);
                            data.dated = dated;
                            data.total = Some(total);
                            data.partial = partial;
                        }
                        Err(error) => data.error = Some(error),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    pub(super) fn discover_disks(&mut self, batch: Option<BatchId>, cx: &mut Context<Self>) {
        if self.disks.discovering {
            return;
        }
        let Some(backend) = self.backend.clone() else {
            return;
        };
        self.disks.discovering = true;
        self.disks.discovery_generation += 1;
        let generation = self.disks.discovery_generation;
        #[cfg(test)]
        let discovery_override = self.disks.discovery_override.clone();
        let task = cx.background_executor().spawn(async move {
            #[cfg(test)]
            if let Some(discover) = discovery_override {
                return discover();
            }
            backend::discovery(&backend)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.disks.discovery_generation != generation {
                    return;
                }
                this.disks.discovering = false;
                match result {
                    Ok((listing, roots)) => {
                        this.volumes = Some(listing);
                        this.roots = roots;
                        this.reconcile_disks(cx);
                        if let Some(batch) = batch {
                            let targets = this.disks.registry.mounted_targets();
                            this.batch_event(BatchEvent::Discovered { batch, targets }, cx);
                        }
                    }
                    Err(error) => {
                        this.disks.notice = Some(format!(
                            "Disk discovery unavailable: {error}. Cached folders remain available."
                        ));
                        this.reconcile_disks(cx);
                        if let Some(batch) = batch {
                            this.batch_event(BatchEvent::DiscoveryFailed { batch, error }, cx);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    pub(super) fn batch_event(&mut self, event: BatchEvent, cx: &mut Context<Self>) {
        let effects = self.disks.batch.reduce(event);
        for effect in effects {
            match effect {
                Effect::Discover { batch } => self.discover_disks(Some(batch), cx),
                Effect::Start { token, target } => self.start_disk_scan(token, *target, cx),
                Effect::Cancel { token } => {
                    if self.disks.batch.accepts(token)
                        && let Some(run) = &mut self.running
                    {
                        run.cancel_requested = true;
                        let cancel = run.cancel.clone();
                        cx.background_executor()
                            .spawn(async move {
                                cancel.cancel();
                            })
                            .detach();
                    }
                }
            }
        }
        cx.notify();
    }
    fn start_disk_scan(&mut self, token: ScanToken, target: ScanTarget, cx: &mut Context<Self>) {
        let (Some(backend), Some(cache_directory)) =
            (self.backend.clone(), self.cache_directory.clone())
        else {
            return;
        };
        let path = match target.path.to_path_buf() {
            Ok(path) => path,
            Err(error) => {
                self.batch_event(
                    BatchEvent::Finished {
                        token,
                        outcome: ScanOutcome::Failed(error.to_string()),
                    },
                    cx,
                );
                return;
            }
        };
        self.generation += 1;
        let generation = self.generation;
        let cancel = Cancellation::new();
        let progress = Arc::new(backend::Progress::default());
        self.running = Some(Running {
            cancel: cancel.clone(),
            progress: progress.clone(),
            generation,
            executor: cx.background_executor().clone(),
            cancel_requested: false,
            armed: true,
        });
        let request = backend::ScanRequest {
            path,
            cache_directory,
            refresh_root: target.refresh_root,
            expected_root: Some(target.expected_root),
            generation,
        };
        let task = cx
            .background_executor()
            .spawn(async move { backend::scan(&backend, request, &cancel, &progress) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if !this.disks.batch.accepts(token)
                    || this
                        .running
                        .as_ref()
                        .is_none_or(|run| run.generation != generation)
                {
                    return;
                }
                if let Some(run) = &mut this.running {
                    run.armed = false;
                }
                this.running = None;
                let outcome = match result {
                    Ok(Some(done)) => {
                        let is_partial = partial(&done.snapshot);
                        let scan = done.snapshot.scan;
                        this.roots = done.roots;
                        this.publish_card(token.card_id, done.snapshot, cx);
                        if let Some(warning) = done.maintenance_warning {
                            this.disks.notice = Some(warning);
                        }
                        ScanOutcome::Published {
                            scan,
                            partial: is_partial,
                        }
                    }
                    Ok(None) => ScanOutcome::Cancelled,
                    Err(error) => ScanOutcome::Failed(error),
                };
                this.batch_event(BatchEvent::Finished { token, outcome }, cx);
                cx.notify();
            });
        })
        .detach();
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            loop {
                executor.timer(Duration::from_millis(250)).await;
                if !this
                    .update(cx, |this, cx| {
                        let active = this
                            .running
                            .as_ref()
                            .is_some_and(|run| run.generation == generation);
                        if active {
                            cx.notify();
                        }
                        active
                    })
                    .unwrap_or(false)
                {
                    break;
                }
            }
        })
        .detach();
    }
    fn publish_card(&mut self, card: CardId, snapshot: Snapshot, cx: &mut Context<Self>) {
        let keep_snapshot = self.disks.selected == Some(card)
            || self
                .disks
                .registry
                .cards()
                .iter()
                .filter(|c| c.connected)
                .chain(self.disks.registry.cards().iter().filter(|c| !c.connected))
                .take(home::MAX_PREVIEWS)
                .any(|c| c.id == card);
        let data = self.disks.cards.entry(card).or_default();
        data.epoch += 1;
        data.scan = Some(snapshot.scan);
        data.dated = date(snapshot.interval.1.unwrap_or(snapshot.interval.0));
        data.total = Some(DisplayTotals::from_entry(&snapshot.root_entry));
        data.partial = partial(&snapshot);
        data.preview = None;
        data.metric = None;
        data.error = None;
        data.snapshot = keep_snapshot.then(|| Arc::new(snapshot));
        if self.disks.selected == Some(card) && self.screen == Screen::Explore {
            self.explore.new_scan_available = data.scan;
        }
        self.reconcile_disks(cx); // Deliberately does not navigate or replace the foreground explorer.
    }
    pub(super) fn queue_visible_previews(&mut self, cx: &mut Context<Self>) {
        if self.backend.is_none() {
            return;
        }
        let visible: Vec<_> = self
            .disks
            .registry
            .cards()
            .iter()
            .filter(|card| card.connected)
            .chain(
                self.disks
                    .registry
                    .cards()
                    .iter()
                    .filter(|card| !card.connected),
            )
            .take(home::MAX_PREVIEWS)
            .map(|card| card.id)
            .collect();
        for (id, data) in &mut self.disks.cards {
            if !visible.contains(id) {
                data.preview = None;
                data.geometry.borrow_mut().take();
                if self.screen != Screen::Explore || self.disks.selected != Some(*id) {
                    data.snapshot = None;
                }
                data.metric = None;
                data.epoch = data.epoch.wrapping_add(1);
            }
        }
        self.disks
            .preview_queue
            .retain(|job| visible.contains(&job.card));
        for id in visible {
            let Some(root) = self
                .disks
                .registry
                .card(id)
                .and_then(|card| card.root.clone())
            else {
                continue;
            };
            let data = self.disks.cards.entry(id).or_default();
            if data.metric == Some(self.metric) && data.preview.is_some() {
                continue;
            }
            if self.disks.preview_active.as_ref().is_some_and(|job| {
                job.card == id
                    && job.epoch == data.epoch
                    && job.metric == self.metric
                    && job.revision == self.disks.revision
            }) {
                continue;
            }
            if self
                .disks
                .preview_queue
                .iter()
                .any(|job| job.card == id && job.epoch == data.epoch && job.metric == self.metric)
            {
                continue;
            }
            data.preview = None;

            data.error = None;
            data.epoch = data.epoch.wrapping_add(1);
            self.disks.preview_queue.retain(|job| job.card != id);
            self.disks.preview_queue.push_back(PreviewJob {
                card: id,
                epoch: data.epoch,
                revision: self.disks.revision,
                scan: data.scan,
                root,
                metric: self.metric,
            });
        }
        self.start_preview(cx);
    }
    fn start_preview(&mut self, cx: &mut Context<Self>) {
        if self.disks.preview_busy {
            return;
        }
        let (Some(job), Some(backend)) =
            (self.disks.preview_queue.pop_front(), self.backend.clone())
        else {
            return;
        };
        self.disks.preview_busy = true;
        self.disks.preview_active = Some(job.clone());
        let key = job.clone();
        let task = cx.background_executor().spawn(async move {
            let snapshot = match job.scan {
                Some(scan) => backend::open_scan(&backend, job.root, scan),
                None => backend::open_root(&backend, job.root),
            }?;
            let abort = backend::TreeAbort(Arc::new(std::sync::atomic::AtomicU64::new(0)), 0);
            let tree = backend::tree(
                &backend,
                snapshot.scan,
                snapshot.root_entry.entry.node,
                job.metric,
                tree::TreeBudget::PREVIEW,
                &abort,
            )?
            .ok_or("Preview superseded")?;
            Ok::<_, String>((snapshot, Arc::new(tree)))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.disks.preview_busy = false;
                this.disks.preview_active = None;
                if this.disks.revision == key.revision
                    && let Some(data) = this.disks.cards.get_mut(&key.card)
                    && data.epoch == key.epoch
                {
                    match result {
                        Ok((snapshot, preview)) => {
                            data.scan = Some(snapshot.scan);
                            data.dated = date(snapshot.interval.1.unwrap_or(snapshot.interval.0));
                            data.total = Some(DisplayTotals::from_entry(&snapshot.root_entry));
                            data.partial = partial(&snapshot);
                            data.snapshot = Some(Arc::new(snapshot));
                            data.preview = Some(preview);
                            data.metric = Some(key.metric);
                            data.error = None;
                        }
                        Err(error) => {
                            data.error = Some(error);

                            data.metric = Some(key.metric);
                        }
                    }
                }
                this.start_preview(cx);
                cx.notify();
            });
        })
        .detach();
    }
}
impl Space {
    pub(super) fn acquisition_busy(&self) -> bool {
        self.backend.is_none()
            || self.cache_directory.is_none()
            || self.disks.batch.is_busy()
            || self.disks.discovering
            || self.disks.folder_preflight
            || self.picker_open
    }
    pub(super) fn stop_acquisition(&mut self, cx: &mut Context<Self>) {
        if self.disks.folder_preflight || self.picker_open {
            self.disks.folder_cancelled = true;
            self.disks.notice =
                Some("Stopping; waiting for folder selection or identity check to finish.".into());
        }
        self.batch_event(BatchEvent::Stop, cx);
    }
    pub(super) fn scan_folder_picker(&mut self, cx: &mut Context<Self>) {
        if self.acquisition_busy() {
            self.disks.notice = Some(
                "Wait for the current scan or stop its queue before choosing another source."
                    .into(),
            );
            cx.notify();
            return;
        }
        self.picker_open = true;
        self.disks.folder_cancelled = false;
        let result = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose a folder to scan".into()),
        });
        cx.spawn(async move |this, cx| {
            let result = result.await;
            let _ = this.update(cx, |this, cx| {
                this.picker_open = false;
                if this.disks.folder_cancelled {
                    this.disks.notice = Some("Folder scan cancelled.".into());
                    cx.notify();
                    return;
                }
                match result {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.into_iter().next() {
                            this.prepare_folder_scan(path, None, cx);
                        }
                    }
                    Ok(Ok(None)) => this.disks.notice = Some("Folder selection cancelled.".into()),
                    Ok(Err(error)) => this.disks.notice = Some(error.to_string()),
                    Err(error) => this.disks.notice = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn prepare_folder_scan(
        &mut self,
        path: PathBuf,
        root: Option<RootRecord>,
        cx: &mut Context<Self>,
    ) {
        if self.acquisition_busy() {
            return;
        }
        self.disks.folder_preflight = true;
        self.disks.folder_cancelled = false;
        let task = cx.background_executor().spawn(async move {
            space_scan::inspect_root(&path)
                .map(|identity| (path, identity, root))
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move|this,cx|{let result=task.await;let _=this.update(cx,|this,cx|{
            this.disks.folder_preflight=false;if this.disks.folder_cancelled{this.disks.notice=Some("Folder scan cancelled.".into());cx.notify();return;}
            match result{Ok((path,identity,root))=>{if root.as_ref().is_some_and(|root|!disk_model::same_root(&root.identity,&identity)){this.disks.notice=Some("The saved source identity changed. Choose the folder explicitly to start a new lineage.".into());cx.notify();return;}let id=this.disks.registry.register_folder(space_core::NativePath::from_path(&path),identity,root);if let Some(target)=this.disks.registry.target(id){this.batch_event(BatchEvent::ScanTargets(vec![target]),cx);}},Err(error)=>this.disks.notice=Some(format!("Folder cannot be scanned: {error}"))}cx.notify();
        });}).detach();
        cx.notify();
    }
    pub(super) fn rescan_selected_disk(&mut self, cx: &mut Context<Self>) {
        if self.acquisition_busy() {
            self.disks.notice =
                Some("A source acquisition is active; stop it or wait before rescanning.".into());
            cx.notify();
            return;
        }
        if let Some(id) = self.disks.selected
            && let Some(target) = self.disks.registry.target(id)
        {
            self.batch_event(BatchEvent::ScanTargets(vec![target]), cx);
            return;
        }
        if let Some(snapshot) = &self.explore.snapshot {
            match snapshot.path.to_path_buf() {
                Ok(path) => self.prepare_folder_scan(path, Some(snapshot.root.clone()), cx),
                Err(error) => self.disks.notice = Some(error.to_string()),
            }
        } else {
            self.batch_event(BatchEvent::ScanAll, cx);
        }
    }
}
impl Space {
    pub(super) fn remember_explorer(&mut self) {
        if let (Some(card), Some(snapshot)) = (self.disks.selected, self.explore.snapshot.as_ref())
        {
            self.disks.bookmarks.insert(
                card,
                Bookmark {
                    scan: snapshot.scan,
                    crumbs: self.explore.crumbs.clone(),
                    selected: self.explore.selected,
                    list_mode: self.explore.list_mode,
                },
            );
            let retained = self
                .disks
                .registry
                .cards()
                .iter()
                .filter(|c| c.connected)
                .chain(self.disks.registry.cards().iter().filter(|c| !c.connected))
                .take(home::MAX_PREVIEWS)
                .any(|c| c.id == card);
            if !retained && let Some(data) = self.disks.cards.get_mut(&card) {
                data.snapshot = None;
            }
        }
    }
    pub(super) fn open_card(&mut self, ticket: ticket::Ticket, cx: &mut Context<Self>) {
        let Some(data) = self.disks.cards.get_mut(&ticket.card) else {
            return;
        };
        if data.epoch != ticket.query_epoch || data.scan != ticket.scan || data.opening {
            return;
        }
        let Some(scan) = ticket.scan else {
            return;
        };
        self.disks.open_request += 1;
        if let Some(snapshot) = data.snapshot.clone() {
            self.commit_card(ticket.card, snapshot, cx);
            return;
        }
        let (Some(root), Some(backend)) = (
            self.disks
                .registry
                .card(ticket.card)
                .and_then(|c| c.root.clone()),
            self.backend.clone(),
        ) else {
            return;
        };
        data.opening = true;
        let revision = self.disks.revision;
        let request = self.disks.open_request;
        let task = cx
            .background_executor()
            .spawn(async move { backend::open_scan(&backend, root, scan) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                let Some(data) = this.disks.cards.get_mut(&ticket.card) else {
                    return;
                };
                data.opening = false;
                if this.disks.revision != revision
                    || data.scan != Some(scan)
                    || this.disks.open_request != request
                {
                    cx.notify();
                    return;
                }
                match result {
                    Ok(snapshot) => {
                        let snapshot = Arc::new(snapshot);
                        data.snapshot = Some(snapshot.clone());
                        this.commit_card(ticket.card, snapshot, cx);
                    }
                    Err(error) => data.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn commit_card(&mut self, card: CardId, snapshot: Arc<Snapshot>, cx: &mut Context<Self>) {
        self.remember_explorer();
        self.clear_search(cx);
        let bookmark = self
            .disks
            .bookmarks
            .get(&card)
            .filter(|b| b.scan == snapshot.scan)
            .cloned()
            .unwrap_or(Bookmark {
                scan: snapshot.scan,
                crumbs: vec![(
                    snapshot.root_entry.entry.node,
                    snapshot.root_entry.entry.display_name.clone(),
                    DisplayTotals::from_entry(&snapshot.root_entry),
                )],
                selected: None,
                list_mode: explore::ListMode::Children,
            });
        self.disks.selected = Some(card);
        self.explore.snapshot = Some(snapshot);
        self.explore.new_scan_available = None;
        self.explore.disk_menu_open = false;
        self.enter_scope_from_bookmark(bookmark, cx);
    }
    pub(super) fn home_event(
        &mut self,
        event: home::Event,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.home_model().allows(event) {
            return;
        }
        match event {
            home::Event::ScanAll => self.batch_event(BatchEvent::ScanAll, cx),
            home::Event::Stop => self.stop_acquisition(cx),
            home::Event::ScanFolder => self.scan_folder_picker(cx),
            home::Event::RefreshDiscovery => self.discover_disks(None, cx),
            home::Event::ToggleIssues => self.disks.issues_open = !self.disks.issues_open,
            home::Event::Explore(ticket) => self.open_card(ticket, cx),
            home::Event::Rescan(ticket) | home::Event::Retry(ticket) => {
                if let Some(target) = self.disks.registry.target(ticket.card) {
                    self.batch_event(BatchEvent::ScanTargets(vec![target]), cx);
                }
            }
        }
        cx.notify();
    }
    pub(super) fn home_model(&self) -> home::Model {
        let cards = self
            .disks
            .registry
            .cards()
            .iter()
            .filter(|c| c.connected)
            .chain(self.disks.registry.cards().iter().filter(|c| !c.connected))
            .map(|card| {
                let data = self.disks.cards.get(&card.id);
                let scan = data.and_then(|d| d.scan);
                let base = if !card.connected && scan.is_some() {
                    home::Status::Offline
                } else if data.is_some_and(|d| d.partial) {
                    home::Status::Partial
                } else if scan.is_some() {
                    home::Status::Ready
                } else {
                    home::Status::Idle
                };
                let (status, error) = match self.disks.batch.status(card.id) {
                    Some(CardStatus::Queued { .. }) => (home::Status::Queued, None),
                    Some(CardStatus::Scanning { .. }) => (
                        home::Status::Scanning {
                            entries: self
                                .running
                                .as_ref()
                                .map_or(0, |r| r.progress.entries.load(Ordering::Relaxed)),
                        },
                        None,
                    ),
                    Some(CardStatus::Stopping) => (home::Status::Stopping, None),
                    Some(CardStatus::Failed(e)) => (home::Status::Failed, Some(e.clone())),
                    Some(CardStatus::Cancelled) => (
                        home::Status::Cancelled,
                        Some("Stopped; any previous map is retained.".into()),
                    ),
                    _ => (base, None),
                };
                home::Card {
                    ticket: ticket::Ticket {
                        card: card.id,
                        scan,
                        query_epoch: data.map_or(0, |d| d.epoch),
                    },
                    name: card.label.clone(),
                    location: card
                        .aliases
                        .first()
                        .unwrap_or(&card.path)
                        .to_path_buf()
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_else(|_| "Native path unavailable".into()),
                    kind: if card.kind == disk_model::CardKind::Disk {
                        "Disk"
                    } else {
                        "Saved folder"
                    }
                    .into(),
                    connected: card.connected,
                    capacity: card
                        .volume
                        .as_ref()
                        .and_then(|v| v.capacity.as_ref())
                        .map(|c| home::Capacity {
                            total: c.total_bytes,
                            available: c.available_bytes,
                        }),
                    status,
                    last_scan: data
                        .filter(|d| !d.dated.is_empty())
                        .map(|d| d.dated.clone()),
                    cached_total: data.and_then(|d| d.total).map_or_else(
                        || "No scan yet".into(),
                        |t| format!("{} observed", tree::format_bytes(t.bytes(self.metric))),
                    ),
                    preview: data.and_then(|d| d.preview.clone()),
                    geometry: data.map(|d| d.geometry.clone()).unwrap_or_default(),
                    error: error.or_else(|| data.and_then(|d| d.error.clone())),
                    opening: data.is_some_and(|d| d.opening),
                    can_rescan: card.connected,
                }
            })
            .collect();
        home::Model {
            cards,
            included_targets: self
                .disks
                .registry
                .cards()
                .iter()
                .filter(|c| c.connected && c.kind == disk_model::CardKind::Disk)
                .count(),
            discovering: self.disks.discovering || self.disks.initializing,
            unavailable: self.backend.is_none(),
            busy: self.disks.batch.is_busy() || self.disks.folder_preflight || self.picker_open,
            stopping: self.disks.batch.stopping()
                || self.disks.folder_cancelled && (self.disks.folder_preflight || self.picker_open),
            batch_summary: self.disks.batch.active().map(|a| {
                format!(
                    "Scanning {} of {} · {} · {} waiting",
                    a.ordinal,
                    a.total,
                    self.disks
                        .registry
                        .card(a.token.card_id)
                        .map_or("Disk", |c| c.label.as_str()),
                    self.disks.batch.queued_total()
                )
            }),
            metric_label: metric_label(self.metric).into(),
            discovery_issues: self
                .volumes
                .as_ref()
                .map(|v| {
                    v.issues
                        .iter()
                        .take(64)
                        .map(|i| i.detail.clone())
                        .chain(
                            (!v.complete).then_some(
                                "Disk discovery is incomplete. You can scan a folder explicitly."
                                    .into(),
                            ),
                        )
                        .collect()
                })
                .unwrap_or_default(),
            issues_open: self.disks.issues_open,
            notice: self
                .disks
                .notice
                .clone()
                .or_else(|| self.disks.batch.discovery_error().map(str::to_owned)),
        }
    }
}
