mod backend;
mod cache_management;
mod changes_controller;
mod changes_panel;
mod dashboard;
mod dashboard_controller;
mod dashboard_render;
#[cfg(test)]
mod dashboard_transition_tests;
mod deferred;
mod disk_model;
mod explorer;
mod filters;
mod intent;
mod paths;
mod scan_batch;
mod text_input;
mod treemap;

use backend::{Backend, Query, Snapshot, View};
use gpui::{
    App, Bounds, ClipboardItem, Context, Div, FocusHandle, KeyBinding, PathPromptOptions, Role,
    ScrollStrategy, SharedString, Stateful, UniformListScrollHandle, Window, WindowBounds,
    WindowOptions, actions, div, prelude::*, px, rgb, size, uniform_list,
};
use space_core::{Cancellation, EntryKind, Metric, NodeId};
use space_store::{PageCursor, RootRecord, StoredEntry};
use std::{
    path::PathBuf,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

actions!(
    space,
    [
        ChooseFolder,
        Refresh,
        Cancel,
        CopyPath,
        RevealPath,
        NextRow,
        PreviousRow,
        OpenRow,
        GoUp,
        Folders,
        Largest,
        ShowTreemap,
        ToggleFilters,
        RefreshVolumes,
        ShowCache,
        ShowChanges,
        ShowAliases,
        NextAliases,
        NextVolumes,
        ToggleSubtree,
        ResetFilters,
        ApplyFilters,
        CancelFilter,
        ToggleUnknownSize,
        ToggleUnknownTime,
        ToggleMetric,
        NextPage,
        FirstPage,
        FocusNext,
        FocusPrevious,
        CloseWindow
    ]
);

struct Running {
    cancel: Cancellation,
    progress: Arc<backend::Progress>,
    generation: u64,
    executor: gpui::BackgroundExecutor,
    cancel_requested: bool,
    armed: bool,
}
impl Drop for Running {
    fn drop(&mut self) {
        if self.armed && !self.cancel_requested {
            let cancel = self.cancel.clone();
            self.executor
                .spawn(async move {
                    cancel.cancel();
                })
                .detach();
        }
    }
}

type FilterSession = deferred::Deferred<backend::Filtered>;
struct Space {
    disks: dashboard_controller::State,
    changes: changes_controller::State,
    executor: gpui::BackgroundExecutor,
    filters_visible: bool,
    cache_visible: bool,
    cache_model: cache_management::Model,
    cache_before: Option<space_core::ScanId>,
    cache_epoch: u64,
    maintenance_busy: bool,
    volumes: Option<space_scan::VolumeListing>,
    focus: FocusHandle,
    list_focus: FocusHandle,
    scroll: UniformListScrollHandle,
    backend: Option<deferred::Deferred<Backend>>,
    cache_directory: Option<PathBuf>,
    snapshot: Option<Arc<Snapshot>>,
    pending_snapshot: Option<Arc<Snapshot>>,
    roots: Vec<RootRecord>,
    intent: intent::Intent,
    picker_open: bool,
    running: Option<Running>,
    generation: u64,
    status: String,
    view: View,
    metric: Metric,
    applied_metric: Metric,
    applied_filter: space_store::QueryFilter,
    breadcrumbs: Vec<(NodeId, String)>,
    rows: Vec<StoredEntry>,
    row_paths: Vec<String>,
    alias_nodes: Vec<(NodeId, String)>,
    alias_origin: Option<NodeId>,
    alias_next: Option<PageCursor>,
    alias_selected: Option<NodeId>,
    alias_busy: bool,
    alias_generation: u64,
    selected: usize,
    next_page: Option<PageCursor>,
    query_generation: u64,
    query_busy: bool,
    pending_query: Option<(u64, Query)>,
    snapshot_request: u64,
    filter_inputs: Vec<gpui::Entity<text_input::TextInput>>,
    filter_subscriptions: Vec<gpui::Subscription>,
    filter_config: space_store::QueryFilter,
    draft_unknown_size: bool,
    draft_unknown_time: bool,
    filter_session: Option<FilterSession>,
    filter_epoch: u64,
    filter_busy: bool,
    filter_pending: Option<(u64, space_store::QueryFilter)>,
    filter_cancel: Option<Cancellation>,
    filter_status: String,
    filter_error: Option<String>,
    subtree_only: bool,
    treemap: treemap::Model,
    current_totals: Option<StoredEntry>,
    chart_selected: Option<NodeId>,
}

impl Drop for Space {
    fn drop(&mut self) {
        // Close the queue synchronously; Running::drop dispatches cancellation
        // off the UI thread, including the publication-guard wait.
        let _ = self.disks.batch.reduce(scan_batch::Event::Shutdown);
        if let Some(pending) = self.pending_query.take() {
            self.executor
                .spawn(async move {
                    drop(pending);
                })
                .detach();
        }
        if let Some(cancel) = self.filter_cancel.take() {
            self.executor
                .spawn(async move {
                    cancel.cancel();
                })
                .detach();
        }
    }
}

fn reset_empty_presentation(
    next: &mut Option<PageCursor>,
    selected: &mut usize,
    filter: &mut space_store::QueryFilter,
    status: &mut String,
) {
    *next = None;
    *selected = 0;
    *filter = space_store::QueryFilter::default();
    *status = "No cached scan is displayed.".into();
}

fn clear_chart(model: &mut treemap::Model, selected: &mut Option<NodeId>) {
    *model = treemap::Model::default();
    *selected = None;
}
fn alias_selection_is_current(
    rendered: (u64, Option<space_core::ScanId>),
    current: (u64, Option<space_core::ScanId>),
    node: NodeId,
    entries: &[(NodeId, String)],
) -> bool {
    rendered == current && current.1.is_some() && entries.iter().any(|(id, _)| *id == node)
}
fn chart_folder(model: &treemap::Model, node: NodeId) -> Option<String> {
    model
        .items
        .iter()
        .find(|item| item.node == node && item.is_directory)
        .map(|item| item.label.clone())
}
fn control(id: &'static str, label: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label)
        .focusable()
        .tab_stop(true)
        .border_2()
        .border_color(rgb(0x285166))
        .focus(|s| s.border_color(rgb(0x90d5ff)))
        .px_2()
        .py_1()
        .bg(rgb(0x285166))
        .cursor_pointer()
        .child(label)
}
fn total_text(row: &StoredEntry, metric: Metric) -> String {
    let total = match metric {
        Metric::Logical => row.logical,
        Metric::Allocated => row.allocated,
    };
    let mut text = dashboard::format_bytes(total.observed_bytes);
    if total.unknown_count > 0 {
        text.push_str(&format!(" + {} unknown", total.unknown_count));
    }
    if total.estimated_count > 0 {
        text.push_str(" (estimated)");
    }
    text
}

impl Space {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let mut app = Self {
            disks: dashboard_controller::State::default(),
            changes: changes_controller::State::new(cx.background_executor().clone()),
            executor: cx.background_executor().clone(),
            filters_visible: false,
            cache_visible: false,
            cache_model: cache_management::Model::default(),
            cache_before: None,
            cache_epoch: 0,
            maintenance_busy: false,
            volumes: None,
            focus,
            list_focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            backend: None,
            cache_directory: None,
            snapshot: None,
            pending_snapshot: None,
            roots: vec![],
            intent: intent::Intent::default(),
            picker_open: false,
            running: None,
            generation: 0,
            status: "Opening local cache…".into(),
            view: View::Treemap,
            metric: Metric::Logical,
            applied_metric: Metric::Logical,
            applied_filter: space_store::QueryFilter::default(),
            breadcrumbs: vec![],
            rows: vec![],
            row_paths: vec![],
            alias_nodes: vec![],
            alias_origin: None,
            alias_next: None,
            alias_selected: None,
            alias_busy: false,
            alias_generation: 0,
            selected: 0,
            next_page: None,
            query_generation: 0,
            query_busy: false,
            pending_query: None,
            snapshot_request: 0,
            filter_inputs: [
                ("Name contains", "literal text"),
                ("Path contains", "relative display path"),
                ("Extension", "e.g. rs"),
                ("Minimum bytes", "0"),
                ("Maximum bytes", "unbounded"),
                ("Modified from UTC", "YYYY-MM-DD / RFC3339"),
                ("Modified through UTC", "YYYY-MM-DD / RFC3339"),
            ]
            .into_iter()
            .map(|(label, placeholder)| {
                cx.new(|cx| text_input::TextInput::new(label, placeholder, cx))
            })
            .collect(),
            filter_subscriptions: vec![],
            filter_config: space_store::QueryFilter::default(),
            draft_unknown_size: false,
            draft_unknown_time: false,
            filter_session: None,
            filter_epoch: 0,
            filter_busy: false,
            filter_pending: None,
            filter_cancel: None,
            filter_status: "Unfiltered totals".into(),
            filter_error: None,
            subtree_only: true,
            treemap: treemap::Model::default(),
            current_totals: None,
            chart_selected: None,
        };
        app.filter_subscriptions = app
            .filter_inputs
            .iter()
            .map(|input| {
                cx.subscribe(input, |this, _, event, cx| {
                    let text_input::TextInputEvent::Edited(value) = event;
                    let _ = value;
                    this.debounce_filters(cx);
                })
            })
            .collect();
        app.initialize(cx);

        app
    }
    fn show_aliases(&mut self, _: &ShowAliases, _: &mut Window, cx: &mut Context<Self>) {
        let node = if self.view == View::Treemap {
            self.chart_selected
        } else {
            self.rows.get(self.selected).map(|r| r.entry.node)
        };
        if let Some(node) = node {
            self.load_aliases(node, None, cx);
        }
    }
    fn next_aliases(&mut self, _: &NextAliases, _: &mut Window, cx: &mut Context<Self>) {
        if let (Some(node), Some(cursor)) = (self.alias_origin, self.alias_next) {
            self.load_aliases(node, Some(cursor), cx);
        }
    }
    fn load_aliases(&mut self, node: NodeId, cursor: Option<PageCursor>, cx: &mut Context<Self>) {
        if self.alias_busy || self.maintenance_busy {
            return;
        }
        let (Some(backend), Some(snapshot)) = (self.backend.clone(), self.snapshot.clone()) else {
            return;
        };
        self.alias_busy = true;
        let generation = self.query_generation;
        let alias_generation = self.alias_generation;
        let task = cx.background_executor().spawn(async move {
            let _pin = &snapshot;
            backend::aliases(&backend, snapshot.scan, node, cursor)
        });
        cx.spawn(async move |this,cx|{let result=task.await;let _=this.update(cx,|this,cx|{
            this.alias_busy=false;
            if this.query_generation==generation && this.alias_generation==alias_generation {
                match result {Ok(page)=>{this.alias_origin=Some(node);this.alias_nodes=page.entries;this.alias_next=page.next;this.alias_selected=None;this.status="Choose a cached alias, then Copy or Reveal. Filter-independent aliases share one identity; only the canonical owner is charged.".into();},Err(error)=>this.status=format!("Aliases unavailable: {error}")}
            }cx.notify();
        });}).detach();
        cx.notify();
    }
    fn clear_aliases(&mut self) {
        self.alias_generation = self.alias_generation.wrapping_add(1);
        self.alias_nodes.clear();
        self.alias_origin = None;
        self.alias_next = None;
        self.alias_selected = None;
    }
    fn clear_empty_view(&mut self) {
        self.changes.invalidate();
        self.snapshot = None;
        self.pending_snapshot = None;
        self.filter_session = None;
        self.rows.clear();
        self.row_paths.clear();
        self.breadcrumbs.clear();
        self.current_totals = None;
        clear_chart(&mut self.treemap, &mut self.chart_selected);
        self.clear_aliases();
        reset_empty_presentation(
            &mut self.next_page,
            &mut self.selected,
            &mut self.applied_filter,
            &mut self.filter_status,
        );
        self.applied_metric = self.metric;
    }
    fn show_cache(&mut self, _: &ShowCache, _: &mut Window, cx: &mut Context<Self>) {
        self.cache_visible = !self.cache_visible;
        if self.cache_visible {
            self.load_cache_panel(None, self.snapshot.as_ref().map(|s| s.scan), None, cx);
        }
        cx.notify();
    }
    fn load_cache_panel(
        &mut self,
        before: Option<space_core::ScanId>,
        selected: Option<space_core::ScanId>,
        after: Option<i64>,
        cx: &mut Context<Self>,
    ) {
        if self.cache_model.busy || self.maintenance_busy {
            return;
        }
        let Some(backend) = self.backend.clone() else {
            return;
        };
        self.cache_model.busy = true;
        self.cache_epoch += 1;
        let epoch = self.cache_epoch;
        self.cache_before = before;
        let task = cx
            .background_executor()
            .spawn(async move { backend::inspect_cache(&backend, before, selected, after) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.cache_epoch != epoch {
                    return;
                }
                this.cache_model.busy = false;
                match result {
                    Ok(data) => {
                        this.cache_model.error = None;
                        this.cache_model.rows = data
                            .rows
                            .into_iter()
                            .map(|row| cache_management::Row {
                                scan: row.info.spec.key.id,
                                root: row
                                    .path
                                    .to_path_buf()
                                    .map(|p| p.to_string_lossy().into_owned())
                                    .unwrap_or_else(|_| "Foreign native path".into()),
                                status: format!("{:?}", row.info.status),
                                coverage: row
                                    .info
                                    .coverage
                                    .map(|c| {
                                        format!(
                                            "{} errors, {} unknown, {} excluded, {} boundaries",
                                            c.error_count,
                                            c.unknown_entries,
                                            c.excluded_scopes,
                                            c.boundary_scopes
                                        )
                                    })
                                    .unwrap_or_else(|| "Unpublished observation".into()),
                                started: format!("{} UTC-seconds", row.info.started.seconds),
                                ended: row
                                    .info
                                    .ended
                                    .map(|t| format!("{} UTC-seconds", t.seconds))
                                    .unwrap_or_else(|| "unfinished".into()),
                                sizes: row
                                    .totals
                                    .map(|t| {
                                        format!(
                                            "logical {} · allocated {}",
                                            total_text(&t, Metric::Logical),
                                            total_text(&t, Metric::Allocated)
                                        )
                                    })
                                    .unwrap_or_default(),
                                removable: !matches!(
                                    row.info.status,
                                    space_core::ScanStatus::Scanning
                                        | space_core::ScanStatus::Finalizing
                                ),
                            })
                            .collect();
                        this.cache_model.selected = selected
                            .filter(|id| this.cache_model.rows.iter().any(|r| r.scan == *id));
                        this.cache_model.next = data.next;
                        this.cache_model.footprint = Some(data.footprint);
                        this.cache_model.diagnostics = data
                            .issues
                            .as_ref()
                            .map(|page| {
                                page.issues
                                    .iter()
                                    .map(|issue| {
                                        format!(
                                            "{:?} · node {:?} · {} · {}",
                                            issue.class,
                                            issue.node,
                                            issue
                                                .name
                                                .as_ref()
                                                .map(|n| n.display())
                                                .unwrap_or_default(),
                                            issue.detail
                                        )
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        this.cache_model.diagnostic_next = data.issues.and_then(|page| page.next);
                    }
                    Err(error) => this.cache_model.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn cache_event(
        &mut self,
        event: cache_management::Event,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.cache_model.allows(event) || self.maintenance_busy {
            return;
        }
        use cache_management::Event;
        match event {
            Event::Select(scan) => self.load_cache_panel(self.cache_before, Some(scan), None, cx),
            Event::FirstPage | Event::RefreshRequested => {
                self.load_cache_panel(None, None, None, cx)
            }
            Event::NextPage(before) => self.load_cache_panel(Some(before), None, None, cx),
            Event::MoreDiagnostics(after) => self.load_cache_panel(
                self.cache_before,
                self.cache_model.selected,
                Some(after),
                cx,
            ),
            Event::RemoveRequested(scan) => self.confirm_cache_removal(Some(scan), window, cx),
            Event::ClearRequested => self.confirm_cache_removal(None, window, cx),
        }
    }
    fn cache_idle(&self) -> bool {
        self.running.is_none()
            && !self.filter_busy
            && !self.query_busy
            && !self.intent.path_busy
            && !self.alias_busy
            && self.changes.idle()
            && !self.picker_open
            && self.disks.idle()
    }
    fn confirm_cache_removal(
        &mut self,
        scan: Option<space_core::ScanId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.cache_idle() {
            self.cache_model.warning = Some(
                "Wait for current scan, filter and view operations before removing cached data."
                    .into(),
            );
            cx.notify();
            return;
        }
        let Some(backend) = self.backend.clone() else {
            return;
        };
        let epoch = self.cache_epoch;
        let snapshot_request = self.snapshot_request;
        self.maintenance_busy = true;
        self.cache_model.busy = true;
        let title = scan
            .map(|id| format!("Remove cached scan {}?", id.get()))
            .unwrap_or_else(|| "Clear all cached scan metadata?".into());
        let response=window.prompt(gpui::PromptLevel::Warning,&title,Some("This removes only Space's local cached metadata, never source files. There is no undo; recovery requires rescanning. Current views will close and the latest remaining scan will reopen."),&["Cancel","Remove cached metadata"],cx);
        cx.spawn(async move |this,cx| {let response=response.await;let _=this.update(cx,|this,cx| {
            this.maintenance_busy=false;this.cache_model.busy=false;
            if !matches!(response,Ok(1)) {cx.notify();return;}
            let event=scan.map(cache_management::Event::RemoveRequested).unwrap_or(cache_management::Event::ClearRequested);
            if this.cache_epoch!=epoch || this.snapshot_request!=snapshot_request || !this.cache_idle() || !this.cache_model.allows(event) {
                this.cache_model.warning=Some("Cache selection or active work changed. Nothing removed; review and confirm again.".into());cx.notify();return;
            }
            this.maintenance_busy=true;this.cache_model.busy=true;
            this.query_generation+=1;let pending_query=this.pending_query.take();this.filter_epoch+=1;this.filter_pending=None;
            let snapshot=this.snapshot.take();let pending=this.pending_snapshot.take();
            let filtered=this.filter_session.take();
            let comparison=this.changes.session.take();this.changes.invalidate();
            let card_pins=this.disks.release_cards();
            this.rows.clear();this.row_paths.clear();this.clear_aliases();this.breadcrumbs.clear();clear_chart(&mut this.treemap,&mut this.chart_selected);this.current_totals=None;
            let task=cx.background_executor().spawn(async move {
                if let Some((_,query))=pending_query && let Some(filtered)=query.filtered{filtered.close_on_worker();}
                drop(card_pins);drop(snapshot);drop(pending);if let Some(filtered)=filtered{filtered.close_on_worker();}
                if let Some(comparison)=comparison{comparison.close_on_worker();}
                let outcome=backend::remove_cached(&backend,scan);
                let reloaded=backend::reload(&backend);
                (outcome,reloaded)
            });
            cx.spawn(async move |this,cx| {let (outcome,reloaded)=task.await;let _=this.update(cx,|this,cx| {
                this.maintenance_busy=false;this.cache_model.busy=false;
                this.cache_model.warning=Some(match outcome {Ok(())=>"Cached metadata removed. Source files were untouched.".into(),Err(error)=>format!("Cache removal failed: {error}")});
                match reloaded {Ok(state)=>{this.roots=state.roots;if let Some(snapshot)=state.snapshot{this.apply_snapshot(snapshot,cx);}else{this.clear_empty_view();}},Err(error)=>this.status=format!("Cache reload failed: {error}")}
                this.reconcile_disks(cx);this.load_cache_panel(None,None,None,cx);cx.notify();
            });}).detach();cx.notify();
        });}).detach();
        cx.notify();
    }
    fn load_volumes(&mut self, cx: &mut Context<Self>) {
        self.discover_disks(None, cx);
    }
    fn refresh_volumes(&mut self, _: &RefreshVolumes, _: &mut Window, cx: &mut Context<Self>) {
        self.discover_disks(None, cx);
    }
    fn next_volumes(&mut self, _: &NextVolumes, _: &mut Window, cx: &mut Context<Self>) {
        self.disks.page =
            (self.disks.page + 1) % self.disks.registry.cards().len().div_ceil(8).max(1);
        self.queue_visible_previews(cx);
        cx.notify();
    }
    fn initialize(&mut self, cx: &mut Context<Self>) {
        let directory = match paths::cache_directory() {
            Ok(path) => path,
            Err(error) => {
                self.disks.initializing = false;
                self.disks.notice = Some(format!(
                    "Cache unavailable: {error}. Restart Space after resolving the cache location."
                ));
                self.status = error;
                return;
            }
        };
        self.cache_directory = Some(directory.clone());
        let executor = cx.background_executor().clone();
        let task = cx.background_executor().spawn(async move {
            backend::open(&directory).map(|opened| {
                let backend = deferred::Deferred::new(Arc::new(opened.backend), move |value| {
                    executor
                        .spawn(async move {
                            drop(value);
                        })
                        .detach();
                });
                (backend, opened.roots, opened.snapshot)
            })
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.disks.initializing = false;
                match result {
                    Ok((backend, roots, snapshot)) => {
                        this.backend = Some(backend);
                        this.roots = roots;
                        drop(snapshot);
                        this.reconcile_disks(cx);
                        this.load_volumes(cx);
                        this.status =
                            "Choose Scan all disks to begin, or open an existing cached map."
                                .into();
                    }
                    Err(error) => {
                        this.status =
                            format!("Cache unavailable: {error}. Original data was not replaced. Restart Space after resolving the cache error.");
                        this.disks.notice = Some(this.status.clone())
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn apply_snapshot(&mut self, snapshot: Snapshot, cx: &mut Context<Self>) {
        self.changes.invalidate();
        self.snapshot_request += 1;
        self.pending_snapshot = Some(Arc::new(snapshot));
        self.rebuild_filters(cx);
    }
    fn commit_pending_snapshot(&mut self) -> Option<PageCursor> {
        if let Some(snapshot) = self.pending_snapshot.take() {
            self.breadcrumbs = vec![(
                snapshot.root_entry.entry.node,
                snapshot.root_entry.entry.display_name.clone(),
            )];
            self.disks.displayed_scope = None;
            self.disks.page_cursor = None;
            self.restore_disk_bookmark(&snapshot);
            self.snapshot = Some(snapshot);
            self.status="Showing an immutable cached scan. Live root has not been revalidated; offline browsing is available.".into();
            return self.disks.page_cursor;
        }
        None
    }
    fn choose_folder(&mut self, _: &ChooseFolder, _: &mut Window, cx: &mut Context<Self>) {
        self.scan_folder_picker(cx);
    }
    fn refresh(&mut self, _: &Refresh, _: &mut Window, cx: &mut Context<Self>) {
        self.rescan_selected_disk(cx);
    }
    fn cancel(&mut self, _: &Cancel, window: &mut Window, cx: &mut Context<Self>) {
        if self.disks.batch.is_busy() || self.disks.folder_preflight || self.picker_open {
            self.stop_acquisition(cx);
            return;
        }
        if self.running.is_none() && self.changes.build_busy {
            if let Some(build) = self.changes.model.building {
                let event = self
                    .changes
                    .model
                    .event(changes_panel::Action::CancelRequested {
                        request: build.request,
                    });
                self.changes_event(event, window, cx);
            }
            return;
        }
        if self.running.is_none() && self.filter_busy {
            self.cancel_filter(&CancelFilter, window, cx);
            return;
        }
        if let Some(run) = &mut self.running {
            if !run.cancel_requested {
                run.cancel_requested = true;
                let cancel = run.cancel.clone();
                cx.background_executor()
                    .spawn(async move {
                        cancel.cancel();
                    })
                    .detach();
            }
            self.disks.notice = Some("Stopping; waiting for the worker acknowledgement.".into());
        }
        cx.notify();
    }
    fn toggle_filters(&mut self, _: &ToggleFilters, _: &mut Window, cx: &mut Context<Self>) {
        self.filters_visible = !self.filters_visible;
        cx.notify();
    }
    fn debounce_filters(&mut self, cx: &mut Context<Self>) {
        self.filter_error = Some(
            "Draft filter edits. Press Apply filters to compute; displayed results are unchanged."
                .into(),
        );
        cx.notify();
    }
    fn apply_filters(&mut self, _: &ApplyFilters, _: &mut Window, cx: &mut Context<Self>) {
        self.apply_filter_inputs(cx);
    }
    fn cancel_filter(&mut self, _: &CancelFilter, _: &mut Window, cx: &mut Context<Self>) {
        self.filter_epoch += 1;
        self.filter_pending = None;
        self.pending_snapshot = None;
        self.disks.cancel_navigation();
        self.filter_config = self.applied_filter.clone();
        self.metric = self.applied_metric;
        if let Some(cancel) = self.filter_cancel.take() {
            self.executor
                .spawn(async move {
                    cancel.cancel();
                })
                .detach();
        }
        self.filter_status="Filter cancelled; previous applied results retained. Draft inputs can be applied again.".into();
        self.queue_visible_previews(cx);
        cx.notify();
    }
    fn apply_filter_inputs(&mut self, cx: &mut Context<Self>) {
        if self.changes.visible {
            self.status =
                "Return to Explorer to change the captured comparison filters or metric.".into();
            cx.notify();
            return;
        }
        if self.maintenance_busy {
            return;
        }
        let values: Vec<_> = self
            .filter_inputs
            .iter()
            .map(|input| input.read(cx).text().to_owned())
            .collect();
        let fields = filters::Fields {
            name: values[0].clone(),
            path: values[1].clone(),
            extension: values[2].clone(),
            min_size: values[3].clone(),
            max_size: values[4].clone(),
            modified_after: values[5].clone(),
            modified_before: values[6].clone(),
            include_unknown_size: self.draft_unknown_size,
            include_unknown_time: self.draft_unknown_time,
        };
        match fields.apply_to(&mut self.filter_config) {
            Ok(()) => {
                self.filter_error = None;
                self.disks.explorer_notice = None;
                self.disks.invalidate_page_order();
                self.rebuild_filters(cx);
            }
            Err(error) => {
                self.filter_error = Some(format!(
                    "Invalid filter: {error}. Previous applied results remain."
                ));
                cx.notify();
            }
        }
    }
    fn rebuild_filters(&mut self, cx: &mut Context<Self>) {
        self.filter_epoch += 1;
        if self.filter_busy {
            self.filter_pending = Some((self.filter_epoch, self.filter_config.clone()));
            if let Some(cancel) = self.filter_cancel.take() {
                cx.background_executor()
                    .spawn(async move {
                        cancel.cancel();
                    })
                    .detach();
            }
            self.filter_status = "Replacing filter; waiting for cancelled query…".into();
        } else {
            self.start_filter(self.filter_epoch, self.filter_config.clone(), cx);
        }
        cx.notify();
    }
    fn start_filter(
        &mut self,
        epoch: u64,
        config: space_store::QueryFilter,
        cx: &mut Context<Self>,
    ) {
        if config.is_empty() {
            let cursor = self.commit_pending_snapshot();
            self.filter_session = None;
            self.applied_filter = config;
            self.applied_metric = self.metric;
            self.filter_status = "Unfiltered totals".into();
            self.request_query(cursor, cx);
            return;
        }
        let (Some(backend), Some(snapshot)) = (
            self.backend.clone(),
            self.pending_snapshot
                .clone()
                .or_else(|| self.snapshot.clone()),
        ) else {
            return;
        };
        self.filter_busy = true;
        self.filter_status = "Computing filter… Previous applied view remains browsable. Large scans can take a minute; Cancel filter stops this query.".into();
        let metric = self.metric;
        let cancel = Cancellation::new();
        self.filter_cancel = Some(cancel.clone());
        let executor = cx.background_executor().clone();
        let applied_config = config.clone();
        let task = cx.background_executor().spawn(async move {
            backend::filter(&backend, snapshot.scan, metric, &config, &cancel).map(|value| {
                FilterSession::new(value, move |value| {
                    executor
                        .spawn(async move {
                            drop(value);
                        })
                        .detach();
                })
            })
        });
        cx.spawn(async move |this,cx| {
            let result = task.await;
            let _ = this.update(cx,|this,cx| {
                this.filter_busy = false;
                this.filter_cancel = None;
                if this.filter_epoch == epoch {
                    match result {
                        Ok(session) => {
                            let cursor = this.commit_pending_snapshot();
                            this.applied_metric=metric;
                            this.applied_filter=applied_config;
                            let filtered=&session;
                            this.filter_status=format!("Filtered: {} matching non-directory paths (aliases included) · {}. Canonical owners alone contribute bytes; coverage remains conservative.",filtered.matching_count,total_text(&filtered.root,metric));
                            this.filter_session=Some(session);
                            this.request_query(cursor,cx);
                        }
                        Err(error) => { this.pending_snapshot = None; this.disks.cancel_navigation(); this.filter_status=format!("Filter failed: {error}. Previous applied view remains available."); },
                    }
                }
                if let Some((epoch,config))=this.filter_pending.take() {this.start_filter(epoch,config,cx);}
                cx.notify();
            });
        }).detach();
    }
    fn reset_filters(&mut self, _: &ResetFilters, window: &mut Window, cx: &mut Context<Self>) {
        if self.changes.visible {
            self.status =
                "Return to Explorer to change the captured comparison filters or metric.".into();
            cx.notify();
            return;
        }
        for input in &self.filter_inputs {
            input.update(cx, |input, cx| input.set_text("", window, cx));
        }
        self.filter_config = space_store::QueryFilter::default();
        self.draft_unknown_size = false;
        self.draft_unknown_time = false;
        self.filter_error = None;
        self.disks.invalidate_page_order();
        self.rebuild_filters(cx);
    }
    fn unknown_size(&mut self, _: &ToggleUnknownSize, _: &mut Window, cx: &mut Context<Self>) {
        self.draft_unknown_size = !self.draft_unknown_size;
        self.debounce_filters(cx);
    }
    fn unknown_time(&mut self, _: &ToggleUnknownTime, _: &mut Window, cx: &mut Context<Self>) {
        self.draft_unknown_time = !self.draft_unknown_time;
        self.debounce_filters(cx);
    }
    fn show_treemap(&mut self, _: &ShowTreemap, _: &mut Window, cx: &mut Context<Self>) {
        self.view = View::Treemap;
        self.request_query(None, cx);
    }
    fn toggle_subtree(&mut self, _: &ToggleSubtree, _: &mut Window, cx: &mut Context<Self>) {
        self.subtree_only = !self.subtree_only;
        self.request_query(None, cx);
    }
    fn chart_event(&mut self, event: treemap::Event, _: &mut Window, cx: &mut Context<Self>) {
        match event {
            treemap::Event::Select(node) => {
                if !self.treemap.items.iter().any(|item| item.node == node) {
                    return;
                }
                self.clear_aliases();
                self.chart_selected = Some(node);
                if let Some(ix) = self.rows.iter().position(|r| r.entry.node == node) {
                    self.selected = ix;
                    self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
                }
                cx.notify();
            }
            treemap::Event::Open(node) => {
                if let Some(label) = chart_folder(&self.treemap, node) {
                    self.breadcrumbs.push((node, label));
                    self.request_query(None, cx);
                }
            }
            treemap::Event::OpenOther => {
                self.view = View::Folders;
                self.request_query(None, cx);
            }
        }
    }
    fn request_query(&mut self, cursor: Option<PageCursor>, cx: &mut Context<Self>) {
        if self.maintenance_busy {
            return;
        }
        self.remember_scope_page();
        self.disks.page_cursor = cursor;
        let (Some(snapshot), Some((parent, _))) = (&self.snapshot, self.breadcrumbs.last()) else {
            return;
        };
        self.query_generation += 1;
        let generation = self.query_generation;
        let query = Query {
            scan: snapshot.scan,
            parent: *parent,
            view: self.view,
            metric: self.applied_metric,
            cursor,
            subtree_only: self.subtree_only,
            filtered: self.filter_session.clone(),
        };
        self.rows.clear();
        self.row_paths.clear();
        self.clear_aliases();
        clear_chart(&mut self.treemap, &mut self.chart_selected);
        self.current_totals = None;
        self.selected = 0;
        self.next_page = None;
        if self.query_busy {
            if let Some(old) = self.pending_query.replace((generation, query)) {
                self.executor
                    .spawn(async move {
                        drop(old);
                    })
                    .detach();
            }
        } else {
            self.start_query(generation, query, cx);
        }
        cx.notify();
    }
    fn start_query(&mut self, generation: u64, query: Query, cx: &mut Context<Self>) {
        let (Some(backend), Some(pin)) = (self.backend.clone(), self.snapshot.clone()) else {
            self.executor
                .spawn(async move {
                    drop(query);
                })
                .detach();
            return;
        };
        self.query_busy = true;
        let scan = query.scan;
        let parent = query.parent;
        let task = cx.background_executor().spawn(async move {
            let _pin = pin;
            backend::query(&backend, query)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.query_busy = false;
                if this.query_generation == generation {
                    match result {
                        Ok(result) => {
                            this.current_totals = Some(result.current);
                            this.treemap = result
                                .summary
                                .map(|summary| treemap::Model {
                                    items: summary
                                        .page
                                        .entries
                                        .into_iter()
                                        .map(|row| {
                                            let total = if this.applied_metric == Metric::Logical {
                                                row.logical
                                            } else {
                                                row.allocated
                                            };
                                            treemap::Item {
                                                node: row.entry.node,
                                                label: row.entry.display_name.clone(),
                                                detail: format!(
                                                    "{} · {}",
                                                    row.entry.display_name,
                                                    total_text(&row, this.applied_metric)
                                                ),
                                                bytes: total.observed_bytes,
                                                unknown_count: total.unknown_count,
                                                is_directory: row.entry.kind
                                                    == EntryKind::Directory,
                                            }
                                        })
                                        .collect(),
                                    other_bytes: u128::from(summary.other_bytes),
                                    other_count: summary.other_count,
                                    unknown_count: summary.unknown_count,
                                    zero_count: summary.zero_count,
                                })
                                .unwrap_or_default();
                            this.row_paths = result.display_paths;
                            this.rows = result.page.entries;
                            this.next_page = result.page.next;
                            this.scroll.scroll_to_item(0, ScrollStrategy::Top);
                            this.restore_query_selection(scan, parent);
                        }
                        Err(error) => this.status = format!("Cached query failed: {error}"),
                    }
                }
                if let Some((generation, query)) = this.pending_query.take() {
                    this.start_query(generation, query, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn folders(&mut self, _: &Folders, _: &mut Window, cx: &mut Context<Self>) {
        self.view = View::Folders;
        self.request_query(None, cx);
    }
    fn largest(&mut self, _: &Largest, _: &mut Window, cx: &mut Context<Self>) {
        self.view = View::Largest;
        self.request_query(None, cx);
    }
    fn metric(&mut self, _: &ToggleMetric, _: &mut Window, cx: &mut Context<Self>) {
        if self.changes.visible {
            self.status =
                "Return to Explorer to change the captured comparison filters or metric.".into();
            cx.notify();
            return;
        }
        self.disks.invalidate_page_order();
        self.metric = if self.metric == Metric::Logical {
            Metric::Allocated
        } else {
            Metric::Logical
        };
        self.rebuild_filters(cx);
        self.queue_visible_previews(cx);
    }
    fn next_page(&mut self, _: &NextPage, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(cursor) = self.next_page {
            if self.view == View::Treemap {
                self.view = View::Folders;
            }
            self.request_query(Some(cursor), cx);
        }
    }
    fn first_page(&mut self, _: &FirstPage, _: &mut Window, cx: &mut Context<Self>) {
        self.request_query(None, cx);
    }
    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.clear_aliases();
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(self.rows.len().saturating_sub(1));
        self.chart_selected = self.rows.get(self.selected).map(|r| r.entry.node);
        self.scroll
            .scroll_to_item(self.selected, ScrollStrategy::Nearest);
        cx.notify();
    }
    fn open_row(&mut self, _: &OpenRow, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(row) = self.rows.get(self.selected)
            && row.entry.kind == EntryKind::Directory
        {
            self.breadcrumbs
                .push((row.entry.node, row.entry.display_name.clone()));
            if self.view == View::Largest {
                self.view = View::Treemap;
            }
            self.request_query(None, cx);
        }
    }
    fn up(&mut self, _: &GoUp, _: &mut Window, cx: &mut Context<Self>) {
        if self.breadcrumbs.len() > 1 {
            let exited = self.breadcrumbs.pop().unwrap().0;
            let parent = self.breadcrumbs.last().unwrap().0;
            if let Some(snapshot) = &self.snapshot {
                self.disks.restore_selection = Some((snapshot.scan, exited));
            }
            if self.view == View::Largest {
                self.view = View::Treemap;
            }
            self.request_query(self.saved_scope_page(parent), cx);
        }
    }
    fn path_action(&mut self, reveal: bool, cx: &mut Context<Self>) {
        if self.maintenance_busy || self.intent.path_busy {
            return;
        }
        let (Some(backend), Some(snapshot)) = (self.backend.clone(), self.snapshot.clone()) else {
            return;
        };
        let node = if let Some(node) = self.alias_selected {
            node
        } else if self.view == View::Treemap {
            self.chart_selected
                .unwrap_or(snapshot.root_entry.entry.node)
        } else {
            self.rows
                .get(self.selected)
                .map_or(snapshot.root_entry.entry.node, |r| r.entry.node)
        };
        self.intent.path_busy = true;
        let scan = snapshot.scan;
        let task = cx.background_executor().spawn(async move {
            if reveal {
                backend::validated_reveal_path(&backend, &snapshot, node)
            } else {
                backend::native_path(&backend, &snapshot, node)
            }
        });
        cx.spawn(async move |this,cx| { let result=task.await; let _=this.update(cx,|this,cx|{
            if !this.intent.complete_path(this.snapshot.as_ref().is_some_and(|s|s.scan==scan)) {cx.notify(); return;}
            this.status=match result {
                Ok(path) if reveal=>{cx.reveal_path(&path);"Root identity verified; reveal requested from the OS. Verify the native file manager result.".into()},
                Ok(path)=>match path.to_str(){Some(text)=>{cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));"Cached native path copied. Current existence is not implied.".into()},None=>"Native path cannot be represented losslessly as clipboard text; nothing copied.".into()},
                Err(error)=>error,
            };cx.notify();
        });}).detach();
    }
}

impl Render for Space {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let screen = self.render_screen(window, cx);
        div()
            .id("space")
            .role(Role::Application)
            .aria_label("Space disk explorer")
            .track_focus(&self.focus)
            .key_context("Space")
            .on_action(cx.listener(Self::choose_folder))
            .on_action(cx.listener(Self::refresh))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::show_changes))
            .on_action(cx.listener(Self::show_aliases))
            .on_action(cx.listener(Self::next_aliases))
            .on_action(cx.listener(Self::show_cache))
            .on_action(cx.listener(Self::refresh_volumes))
            .on_action(cx.listener(Self::next_volumes))
            .on_action(cx.listener(Self::show_treemap))
            .on_action(cx.listener(Self::toggle_subtree))
            .on_action(cx.listener(Self::toggle_filters))
            .on_action(cx.listener(Self::reset_filters))
            .on_action(cx.listener(Self::apply_filters))
            .on_action(cx.listener(Self::cancel_filter))
            .on_action(cx.listener(Self::unknown_size))
            .on_action(cx.listener(Self::unknown_time))
            .on_action(cx.listener(Self::folders))
            .on_action(cx.listener(Self::largest))
            .on_action(cx.listener(Self::metric))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::next_page))
            .on_action(cx.listener(Self::first_page))
            .on_action(cx.listener(|this, _: &CopyPath, _, cx| this.path_action(false, cx)))
            .on_action(cx.listener(|this, _: &RevealPath, _, cx| this.path_action(true, cx)))
            .on_action(cx.listener(|_, _: &FocusNext, w, cx| w.focus_next(cx)))
            .on_action(cx.listener(|_, _: &FocusPrevious, w, cx| w.focus_prev(cx)))
            .on_action(|_: &CloseWindow, w, _| w.remove_window())
            .size_full()
            .overflow_hidden()
            .flex()
            .flex_col()
            .bg(rgb(0x12212c))
            .text_color(rgb(0xe6edf3))
            .child(screen)
    }
}

fn main() {
    if std::env::args().any(|arg| arg == "--version") {
        println!("Space {} (development)", env!("CARGO_PKG_VERSION"));
        return;
    }
    #[cfg(target_os = "linux")]
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("Space requires an X11 or Wayland desktop.");
        std::process::exit(1);
    }
    gpui_platform::application().run(|cx: &mut App| {
        text_input::init(cx);
        cx.bind_keys([
            KeyBinding::new("ctrl-o", ChooseFolder, Some("Space")),
            KeyBinding::new("ctrl-r", Refresh, Some("Space")),
            KeyBinding::new("escape", Cancel, Some("Space")),
            KeyBinding::new("ctrl-shift-c", CopyPath, Some("Space")),
            KeyBinding::new("ctrl-shift-r", RevealPath, Some("Space")),
            KeyBinding::new("alt-up", GoUp, Some("Space")),
            KeyBinding::new("down", NextRow, Some("SpaceList")),
            KeyBinding::new("up", PreviousRow, Some("SpaceList")),
            KeyBinding::new("enter", OpenRow, Some("SpaceList")),
            KeyBinding::new("tab", FocusNext, Some("Space")),
            KeyBinding::new("shift-tab", FocusPrevious, Some("Space")),
            KeyBinding::new("ctrl-w", CloseWindow, Some("Space")),
            KeyBinding::new("cmd-w", CloseWindow, Some("Space")),
        ]);
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
        if let Err(error) = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("Space".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |w, cx| cx.new(|cx| Space::new(w, cx)),
        ) {
            eprintln!("Unable to open Space: {error}");
            cx.quit();
            return;
        }
        cx.activate(true);
    });
}

#[cfg(test)]
mod presentation_tests {
    use super::*;
    #[test]
    fn stale_alias_click_cannot_target_another_scan_or_selection() {
        let node = NodeId::try_from(1).unwrap();
        let a = space_core::ScanId::try_from(1).unwrap();
        let b = space_core::ScanId::try_from(2).unwrap();
        let rows = [(node, "alias".into())];
        assert!(alias_selection_is_current(
            (1, Some(a)),
            (1, Some(a)),
            node,
            &rows
        ));
        assert!(!alias_selection_is_current(
            (1, Some(a)),
            (2, Some(a)),
            node,
            &rows
        ));
        assert!(!alias_selection_is_current(
            (1, Some(a)),
            (1, Some(b)),
            node,
            &rows
        ));
        assert!(!alias_selection_is_current(
            (1, Some(a)),
            (1, Some(a)),
            node,
            &[]
        ));
    }

    #[test]
    fn empty_reload_clears_applied_counts_pages_and_selection() {
        let mut next = Some(PageCursor {
            ordinal: 42,
            value: 100,
        });
        let mut selected = 9;
        let mut filter = space_store::QueryFilter {
            name: Some("old".into()),
            ..Default::default()
        };
        let mut status = "Filtered: 99 matches, 12345 bytes".into();
        reset_empty_presentation(&mut next, &mut selected, &mut filter, &mut status);
        assert!(next.is_none());
        assert_eq!(selected, 0);
        assert!(filter.is_empty());
        assert_eq!(status, "No cached scan is displayed.");
    }
    #[test]
    fn old_sibling_chart_event_is_rejected_while_next_scope_loads() {
        let a = NodeId::try_from(1).unwrap();
        let b = NodeId::try_from(2).unwrap();
        let mut model = treemap::Model {
            items: [(a, "A"), (b, "B")]
                .into_iter()
                .map(|(node, label)| treemap::Item {
                    node,
                    label: label.into(),
                    detail: String::new(),
                    bytes: 1,
                    unknown_count: 0,
                    is_directory: true,
                })
                .collect(),
            ..Default::default()
        };
        assert_eq!(chart_folder(&model, a).as_deref(), Some("A"));
        let mut selected = Some(a);
        clear_chart(&mut model, &mut selected);
        assert!(chart_folder(&model, b).is_none());
        assert!(model.items.is_empty());
        assert!(selected.is_none());
    }
}
