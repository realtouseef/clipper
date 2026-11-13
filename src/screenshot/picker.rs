//! Window picker: a transparent overlay on one display that highlights the window under the
//! pointer. Click captures it, Esc cancels. Like the region selector, it is hidden from
//! screen capture and closes before the shot is taken.

use crate::theme as tk;
use super::capture::{WindowInfo, window_at};
use crate::region::Display;

use gpui_kit::prelude::*;
use gpui_kit::{
    AnyWindowHandle, App, DisplayId, FocusHandle, FontWeight, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    Window, WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, div, px, rgb,
};

type Done = Box<dyn Fn(Option<WindowInfo>, &mut App)>;

pub struct Picker {
    display: Display,
    windows: Vec<WindowInfo>,
    hover: Option<WindowInfo>,
    focus: FocusHandle,
    done: Option<Done>,
}

pub fn open(display: Display, windows: Vec<WindowInfo>, done: Done, cx: &mut App) -> Option<AnyWindowHandle> {
    let gd = cx.find_display(DisplayId::new(display.id as u64))?;
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(gd.bounds())),
        titlebar: None,
        focus: true,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        display_id: Some(gd.id()),
        window_background: WindowBackgroundAppearance::Transparent,
        ..Default::default()
    };
    let (handle, _) = gpui_kit::open_window(options, cx, move |window, cx| {
        crate::selector::overlay_window(window, 1000, false, (display.origin.0, display.origin.1, display.size.0, display.size.1));
        let hover = window_at(&windows, crate::cursor::cursor_location()).cloned();
        cx.new(|cx| {
            let focus = cx.focus_handle();
            focus.focus(window, cx);
            Picker { display, windows, hover, focus, done: Some(done) }
        })
    })
    .ok()?;
    crate::selector::clear_root(handle, cx);
    Some(handle)
}

impl Picker {
    fn global(&self, local: (f32, f32)) -> (f64, f64) {
        (self.display.origin.0 + local.0 as f64, self.display.origin.1 + local.1 as f64)
    }

    fn finish(&mut self, pick: Option<WindowInfo>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(done) = self.done.take() {
            window.remove_window();
            cx.defer(move |cx| done(pick, cx));
        }
    }
}

impl Render for Picker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = div()
            .size_full()
            .relative()
            .track_focus(&self.focus)
            .cursor_pointer()
            .bg(tk::scrim(0.18))
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                let g = this.global((f32::from(ev.position.x), f32::from(ev.position.y)));
                let hover = window_at(&this.windows, g).cloned();
                if hover != this.hover {
                    this.hover = hover;
                    cx.notify();
                }
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                let g = this.global((f32::from(ev.position.x), f32::from(ev.position.y)));
                let pick = window_at(&this.windows, g).cloned();
                if pick.is_some() {
                    this.finish(pick, window, cx);
                }
            }))
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                if ev.keystroke.key == "escape" {
                    this.finish(None, window, cx);
                }
            }));
        let hint = div()
            .absolute()
            .top(px(24.))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(
                div()
                    .px_4()
                    .py_2()
                    .rounded_full()
                    .bg(tk::alpha(tk::DARK.paper, 0.92))
                    .text_size(px(tk::T13))
                    .font_family(tk::font())
                    .text_color(rgb(tk::DARK.ink))
                    .child("Click a window to capture it. Press Esc to cancel."),
            );
        let Some(w) = &self.hover else { return root.child(hint) };
        let (x, y) = ((w.x - self.display.origin.0) as f32, (w.y - self.display.origin.1) as f32);
        let label = if w.title.is_empty() { w.app.clone() } else { format!("{}: {}", w.app, w.title) };
        let px_size = format!("{} × {}", (w.w * self.display.scale).round(), (w.h * self.display.scale).round());
        root.child(
            div()
                .absolute()
                .left(px(x))
                .top(px(y))
                .w(px(w.w as f32))
                .h(px(w.h as f32))
                .rounded(px(10.))
                .border_2()
                .border_color(rgb(tk::DARK.ink))
                .bg(tk::alpha(tk::DARK.ink, 0.12))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_1()
                        .px_4()
                        .py_2()
                        .rounded(px(tk::R_PANEL))
                        .bg(rgb(tk::DARK.ink))
                        .text_color(rgb(tk::DARK.paper))
                        .child(div().text_size(px(tk::T13)).font_weight(FontWeight::SEMIBOLD).max_w(px(360.)).truncate().child(label))
                        .child(div().text_size(px(tk::T12)).child(px_size)),
                ),
        )
        .child(hint)
    }
}
