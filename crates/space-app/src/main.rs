mod backend;
mod dashboard_controller;
mod deferred;
mod disk_model;
mod display_totals;
mod explore;
mod explore_controller;
mod home;
mod intent;
#[cfg(test)]
mod journey_tests;
mod paths;
mod scan_batch;
#[cfg(test)]
mod test_fixture;
mod text_input;
mod theme;
mod ticket;
mod tree;
mod treemap_view;
use backend::{Backend, Query, Snapshot, View};
use display_totals::DisplayTotals;
use gpui::{
    App, Bounds, ClipboardItem, Context, FocusHandle, Focusable, KeyBinding, PathPromptOptions,
    Role, UniformListScrollHandle, Window, WindowBounds, WindowOptions, actions, div, prelude::*,
    px, rgb, size,
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
        Rescan,
        Cancel,
        CopyPath,
        RevealPath,
        NextRow,
        PreviousRow,
        OpenRow,
        GoUp,
        ZoomIn,
        ToggleList,
        ToggleMetric,
        NextPage,
        FirstPage,
        FocusSearch,
        FocusNext,
        FocusPrevious,
        CloseWindow
    ]
);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Screen {
    Home,
    Explore,
}
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
    screen: Screen,
    disks: dashboard_controller::State,
    explore: explore_controller::State,
    executor: gpui::BackgroundExecutor,
    backend: Option<deferred::Deferred<Backend>>,
    cache_directory: Option<PathBuf>,
    roots: Vec<RootRecord>,
    volumes: Option<space_scan::VolumeListing>,
    running: Option<Running>,
    generation: u64,
    metric: Metric,
    search_input: gpui::Entity<text_input::TextInput>,
    search_subscription: Option<gpui::Subscription>,
    picker_open: bool,
    intent: intent::Intent,
    focus: FocusHandle,
    list_focus: FocusHandle,
    scroll: UniformListScrollHandle,
    status: String,
}
impl Drop for Space {
    fn drop(&mut self) {
        let _ = self.disks.batch.reduce(scan_batch::Event::Shutdown);
        self.clear_search_workers();
        self.explore
            .latest_generation
            .fetch_add(1, Ordering::Release);
    }
}
fn metric_bytes(row: &StoredEntry, metric: Metric) -> u128 {
    u128::from(if metric == Metric::Logical {
        row.logical.observed_bytes
    } else {
        row.allocated.observed_bytes
    })
}
fn metric_label(metric: Metric) -> &'static str {
    if metric == Metric::Logical {
        "Logical"
    } else {
        "Allocated"
    }
}

impl Space {
    fn empty(cx: &mut Context<Self>) -> Self {
        let mut app = Self {
            screen: Screen::Home,
            disks: Default::default(),
            explore: Default::default(),
            executor: cx.background_executor().clone(),
            backend: None,
            cache_directory: None,
            roots: vec![],
            volumes: None,
            running: None,
            generation: 0,
            metric: Metric::Logical,
            search_input: cx.new(|cx| {
                text_input::TextInput::new("Search file names", "Search names in this disk", cx)
            }),
            search_subscription: None,
            picker_open: false,
            intent: Default::default(),
            focus: cx.focus_handle(),
            list_focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            status: String::new(),
        };
        app.search_subscription = Some(cx.subscribe(&app.search_input, |this, _, event, cx| {
            let text_input::TextInputEvent::Edited(text) = event;
            this.search_input_edited(text.clone(), cx);
        }));
        app
    }

    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut app = Self::empty(cx);
        app.disks.initializing = true;
        window.focus(&app.focus, cx);
        app.initialize(cx);
        app
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
                        this.discover_disks(None,cx);
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
}
impl Space {
    fn send(&mut self, action: explore::Action, window: &mut Window, cx: &mut Context<Self>) {
        let event = self.explore_model().event(action);
        self.explore_event(event, window, cx);
    }
    fn move_row(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let len = self.explore.list_rows.len();
        if len == 0 {
            return;
        }
        let index = self
            .explore
            .list_selected
            .map_or(0, |i| i.saturating_add_signed(delta).min(len - 1));
        self.send(explore::Action::ListSelect(index), window, cx);
        self.scroll
            .scroll_to_item(index, gpui::ScrollStrategy::Nearest);
    }
    fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use explore::Action;
        if self.screen == Screen::Home {
            self.stop_acquisition(cx);
            return;
        }
        let action = if self.explore.disk_menu_open {
            Action::ToggleDiskMenu
        } else if !self.search_input.read(cx).text().is_empty()
            && self.search_input.focus_handle(cx).is_focused(window)
        {
            Action::SearchClear
        } else if self.explore.selected.is_some() {
            Action::Select(None)
        } else {
            Action::Stop
        };
        self.send(action, window, cx);
    }
}
impl Render for Space {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.explore.search.notify_input) {
            window.notify_text_input_changed(&self.search_input.focus_handle(cx));
        }
        if self.screen == Screen::Explore && self.explore.focus_pending {
            if !self.search_input.focus_handle(cx).is_focused(window) {
                window.focus(&self.focus, cx);
            }
            self.explore.focus_pending = false;
        }
        let size = window.viewport_size();
        let width: f32 = size.width.into();
        let height: f32 = size.height.into();
        let screen = match self.screen {
            Screen::Home => home::render(&self.home_model(), width, height, cx, Self::home_event),
            Screen::Explore => {
                let model = self.explore_model();
                let l =
                    explore::layout(width, height, model.list_visible, model.new_scan_available);
                explore::render(
                    &model,
                    l,
                    explore::Controls {
                        search: &self.search_input,
                        map_focus: &self.focus,
                        list_focus: &self.list_focus,
                        scroll: &self.scroll,
                        geometry: self.explore.geometry.clone(),
                    },
                    cx,
                    Self::explore_event,
                )
            }
        };
        div()
            .id("space")
            .role(Role::Application)
            .aria_label("Space disk explorer")
            .key_context("Space")
            .when(self.screen == Screen::Home, |d| d.track_focus(&self.focus))
            .size_full()
            .overflow_hidden()
            .bg(rgb(theme::BG))
            .text_color(rgb(theme::TEXT))
            .text_size(px(theme::TEXT_BASE))
            .on_action(cx.listener(|this, _: &ChooseFolder, _, cx| this.scan_folder_picker(cx)))
            .on_action(cx.listener(|this, _: &Rescan, w, cx| {
                if this.screen == Screen::Explore {
                    this.send(explore::Action::Rescan, w, cx)
                } else {
                    this.home_event(home::Event::ScanAll, w, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &Cancel, w, cx| this.escape(w, cx)))
            .on_action(
                cx.listener(|this, _: &CopyPath, w, cx| this.send(explore::Action::Copy, w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &RevealPath, w, cx| {
                    this.send(explore::Action::Reveal, w, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &GoUp, w, cx| this.send(explore::Action::Up, w, cx)))
            .on_action(cx.listener(|this, _: &ZoomIn, w, cx| {
                if let Some(n) = this.explore.selected {
                    this.send(explore::Action::ZoomInto(n), w, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleList, w, cx| {
                this.send(explore::Action::ToggleList, w, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleMetric, w, cx| {
                this.send(explore::Action::ToggleMetric, w, cx)
            }))
            .on_action(
                cx.listener(|this, _: &NextPage, w, cx| {
                    this.send(explore::Action::NextPage, w, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &FirstPage, w, cx| {
                this.send(explore::Action::FirstPage, w, cx)
            }))
            .on_action(cx.listener(|this, _: &NextRow, w, cx| this.move_row(1, w, cx)))
            .on_action(cx.listener(|this, _: &PreviousRow, w, cx| this.move_row(-1, w, cx)))
            .on_action(cx.listener(|this, _: &OpenRow, w, cx| {
                if let Some(i) = this.explore.list_selected {
                    this.send(explore::Action::ListOpen(i), w, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &FocusSearch, w, cx| {
                if this.screen == Screen::Explore {
                    w.focus(&this.search_input.focus_handle(cx), cx);
                }
            }))
            .on_action(cx.listener(|_, _: &FocusNext, w, cx| w.focus_next(cx)))
            .on_action(cx.listener(|_, _: &FocusPrevious, w, cx| w.focus_prev(cx)))
            .on_action(|_: &CloseWindow, w, _| w.remove_window())
            .child(screen)
    }
}
#[cfg(target_os = "macos")]
const PRIMARY: &str = "cmd";
#[cfg(not(target_os = "macos"))]
const PRIMARY: &str = "ctrl";
fn bind_keys(cx: &mut App) {
    text_input::init(cx);
    cx.bind_keys([
        KeyBinding::new(&format!("{PRIMARY}-o"), ChooseFolder, Some("Space")),
        KeyBinding::new(&format!("{PRIMARY}-r"), Rescan, Some("Space")),
        KeyBinding::new("escape", Cancel, Some("Space")),
        KeyBinding::new(&format!("{PRIMARY}-shift-c"), CopyPath, Some("Space")),
        KeyBinding::new(&format!("{PRIMARY}-shift-r"), RevealPath, Some("Space")),
        KeyBinding::new("backspace", GoUp, Some("SpaceMap")),
        KeyBinding::new("alt-up", GoUp, Some("SpaceMap")),
        KeyBinding::new("enter", ZoomIn, Some("SpaceMap")),
        KeyBinding::new("down", NextRow, Some("SpaceList")),
        KeyBinding::new("up", PreviousRow, Some("SpaceList")),
        KeyBinding::new("enter", OpenRow, Some("SpaceList")),
        KeyBinding::new(&format!("{PRIMARY}-l"), ToggleList, Some("Space")),
        KeyBinding::new(&format!("{PRIMARY}-f"), FocusSearch, Some("Space")),
        KeyBinding::new("tab", FocusNext, Some("Space")),
        KeyBinding::new("shift-tab", FocusPrevious, Some("Space")),
        KeyBinding::new(&format!("{PRIMARY}-w"), CloseWindow, Some("Space")),
    ]);
}
fn main() {
    if std::env::args().any(|a| a == "--version") {
        println!("Space {} (development)", env!("CARGO_PKG_VERSION"));
        return;
    }
    #[cfg(target_os = "linux")]
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("Space requires an X11 or Wayland desktop.");
        std::process::exit(1);
    }
    gpui_platform::application().run(|cx: &mut App| {
        bind_keys(cx);
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
                window_min_size: Some(size(px(900.), px(600.))),
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
