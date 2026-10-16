mod backend;
mod intent;
mod paths;

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
        ToggleMetric,
        NextPage,
        FirstPage,
        NextSavedRoot,
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

struct Space {
    focus: FocusHandle,
    list_focus: FocusHandle,
    scroll: UniformListScrollHandle,
    backend: Option<Backend>,
    cache_directory: Option<PathBuf>,
    snapshot: Option<Arc<Snapshot>>,
    roots: Vec<RootRecord>,
    intent: intent::Intent,
    picker_open: bool,
    running: Option<Running>,
    generation: u64,
    status: String,
    scan_status: String,
    view: View,
    metric: Metric,
    breadcrumbs: Vec<(NodeId, String)>,
    rows: Vec<StoredEntry>,
    selected: usize,
    next_page: Option<PageCursor>,
    query_generation: u64,
    query_busy: bool,
    pending_query: Option<(u64, Query)>,
    snapshot_request: u64,
    root_busy: bool,
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
    let mut text = format!("{} B", total.observed_bytes);
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
            focus,
            list_focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            backend: None,
            cache_directory: None,
            snapshot: None,
            roots: vec![],
            intent: intent::Intent::default(),
            picker_open: false,
            running: None,
            generation: 0,
            status: "Opening local cache…".into(),
            scan_status: "No scan running.".into(),
            view: View::Folders,
            metric: Metric::Logical,
            breadcrumbs: vec![],
            rows: vec![],
            selected: 0,
            next_page: None,
            query_generation: 0,
            query_busy: false,
            pending_query: None,
            snapshot_request: 0,
            root_busy: false,
        };
        app.initialize(cx);
        app
    }
    fn initialize(&mut self, cx: &mut Context<Self>) {
        let directory = match paths::cache_directory() {
            Ok(path) => path,
            Err(error) => {
                self.status = error;
                return;
            }
        };
        self.cache_directory = Some(directory.clone());
        let task = cx
            .background_executor()
            .spawn(async move { backend::open(&directory) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(opened) => {
                        this.backend = Some(opened.backend); this.roots = opened.roots;
                        if let Some(snapshot) = opened.snapshot { this.apply_snapshot(snapshot, cx); }
                        else { this.status = "Choose a folder, then Scan / Refresh. No source drive is scanned automatically.".into(); }
                    }
                    Err(error) => this.status = format!("Cache unavailable: {error}. Original data was not replaced."),
                } cx.notify();
            });
        }).detach();
    }
    fn apply_snapshot(&mut self, snapshot: Snapshot, cx: &mut Context<Self>) {
        self.snapshot_request += 1;
        self.breadcrumbs = vec![(
            snapshot.root_entry.entry.node,
            snapshot.root_entry.entry.display_name.clone(),
        )];
        self.snapshot = Some(Arc::new(snapshot));
        self.status = "Showing an immutable cached scan. Live root has not been revalidated; offline browsing is available.".into();
        self.request_query(None, cx);
    }
    fn choose_folder(&mut self, _: &ChooseFolder, _: &mut Window, cx: &mut Context<Self>) {
        if self.picker_open {
            return;
        }
        self.picker_open = true;
        let result = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose folder to scan".into()),
        });
        cx.spawn(async move |this, cx| {
            let result = result.await;
            let _ = this.update(cx, |this, cx| {
                this.picker_open = false;
                match result {
                    Ok(Ok(Some(paths))) => if let Some(path) = paths.into_iter().next() {
                        this.intent.chosen = Some(path); this.status = "Folder selected. Press Scan / Refresh to scan; existing cached view stays visible.".into();
                    },
                    Ok(Ok(None)) => this.status = "Folder selection cancelled; previous selection retained.".into(),
                    Ok(Err(error)) => this.status = format!("Folder picker failed: {error}"),
                    Err(error) => this.status = format!("Folder picker closed unexpectedly: {error}"),
                } cx.notify();
            });
        }).detach();
        cx.notify();
    }
    fn refresh(&mut self, _: &Refresh, _: &mut Window, cx: &mut Context<Self>) {
        if self.running.is_some() {
            self.status = "A scan is still active. Cancel it and wait for writer shutdown before starting another.".into();
            cx.notify();
            return;
        }
        let (Some(backend), Some(cache_directory)) =
            (self.backend.clone(), self.cache_directory.clone())
        else {
            return;
        };
        let (path, refresh_root) = if let Some(path) = &self.intent.chosen {
            (path.clone(), None)
        } else if let Some(snapshot) = &self.snapshot {
            match snapshot.path.to_path_buf() {
                Ok(path) => (path, Some(snapshot.root.clone())),
                Err(error) => {
                    self.status = error.to_string();
                    cx.notify();
                    return;
                }
            }
        } else {
            self.status = "Choose a folder first.".into();
            cx.notify();
            return;
        };
        // Consume this request now; later picker choices survive asynchronous publication.
        self.intent.consume_choice();
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
        self.scan_status = format!(
            "Scanning {}. The displayed cache remains the previous published snapshot.",
            path.to_string_lossy()
        );
        let request = backend::ScanRequest {
            path,
            cache_directory,
            refresh_root,
            generation,
        };
        let task = cx
            .background_executor()
            .spawn(async move { backend::scan(&backend, request, &cancel, &progress) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.running.as_ref().is_none_or(|run|run.generation != generation) { return; }
                if let Some(run) = &mut this.running { run.armed = false; }
                this.running = None;
                match result {
                    Ok(Some(done)) => { this.roots = done.roots; this.scan_status = done.maintenance_warning.map(|warning|format!("Scan published; cache maintenance warning: {warning}")).unwrap_or_else(||"Scan published. Coverage and unknown bytes are shown with cached totals.".into()); this.apply_snapshot(done.snapshot, cx); }
                    Ok(None) => this.scan_status = "Scan cancelled. Previous published scan retained.".into(),
                    Err(error) => this.scan_status = format!("Scan stopped: {error}. The current cached view remains available; reopen the saved root to inspect the latest publication."),
                } cx.notify();
            });
        }).detach();
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            loop {
                executor.timer(Duration::from_millis(100)).await;
                if !this
                    .update(cx, |this, cx| {
                        let active = this
                            .running
                            .as_ref()
                            .is_some_and(|r| r.generation == generation);
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
        cx.notify();
    }
    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
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
            self.scan_status = "Cancellation requested. Waiting for the worker; a blocked OS call may take longer.".into();
        }
        cx.notify();
    }
    fn request_query(&mut self, cursor: Option<PageCursor>, cx: &mut Context<Self>) {
        let (Some(snapshot), Some((parent, _))) = (&self.snapshot, self.breadcrumbs.last()) else {
            return;
        };
        self.query_generation += 1;
        let generation = self.query_generation;
        let query = Query {
            scan: snapshot.scan,
            parent: *parent,
            view: self.view,
            metric: self.metric,
            cursor,
        };
        self.rows.clear();
        self.selected = 0;
        self.next_page = None;
        if self.query_busy {
            self.pending_query = Some((generation, query));
        } else {
            self.start_query(generation, query, cx);
        }
        cx.notify();
    }
    fn start_query(&mut self, generation: u64, query: Query, cx: &mut Context<Self>) {
        let (Some(backend), Some(pin)) = (self.backend.clone(), self.snapshot.clone()) else {
            return;
        };
        self.query_busy = true;
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
                        Ok(page) => {
                            this.rows = page.entries;
                            this.next_page = page.next;
                            this.scroll.scroll_to_item(0, ScrollStrategy::Top);
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
        self.metric = if self.metric == Metric::Logical {
            Metric::Allocated
        } else {
            Metric::Logical
        };
        self.request_query(None, cx);
    }
    fn next_page(&mut self, _: &NextPage, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(cursor) = self.next_page {
            self.request_query(Some(cursor), cx);
        }
    }
    fn first_page(&mut self, _: &FirstPage, _: &mut Window, cx: &mut Context<Self>) {
        self.request_query(None, cx);
    }
    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(self.rows.len().saturating_sub(1));
        self.scroll
            .scroll_to_item(self.selected, ScrollStrategy::Nearest);
        cx.notify();
    }
    fn open_row(&mut self, _: &OpenRow, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(row) = self.rows.get(self.selected) {
            if row.entry.kind == EntryKind::Directory {
                self.breadcrumbs
                    .push((row.entry.node, row.entry.display_name.clone()));
                self.view = View::Folders;
                self.request_query(None, cx);
            }
        }
    }
    fn up(&mut self, _: &GoUp, _: &mut Window, cx: &mut Context<Self>) {
        if self.breadcrumbs.len() > 1 {
            self.breadcrumbs.pop();
            self.view = View::Folders;
            self.request_query(None, cx);
        }
    }
    fn next_saved(&mut self, _: &NextSavedRoot, _: &mut Window, cx: &mut Context<Self>) {
        if self.root_busy || self.roots.is_empty() {
            return;
        }
        let index = self
            .snapshot
            .as_ref()
            .and_then(|s| self.roots.iter().position(|r| r.id == s.root.id))
            .map_or(0, |index| (index + 1) % self.roots.len());
        let Some(backend) = self.backend.clone() else {
            return;
        };
        self.root_busy = true;
        let root = self.roots[index].clone();
        self.snapshot_request += 1;
        let generation = self.snapshot_request;
        let task = cx
            .background_executor()
            .spawn(async move { backend::open_root(&backend, root) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.root_busy = false;
                if this.snapshot_request != generation {
                    return;
                }
                match result {
                    Ok(snapshot) => this.apply_snapshot(snapshot, cx),
                    Err(error) => this.status = error,
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn path_action(&mut self, reveal: bool, cx: &mut Context<Self>) {
        if self.intent.path_busy {
            return;
        }
        let (Some(backend), Some(snapshot)) = (self.backend.clone(), self.snapshot.clone()) else {
            return;
        };
        let node = self
            .rows
            .get(self.selected)
            .map_or(snapshot.root_entry.entry.node, |r| r.entry.node);
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let metric = if self.metric == Metric::Logical {
            "logical / apparent"
        } else {
            "allocated (unknown excluded from largest)"
        };
        let progress = self
            .running
            .as_ref()
            .map(|r| {
                format!(
                    "{} observed entries · {} stored batches",
                    r.progress.entries.load(Ordering::Relaxed),
                    r.progress.batches.load(Ordering::Relaxed)
                )
            })
            .unwrap_or_default();
        let snapshot_label=self.snapshot.as_ref().map(|s|format!("Cached scan {} · {} · {} · {} errors · {} unknown entries · {} excluded/boundary scopes",s.scan.get(),s.path.to_path_buf().map(|p|p.to_string_lossy().into_owned()).unwrap_or_else(|_|"Foreign native path".into()),total_text(&s.root_entry,self.metric),s.root_entry.coverage.error_count,s.root_entry.coverage.unknown_entries,s.root_entry.coverage.excluded_scopes+s.root_entry.coverage.boundary_scopes)).unwrap_or_else(||"No published scan yet.".into());
        let interval_label = self
            .snapshot
            .as_ref()
            .map(|snapshot| {
                let display = |time: space_core::Timestamp| {
                    chrono::DateTime::from_timestamp(time.seconds, time.nanos)
                        .map(|date| date.format("%Y-%m-%d %H:%M:%S UTC").to_string())
                        .unwrap_or_else(|| "timestamp unavailable".into())
                };
                format!(
                    "Observed {} → {} (an observation interval, not an atomic filesystem snapshot)",
                    display(snapshot.interval.0),
                    snapshot
                        .interval
                        .1
                        .map(display)
                        .unwrap_or_else(|| "unfinished".into())
                )
            })
            .unwrap_or_default();
        let chosen = self
            .intent
            .chosen
            .as_ref()
            .map(|p| format!("Next scan: {}", p.to_string_lossy()))
            .unwrap_or_default();
        let view = if self.view == View::Folders {
            "Folders: native-name order"
        } else {
            "Largest files: whole scan, charged unique sizes descending"
        };
        div().id("space").role(Role::Application).aria_label("Space disk explorer").track_focus(&self.focus).key_context("Space")
            .on_action(cx.listener(Self::choose_folder)).on_action(cx.listener(Self::refresh)).on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::folders)).on_action(cx.listener(Self::largest)).on_action(cx.listener(Self::metric))
            .on_action(cx.listener(Self::up)).on_action(cx.listener(Self::next_page)).on_action(cx.listener(Self::first_page)).on_action(cx.listener(Self::next_saved))
            .on_action(cx.listener(|this,_:&CopyPath,_,cx|this.path_action(false,cx)))
            .on_action(cx.listener(|this,_:&RevealPath,_,cx|this.path_action(true,cx)))
            .on_action(cx.listener(|_,_:&FocusNext,w,cx|w.focus_next(cx))).on_action(cx.listener(|_,_:&FocusPrevious,w,cx|w.focus_prev(cx)))
            .on_action(|_:&CloseWindow,w,_|w.remove_window())
            .size_full().flex().flex_col().gap_2().p_4().bg(rgb(0x17212b)).text_color(rgb(0xe6edf3))
            .child(div().text_xl().child("Space · local disk explorer"))
            .child(div().flex().flex_wrap().gap_2()
                .child(control("choose","Choose folder (Ctrl+O)").on_click(cx.listener(|t,_,w,cx|t.choose_folder(&ChooseFolder,w,cx))))
                .child(control("refresh","Scan / Refresh (Ctrl+R)").on_click(cx.listener(|t,_,w,cx|t.refresh(&Refresh,w,cx))))
                .child(control("cancel","Cancel (Esc)").on_click(cx.listener(|t,_,w,cx|t.cancel(&Cancel,w,cx))))
                .child(control("saved","Next saved root").on_click(cx.listener(|t,_,w,cx|t.next_saved(&NextSavedRoot,w,cx)))))
            .child(chosen).child(snapshot_label).child(interval_label).child(self.scan_status.clone()).child(progress)
            .child(div().flex().flex_wrap().gap_2()
                .child(control("folders","Folders").on_click(cx.listener(|t,_,w,cx|t.folders(&Folders,w,cx))))
                .child(control("largest","Largest files").on_click(cx.listener(|t,_,w,cx|t.largest(&Largest,w,cx))))
                .child(control("metric","Toggle size metric").on_click(cx.listener(|t,_,w,cx|t.metric(&ToggleMetric,w,cx))))
                .child(control("up","Up (Alt+Up)").on_click(cx.listener(|t,_,w,cx|t.up(&GoUp,w,cx))))
                .child(control("open","Open folder (Enter)").on_click(cx.listener(|t,_,w,cx|t.open_row(&OpenRow,w,cx)))))
            .child(format!("{view} · {metric}"))
            .child(format!("Scope: {}",if self.view == View::Largest {"whole saved root".into()} else {self.breadcrumbs.iter().map(|(_,name)|name.as_str()).collect::<Vec<_>>().join(" / ")}))
            .child(self.status.clone())
            .child(div().id("entries").role(Role::ListBox).aria_label("Cached entries").track_focus(&self.list_focus).tab_stop(true).key_context("SpaceList")
                .flex_1().min_h_0().border_2().border_color(rgb(0x285166)).focus(|s|s.border_color(rgb(0x90d5ff)))
                .on_action(cx.listener(|t,_:&NextRow,_,cx|t.move_selection(1,cx))).on_action(cx.listener(|t,_:&PreviousRow,_,cx|t.move_selection(-1,cx)))
                .on_action(cx.listener(Self::open_row))
                .child(uniform_list("rows",self.rows.len(),cx.processor(|this,range:std::ops::Range<usize>,_,cx|{
                    range.map(|ix|{let row=&this.rows[ix];let label=format!("{} {} {} · {}{}",if ix==this.selected {">"}else{" "},if row.entry.kind==EntryKind::Directory {"[folder]"}else{"[file]"},row.entry.display_name,total_text(row,this.metric),if !row.canonical {" · hardlink alias"}else{""});
                        div().id(ix).role(Role::ListBoxOption).aria_label(SharedString::from(label.clone())).aria_selected(ix==this.selected)
                            .aria_position_in_set(ix+1).aria_size_of_set(this.rows.len()).h(px(30.)).px_2().cursor_pointer()
                            .when(ix==this.selected,|e|e.bg(rgb(0x285166)).aria_active_descendant())
                            .on_click(cx.listener(move |t,_,w,cx|{w.focus(&t.list_focus,cx);t.selected=ix;cx.notify();})).child(label)
                    }).collect::<Vec<_>>()
                })).track_scroll(&self.scroll).h_full()))
            .child(if self.query_busy {"Loading cached page…"}else if self.rows.is_empty(){"No entries in this view."}else{""})
            .child(div().flex().flex_wrap().gap_2()
                .child(control("first","First page").on_click(cx.listener(|t,_,w,cx|t.first_page(&FirstPage,w,cx))))
                .child(control("next","Next page").on_click(cx.listener(|t,_,w,cx|t.next_page(&NextPage,w,cx))))
                .child(control("copy","Copy path (Ctrl+Shift+C)").on_click(cx.listener(|t,_,_,cx|t.path_action(false,cx))))
                .child(control("reveal","Reveal (Ctrl+Shift+R)").on_click(cx.listener(|t,_,_,cx|t.path_action(true,cx)))))
            .child("Metadata only. Totals describe observed included files, not reclaimable space. No deletion or content opening.")
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
        let bounds = Bounds::centered(None, size(px(1100.), px(820.)), cx);
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
