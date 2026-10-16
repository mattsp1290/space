//! Asynchronous Changes controller. SQLite and live path work stay on workers.
use super::*;
use backend::comparison as service;
use changes_panel as panel;
use space_core::{CoverageSummary, MetricChange, MetricTotal, ScanId, ScanSpec, Timestamp};
use space_store::{
    ComparisonCursor, ComparisonEndpoint, ComparisonEntry, ComparisonNodeId, ComparisonProgress,
};

type Session = deferred::Deferred<service::Compared>;
pub(super) struct State {
    pub visible: bool,
    pub model: panel::Model,
    pub session: Option<Session>,
    rows: Vec<ComparisonEntry>,
    crumbs: Vec<(ComparisonNodeId, String)>,
    next: Option<ComparisonCursor>,
    pub build_busy: bool,
    pub query_busy: bool,
    pub candidates_busy: bool,
    request: u64,
    cancel: Option<Cancellation>,
    progress: Option<ComparisonProgress>,
    metric: Metric,
    filter: space_store::QueryFilter,
    executor: gpui::BackgroundExecutor,
}
impl State {
    pub fn new(executor: gpui::BackgroundExecutor) -> Self {
        Self {
            visible: false,
            model: panel::Model::default(),
            session: None,
            rows: vec![],
            crumbs: vec![],
            next: None,
            build_busy: false,
            query_busy: false,
            candidates_busy: false,
            request: 0,
            cancel: None,
            progress: None,
            metric: Metric::Logical,
            filter: Default::default(),
            executor,
        }
    }
    pub fn idle(&self) -> bool {
        !self.build_busy && !self.query_busy && !self.candidates_busy
    }
    pub fn invalidate(&mut self) {
        self.model.epoch = self.model.epoch.wrapping_add(1);
        self.visible = false;
        self.model = panel::Model {
            epoch: self.model.epoch,
            ..Default::default()
        };
        self.model.comparison = None;
        self.model.building = None;
        self.model.rows.clear();
        self.rows.clear();
        self.crumbs.clear();
        self.model.breadcrumbs.clear();
        self.next = None;
        self.model.next = None;
        self.session = None;
        if let Some(cancel) = self.cancel.take() {
            self.executor
                .spawn(async move {
                    cancel.cancel();
                })
                .detach();
        }
    }
}
impl Drop for State {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            self.executor
                .spawn(async move {
                    cancel.cancel();
                })
                .detach();
        }
    }
}
fn time(t: Timestamp) -> String {
    chrono::DateTime::from_timestamp(t.seconds, t.nanos)
        .map(|t| t.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| "unavailable".into())
}
fn coverage(c: Option<&CoverageSummary>) -> String {
    c.map(|c| {
        format!(
            "{:?}; {} errors, {} unknown entries, {} excluded scopes, {} boundaries",
            c, c.error_count, c.unknown_entries, c.excluded_scopes, c.boundary_scopes
        )
    })
    .unwrap_or_else(|| "Coverage unavailable".into())
}
fn observation(
    scan: ScanId,
    spec: &ScanSpec,
    path: &space_core::NativePath,
    interval: (Timestamp, Option<Timestamp>),
    cov: Option<&CoverageSummary>,
    metric: Metric,
) -> panel::Observation {
    let policy = &spec.policy;
    panel::Observation {
        scan,
        root: path
            .to_path_buf()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "Foreign native path".into()),
        interval: format!(
            "{} → {}",
            time(interval.0),
            interval.1.map(time).unwrap_or_else(|| "unfinished".into())
        ),
        lineage: spec.lineage_id.get().to_string(),
        policy: format!(
            "v{}; {} path exclusions; {} identity exclusions; nested mounts excluded {}; virtual excluded {}; network excluded {}",
            policy.version,
            policy.exclusions.len(),
            policy.excluded_directory_identities.len(),
            policy.exclude_nested_mounts,
            policy.exclude_virtual_filesystems,
            policy.exclude_network_filesystems
        ),
        metric: format!("{metric:?}"),
        coverage: coverage(cov),
    }
}
fn snapshot_observation(snapshot: &Snapshot, metric: Metric) -> panel::Observation {
    observation(
        snapshot.scan,
        &snapshot.spec,
        &snapshot.path,
        snapshot.interval,
        Some(&snapshot.root_entry.coverage),
        metric,
    )
}
fn amount(value: MetricTotal) -> String {
    format!(
        "{} observed B; {} unknown; {} estimated",
        value.observed_bytes, value.unknown_count, value.estimated_count
    )
}
fn change(entry: &ComparisonEntry, metric: Metric) -> MetricChange {
    if metric == Metric::Logical {
        entry.logical
    } else {
        entry.allocated
    }
}
fn amounts(delta: MetricChange) -> panel::Amounts {
    panel::Amounts {
        before: amount(delta.before),
        after: amount(delta.after),
        net: delta
            .net
            .map(|n| format!("{n:+} B"))
            .unwrap_or_else(|| "Unknown / accounting redistribution".into()),
        gross_added: format!("{} known B", delta.gross_increase),
        gross_removed: format!("{} known B", delta.gross_decrease),
    }
}
fn row_id(id: ComparisonNodeId) -> panel::RowId {
    panel::RowId(id.get() as u64)
}
fn label(entry: &ComparisonEntry) -> String {
    entry
        .after
        .as_ref()
        .or(entry.before.as_ref())
        .map(|e| e.entry.display_name.clone())
        .unwrap_or_else(|| "Unknown path".into())
}
fn is_directory(entry: &ComparisonEntry) -> bool {
    entry
        .before
        .as_ref()
        .is_some_and(|e| e.entry.kind == EntryKind::Directory)
        || entry
            .after
            .as_ref()
            .is_some_and(|e| e.entry.kind == EntryKind::Directory)
}
fn row(entry: &ComparisonEntry, metric: Metric) -> panel::Row {
    let delta = change(entry, metric);
    let mut values = amounts(delta);
    let raw = |entry: &Option<StoredEntry>| {
        entry
            .as_ref()
            .map(|e| {
                amount(if metric == Metric::Logical {
                    e.logical
                } else {
                    e.allocated
                })
            })
            .unwrap_or_else(|| "Not observed at this endpoint".into())
    };
    values.before = raw(&entry.before);
    values.after = raw(&entry.after);
    let endpoint = |e: &Option<StoredEntry>| {
        e.as_ref().map(|e| panel::Endpoint {
            path: format!(
                "Cached native path for {} (resolved only on request)",
                e.entry.display_name
            ),
            can_copy: true,
            can_reveal: true,
        })
    };
    panel::Row {
        id: row_id(entry.id),
        name: label(entry),
        kind: if is_directory(entry) {
            "Directory"
        } else {
            "Entry"
        }
        .into(),
        amounts: values,
        status: format!(
            "{:?}; accounting {:?}; {} unknown changes",
            entry.change, delta.accounting, delta.unknown_changes
        ),
        coverage: format!(
            "Before {:?}; after {:?}. Unknown scopes never imply deletion.",
            entry.before_presence, entry.after_presence
        ),
        before: endpoint(&entry.before),
        after: endpoint(&entry.after),
        can_open: is_directory(entry),
        details: vec![
            format!(
                "{} matching paths; {} changed paths",
                entry.matching_paths, entry.changed_paths
            ),
            format!(
                "Comparable contributions: {} → {}",
                amount(delta.before),
                amount(delta.after)
            ),
            format!(
                "Before coverage: {}",
                coverage(entry.before.as_ref().map(|e| &e.coverage))
            ),
            format!(
                "After coverage: {}",
                coverage(entry.after.as_ref().map(|e| &e.coverage))
            ),
        ],
    }
}
fn ranking(view: panel::View) -> service::Ranking {
    match view {
        panel::View::Hierarchy => service::Ranking::Hierarchy,
        panel::View::LargestIncreases => service::Ranking::Increases,
        panel::View::LargestDecreases => service::Ranking::Decreases,
        panel::View::GrowingDirectories => service::Ranking::GrowingDirectories,
    }
}
impl Space {
    pub(super) fn show_changes(&mut self, _: &ShowChanges, _: &mut Window, cx: &mut Context<Self>) {
        if self.maintenance_busy {
            return;
        }
        if self.changes.visible {
            self.changes.invalidate();
            cx.notify();
            return;
        }
        let Some(snapshot) = self.snapshot.as_ref() else {
            self.status = "Open a published scan before comparing.".into();
            cx.notify();
            return;
        };
        if self.filter_busy || !self.changes.idle() {
            self.status = "Wait for current filter/comparison work to finish.".into();
            cx.notify();
            return;
        }
        self.changes.visible = true;
        self.changes.metric = self.applied_metric;
        self.changes.filter = self.applied_filter.clone();
        self.changes.model.current = Some(snapshot_observation(snapshot, self.applied_metric));
        self.changes.model.filter = format!("{:?}", self.applied_filter);
        self.changes.model.notice=Some("Captured explorer filters and metric. Return to Explorer to edit them, then compare again. Renames appear as removed plus added.".into());
        self.load_baselines(None, cx);
    }
    fn load_baselines(&mut self, before: Option<ScanId>, cx: &mut Context<Self>) {
        if !self.changes.idle() || self.maintenance_busy {
            return;
        }
        let (Some(backend), Some(snapshot)) = (self.backend.clone(), self.snapshot.clone()) else {
            return;
        };
        self.changes.candidates_busy = true;
        self.changes.model.busy = true;
        self.changes.model.epoch += 1;
        let epoch = self.changes.model.epoch;
        let metric = self.changes.metric;
        let task = cx
            .background_executor()
            .spawn(async move { service::candidates(&backend, &snapshot, before) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.changes.candidates_busy = false;
                if this.changes.model.epoch == epoch {
                    this.changes.model.busy = false;
                    match result {
                        Ok(page) => {
                            this.changes.model.baseline_next = page.next;
                            this.changes.model.baselines = page
                                .entries
                                .into_iter()
                                .map(|candidate| panel::Baseline {
                                    observation: observation(
                                        candidate.scan.spec.key.id,
                                        &candidate.scan.spec,
                                        &candidate.path,
                                        (candidate.scan.started, candidate.scan.ended),
                                        candidate.scan.coverage.as_ref(),
                                        metric,
                                    ),
                                    unavailable_reason: candidate.unavailable,
                                })
                                .collect();
                            this.changes.model.selected_baseline = this
                                .changes
                                .model
                                .baselines
                                .iter()
                                .find(|b| b.unavailable_reason.is_none())
                                .map(|b| b.observation.scan);
                            this.changes.model.error = None;
                        }
                        Err(error) => this.changes.model.error = Some(error),
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    pub(super) fn changes_event(
        &mut self,
        event: panel::Event,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.maintenance_busy || !self.changes.model.allows(event) {
            return;
        }
        match event.action {
            panel::Action::CloseRequested => self.changes.invalidate(),
            panel::Action::CancelRequested { .. } => {
                self.changes.model.stop_build_pending_ack();
                if let Some(cancel) = self.changes.cancel.take() {
                    cx.background_executor()
                        .spawn(async move {
                            cancel.cancel();
                        })
                        .detach();
                }
                self.changes.model.notice =
                    Some("Stopping comparison. Previous result remains available.".into());
            }
            panel::Action::BaselineSelected(scan) => {
                self.changes.model.selected_baseline = Some(scan);
                self.changes.model.epoch += 1;
            }
            panel::Action::BaselineFirstPage => self.load_baselines(None, cx),
            panel::Action::BaselineNextPage(scan) => self.load_baselines(Some(scan), cx),
            panel::Action::CompareRequested { before, after } => {
                self.start_comparison(before, after, cx)
            }
            panel::Action::RowSelected { row, .. } => self.changes.model.selected = Some(row),
            panel::Action::Open { row, .. } => {
                if let Some(entry) = self.changes.rows.iter().find(|e| row_id(e.id) == row) {
                    if self
                        .changes
                        .crumbs
                        .last()
                        .is_some_and(|(id, _)| *id == entry.id)
                    {
                        return;
                    }
                    self.changes.crumbs.push((entry.id, label(entry)));
                    self.query_comparison(None, cx);
                }
            }
            panel::Action::Breadcrumb { row, .. } => {
                if let Some(index) = self
                    .changes
                    .crumbs
                    .iter()
                    .position(|(id, _)| row_id(*id) == row)
                {
                    self.changes.crumbs.truncate(index + 1);
                    self.query_comparison(None, cx);
                }
            }
            panel::Action::UpRequested { .. } => {
                self.changes.crumbs.pop();
                self.query_comparison(None, cx);
            }
            panel::Action::FirstPage { .. } => self.query_comparison(None, cx),
            panel::Action::NextPage { .. } => self.query_comparison(self.changes.next, cx),
            panel::Action::ShowView { view, .. } => {
                self.changes.model.view = view;
                self.query_comparison(None, cx);
            }
            panel::Action::CopyRequested { row, side, .. } => {
                self.comparison_path(row, side, false, cx)
            }
            panel::Action::RevealRequested { row, side, .. } => {
                self.comparison_path(row, side, true, cx)
            }
        }
        cx.notify();
    }
    fn start_comparison(&mut self, before: ScanId, after: ScanId, cx: &mut Context<Self>) {
        if !self.changes.idle()
            || self.filter_busy
            || self.snapshot.as_ref().is_none_or(|s| s.scan != after)
        {
            return;
        }
        let Some(backend) = self.backend.clone() else {
            return;
        };
        self.changes.model.epoch += 1;
        let epoch = self.changes.model.epoch;
        self.changes.request += 1;
        let request = self.changes.request;
        self.changes.build_busy = true;
        self.changes.model.building = Some(panel::Build {
            request,
            before,
            after,
        });
        self.changes.model.error = None;
        let cancel = Cancellation::new();
        let progress = ComparisonProgress::default();
        self.changes.cancel = Some(cancel.clone());
        self.changes.progress = Some(progress.clone());
        let metric = self.changes.metric;
        let filter = self.changes.filter.clone();
        let executor = cx.background_executor().clone();
        let task = cx.background_executor().spawn(async move {
            service::build(&backend, before, after, metric, &filter, &cancel, &progress).map(
                |value| {
                    Session::new(value, move |value| {
                        executor
                            .spawn(async move {
                                drop(value);
                            })
                            .detach();
                    })
                },
            )
        });
        cx.spawn(async move|this,cx|{let result=task.await;let _=this.update(cx,|this,cx|{
            if this.changes.request!=request{return;}
            this.changes.build_busy=false;this.changes.cancel=None;this.changes.progress=None;
            if this.changes.model.epoch!=epoch {this.changes.model.busy=false;cx.notify();return;}
            this.changes.model.building=None;
            match result {Ok(session)=>{
                this.changes.model.comparison=Some(panel::Comparison{id:request,before:snapshot_observation(&session.before,metric),after:snapshot_observation(&session.after,metric),amounts:amounts(change(&session.root,metric)),coverage:format!("{} matching paths; {} changed paths; {} unknown contributions",session.root.matching_paths,session.root.changed_paths,change(&session.root,metric).unknown_changes),filter:format!("{:?}",session.filter)});
                this.changes.crumbs=vec![(session.root.id,label(&session.root))];this.changes.session=Some(session);this.changes.model.view=panel::View::Hierarchy;this.changes.model.notice=None;this.query_comparison(None,cx);
            },Err(error)=>this.changes.model.error=Some(format!("Comparison unavailable: {error}. Previous displayed result is retained."))}cx.notify();
        });}).detach();
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            loop {
                executor.timer(Duration::from_millis(200)).await;
                if !this
                    .update(cx, |this, cx| {
                        if this.changes.request != request
                            || !this.changes.build_busy
                            || this.changes.model.building.is_none()
                        {
                            return false;
                        }
                        if let Some(progress) = &this.changes.progress {
                            let p = progress.snapshot();
                            this.changes.model.notice = Some(if p.total == 0 {
                                format!(
                                    "Computing comparison: {:?}. Previous views remain available.",
                                    p.phase
                                )
                            } else {
                                format!(
                                    "Computing comparison: {:?}; {} / {} items in this phase.",
                                    p.phase, p.processed, p.total
                                )
                            });
                        }
                        cx.notify();
                        true
                    })
                    .unwrap_or(false)
                {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }
    fn query_comparison(&mut self, cursor: Option<ComparisonCursor>, cx: &mut Context<Self>) {
        if self.changes.query_busy || self.changes.build_busy || self.maintenance_busy {
            return;
        }
        let (Some(session), Some((parent, _))) =
            (self.changes.session.clone(), self.changes.crumbs.last())
        else {
            return;
        };
        let parent = *parent;
        let view = ranking(self.changes.model.view);
        self.changes.model.epoch += 1;
        let epoch = self.changes.model.epoch;
        self.changes.query_busy = true;
        self.changes.model.busy = true;
        self.changes.model.rows.clear();
        self.changes.rows.clear();
        self.changes.model.selected = None;
        self.changes.next = None;
        self.changes.model.next = None;
        self.changes.model.breadcrumbs = self
            .changes
            .crumbs
            .iter()
            .map(|(id, label)| panel::Breadcrumb {
                id: row_id(*id),
                label: label.clone(),
            })
            .collect();
        let task = cx
            .background_executor()
            .spawn(async move { service::query(&session, parent, view, cursor) });
        cx.spawn(async move|this,cx|{let result=task.await;let _=this.update(cx,|this,cx|{
            this.changes.query_busy=false;
            if this.changes.model.epoch==epoch {this.changes.model.busy=false;match result {Ok(page)=>{this.changes.crumbs=page.ancestors;this.changes.model.breadcrumbs=this.changes.crumbs.iter().map(|(id,label)|panel::Breadcrumb{id:row_id(*id),label:label.clone()}).collect();this.changes.model.rows=page.page.entries.iter().map(|e|row(e,this.changes.metric)).collect();this.changes.rows=page.page.entries;this.changes.next=page.page.next;this.changes.model.next=this.changes.next.map(|_|epoch);this.changes.model.selected=this.changes.model.rows.first().map(|r|r.id);this.changes.model.error=None;this.changes.model.notice=Some(format!("Current scope: {} matching paths; {} changed paths. Ranked ancestors and descendants overlap; do not sum ranking rows.",page.current.matching_paths,page.current.changed_paths));},Err(error)=>this.changes.model.error=Some(error)}}cx.notify();
        });}).detach();
        cx.notify();
    }
    fn comparison_path(
        &mut self,
        row: panel::RowId,
        side: panel::Side,
        reveal: bool,
        cx: &mut Context<Self>,
    ) {
        if self.intent.path_busy {
            return;
        }
        let (Some(backend), Some(session), Some(entry)) = (
            self.backend.clone(),
            self.changes.session.clone(),
            self.changes.rows.iter().find(|e| row_id(e.id) == row),
        ) else {
            return;
        };
        let id = entry.id;
        let epoch = self.changes.model.epoch;
        let endpoint = match side {
            panel::Side::Before => ComparisonEndpoint::Before,
            panel::Side::After => ComparisonEndpoint::After,
        };
        self.intent.path_busy = true;
        self.changes.model.busy = true;
        let task = cx
            .background_executor()
            .spawn(async move { service::path(&backend, &session, id, endpoint, reveal) });
        cx.spawn(async move|this,cx|{let result=task.await;let _=this.update(cx,|this,cx|{
            this.intent.path_busy=false;
            if this.changes.model.epoch!=epoch {cx.notify();return;}
            this.changes.model.busy=false;this.changes.model.notice=Some(match result {Ok(path) if reveal=>{cx.reveal_path(&path);"Selected endpoint root identity verified; OS reveal requested.".into()},Ok(path)=>match path.to_str(){Some(text)=>{cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));"Selected endpoint native path copied; live existence is not implied.".into()},None=>"Native path is not losslessly representable as clipboard text; nothing copied.".into()},Err(error)=>error});cx.notify();
        });}).detach();
        cx.notify();
    }
}
