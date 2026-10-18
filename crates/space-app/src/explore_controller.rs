//! Scope loading, navigation and name search. Results are publications of epochs.
use super::*;
use explore::{Action, Event, ListMode};
use std::{cell::RefCell, rc::Rc, sync::atomic::AtomicU64};
#[derive(Default)]
pub(super) struct SearchState {
    pub text: String,
    pub epoch: u64,
    pub debounce: Option<gpui::Task<()>>,
    pub session: Option<deferred::Deferred<backend::Filtered>>,
    pub cancel: Option<Cancellation>,
    pub busy: bool,
    pub error: Option<String>,
    pub matching: Option<u64>,
}
#[derive(Clone)]
pub(super) struct PendingScope {
    pub generation: u64,
    pub parent: NodeId,
    pub restore: Option<NodeId>,
    pub cursor: Option<PageCursor>,
}
pub(super) struct State {
    pub snapshot: Option<Arc<Snapshot>>,
    pub crumbs: Vec<(NodeId, String, u128)>,
    pub query_generation: u64,
    pub latest_generation: Arc<AtomicU64>,
    pub tree: Option<Arc<tree::Tree>>,
    pub tree_busy: bool,
    pub tree_loading: bool,
    pub pending: Option<PendingScope>,
    pub geometry: Rc<RefCell<Option<treemap_view::GeometryCache>>>,
    pub focus_pending: bool,
    pub list_rows: Vec<StoredEntry>,
    pub list_paths: Vec<String>,
    pub list_next: Option<PageCursor>,
    pub list_mode: ListMode,
    pub list_visible: bool,
    pub list_selected: Option<usize>,
    pub selected: Option<NodeId>,
    pub hovered: Option<tree::Target>,
    pub selection_path: Option<(NodeId, String)>,
    pub scope_entry: Option<StoredEntry>,
    pub disk_menu_open: bool,
    pub status: Option<String>,
    pub new_scan_available: Option<space_core::ScanId>,
    pub search: SearchState,
}
impl Default for State {
    fn default() -> Self {
        Self {
            snapshot: None,
            crumbs: vec![],
            query_generation: 0,
            latest_generation: Arc::new(AtomicU64::new(0)),
            tree: None,
            tree_busy: false,
            tree_loading: false,
            pending: None,
            geometry: Rc::default(),
            focus_pending: false,
            list_rows: vec![],
            list_paths: vec![],
            list_next: None,
            list_mode: ListMode::Children,
            list_visible: true,
            list_selected: None,
            selected: None,
            hovered: None,
            selection_path: None,
            scope_entry: None,
            disk_menu_open: false,
            status: None,
            new_scan_available: None,
            search: Default::default(),
        }
    }
}
impl Space {
    pub(super) fn search_input_edited(&mut self, text: String, cx: &mut Context<Self>) {
        let model = self.explore_model();
        let event = model.event(Action::SearchChanged(text));
        if model.allows(&event)
            && let Action::SearchChanged(text) = event.action
        {
            self.search_changed(text, cx);
        }
    }

    pub(super) fn enter_scope_from_bookmark(
        &mut self,
        bookmark: dashboard_controller::Bookmark,
        cx: &mut Context<Self>,
    ) {
        self.screen = Screen::Explore;
        self.explore.crumbs = bookmark.crumbs;
        self.explore.list_mode = bookmark.list_mode;
        if let Some(parent) = self.explore.crumbs.last().map(|c| c.0) {
            self.enter_scope(parent, bookmark.selected, None, cx);
        }
    }
    fn invalidate_scope(&mut self) {
        self.explore.query_generation += 1;
        self.explore
            .latest_generation
            .store(self.explore.query_generation, Ordering::Release);
        self.explore.tree = None;
        self.explore.list_rows.clear();
        self.explore.list_paths.clear();
        self.explore.list_next = None;
        self.explore.selected = None;
        self.explore.list_selected = None;
        self.explore.hovered = None;
        self.explore.selection_path = None;
        self.explore.scope_entry = None;
        self.explore.geometry.borrow_mut().take();
        self.explore.pending = None;
    }
    pub(super) fn enter_scope(
        &mut self,
        parent: NodeId,
        restore: Option<NodeId>,
        cursor: Option<PageCursor>,
        cx: &mut Context<Self>,
    ) {
        self.invalidate_scope();
        self.explore.tree_loading = true;
        self.explore.focus_pending = true;
        let request = PendingScope {
            generation: self.explore.query_generation,
            parent,
            restore,
            cursor,
        };
        if self.explore.tree_busy {
            self.explore.pending = Some(request);
        } else {
            self.start_scope(request, cx);
        }
        cx.notify();
    }
    fn start_scope(&mut self, request: PendingScope, cx: &mut Context<Self>) {
        let (Some(backend), Some(snapshot)) = (self.backend.clone(), self.explore.snapshot.clone())
        else {
            return;
        };
        self.explore.tree_busy = true;
        let scan = snapshot.scan;
        let generation = request.generation;
        let metric = self.metric;
        let mode = self.explore.list_mode;
        let filtered = self.explore.search.session.clone();
        let abort = backend::TreeAbort(self.explore.latest_generation.clone(), generation);
        let work = request.clone();
        let task = cx.background_executor().spawn(async move {
            let tree = if let Some(filtered) = &filtered {
                backend::tree_filtered(
                    filtered,
                    work.parent,
                    metric,
                    tree::TreeBudget::EXPLORE,
                    &abort,
                )
            } else {
                backend::tree(
                    &backend,
                    scan,
                    work.parent,
                    metric,
                    tree::TreeBudget::EXPLORE,
                    &abort,
                )
            }?;
            let Some(tree) = tree else {
                return Ok::<_, String>(None);
            };
            let rows = backend::query(
                &backend,
                Query {
                    scan,
                    parent: work.parent,
                    view: if mode == ListMode::Children {
                        View::Folders
                    } else {
                        View::Largest
                    },
                    metric,
                    cursor: work.cursor,
                    subtree_only: true,
                    filtered,
                },
            )?;
            Ok(Some((tree, rows)))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.explore.tree_busy = false;
                if this.explore.query_generation == generation
                    && this
                        .explore
                        .snapshot
                        .as_ref()
                        .is_some_and(|s| s.scan == scan)
                    && this.screen == Screen::Explore
                {
                    match result {
                        Ok(Some((tree, rows))) => {
                            this.explore.tree = Some(Arc::new(tree));
                            this.explore.list_rows = rows.page.entries;
                            this.explore.list_next = rows.page.next;
                            this.explore.list_paths = rows.display_paths;
                            this.explore.scope_entry = Some(rows.current);
                            this.explore.tree_loading = false;
                            this.explore.status = None;
                            if let Some(node) = request.restore
                                && this.explore_model().contains(node)
                            {
                                this.explore.selected = Some(node);
                                this.explore.list_selected = this
                                    .explore
                                    .list_rows
                                    .iter()
                                    .position(|r| r.entry.node == node);
                            }
                        }
                        Err(error) => {
                            this.explore.tree_loading = false;
                            this.explore.status = Some(error);
                        }
                        Ok(None) => {}
                    }
                }
                if let Some(pending) = this.explore.pending.take() {
                    this.start_scope(pending, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
    pub(super) fn clear_search_workers(&mut self) {
        self.explore.search.debounce = None;
        if let Some(cancel) = self.explore.search.cancel.take() {
            self.executor
                .spawn(async move {
                    cancel.cancel();
                })
                .detach();
        }
        self.explore.search.session = None;
    }
    pub(super) fn clear_search(&mut self, cx: &mut Context<Self>) {
        self.clear_search_workers();
        self.explore.search.epoch += 1;
        self.explore.search.text.clear();
        self.explore.search.busy = false;
        self.explore.search.matching = None;
        self.explore.search.error = None;
        cx.notify();
    }
    pub(super) fn search_changed(&mut self, text: String, cx: &mut Context<Self>) {
        if self.explore.search.text == text {
            return;
        }
        self.clear_search_workers();
        self.explore.search.epoch += 1;
        self.explore.search.text = text;
        self.explore.search.error = None;
        self.explore.search.matching = None;
        self.invalidate_scope();
        self.explore.tree_loading = true;
        if self.explore.search.text.is_empty() {
            self.explore.search.busy = false;
            if let Some(parent) = self.explore.crumbs.last().map(|c| c.0) {
                self.enter_scope(parent, None, None, cx);
            }
            return;
        }
        self.explore.search.busy = true;
        let epoch = self.explore.search.epoch;
        let executor = self.executor.clone();
        self.explore.search.debounce = Some(cx.spawn(async move |this, cx| {
            executor.timer(Duration::from_millis(300)).await;
            let _ = this.update(cx, |this, cx| {
                if this.explore.search.epoch == epoch {
                    this.start_search(epoch, cx);
                }
            });
        }));
        cx.notify();
    }
    pub(super) fn start_search(&mut self, epoch: u64, cx: &mut Context<Self>) {
        let (Some(backend), Some(snapshot)) = (self.backend.clone(), self.explore.snapshot.clone())
        else {
            return;
        };
        let scan = snapshot.scan;
        let metric = self.metric;
        let name = self.explore.search.text.clone();
        let cancel = Cancellation::new();
        self.explore.search.cancel = Some(cancel.clone());
        let executor = self.executor.clone();
        let task = cx.background_executor().spawn(async move {
            backend::filter(
                &backend,
                scan,
                metric,
                &space_store::QueryFilter {
                    name: Some(name),
                    ..Default::default()
                },
                &cancel,
            )
            .map(|filtered| {
                deferred::Deferred::new(filtered, move |value| {
                    executor
                        .spawn(async move {
                            drop(value);
                        })
                        .detach();
                })
            })
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.explore.search.epoch != epoch
                    || this.screen != Screen::Explore
                    || this
                        .explore
                        .snapshot
                        .as_ref()
                        .is_none_or(|s| s.scan != scan)
                {
                    return;
                }
                this.explore.search.busy = false;
                this.explore.search.cancel = None;
                match result {
                    Ok(session) => {
                        this.explore.search.matching = Some(session.matching_count);
                        this.explore.search.session = Some(session);
                        if let Some(parent) = this.explore.crumbs.last().map(|c| c.0) {
                            this.enter_scope(parent, None, None, cx);
                        }
                    }
                    Err(error) => {
                        this.explore.search.error = Some(error);
                        this.explore.tree_loading = false;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn zoom_into(&mut self, node: NodeId, cx: &mut Context<Self>) {
        let model = self.explore_model();
        let Some(row) = model.node(node).filter(|n| n.is_directory) else {
            return;
        };
        self.explore.crumbs.push((node, row.label, row.bytes));
        self.enter_scope(node, None, None, cx);
    }
    fn select_node(&mut self, node: Option<NodeId>, cx: &mut Context<Self>) {
        if node.is_some_and(|n| !self.explore_model().contains(n)) {
            return;
        }
        self.explore.selected = node;
        self.explore.list_selected = node.and_then(|n| {
            self.explore
                .list_rows
                .iter()
                .position(|r| r.entry.node == n)
        });
        self.explore.focus_pending = true;
        self.explore.selection_path = None;
        if let (Some(node), Some(backend), Some(snapshot)) =
            (node, self.backend.clone(), self.explore.snapshot.clone())
        {
            let generation = self.explore.query_generation;
            let scan = snapshot.scan;
            let task = cx
                .background_executor()
                .spawn(async move { backend::native_path(&backend, &snapshot, node) });
            cx.spawn(async move |this, cx| {
                let result = task.await;
                let _ = this.update(cx, |this, cx| {
                    if this.explore.query_generation == generation
                        && this.explore.selected == Some(node)
                        && this
                            .explore
                            .snapshot
                            .as_ref()
                            .is_some_and(|s| s.scan == scan)
                    {
                        this.explore.selection_path = result
                            .ok()
                            .map(|p| (node, p.to_string_lossy().into_owned()));
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        cx.notify();
    }
    pub(super) fn explore_event(
        &mut self,
        event: Event,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.screen != Screen::Explore || !self.explore_model().allows(&event) {
            return;
        }
        match event.action {
            Action::Home => {
                self.disks.open_request += 1;
                self.remember_explorer();
                self.clear_search(cx);
                self.invalidate_scope();
                self.explore.snapshot = None;
                self.screen = Screen::Home;
                self.explore.disk_menu_open = false;
                self.queue_visible_previews(cx);
            }
            Action::ChooseDisk(ticket) => self.open_card(ticket, cx),
            Action::ToggleDiskMenu => {
                self.explore.disk_menu_open = !self.explore.disk_menu_open;
                if self.explore.disk_menu_open {
                    self.explore.hovered = None;
                }
            }
            Action::Rescan => {
                if !self.explore.search.text.is_empty() {
                    self.clear_search(cx);
                    if let Some(parent) = self.explore.crumbs.last().map(|c| c.0) {
                        self.enter_scope(parent, None, None, cx);
                    }
                }
                self.rescan_selected_disk(cx);
            }
            Action::Stop => self.stop_acquisition(cx),
            Action::ShowNewScan => {
                if let Some(card) = self.disks.selected {
                    self.disks.bookmarks.remove(&card);
                    if let Some(data) = self.disks.cards.get(&card) {
                        let t = ticket::Ticket {
                            card,
                            scan: data.scan,
                            query_epoch: data.epoch,
                        };
                        self.open_card(t, cx);
                    }
                }
            }
            Action::Up => {
                if self.explore.crumbs.len() > 1 {
                    let exited = self.explore.crumbs.pop().unwrap().0;
                    let parent = self.explore.crumbs.last().unwrap().0;
                    self.enter_scope(parent, Some(exited), None, cx);
                }
            }
            Action::Crumb(node) => {
                if let Some(i) = self.explore.crumbs.iter().position(|c| c.0 == node) {
                    self.explore.crumbs.truncate(i + 1);
                    self.enter_scope(node, None, None, cx);
                }
            }
            Action::ZoomInto(node) => self.zoom_into(node, cx),
            Action::Select(node) => self.select_node(node, cx),
            Action::Hover(target) => {
                if self.explore.hovered == target {
                    return;
                }
                self.explore.hovered = target;
            }
            Action::OpenOther(parent) => {
                self.explore.list_mode = ListMode::Children;
                self.explore.list_visible = true;
                if self.explore.crumbs.last().is_some_and(|c| c.0 != parent) {
                    self.zoom_into(parent, cx);
                } else {
                    self.enter_scope(parent, None, None, cx);
                }
            }
            Action::ListMode(mode) => {
                self.explore.list_mode = mode;
                if let Some(parent) = self.explore.crumbs.last().map(|c| c.0) {
                    self.enter_scope(parent, None, None, cx);
                }
            }
            Action::ToggleList => self.explore.list_visible = !self.explore.list_visible,
            Action::ListSelect(i) => {
                if let Some(row) = self.explore.list_rows.get(i) {
                    let node = row.entry.node;
                    self.select_node(Some(node), cx);
                    self.explore.focus_pending = false;
                    window.focus(&self.list_focus, cx);
                }
            }
            Action::ListOpen(i) => {
                if let Some(row) = self.explore.list_rows.get(i) {
                    let node = row.entry.node;
                    if row.entry.kind == EntryKind::Directory {
                        self.zoom_into(node, cx);
                    } else {
                        self.select_node(Some(node), cx);
                    }
                }
            }
            Action::NextPage | Action::FirstPage => {
                let cursor = if matches!(event.action, Action::NextPage) {
                    self.explore.list_next
                } else {
                    None
                };
                if let Some(parent) = self.explore.crumbs.last().map(|c| c.0) {
                    self.enter_scope(parent, None, cursor, cx);
                }
            }
            Action::ToggleMetric => {
                self.metric = if self.metric == Metric::Logical {
                    Metric::Allocated
                } else {
                    Metric::Logical
                };
                self.queue_visible_previews(cx);
                if self.explore.search.text.is_empty() {
                    if let Some(parent) = self.explore.crumbs.last().map(|c| c.0) {
                        self.enter_scope(parent, None, None, cx);
                    }
                } else {
                    let text = self.explore.search.text.clone();
                    self.explore.search.text.clear();
                    self.search_changed(text, cx);
                }
            }
            Action::Copy => self.path_action(false, cx),
            Action::Reveal => self.path_action(true, cx),
            Action::SearchChanged(text) => self.search_changed(text, cx),
            Action::SearchClear => {
                self.clear_search(cx);
                if let Some(parent) = self.explore.crumbs.last().map(|c| c.0) {
                    self.enter_scope(parent, None, None, cx);
                }
            }
        }
        cx.notify();
    }
    fn path_action(&mut self, reveal: bool, cx: &mut Context<Self>) {
        if self.intent.path_busy {
            return;
        }
        let (Some(backend), Some(snapshot), Some(node)) = (
            self.backend.clone(),
            self.explore.snapshot.clone(),
            self.explore.selected,
        ) else {
            return;
        };
        self.intent.path_busy = true;
        let generation = self.explore.query_generation;
        let scan = snapshot.scan;
        let task = cx.background_executor().spawn(async move {
            if reveal {
                backend::validated_reveal_path(&backend, &snapshot, node)
            } else {
                backend::native_path(&backend, &snapshot, node)
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                let current = this.screen == Screen::Explore
                    && this.explore.query_generation == generation
                    && this.explore.selected == Some(node)
                    && this
                        .explore
                        .snapshot
                        .as_ref()
                        .is_some_and(|s| s.scan == scan);
                if !this.intent.complete_path(current) {
                    cx.notify();
                    return;
                }
                this.explore.status = Some(match result {
                    Ok(path) if reveal => {
                        cx.reveal_path(&path);
                        "Reveal requested".into()
                    }
                    Ok(path) => match path.to_str() {
                        Some(text) => {
                            cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
                            "Path copied".into()
                        }
                        None => "Native path cannot be represented losslessly as text".into(),
                    },
                    Err(error) => error,
                });
                cx.notify();
            });
        })
        .detach();
    }
    pub(super) fn explore_model(&self) -> explore::Model {
        let home = self.home_model();
        let card = self
            .disks
            .selected
            .and_then(|c| self.disks.registry.card(c));
        let total = self.explore.tree.as_ref().map_or(0, |t| t.total_bytes);
        let rows = self
            .explore
            .list_rows
            .iter()
            .enumerate()
            .map(|(i, row)| explore::ListRow {
                node: row.entry.node,
                label: row.entry.display_name.clone(),
                bytes: metric_bytes(row, self.metric),
                is_directory: row.entry.kind == EntryKind::Directory,
                unknown_count: if self.metric == Metric::Logical {
                    row.logical.unknown_count
                } else {
                    row.allocated.unknown_count
                },
                path: self.explore.list_paths.get(i).cloned(),
            })
            .collect();
        explore::Model {
            ticket: self.disks.selected.map(|card| ticket::Ticket {
                card,
                scan: self.explore.snapshot.as_ref().map(|s| s.scan),
                query_epoch: self.explore.query_generation,
            }),
            generation: self.explore.query_generation,
            disks: home
                .cards
                .into_iter()
                .map(|c| explore::DiskChoice {
                    ticket: c.ticket,
                    name: c.name,
                    status: c.status.label(),
                    connected: c.connected,
                })
                .collect(),
            disk_menu_open: self.explore.disk_menu_open,
            name: card.map_or_else(|| "Saved scan".into(), |c| c.label.clone()),
            crumbs: self.explore.crumbs.clone(),
            scope_total: total,
            metric: self.metric,
            busy: self.acquisition_busy(),
            stopping: self.disks.batch.stopping(),
            offline: card.is_some_and(|c| !c.connected),
            partial: self.explore.snapshot.as_ref().is_some_and(|s| {
                s.root_entry.coverage.error_count > 0 || s.root_entry.coverage.unknown_entries > 0
            }),
            tree: self.explore.tree.clone(),
            tree_loading: self.explore.tree_loading,
            selected: self.explore.selected,
            hovered: self.explore.hovered,
            list_visible: self.explore.list_visible,
            list_mode: self.explore.list_mode,
            rows,
            list_selected: self.explore.list_selected,
            has_next_page: self.explore.list_next.is_some(),
            selection_path: self.explore.selection_path.as_ref().map(|(_, p)| p.clone()),
            status: self.explore.status.clone(),
            new_scan_available: self.explore.new_scan_available.is_some(),
            search_text: self.explore.search.text.clone(),
            search_busy: self.explore.search.busy,
            matching: self.explore.search.matching,
            search_error: self.explore.search.error.clone(),
        }
    }
}
