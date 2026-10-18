//! Shared dark theme. Colors are defined here, including text input colors.
use gpui::{Div, Hsla, Role, SharedString, Stateful, div, hsla, prelude::*, px, rgb};
pub const BG: u32 = 0x10151d;
pub const SURFACE: u32 = 0x19212c;
pub const SURFACE_RAISED: u32 = 0x222e3d;
pub const BORDER: u32 = 0x344256;
pub const TEXT: u32 = 0xe9edf5;
pub const TEXT_MUTED: u32 = 0xa4b1c4;
pub const ACCENT: u32 = 0x9bdac7;
pub const ACCENT_TEXT: u32 = 0x12251f;
pub const WARNING: u32 = 0xf2ce8e;
pub const SUCCESS: u32 = 0x9bdac7;
pub const MAP_BG: u32 = 0x101923;
pub const OTHER_TILE: u32 = 0x3c4857;
pub const SELECTION: u32 = 0x9bdac740;
pub const SPACE_5: f32 = 20.;
pub const SPACE_6: f32 = 24.;
pub const RADIUS_1: f32 = 4.;
pub const RADIUS_3: f32 = 10.;
pub const TEXT_XS: f32 = 11.;
pub const TEXT_SM: f32 = 13.;
pub const TEXT_BASE: f32 = 14.;
pub const TEXT_2XL: f32 = 28.;
#[derive(Clone, Copy)]
pub enum TileState {
    Normal,
    Hovered,
    Selected,
}
pub fn tile_color(family: usize, depth: u8, is_directory: bool, state: TileState) -> Hsla {
    let light = (0.30 + 0.07 * f32::from(depth)).min(0.58)
        + if matches!(state, TileState::Hovered) {
            0.06
        } else {
            0.
        };
    hsla(
        (family % 8) as f32 / 8.,
        if is_directory { 0.42 } else { 0.22 },
        light,
        1.,
    )
}
pub fn button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    enabled: bool,
    primary: bool,
) -> Stateful<Div> {
    let label = label.into();
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label.clone())
        .tab_stop(enabled)
        .focusable()
        .flex_shrink_0()
        .h(px(30.))
        .max_w(px(240.))
        .overflow_hidden()
        .text_ellipsis()
        .px(px(10.))
        .py(px(5.))
        .rounded(px(6.))
        .text_size(px(TEXT_SM))
        .bg(rgb(if primary { ACCENT } else { SURFACE_RAISED }))
        .text_color(rgb(if primary { ACCENT_TEXT } else { TEXT }))
        .opacity(if enabled { 1. } else { 0.4 })
        .when(enabled, |d| d.cursor_pointer())
        .focus(|d| d.border_1().border_color(rgb(ACCENT)))
        .child(label)
}
#[derive(Clone, Copy)]
pub enum StatusKind {
    Progress,
    Warning,
    Good,
    Muted,
}
pub fn status_color(kind: StatusKind) -> u32 {
    match kind {
        StatusKind::Progress => ACCENT,
        StatusKind::Warning => WARNING,
        StatusKind::Good => SUCCESS,
        StatusKind::Muted => TEXT_MUTED,
    }
}
pub fn pill(label: impl Into<SharedString>, kind: StatusKind) -> Div {
    div()
        .flex_shrink_0()
        .px(px(6.))
        .py(px(2.))
        .rounded(px(RADIUS_1))
        .bg(rgb(SURFACE_RAISED))
        .text_size(px(TEXT_XS))
        .text_color(rgb(status_color(kind)))
        .child(label.into())
}
pub fn text_button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    enabled: bool,
) -> Stateful<Div> {
    button(id, label, enabled, false)
        .bg(rgb(BG))
        .text_color(rgb(TEXT_MUTED))
}
struct Tooltip(SharedString);
impl gpui::Render for Tooltip {
    fn render(
        &mut self,
        _: &mut gpui::Window,
        _: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        div()
            .p(px(8.))
            .rounded(px(RADIUS_1))
            .bg(rgb(SURFACE_RAISED))
            .text_color(rgb(TEXT))
            .text_size(px(TEXT_XS))
            .child(self.0.clone())
    }
}
pub fn tooltip(
    text: impl Into<SharedString>,
) -> impl Fn(&mut gpui::Window, &mut gpui::App) -> gpui::AnyView {
    let text = text.into();
    move |_, cx| cx.new(|_| Tooltip(text.clone())).into()
}
#[cfg(test)]
mod tests {
    #[test]
    fn s1_tokens_and_s2_no_suppressed_dead_code() {
        for entry in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/src")).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "rs") || path.file_name().unwrap() == "theme.rs"
            {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            for needle in [
                "rgb(0x",
                "rgba(0x",
                "hsla(",
                concat!("allow(", "dead_code)"),
                concat!("allow(", "unused"),
                concat!("expect(", "dead_code)"),
                concat!("expect(", "unused"),
            ] {
                assert!(
                    !text.contains(needle),
                    "{} contains {needle}",
                    path.display()
                );
            }
        }
    }
}
