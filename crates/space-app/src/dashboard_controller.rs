//! Dashboard orchestration. Only bounded visible previews own pins; workers own I/O.
use super::*;
use disk_model::{CardId, DiskRegistry, ScanTarget};
use scan_batch::{
    BatchId, CardStatus, Effect, Event as BatchEvent, ScanBatch, ScanOutcome, ScanToken,
};
use space_core::{ScanId, Timestamp};
use std::collections::{BTreeMap, VecDeque};

#[derive(Default)]
struct CardData {
    scan: Option<ScanId>,
    snapshot: Option<Arc<Snapshot>>,
    preview: Option<treemap::Model>,
    epoch: u64,
    metric: Option<Metric>,
    dated: String,
    total: String,
    partial: bool,
    error: Option<String>,
}
#[derive(Clone)]
struct Bookmark {
    scan: ScanId,
    crumbs: Vec<(NodeId, String)>,
    selected: Option<NodeId>,
    view: View,
    subtree: bool,
    cursor: Option<PageCursor>,
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
pub(super) struct State {
    pub home: bool,
    pub registry: DiskRegistry,
    pub batch: ScanBatch,
    cards: BTreeMap<CardId, CardData>,
    pub selected: Option<CardId>,
    pub page: usize,
    pub discovering: bool,
    discovery_generation: u64,
    pub folder_preflight: bool,
    folder_cancelled: bool,
    pub preview_busy: bool,
    preview_queue: VecDeque<PreviewJob>,
    preview_active: Option<PreviewJob>,
    revision: u64,
    pub details_visible: bool,
    pub list_visible: bool,
    pub inspector_visible: bool,
    pub advanced_filters: bool,
    bookmarks: BTreeMap<CardId, Bookmark>,
    pending_bookmark: Option<Bookmark>,
    pending_card: Option<(CardId, ScanId)>,
    pending_notice: Option<String>,
    pub initializing: bool,
    pub restore_selection: Option<(ScanId, NodeId)>,
    pub page_cursor: Option<PageCursor>,
    pub displayed_scope: Option<(ScanId, NodeId)>,
    pages: BTreeMap<(ScanId, NodeId), (Option<PageCursor>, Option<NodeId>)>,
    pub notice: Option<String>,
    pub explorer_notice: Option<String>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            home: true,
            registry: Default::default(),
            batch: Default::default(),
            cards: Default::default(),
            selected: None,
            page: 0,
            discovering: false,
            discovery_generation: 0,
            folder_preflight: false,
            folder_cancelled: false,
            preview_busy: false,
            preview_queue: Default::default(),
            preview_active: None,
            revision: 0,
            details_visible: false,
            list_visible: false,
            inspector_visible: true,
            advanced_filters: false,
            bookmarks: Default::default(),
            pending_bookmark: None,
            pending_card: None,
            pending_notice: None,
            initializing: true,
            restore_selection: None,
            page_cursor: None,
            displayed_scope: None,
            pages: Default::default(),
            notice: None,
            explorer_notice: None,
        }
    }
}
impl State {
    fn stage_navigation(&mut self, card: CardId, bookmark: Bookmark) {
        self.pending_notice = None;
        self.pending_card = Some((card, bookmark.scan));
        self.pending_bookmark = Some(bookmark);
    }
    pub fn cancel_navigation(&mut self) {
        self.pending_notice = None;
        self.pending_card = None;
        self.pending_bookmark = None;
    }
    fn commit_navigation(&mut self, scan: ScanId) -> Option<Bookmark> {
        if self
            .pending_card
            .is_none_or(|(_, expected)| expected != scan)
        {
            return None;
        }
        let pending = self.pending_card.take();
        let bookmark = self.pending_bookmark.take();
        if let Some((card, expected)) = pending
            && expected == scan
        {
            self.selected = Some(card);
            self.home = false;
            self.explorer_notice = self.pending_notice.take();
            return bookmark.filter(|value| value.scan == scan);
        }
        None
    }
    pub fn invalidate_page_order(&mut self) {
        self.pages.clear();
        self.page_cursor = None;
        self.displayed_scope = None;
        for bookmark in self.bookmarks.values_mut() {
            bookmark.cursor = None;
        }
    }
    pub fn idle(&self) -> bool {
        !self.batch.is_busy() && !self.discovering && !self.folder_preflight && !self.preview_busy
    }
    pub fn release_cards(&mut self) -> Vec<Arc<Snapshot>> {
        self.revision = self.revision.wrapping_add(1);
        self.preview_queue.clear();
        let pins = self
            .cards
            .values_mut()
            .filter_map(|data| data.snapshot.take())
            .collect();
        self.cards.clear();
        self.bookmarks.clear();
        self.cancel_navigation();
        self.explorer_notice = None;
        self.selected = None;
        self.home = true;
        self.restore_selection = None;
        self.pages.clear();
        self.displayed_scope = None;
        self.page_cursor = None;
        pins
    }
}
fn date(t: Timestamp) -> String {
    chrono::DateTime::from_timestamp(t.seconds, t.nanos)
        .map(|time| time.format("%b %-d, %H:%M UTC").to_string())
        .unwrap_or_else(|| "Date unavailable".into())
}
fn model_from_summary(summary: space_store::ChildrenSummary, metric: Metric) -> treemap::Model {
    treemap::Model {
        items: summary
            .page
            .entries
            .into_iter()
            .map(|row| {
                let total = if metric == Metric::Logical {
                    row.logical
                } else {
                    row.allocated
                };
                treemap::Item {
                    node: row.entry.node,
                    label: row.entry.display_name.clone(),
                    detail: format!("{} · {}", row.entry.display_name, total_text(&row, metric)),
                    bytes: total.observed_bytes,
                    unknown_count: total.unknown_count,
                    is_directory: row.entry.kind == EntryKind::Directory,
                }
            })
            .collect(),
        other_bytes: u128::from(summary.other_bytes),
        other_count: summary.other_count,
        unknown_count: summary.unknown_count,
        zero_count: summary.zero_count,
    }
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
        self.queue_visible_previews(cx);
    }
    pub(super) fn discover_disks(&mut self, batch: Option<BatchId>, cx: &mut Context<Self>) {
        if self.disks.discovering || self.maintenance_busy {
            return;
        }
        let Some(backend) = self.backend.clone() else {
            return;
        };
        self.disks.discovering = true;
        self.disks.discovery_generation += 1;
        let generation = self.disks.discovery_generation;
        let task = cx
            .background_executor()
            .spawn(async move { backend::discovery(&backend) });
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
        let data = self.disks.cards.entry(card).or_default();
        data.epoch += 1;
        data.scan = Some(snapshot.scan);
        data.dated = date(snapshot.interval.1.unwrap_or(snapshot.interval.0));
        data.total = total_text(&snapshot.root_entry, self.metric);
        data.partial = partial(&snapshot);
        data.preview = None;
        data.metric = None;
        data.error = None;
        data.snapshot = Some(Arc::new(snapshot));
        self.reconcile_disks(cx); // Deliberately does not navigate or replace the foreground explorer.
    }
    pub(super) fn queue_visible_previews(&mut self, cx: &mut Context<Self>) {
        if self.maintenance_busy || self.backend.is_none() {
            return;
        }
        self.disks.page = self
            .disks
            .page
            .min(self.disks.registry.cards().len().div_ceil(8).max(1) - 1);
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
            .skip(self.disks.page * 8)
            .take(8)
            .map(|card| card.id)
            .collect();
        for (id, data) in &mut self.disks.cards {
            if !visible.contains(id) {
                data.snapshot = None;
                data.preview = None;
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
            data.total = "Loading cached totals…".into();
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
        if self.disks.preview_busy || self.maintenance_busy {
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
            let summary = backend::preview(&backend, &snapshot, job.metric)?;
            Ok::<_, String>((snapshot, model_from_summary(summary, job.metric)))
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
                            data.total = total_text(&snapshot.root_entry, key.metric);
                            data.partial = partial(&snapshot);
                            data.snapshot = Some(Arc::new(snapshot));
                            data.preview = Some(preview);
                            data.metric = Some(key.metric);
                            data.error = None;
                        }
                        Err(error) => {
                            data.error = Some(error);
                            data.total = "Cached totals unavailable".into();
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
    fn acquisition_busy(&self) -> bool {
        self.backend.is_none()
            || self.cache_directory.is_none()
            || self.maintenance_busy
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
        if let Some(snapshot) = &self.snapshot {
            match snapshot.path.to_path_buf() {
                Ok(path) => self.prepare_folder_scan(path, Some(snapshot.root.clone()), cx),
                Err(error) => self.disks.notice = Some(error.to_string()),
            }
        } else {
            self.batch_event(BatchEvent::ScanAll, cx);
        }
    }
    fn remember_explorer(&mut self) {
        if let (Some(card), Some(snapshot)) = (self.disks.selected, self.snapshot.as_ref()) {
            let selected = self
                .chart_selected
                .or_else(|| self.rows.get(self.selected).map(|row| row.entry.node));
            self.disks.bookmarks.insert(
                card,
                Bookmark {
                    scan: snapshot.scan,
                    crumbs: self.breadcrumbs.clone(),
                    selected,
                    view: self.view,
                    subtree: self.subtree_only,
                    cursor: self.disks.page_cursor,
                },
            );
        }
    }
    pub(super) fn restore_disk_bookmark(&mut self, snapshot: &Snapshot) {
        if let Some(bookmark) = self.disks.commit_navigation(snapshot.scan) {
            self.breadcrumbs = bookmark.crumbs;
            self.view = bookmark.view;
            self.subtree_only = bookmark.subtree;
            self.disks.page_cursor = bookmark.cursor;
            self.disks.restore_selection = bookmark.selected.map(|node| (snapshot.scan, node));
        }
    }
    fn open_card(
        &mut self,
        ticket: dashboard::Ticket,
        map: Option<treemap::Event>,
        cx: &mut Context<Self>,
    ) {
        if self.maintenance_busy {
            return;
        }
        let Some(data) = self.disks.cards.get(&ticket.card) else {
            return;
        };
        if data.epoch != ticket.query_epoch || data.scan != ticket.scan {
            return;
        }
        let Some(snapshot) = data.snapshot.clone() else {
            return;
        };
        let preview = data.preview.clone();
        self.remember_explorer();

        self.changes.invalidate();
        self.cache_visible = false;
        let mut bookmark = self
            .disks
            .bookmarks
            .get(&ticket.card)
            .filter(|bookmark| bookmark.scan == snapshot.scan)
            .cloned()
            .unwrap_or(Bookmark {
                scan: snapshot.scan,
                crumbs: vec![(
                    snapshot.root_entry.entry.node,
                    snapshot.root_entry.entry.display_name.clone(),
                )],
                selected: None,
                view: View::Treemap,
                subtree: true,
                cursor: None,
            });
        if let Some(event) = map {
            bookmark.crumbs = vec![(
                snapshot.root_entry.entry.node,
                snapshot.root_entry.entry.display_name.clone(),
            )];
            bookmark.view = View::Treemap;
            bookmark.cursor = None;
            match event {
                treemap::Event::Select(node) => {
                    if preview
                        .as_ref()
                        .is_none_or(|p| !p.items.iter().any(|item| item.node == node))
                    {
                        return;
                    }
                    bookmark.selected = Some(node);
                }
                treemap::Event::Open(node) => {
                    let Some(label) = preview.as_ref().and_then(|p| chart_folder(p, node)) else {
                        return;
                    };
                    bookmark.crumbs.push((node, label));
                    bookmark.selected = None;
                }
                treemap::Event::OpenOther => {
                    bookmark.view = View::Folders;
                    bookmark.selected = None;
                }
            }
        }
        if map.is_some() {
            self.filter_config = space_store::QueryFilter::default();
        }
        if self.snapshot.is_some() {
            self.disks.home = false;
        }
        self.disks.stage_navigation(ticket.card, bookmark);
        if map.is_some() {
            self.disks.pending_notice = Some(
                "Opened the unfiltered cached map. Filter inputs remain available as drafts."
                    .into(),
            );
        }
        self.snapshot_request += 1;
        self.pending_snapshot = Some(snapshot);
        self.rebuild_filters(cx);
        cx.notify();
    }
    pub(super) fn dashboard_event(
        &mut self,
        event: dashboard::Event,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let model = self.dashboard_model();
        if !model.allows(event) {
            return;
        }
        match event {
            dashboard::Event::ScanAll => {
                if !self.acquisition_busy() {
                    self.batch_event(BatchEvent::ScanAll, cx);
                }
            }
            dashboard::Event::Cache => self.show_cache(&ShowCache, window, cx),
            dashboard::Event::Stop => self.stop_acquisition(cx),
            dashboard::Event::ScanFolder => self.scan_folder_picker(cx),
            dashboard::Event::RefreshDiscovery => self.discover_disks(None, cx),
            dashboard::Event::ToggleDiscoveryDetails => {
                self.disks.details_visible = !self.disks.details_visible
            }
            dashboard::Event::Page(page) => {
                self.disks.page = page;
                self.queue_visible_previews(cx);
            }
            dashboard::Event::Explore(ticket) => self.open_card(ticket, None, cx),
            dashboard::Event::Map(ticket, event) => self.open_card(ticket, Some(event), cx),
            dashboard::Event::Rescan(ticket) => {
                if !self.acquisition_busy()
                    && let Some(target) = self.disks.registry.target(ticket.card)
                {
                    self.batch_event(BatchEvent::ScanTargets(vec![target]), cx);
                }
            }
            dashboard::Event::Details(ticket) => {
                self.disks.notice=self.disks.cards.get(&ticket.card).and_then(|data|data.error.clone()).or_else(||Some("Cached sizes are observed included bytes, not reclaimable space. Open Cache / diagnostics for detailed scan issues.".into()));
                self.show_cache(&ShowCache, window, cx);
            }
        }
        cx.notify();
    }
}
impl Space {
    pub(super) fn dashboard_model(&self) -> dashboard::Model {
        let cards=self.disks.registry.cards().iter().take(dashboard::MAX_CARDS).map(|card|{
            let data=self.disks.cards.get(&card.id);
            let cached=data.and_then(|data|data.scan);
            let base=if !card.connected && cached.is_some(){dashboard::Status::Offline}else if data.is_some_and(|d|d.partial){dashboard::Status::Partial}else if cached.is_some(){dashboard::Status::Ready}else{dashboard::Status::Idle};
            let (status,error)=match self.disks.batch.status(card.id){
                Some(CardStatus::Queued{..})=>(dashboard::Status::Queued,None),
                Some(CardStatus::Scanning{..})=>(dashboard::Status::Scanning{entries:self.running.as_ref().map_or(0,|run|run.progress.entries.load(Ordering::Relaxed))},None),
                Some(CardStatus::Stopping)=>(dashboard::Status::Stopping,None),
                Some(CardStatus::Failed(error))=>(dashboard::Status::Failed,Some(error.clone())),
                Some(CardStatus::Cancelled)=>(dashboard::Status::Cancelled,Some("Stopped; any previous cached map is retained.".into())),
                _=>(base,None),
            };
            let preview=data.and_then(|d|d.preview.clone());
            let top_items=preview.as_ref().map(|p|p.items.iter().take(3).map(|item|(item.label.clone(),dashboard::format_bytes(item.bytes))).collect()).unwrap_or_default();
            dashboard::Card{ticket:dashboard::Ticket{card:card.id,scan:cached,query_epoch:data.map_or(0,|d|d.epoch)},name:card.label.clone(),location:card.aliases.first().unwrap_or(&card.path).to_path_buf().map(|p|p.to_string_lossy().into_owned()).unwrap_or_else(|_|"Native path unavailable".into()),kind:if card.kind==disk_model::CardKind::Disk{"Disk"}else{"Saved folder"}.into(),connected:card.connected,capacity:card.volume.as_ref().and_then(|volume|volume.capacity.as_ref()).map(|capacity|dashboard::Capacity{total:capacity.total_bytes,free:capacity.available_bytes}),status,last_scan:data.filter(|d|!d.dated.is_empty()).map(|d|d.dated.clone()),cached_total:data.map(|d|d.total.clone()).unwrap_or_else(||"Scan to see what is taking space".into()),preview,top_items,issue_summary:error.or_else(||data.and_then(|d|d.error.clone())).or_else(||data.filter(|d|d.partial).map(|_|"Some locations could not be read. Cached totals include observed entries only.".into())),can_rescan:card.connected}
        }).collect();
        let batch_summary = self
            .disks
            .batch
            .active()
            .map(|active| {
                format!(
                    "Scanning target {} of {} · {} waiting",
                    active.ordinal,
                    active.total,
                    self.disks.batch.queued_total()
                )
            })
            .or_else(|| {
                self.disks
                    .folder_preflight
                    .then(|| "Checking selected folder identity…".into())
            })
            .or_else(|| self.disks.batch.discovery_error().map(str::to_owned));
        let mut issues = self
            .volumes
            .as_ref()
            .map(|listing| {
                listing
                    .issues
                    .iter()
                    .take(50)
                    .map(|issue| issue.detail.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if self
            .volumes
            .as_ref()
            .is_some_and(|listing| !listing.complete)
        {
            issues.insert(0,"Disk discovery is best-effort and may omit targets. Scan folder remains available for supported local folders.".into());
        }
        dashboard::Model {
            cards,
            page: self.disks.page,
            included_targets: self
                .disks
                .registry
                .cards()
                .iter()
                .filter(|card| card.connected && card.kind == disk_model::CardKind::Disk)
                .count(),
            discovering: self.disks.discovering || self.disks.initializing,
            unavailable: self.backend.is_none(),
            busy: self.disks.batch.is_busy()
                || self.disks.folder_preflight
                || self.picker_open
                || self.maintenance_busy,
            stopping: self.disks.batch.stopping()
                || self.disks.folder_cancelled && (self.disks.folder_preflight || self.picker_open),
            batch_summary,
            metric_label: if self.metric == Metric::Logical {
                "Logical file size"
            } else {
                "Allocated size"
            }
            .into(),
            discovery_issues: issues,
            discovery_details_visible: self.disks.details_visible,
            notice: self.disks.notice.clone(),
        }
    }
    pub(super) fn explorer_model(&self) -> explorer::Model {
        let dashboard = self.dashboard_model();
        let ticket = self.snapshot.as_ref().and_then(|snapshot| {
            self.disks.selected.map(|card| dashboard::Ticket {
                card,
                scan: Some(snapshot.scan),
                query_epoch: self.query_generation,
            })
        });
        let card = self
            .disks
            .selected
            .and_then(|id| self.disks.registry.card(id));
        explorer::Model {
            ticket,
            cards: dashboard
                .cards
                .iter()
                .map(|card| explorer::SidebarItem {
                    ticket: dashboard::Ticket {
                        scan: if self
                            .disks
                            .cards
                            .get(&card.ticket.card)
                            .is_some_and(|data| data.snapshot.is_some())
                        {
                            card.ticket.scan
                        } else {
                            None
                        },
                        ..card.ticket
                    },
                    name: card.name.clone(),
                    status: card.status.label(),
                    connected: card.connected,
                })
                .collect(),
            sidebar_page: self.disks.page,
            name: card
                .map(|card| card.label.clone())
                .unwrap_or_else(|| "Cached explorer".into()),
            location: self
                .snapshot
                .as_ref()
                .map(|s| {
                    s.path
                        .to_path_buf()
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default()
                })
                .unwrap_or_default(),
            metric_label: if self.applied_metric == Metric::Logical {
                "Logical file size"
            } else {
                "Allocated size"
            }
            .into(),
            scope_label: if self.view == View::Largest && !self.subtree_only {
                "Entire disk".into()
            } else {
                "Current folder".into()
            },
            status: if self.query_busy {
                "Loading cached view…".into()
            } else {
                self.current_totals
                    .as_ref()
                    .map(|row| total_text(row, self.applied_metric))
                    .unwrap_or_default()
            },
            breadcrumbs: self
                .breadcrumbs
                .iter()
                .map(|(node, label)| explorer::Breadcrumb {
                    node: *node,
                    label: label.clone(),
                })
                .collect(),
            view: match self.view {
                View::Treemap => explorer::View::Treemap,
                View::Folders => explorer::View::Folders,
                View::Largest => explorer::View::Largest,
            },
            entire_disk: !self.subtree_only,
            can_up: self.breadcrumbs.len() > 1,
            can_open: self
                .rows
                .get(self.selected)
                .is_some_and(|row| row.entry.kind == EntryKind::Directory),
            can_rescan: card.is_some_and(|card| card.connected),
            busy: self.acquisition_busy(),
            stopping: self.disks.batch.stopping(),
            list_visible: self.disks.list_visible,
            inspector_visible: self.disks.inspector_visible,
            filters_visible: self.filters_visible,
            offline: card.is_some_and(|card| !card.connected),
        }
    }
    pub(super) fn explorer_event(
        &mut self,
        event: explorer::Event,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.explorer_model().allows(event) {
            return;
        }
        use explorer::Action;
        match event.action {
            Action::Home => {
                if self.pending_snapshot.is_some() {
                    self.cancel_filter(&CancelFilter, window, cx);
                }
                self.remember_explorer();
                self.disks.home = true;
                self.changes.invalidate();
                self.queue_visible_previews(cx);
            }
            Action::Select(ticket) => self.open_card(ticket, None, cx),
            Action::SidebarPage(page) => {
                self.disks.page = page;
                self.queue_visible_previews(cx);
            }
            Action::Rescan => self.rescan_selected_disk(cx),
            Action::Stop => self.stop_acquisition(cx),
            Action::View(view) => {
                self.view = match view {
                    explorer::View::Treemap => View::Treemap,
                    explorer::View::Folders => View::Folders,
                    explorer::View::Largest => View::Largest,
                };
                self.request_query(None, cx);
            }
            Action::Scope => self.toggle_subtree(&ToggleSubtree, window, cx),
            Action::Metric => {
                self.metric(&ToggleMetric, window, cx);
            }
            Action::Filters => self.toggle_filters(&ToggleFilters, window, cx),
            Action::Changes => self.show_changes(&ShowChanges, window, cx),
            Action::Cache => self.show_cache(&ShowCache, window, cx),
            Action::Up => self.up(&GoUp, window, cx),
            Action::Breadcrumb(node) => {
                if let Some(index) = self.breadcrumbs.iter().position(|(id, _)| *id == node) {
                    self.breadcrumbs.truncate(index + 1);
                    self.request_query(None, cx);
                }
            }
            Action::OpenFolder => self.open_row(&OpenRow, window, cx),
            Action::ToggleList => self.disks.list_visible = !self.disks.list_visible,
            Action::ToggleInspector => self.disks.inspector_visible = !self.disks.inspector_visible,
            Action::Copy => self.path_action(false, cx),
            Action::Reveal => self.path_action(true, cx),
            Action::Aliases => self.show_aliases(&ShowAliases, window, cx),
        }
        cx.notify();
    }
}

impl Space {
    pub(super) fn remember_scope_page(&mut self) {
        if let Some(scope) = self.disks.displayed_scope {
            let selected = self
                .chart_selected
                .or_else(|| self.rows.get(self.selected).map(|row| row.entry.node));
            self.disks
                .pages
                .insert(scope, (self.disks.page_cursor, selected));
            while self.disks.pages.len() > 256 {
                self.disks.pages.pop_first();
            }
        }
    }
    pub(super) fn saved_scope_page(&self, parent: NodeId) -> Option<PageCursor> {
        self.snapshot
            .as_ref()
            .and_then(|snapshot| self.disks.pages.get(&(snapshot.scan, parent)))
            .and_then(|(cursor, _)| *cursor)
    }
    pub(super) fn restore_query_selection(&mut self, scan: ScanId, parent: NodeId) {
        self.disks.displayed_scope = Some((scan, parent));
        if let Some((selected_scan, node)) = self.disks.restore_selection.take()
            && selected_scan == scan
            && let Some(index) = self.rows.iter().position(|row| row.entry.node == node)
        {
            self.selected = index;
            self.chart_selected = Some(node);
            self.scroll.scroll_to_item(index, ScrollStrategy::Nearest);
        }
    }
}

#[cfg(test)]
mod transition_tests {
    use super::*;
    fn bookmark(scan: u64) -> Bookmark {
        Bookmark {
            scan: ScanId::new(scan).unwrap(),
            crumbs: vec![(NodeId::new(1).unwrap(), "root".into())],
            selected: Some(NodeId::new(2).unwrap()),
            view: View::Folders,
            subtree: true,
            cursor: Some(PageCursor {
                ordinal: 100,
                value: 42,
            }),
        }
    }
    #[test]
    fn navigation_commits_card_and_bookmark_together_or_keeps_previous_card() {
        let mut state = State {
            selected: Some(CardId(1)),
            home: false,
            ..Default::default()
        };
        state.stage_navigation(CardId(2), bookmark(2));
        state.pending_notice = Some("Unfiltered map".into());
        assert!(state.explorer_notice.is_none());
        assert_eq!(state.selected, Some(CardId(1)));
        state.cancel_navigation();
        assert!(state.pending_notice.is_none());
        assert!(state.explorer_notice.is_none());
        assert!(state.commit_navigation(ScanId::new(2).unwrap()).is_none());
        assert_eq!(state.selected, Some(CardId(1)));
        state.stage_navigation(CardId(2), bookmark(2));
        state.pending_notice = Some("Unfiltered map".into());
        let restored = state.commit_navigation(ScanId::new(2).unwrap()).unwrap();
        assert_eq!(state.explorer_notice.as_deref(), Some("Unfiltered map"));
        assert_eq!(state.selected, Some(CardId(2)));
        assert_eq!(restored.cursor.unwrap().ordinal, 100);
        assert_eq!(restored.selected, Some(NodeId::new(2).unwrap()));
    }
    #[test]
    fn superseded_navigation_cannot_commit_old_card_and_reload_returns_home() {
        let mut state = State {
            selected: Some(CardId(1)),
            home: false,
            ..Default::default()
        };
        state.stage_navigation(CardId(2), bookmark(2));
        state.stage_navigation(CardId(3), bookmark(3));
        assert!(state.commit_navigation(ScanId::new(2).unwrap()).is_none());
        assert_eq!(state.selected, Some(CardId(1)));
        assert_eq!(
            state.pending_card,
            Some((CardId(3), ScanId::new(3).unwrap()))
        );
        state.release_cards();
        assert!(state.home);
        assert!(state.selected.is_none());
        assert!(state.commit_navigation(ScanId::new(3).unwrap()).is_none());
    }
}
