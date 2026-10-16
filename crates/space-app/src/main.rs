mod probe;
use gpui::{
    App, Bounds, ClipboardItem, Context, FocusHandle, KeyBinding, PathPromptOptions, Role,
    ScrollStrategy, SharedString, UniformListScrollHandle, Window, WindowBounds, WindowOptions,
    actions, div, prelude::*, px, rgb, size, uniform_list,
};
use probe::{Probe, ROW_COUNT, next_selection};
use std::{path::PathBuf, sync::atomic::Ordering, time::Duration};
actions!(
    space,
    [
        ChooseFolder,
        CopyPath,
        RevealPath,
        StartProbe,
        CancelProbe,
        NextRow,
        PreviousRow,
        FocusNext,
        FocusPrevious,
        CloseWindow
    ]
);
struct Space {
    focus: FocusHandle,
    list_focus: FocusHandle,
    scroll: UniformListScrollHandle,
    selected: usize,
    folder: Option<PathBuf>,
    picker_open: bool,
    status: String,
    generation: u64,
    probe: Option<Probe>,
}
impl Space {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        Self {
            focus,
            list_focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            selected: 0,
            folder: None,
            picker_open: false,
            status: "Choose a folder to test native integration. No files are scanned.".into(),
            generation: 0,
            probe: None,
        }
    }
    fn choose_folder(&mut self, _: &ChooseFolder, _: &mut Window, cx: &mut Context<Self>) {
        if self.picker_open {
            return;
        }
        self.picker_open = true;
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose folder".into()),
        });
        cx.spawn(async move |this, cx| {
            let result = receiver.await;
            let _ = this.update(cx, |this, cx| {
                this.picker_open = false;
                match result {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.into_iter().next() {
                            this.folder = Some(path);
                            this.status =
                                "Folder selected. Synthetic rows are unrelated to its contents."
                                    .into();
                        }
                    }
                    Ok(Ok(None)) => {
                        this.status = "Selection cancelled. Previous folder retained.".into()
                    }
                    Ok(Err(error)) => this.status = format!("Folder picker failed: {error}"),
                    Err(error) => {
                        this.status = format!("Folder picker closed unexpectedly: {error}")
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn copy_path(&mut self, _: &CopyPath, _: &mut Window, cx: &mut Context<Self>) {
        self.status = match self.folder.as_ref() {
            Some(path) => match path.to_str() {
                Some(text) => { cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned())); "Path copied.".into() }
                None => "Native path cannot be represented losslessly as clipboard text. Nothing copied.".into(),
            }, None => "Choose a folder first.".into(),
        };
        cx.notify();
    }
    fn reveal_path(&mut self, _: &RevealPath, _: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.folder.clone() else {
            self.status = "Choose a folder first.".into();
            cx.notify();
            return;
        };
        // This spike has no cached scans. The product must also validate root identity.
        let check_path = path.clone();
        let check = cx
            .background_executor()
            .spawn(async move { check_path.try_exists() });
        cx.spawn(async move |this, cx| {
            let result = check.await;
            let _ = this.update(cx, |this, cx| {
                if this.folder.as_ref() != Some(&path) { return; }
                this.status = match result {
                    Ok(true) => {
                        cx.reveal_path(&path);
                        "Reveal requested. Verify the OS opened the folder; platform errors may not be reported.".into()
                    }
                    Ok(false) => "Selected folder no longer exists. Choose it again.".into(),
                    Err(error) => format!("Cannot verify selected folder: {error}"),
                };
                cx.notify();
            });
        }).detach();
    }
    fn start_probe(&mut self, _: &StartProbe, _: &mut Window, cx: &mut Context<Self>) {
        self.generation = self
            .generation
            .checked_add(1)
            .expect("probe generation exhausted");
        let generation = self.generation;
        let probe = Probe::new(generation);
        let progress = probe.progress.clone();
        self.probe = Some(probe); // Dropping a predecessor cancels its worker.
        cx.background_executor()
            .spawn(async move {
                for step in 1..=100 {
                    if progress.cancelled.load(Ordering::Acquire) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(40));
                    if progress.cancelled.load(Ordering::Acquire) {
                        break;
                    }
                    progress.completed.store(step, Ordering::Release);
                }
            })
            .detach();
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            loop {
                executor.timer(Duration::from_millis(50)).await;
                let running = this
                    .update(cx, |this, cx| {
                        let Some(probe) = &this.probe else {
                            return false;
                        };
                        if probe.generation != generation {
                            return false;
                        }
                        let completed = probe.completed();
                        let cancelled = probe.progress.cancelled.load(Ordering::Acquire);
                        this.status = if cancelled {
                            format!("Synthetic work cancelled at {completed}%.")
                        } else {
                            format!(
                                "Synthetic background work: {completed}% (no filesystem access)"
                            )
                        };
                        cx.notify();
                        completed < 100 && !cancelled
                    })
                    .unwrap_or(false);
                if !running {
                    break;
                }
            }
        })
        .detach();
        self.status = "Synthetic work started.".into();
        cx.notify();
    }
    fn cancel_probe(&mut self, _: &CancelProbe, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(probe) = &self.probe {
            probe.cancel();
        }
        self.status = "Synthetic work cancelled.".into();
        cx.notify();
    }
    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.selected = next_selection(self.selected, delta, ROW_COUNT);
        self.scroll
            .scroll_to_item(self.selected, ScrollStrategy::Nearest);
        cx.notify();
    }
}
impl Render for Space {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let folder = self
            .folder
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "No folder selected".into());
        let lossy = self.folder.as_ref().is_some_and(|p| p.to_str().is_none());
        div()
            .id("space")
            .role(Role::Application)
            .aria_label("Space platform feasibility spike")
            .track_focus(&self.focus)
            .key_context("Space")
            .on_action(cx.listener(Self::choose_folder))
            .on_action(cx.listener(Self::copy_path))
            .on_action(cx.listener(Self::reveal_path))
            .on_action(cx.listener(Self::start_probe))
            .on_action(cx.listener(Self::cancel_probe))
            .on_action(cx.listener(|_, _: &FocusNext, window, cx| window.focus_next(cx)))
            .on_action(cx.listener(|_, _: &FocusPrevious, window, cx| window.focus_prev(cx)))
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .size_full()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .bg(rgb(0x17212b))
            .text_color(rgb(0xe6edf3))
            .child(div().text_xl().child("Space · native platform spike"))
            .child("G0 only. 100,000 synthetic rows; no scan, cache or file modification.")
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        div()
                            .id("choose")
                            .role(Role::Button)
                            .aria_label("Choose folder, Control O")
                            .focusable()
                            .tab_stop(true)
                            .border_2()
                            .border_color(rgb(0x285166))
                            .focus(|style| style.border_color(rgb(0x90d5ff)))
                            .p_2()
                            .bg(rgb(0x285166))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, w, cx| {
                                this.choose_folder(&ChooseFolder, w, cx)
                            }))
                            .child("Choose folder (Ctrl+O)"),
                    )
                    .child(
                        div()
                            .id("copy")
                            .role(Role::Button)
                            .aria_label("Copy selected folder path, Control Shift C")
                            .focusable()
                            .tab_stop(true)
                            .border_2()
                            .border_color(rgb(0x285166))
                            .focus(|style| style.border_color(rgb(0x90d5ff)))
                            .p_2()
                            .bg(rgb(0x285166))
                            .cursor_pointer()
                            .on_click(
                                cx.listener(|this, _, w, cx| this.copy_path(&CopyPath, w, cx)),
                            )
                            .child("Copy path"),
                    )
                    .child(
                        div()
                            .id("reveal")
                            .role(Role::Button)
                            .aria_label("Reveal selected folder, Control R")
                            .focusable()
                            .tab_stop(true)
                            .border_2()
                            .border_color(rgb(0x285166))
                            .focus(|style| style.border_color(rgb(0x90d5ff)))
                            .p_2()
                            .bg(rgb(0x285166))
                            .cursor_pointer()
                            .on_click(
                                cx.listener(|this, _, w, cx| this.reveal_path(&RevealPath, w, cx)),
                            )
                            .child("Reveal (Ctrl+R)"),
                    ),
            )
            .child(folder)
            .when(lossy, |el| {
                el.child("Display contains replacement characters; native path retained.")
            })
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        div()
                            .id("start")
                            .role(Role::Button)
                            .aria_label("Start or restart synthetic work, Control B")
                            .focusable()
                            .tab_stop(true)
                            .border_2()
                            .border_color(rgb(0x285166))
                            .focus(|style| style.border_color(rgb(0x90d5ff)))
                            .p_2()
                            .bg(rgb(0x285166))
                            .cursor_pointer()
                            .on_click(
                                cx.listener(|this, _, w, cx| this.start_probe(&StartProbe, w, cx)),
                            )
                            .child("Start / restart (Ctrl+B)"),
                    )
                    .child(
                        div()
                            .id("cancel")
                            .role(Role::Button)
                            .aria_label("Cancel synthetic work, Escape")
                            .focusable()
                            .tab_stop(true)
                            .border_2()
                            .border_color(rgb(0x285166))
                            .focus(|style| style.border_color(rgb(0x90d5ff)))
                            .p_2()
                            .bg(rgb(0x285166))
                            .cursor_pointer()
                            .on_click(
                                cx.listener(|this, _, w, cx| {
                                    this.cancel_probe(&CancelProbe, w, cx)
                                }),
                            )
                            .child("Cancel (Esc)"),
                    ),
            )
            .child(self.status.clone())
            .child(format!(
                "Selected row {} of {ROW_COUNT}. Up / Down select; Tab moves focus.",
                self.selected + 1
            ))
            .child(
                div()
                    .id("synthetic-list")
                    .role(Role::ListBox)
                    .aria_label("Synthetic entries")
                    .track_focus(&self.list_focus)
                    .tab_stop(true)
                    .key_context("SpaceList")
                    .flex_1()
                    .min_h_0()
                    .border_2()
                    .border_color(rgb(0x285166))
                    .focus(|style| style.border_color(rgb(0x90d5ff)))
                    .on_action(cx.listener(|this, _: &NextRow, _, cx| this.move_selection(1, cx)))
                    .on_action(
                        cx.listener(|this, _: &PreviousRow, _, cx| this.move_selection(-1, cx)),
                    )
                    .child(
                        uniform_list(
                            "rows",
                            ROW_COUNT,
                            cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                                range
                                    .map(|ix| {
                                        div()
                                            .id(ix)
                                            .role(Role::ListBoxOption)
                                            .aria_label(SharedString::from(format!(
                                                "Synthetic row {}",
                                                ix + 1
                                            )))
                                            .aria_position_in_set(ix + 1)
                                            .aria_size_of_set(ROW_COUNT)
                                            .aria_selected(ix == this.selected)
                                            .h(px(28.))
                                            .px_2()
                                            .cursor_pointer()
                                            .when(ix == this.selected, |el| {
                                                el.bg(rgb(0x285166)).aria_active_descendant()
                                            })
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                window.focus(&this.list_focus, cx);
                                                this.selected = ix;
                                                cx.notify();
                                            }))
                                            .child(format!(
                                                "{} Synthetic entry {:06}",
                                                if ix == this.selected { ">" } else { " " },
                                                ix + 1
                                            ))
                                    })
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .track_scroll(&self.scroll)
                        .h_full(),
                    ),
            )
    }
}
fn main() {
    if std::env::args().any(|arg| arg == "--version") {
        println!("Space {} (G0 platform spike)", env!("CARGO_PKG_VERSION"));
        return;
    }
    #[cfg(target_os = "linux")]
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("Space requires X11 or Wayland. Headless startup is not native G0 validation.");
        std::process::exit(1);
    }
    gpui_platform::application().run(|cx: &mut App| {
        cx.bind_keys([
            KeyBinding::new("ctrl-o", ChooseFolder, Some("Space")),
            KeyBinding::new("ctrl-shift-c", CopyPath, Some("Space")),
            KeyBinding::new("ctrl-r", RevealPath, Some("Space")),
            KeyBinding::new("ctrl-b", StartProbe, Some("Space")),
            KeyBinding::new("escape", CancelProbe, Some("Space")),
            KeyBinding::new("down", NextRow, Some("SpaceList")),
            KeyBinding::new("up", PreviousRow, Some("SpaceList")),
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
        let bounds = Bounds::centered(None, size(px(980.), px(700.)), cx);
        if let Err(error) = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("Space · G0".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |w, cx| cx.new(|cx| Space::new(w, cx)),
        ) {
            eprintln!(
                "Unable to open Space window: {error}. Check native GPU/desktop requirements."
            );
            cx.quit();
            return;
        }
        cx.activate(true);
    });
}
