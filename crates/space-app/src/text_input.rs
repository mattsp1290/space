// Adapted from Zed GPUI examples/input.rs at 9f63850f7affbd57772a4f99554d4fd333b50a6f.
// Copyright 2022 - 2025 Zed Industries, Inc. Licensed under Apache-2.0.
// https://github.com/zed-industries/zed/blob/9f63850f7affbd57772a4f99554d4fd333b50a6f/crates/gpui/examples/input.rs
// Space modifications: single-line edit state, bounded undo/redo, composition commit
// events, range hardening, accessible metadata, horizontal scrolling and tests.
// License: https://www.apache.org/licenses/LICENSE-2.0

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, KeyBinding,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    Role, ShapedLine, SharedString, Style, TextRun, UTF16Selection, UnderlineStyle, Window,
    actions, div, fill, point, prelude::*, px, relative, rgb, rgba, size,
};
use unicode_segmentation::*;

actions!(
    space_text_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        ShowCharacterPalette,
        Paste,
        Cut,
        Copy,
        Undo,
        Redo,
        SelectHome,
        SelectEnd,
    ]
);

/// A committed edit; IME preedit updates do not trigger filtering.
pub enum TextInputEvent {
    Edited(String),
}

impl EventEmitter<TextInputEvent> for TextInput {}

pub struct TextInput {
    focus_handle: FocusHandle,
    label: SharedString,
    placeholder: SharedString,
    edit: EditState,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    last_viewport: Option<Bounds<Pixels>>,
    is_selecting: bool,
    scroll_x: Pixels,
    focus_observers_installed: bool,
}

impl TextInput {
    fn left(&mut self, _: &Left, window: &mut Window, cx: &mut Context<Self>) {
        if self.edit.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), window, cx);
        } else {
            self.move_to(self.edit.selected_range.start, window, cx)
        }
    }

    fn right(&mut self, _: &Right, window: &mut Window, cx: &mut Context<Self>) {
        if self.edit.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.edit.selected_range.end), window, cx);
        } else {
            self.move_to(self.edit.selected_range.end, window, cx)
        }
    }

    fn select_left(&mut self, _: &SelectLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), window, cx);
    }

    fn select_right(&mut self, _: &SelectRight, window: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), window, cx);
    }

    fn select_all(&mut self, _: &SelectAll, window: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, window, cx);
        self.select_to(self.edit.content.len(), window, cx)
    }

    fn home(&mut self, _: &Home, window: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, window, cx);
    }

    fn end(&mut self, _: &End, window: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.edit.content.len(), window, cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.edit.selected_range.is_empty() {
            let prev = self.previous_boundary(self.cursor_offset());
            if self.cursor_offset() == prev {
                window.play_system_bell();
                return;
            }
            self.select_to(prev, window, cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.edit.selected_range.is_empty() {
            let next = self.next_boundary(self.cursor_offset());
            if self.cursor_offset() == next {
                window.play_system_bell();
                return;
            }
            self.select_to(next, window, cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        self.is_selecting = true;
        window.request_virtual_keyboard();

        if event.modifiers.shift {
            self.select_to(self.index_for_mouse_position(event.position), window, cx);
        } else {
            self.move_to(self.index_for_mouse_position(event.position), window, cx)
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _window: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position), window, cx);
        }
    }

    fn show_character_palette(
        &mut self,
        _: &ShowCharacterPalette,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.show_character_palette();
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_text_in_range(None, &text, window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.edit.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.edit.content[self.edit.selected_range.clone()].to_string(),
            ));
        }
    }
    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.edit.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.edit.content[self.edit.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx)
        }
    }

    fn move_to(&mut self, offset: usize, window: &Window, cx: &mut Context<Self>) {
        if self.edit.finish_composition() {
            self.edited(window, cx);
        }
        let offset = clamp_byte_boundary(&self.edit.content, offset);
        self.edit.selected_range = offset..offset;
        self.edit.selection_reversed = false;
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify()
    }

    fn cursor_offset(&self) -> usize {
        if self.edit.selection_reversed {
            self.edit.selected_range.start
        } else {
            self.edit.selected_range.end
        }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.edit.content.is_empty() {
            return 0;
        }

        let (Some(bounds), Some(line)) = (self.last_bounds.as_ref(), self.last_layout.as_ref())
        else {
            return 0;
        };
        if line.text.as_ref() != self.edit.content.as_str() {
            return self.cursor_offset();
        }
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.edit.content.len();
        }
        line.closest_index_for_x(position.x - bounds.left())
    }

    fn select_to(&mut self, offset: usize, window: &Window, cx: &mut Context<Self>) {
        if self.edit.finish_composition() {
            self.edited(window, cx);
        }
        let offset = clamp_byte_boundary(&self.edit.content, offset);
        if self.edit.selection_reversed {
            self.edit.selected_range.start = offset
        } else {
            self.edit.selected_range.end = offset
        };
        if self.edit.selected_range.end < self.edit.selected_range.start {
            self.edit.selection_reversed = !self.edit.selection_reversed;
            self.edit.selected_range = self.edit.selected_range.end..self.edit.selected_range.start;
        }
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify()
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        byte_to_utf16(&self.edit.content, offset)
    }
    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }
    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.edit.byte_range(range)
    }
    fn previous_boundary(&self, offset: usize) -> usize {
        previous_boundary(&self.edit.content, offset)
    }
    fn next_boundary(&self, offset: usize) -> usize {
        next_boundary(&self.edit.content, offset)
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.edit.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.edit.selected_range),
            reversed: self.edit.selection_reversed,
        })
    }

    fn set_selected_text_range(
        &mut self,
        range_utf16: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit.selected_range = self.range_from_utf16(&range_utf16);
        self.edit.selection_reversed = false;
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn text_length_utf16(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.edit.content.encode_utf16().count())
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.edit
            .marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.edit.finish_composition() {
            self.edited(window, cx);
        }
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit.replace(range_utf16, new_text, None, false);
        self.edited(window, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit
            .replace(range_utf16, new_text, new_selected_range_utf16, true);
        window.notify_text_input_changed(&self.focus_handle);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let last_layout = self.last_layout.as_ref()?;
        if last_layout.text.as_ref() != self.edit.content.as_str() {
            return None;
        }
        let range = self.range_from_utf16(&range_utf16);
        Some(Bounds::from_corners(
            point(
                bounds.left() + last_layout.x_for_index(range.start),
                bounds.top(),
            ),
            point(
                bounds.left() + last_layout.x_for_index(range.end),
                bounds.bottom(),
            ),
        ))
    }

    fn selection_bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<Bounds<Pixels>> {
        self.bounds_for_range(range_utf16, bounds, window, cx)
            .into_iter()
            .collect()
    }

    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        if !self.last_viewport?.contains(&point) {
            return None;
        }
        let line_x = point.x - self.last_bounds?.left();
        let last_layout = self.last_layout.as_ref()?;

        if last_layout.text.as_ref() != self.edit.content.as_str() {
            return None;
        }
        let utf8_index = last_layout.closest_index_for_x(line_x);
        Some(self.offset_to_utf16(utf8_index))
    }
}

struct TextElement {
    input: Entity<TextInput>,
}

struct PrepaintState {
    line: Option<ShapedLine>,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
    text_bounds: Bounds<Pixels>,
    scroll_x: Pixels,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let content: SharedString = input.edit.content.clone().into();
        let selected_range = input.edit.selected_range.clone();
        let cursor = input.cursor_offset();
        let style = window.text_style();

        let (display_text, text_color) = if content.is_empty() {
            (
                input.placeholder.clone(),
                rgb(crate::theme::TEXT_MUTED).into(),
            )
        } else {
            (content, style.color)
        };

        let run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if let Some(marked_range) = input.edit.marked_range.as_ref() {
            vec![
                TextRun {
                    len: marked_range.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked_range.end - marked_range.start,
                    underline: Some(UnderlineStyle {
                        color: Some(run.color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: display_text.len() - marked_range.end,
                    ..run
                },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect()
        } else {
            vec![run]
        };

        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(display_text, font_size, &runs, None);

        let cursor_pos = line.x_for_index(cursor);
        let available = (bounds.size.width - px(2.)).max(px(0.));
        let scroll_x = if cursor_pos < input.scroll_x {
            cursor_pos
        } else if cursor_pos > input.scroll_x + available {
            cursor_pos - available
        } else {
            input.scroll_x
        };
        let text_bounds = Bounds::new(point(bounds.left() - scroll_x, bounds.top()), bounds.size);
        let bounds = text_bounds;
        let (selection, cursor) = if selected_range.is_empty() {
            (
                None,
                Some(fill(
                    Bounds::new(
                        point(bounds.left() + cursor_pos, bounds.top()),
                        size(px(2.), bounds.bottom() - bounds.top()),
                    ),
                    gpui::blue(),
                )),
            )
        } else {
            (
                Some(fill(
                    Bounds::from_corners(
                        point(
                            bounds.left() + line.x_for_index(selected_range.start),
                            bounds.top(),
                        ),
                        point(
                            bounds.left() + line.x_for_index(selected_range.end),
                            bounds.bottom(),
                        ),
                    ),
                    rgba(crate::theme::SELECTION),
                )),
                None,
            )
        };
        PrepaintState {
            line: Some(line),
            cursor,
            selection,
            text_bounds,
            scroll_x,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(prepaint.text_bounds, self.input.clone()),
            cx,
        );
        if let Some(selection) = prepaint.selection.take() {
            window.paint_quad(selection)
        }
        let line = prepaint.line.take().unwrap();
        line.paint(
            prepaint.text_bounds.origin,
            window.line_height(),
            gpui::TextAlign::Left,
            None,
            window,
            cx,
        )
        .ok();

        if focus_handle.is_focused(window)
            && let Some(cursor) = prepaint.cursor.take()
        {
            window.paint_quad(cursor);
        }

        self.input.update(cx, |input, _cx| {
            input.last_layout = Some(line);
            input.last_bounds = Some(prepaint.text_bounds);
            input.last_viewport = Some(bounds);
            input.scroll_x = prepaint.scroll_x;
        });
    }
}

impl Render for TextInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.focus_observers_installed {
            cx.on_blur(&self.focus_handle, window, |input, window, cx| {
                input.is_selecting = false;
                if input.edit.finish_composition() {
                    input.edited(window, cx);
                }
                cx.notify();
            })
            .detach();
            cx.on_focus(&self.focus_handle, window, |_, _, cx| cx.notify())
                .detach();
            self.focus_observers_installed = true;
        }
        div()
            .flex()
            .flex_col()
            .id("space-filter-input")
            .role(Role::TextInput)
            .aria_label(self.label.clone())
            .aria_value(self.edit.content.clone())
            .aria_placeholder(self.placeholder.clone())
            .aria_description(if self.edit.limit_reached {
                "Input shortened to the 64 KiB limit"
            } else {
                "Single-line filter; maximum 64 KiB of UTF-8 text"
            })
            .tab_stop(true)
            .key_context("SpaceTextInput")
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::show_character_palette))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .text_color(rgb(crate::theme::TEXT))
            .bg(rgb(crate::theme::SURFACE))
            .w_full()
            .overflow_hidden()
            .border_1()
            .border_color(if self.focus_handle.is_focused(window) {
                rgb(crate::theme::ACCENT)
            } else if self.edit.limit_reached {
                rgb(crate::theme::WARNING)
            } else {
                rgb(crate::theme::BORDER)
            })
            .line_height(px(22.))
            .text_size(px(14.))
            .child(
                div()
                    .h(px(30.))
                    .w_full()
                    .p(px(4.))
                    .bg(rgb(crate::theme::SURFACE))
                    .child(TextElement { input: cx.entity() }),
            )
            .when(self.edit.limit_reached, |field| {
                field.child(
                    div()
                        .text_size(px(11.))
                        .text_color(rgb(crate::theme::WARNING))
                        .child("Input shortened to 64 KiB"),
                )
            })
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// Register once when the application starts. Bindings are scoped to focused inputs.
pub fn init(cx: &mut App) {
    let context = Some("SpaceTextInput");
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, context),
        KeyBinding::new("delete", Delete, context),
        KeyBinding::new("left", Left, context),
        KeyBinding::new("right", Right, context),
        KeyBinding::new("shift-left", SelectLeft, context),
        KeyBinding::new("shift-right", SelectRight, context),
        KeyBinding::new("home", Home, context),
        KeyBinding::new("end", End, context),
        KeyBinding::new("shift-home", SelectHome, context),
        KeyBinding::new("shift-end", SelectEnd, context),
        KeyBinding::new("ctrl-cmd-space", ShowCharacterPalette, context),
    ]);
    #[cfg(target_os = "macos")]
    let modifier = "cmd";
    #[cfg(not(target_os = "macos"))]
    let modifier = "ctrl";
    cx.bind_keys([
        KeyBinding::new(&format!("{modifier}-a"), SelectAll, context),
        KeyBinding::new(&format!("{modifier}-c"), Copy, context),
        KeyBinding::new(&format!("{modifier}-x"), Cut, context),
        KeyBinding::new(&format!("{modifier}-v"), Paste, context),
        KeyBinding::new(&format!("{modifier}-z"), Undo, context),
        KeyBinding::new(&format!("{modifier}-shift-z"), Redo, context),
    ]);
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([KeyBinding::new("ctrl-y", Redo, context)]);
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("cmd-left", Home, context),
        KeyBinding::new("cmd-right", End, context),
        KeyBinding::new("cmd-shift-left", SelectHome, context),
        KeyBinding::new("cmd-shift-right", SelectEnd, context),
    ]);
}

impl TextInput {
    pub fn new(
        label: impl Into<SharedString>,
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            label: label.into(),
            placeholder: placeholder.into(),
            edit: EditState::default(),
            last_layout: None,
            last_bounds: None,
            last_viewport: None,
            is_selecting: false,
            scroll_x: px(0.),
            focus_observers_installed: false,
        }
    }

    pub fn text(&self) -> &str {
        &self.edit.content
    }

    /// Reset immediately so the next native edit sees the new buffer. The owner
    /// notifies the platform when a Window is available. Does not emit Edited.
    pub fn reset_text(&mut self, text: &str, cx: &mut Context<Self>) {
        let (content, limit_reached) = single_line_limited(text, MAX_INPUT_BYTES);
        let end = content.len();
        self.edit = EditState {
            content,
            limit_reached,
            selected_range: end..end,
            ..Default::default()
        };
        self.scroll_x = px(0.);
        self.is_selecting = false;
        self.last_layout = None;
        cx.notify();
    }

    fn edited(&mut self, window: &Window, cx: &mut Context<Self>) {
        window.notify_text_input_changed(&self.focus_handle);
        cx.emit(TextInputEvent::Edited(self.edit.content.clone()));
        cx.notify();
    }

    fn undo(&mut self, _: &Undo, window: &mut Window, cx: &mut Context<Self>) {
        if self.edit.undo() {
            self.edited(window, cx);
        }
    }
    fn redo(&mut self, _: &Redo, window: &mut Window, cx: &mut Context<Self>) {
        if self.edit.redo() {
            self.edited(window, cx);
        }
    }
    fn select_home(&mut self, _: &SelectHome, window: &mut Window, cx: &mut Context<Self>) {
        self.select_to(0, window, cx);
    }
    fn select_end(&mut self, _: &SelectEnd, window: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.edit.content.len(), window, cx);
    }
}

#[cfg(test)]
mod geometry_tests {
    use super::*;
    #[test]
    fn scrolled_right_edge_maps_to_text_in_visible_viewport() {
        let viewport = Bounds::new(point(px(100.), px(10.)), size(px(100.), px(22.)));
        let text_bounds = Bounds::new(point(px(-200.), px(10.)), viewport.size);
        let pointer = point(px(195.), px(15.));
        assert!(viewport.contains(&pointer));
        assert!(!text_bounds.contains(&pointer));
        assert_eq!(pointer.x - text_bounds.left(), px(395.));
    }
}

// This section intentionally has no GPUI dependency, so editing invariants can be
// exercised without a display server, platform libraries or a GPU.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Snapshot {
    content: String,
    selected_range: Range<usize>,
    selection_reversed: bool,
}

#[derive(Default, Debug)]
struct EditState {
    content: String,
    limit_reached: bool,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    composition_start: Option<Snapshot>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
}

impl EditState {
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            content: self.content.clone(),
            selected_range: self.selected_range.clone(),
            selection_reversed: self.selection_reversed,
        }
    }
    fn restore(&mut self, snapshot: Snapshot) {
        self.content = snapshot.content;
        self.limit_reached = false;
        self.selected_range = snapshot.selected_range;
        self.selection_reversed = snapshot.selection_reversed;
        self.marked_range = None;
        self.composition_start = None;
    }
    fn push_undo(&mut self, snapshot: Snapshot) {
        self.undo.push(snapshot);
        self.redo.clear();
        self.trim_history();
    }
    fn trim_history(&mut self) {
        while self.undo.len() + self.redo.len() > 100
            || self
                .undo
                .iter()
                .chain(&self.redo)
                .map(|s| s.content.len())
                .sum::<usize>()
                > MAX_HISTORY_BYTES
        {
            if !self.undo.is_empty() {
                self.undo.remove(0);
            } else if !self.redo.is_empty() {
                self.redo.remove(0);
            } else {
                break;
            }
        }
    }
    fn byte_range(&self, range: &Range<usize>) -> Range<usize> {
        let a = utf16_to_byte(&self.content, range.start);
        let b = utf16_to_byte(&self.content, range.end);
        a.min(b)..a.max(b)
    }
    fn replace(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selection: Option<Range<usize>>,
        composing: bool,
    ) {
        let range = range
            .as_ref()
            .map(|r| self.byte_range(r))
            .or_else(|| self.marked_range.clone())
            .unwrap_or_else(|| self.selected_range.clone());
        let before = self.snapshot();
        if composing && self.composition_start.is_none() {
            self.composition_start = Some(before.clone());
        }
        let available = MAX_INPUT_BYTES.saturating_sub(self.content.len() - range.len());
        let (text, limit_reached) = single_line_limited(text, available);
        self.limit_reached = limit_reached;
        self.content.replace_range(range.clone(), &text);
        let end = range.start + text.len();
        self.selected_range = if composing {
            selection
                .map(|r| {
                    let a = utf16_to_byte(&text, r.start);
                    let b = utf16_to_byte(&text, r.end);
                    range.start + a.min(b)..range.start + a.max(b)
                })
                .unwrap_or(end..end)
        } else {
            end..end
        };
        self.selection_reversed = false;
        self.marked_range = (composing && !text.is_empty()).then_some(range.start..end);
        if !composing {
            let baseline = self.composition_start.take().unwrap_or(before);
            if baseline.content != self.content {
                self.push_undo(baseline);
            }
        }
    }
    fn finish_composition(&mut self) -> bool {
        self.marked_range = None;
        if let Some(baseline) = self.composition_start.take() {
            if baseline.content != self.content {
                self.push_undo(baseline);
            }
            return true;
        }
        false
    }
    fn undo(&mut self) -> bool {
        self.finish_composition();
        let Some(previous) = self.undo.pop() else {
            return false;
        };
        self.redo.push(self.snapshot());
        self.restore(previous);
        self.trim_history();
        true
    }
    fn redo(&mut self) -> bool {
        self.finish_composition();
        let Some(next) = self.redo.pop() else {
            return false;
        };
        self.undo.push(self.snapshot());
        self.restore(next);
        self.trim_history();
        true
    }
}

fn clamp_byte_boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

const MAX_INPUT_BYTES: usize = 64 * 1024;
const MAX_HISTORY_BYTES: usize = 512 * 1024;

/// Normalize once, stopping allocation at the byte cap. Truncation is surfaced
/// visibly and through the accessible description; scalar boundaries stay valid.
fn single_line_limited(text: &str, max_bytes: usize) -> (String, bool) {
    let mut output = String::with_capacity(text.len().min(max_bytes));
    let mut chars = text.chars().peekable();
    while let Some(mut c) = chars.next() {
        if c == '\r' && chars.peek() == Some(&'\n') {
            chars.next();
        }
        if matches!(c, '\r' | '\n' | '\t' | '\u{2028}' | '\u{2029}') {
            c = ' ';
        }
        if c.is_control() {
            continue;
        }
        if output.len() + c.len_utf8() > max_bytes {
            return (output, true);
        }
        output.push(c);
    }
    (output, false)
}

// Round malformed offsets inside a surrogate pair up to a scalar boundary and
// clamp out-of-range offsets. Never slice a UTF-8 string at an arbitrary byte.
fn utf16_to_byte(text: &str, offset: usize) -> usize {
    let mut count = 0;
    for (byte, ch) in text.char_indices() {
        if count >= offset {
            return byte;
        }
        count += ch.len_utf16();
    }
    text.len()
}
fn byte_to_utf16(text: &str, offset: usize) -> usize {
    text.char_indices()
        .take_while(|(byte, _)| *byte < offset)
        .map(|(_, c)| c.len_utf16())
        .sum()
}
fn previous_boundary(text: &str, offset: usize) -> usize {
    text.grapheme_indices(true)
        .rev()
        .find_map(|(i, _)| (i < offset).then_some(i))
        .unwrap_or(0)
}
fn next_boundary(text: &str, offset: usize) -> usize {
    text.grapheme_indices(true)
        .find_map(|(i, _)| (i > offset).then_some(i))
        .unwrap_or(text.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn utf16_roundtrip_clamps_surrogates_and_reversed_ranges() {
        let mut state = EditState::default();
        state.replace(None, "A🙂漢", None, false);
        for (byte, _) in state.content.char_indices() {
            assert_eq!(
                utf16_to_byte(&state.content, byte_to_utf16(&state.content, byte)),
                byte
            );
        }
        assert_eq!(utf16_to_byte(&state.content, 2), 5);
        assert_eq!(clamp_byte_boundary(&state.content, 3), 1);
        assert_eq!(clamp_byte_boundary(&state.content, 99), 8);
        assert_eq!(state.byte_range(&Range { start: 99, end: 1 }), 1..8);
        assert_eq!(state.byte_range(&(99..100)), 8..8);
    }
    #[test]
    fn grapheme_navigation_preserves_combining_marks_and_emoji() {
        let text = "e\u{301}👨‍👩‍👧‍👦中";
        assert_eq!(next_boundary(text, 0), 3);
        assert_eq!(previous_boundary(text, 3), 0);
        let end_family = text.len() - "中".len();
        assert_eq!(next_boundary(text, 3), end_family);
        assert_eq!(previous_boundary(text, end_family), 3);
    }
    #[test]
    fn ime_selection_is_relative_and_one_undo_restores_original() {
        let mut state = EditState::default();
        state.replace(None, "A🙂Z", None, false);
        state.replace(Some(1..3), "é🙂", Some(1..3), true);
        assert_eq!(state.content, "Aé🙂Z");
        assert_eq!(state.marked_range, Some(1..7));
        assert_eq!(state.selected_range, 3..7);
        state.replace(None, "漢", Some(1..1), true);
        assert_eq!(state.content, "A漢Z");
        state.replace(None, "漢字", None, false);
        assert_eq!(state.content, "A漢字Z");
        assert!(state.marked_range.is_none());
        assert!(state.undo());
        assert_eq!(state.content, "A🙂Z");
        assert!(state.redo());
        assert_eq!(state.content, "A漢字Z");
    }
    #[test]
    fn cancelled_composition_and_single_line_paste_are_safe() {
        let mut state = EditState::default();
        state.replace(None, "test", None, false);
        state.replace(None, "候補", Some(Range { start: 99, end: 0 }), true);
        state.replace(None, "", Some(0..0), true);
        state.finish_composition();
        assert_eq!(state.content, "test");
        assert_eq!(state.selected_range, 4..4);
        state.replace(
            Some(0..4),
            "one\r\ntwo\nthree\tfour\u{2028}five\0",
            None,
            false,
        );
        assert_eq!(state.content, "one two three four five");
        assert!(state.undo());
        assert_eq!(state.content, "test");
    }
    #[test]
    fn new_edit_clears_redo_and_history_is_bounded() {
        let mut state = EditState::default();
        for _ in 0..150 {
            state.replace(None, "x", None, false);
        }
        assert_eq!(state.undo.len(), 100);
        assert!(state.undo());
        state.replace(None, "y", None, false);
        assert!(!state.redo());
    }
    #[test]
    fn oversized_paste_composition_and_history_are_bounded() {
        let huge = "🙂".repeat(MAX_INPUT_BYTES);
        let mut state = EditState::default();
        state.replace(None, &huge, None, false);
        assert_eq!(state.content.len(), MAX_INPUT_BYTES);
        assert!(state.limit_reached);
        for n in 0..30 {
            state.replace(
                Some(0..usize::MAX),
                &format!("{n}{huge}"),
                Some(usize::MAX..usize::MAX),
                true,
            );
            assert!(state.content.len() <= MAX_INPUT_BYTES);
            assert!(state.content.is_char_boundary(state.selected_range.end));
            state.finish_composition();
        }
        assert!(
            state
                .undo
                .iter()
                .chain(&state.redo)
                .map(|s| s.content.len())
                .sum::<usize>()
                <= MAX_HISTORY_BYTES
        );
        while state.undo() {
            assert!(
                state
                    .undo
                    .iter()
                    .chain(&state.redo)
                    .map(|s| s.content.len())
                    .sum::<usize>()
                    <= MAX_HISTORY_BYTES
            );
        }
        let (programmatic, truncated) = single_line_limited(&huge, MAX_INPUT_BYTES);
        assert!(truncated);
        assert_eq!(programmatic.len(), MAX_INPUT_BYTES);
    }
}
