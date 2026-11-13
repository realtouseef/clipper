//! The region selector: a full-screen transparent overlay on one display. Outside the
//! selection is dimmed. Drag to draw a rectangle; then drag inside it to move it or drag one
//! of the 8 handles to resize it. A readout shows the size in physical pixels. Enter or
//! Confirm sets the region, Esc or Cancel closes.
//!
//! Also the recording border: a thin click-through outline drawn just outside the region
//! while recording. Neither window is ever captured: the screen recorder leaves out every
//! Clipper window (and the border sits outside the region anyway).

use crate::theme as tk;
use crate::region::{Display, Region};
use crate::{Clipper};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyWindowHandle, App, Bounds, DisplayId, Entity, FocusHandle, FontWeight, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, Pixels, Point, Window, WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, div, point, px, rgb, size,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::ffi::{c_char, c_void};

const HANDLE: f32 = 10.0;

/// What a drag is doing.
#[derive(Clone, Copy, Debug)]
enum Drag {
    Draw { from: (f32, f32) },
    Move { grab: (f32, f32), start: Rect },
    /// Resizing: which edges follow the pointer (-1 = left/top, 1 = right/bottom, 0 = fixed).
    Resize { ex: i8, ey: i8, start: Rect },
}

/// Selection in display points (x, y, w, h).
type Rect = (f32, f32, f32, f32);

/// Who gets the result: the recording settings, or a screenshot in progress.
pub enum Target {
    Recording(Entity<Clipper>),
    Screenshot(Box<dyn Fn(Option<Region>, &mut App)>),
}

pub struct Selector {
    target: Option<Target>,
    /// Smallest side in physical pixels.
    min: u32,
    display: Display,
    rect: Option<Rect>,
    drag: Option<Drag>,
    focus: FocusHandle,
}

/// Opens the selector on `display`, starting from `initial` (the last region) if it's on it.
pub fn open(clipper: Entity<Clipper>, display: Display, initial: Option<Region>, cx: &mut App) -> Option<AnyWindowHandle> {
    open_for(Target::Recording(clipper), crate::region::MIN, display, initial, cx)
}

/// Opens the selector for a screenshot: `done` gets the region (any size from `min` px), or
/// `None` if cancelled.
pub fn open_screenshot(display: Display, done: Box<dyn Fn(Option<Region>, &mut App)>, cx: &mut App) -> Option<AnyWindowHandle> {
    open_for(Target::Screenshot(done), 8, display, None, cx)
}

fn open_for(target: Target, min: u32, display: Display, initial: Option<Region>, cx: &mut App) -> Option<AnyWindowHandle> {
    let gd = cx.find_display(DisplayId::new(display.id as u64))?;
    let bounds = gd.bounds();
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
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
    let rect = initial.filter(|r| r.display == display.id).map(|r| {
        let (x, y, w, h) = r.to_points(&display);
        (x as f32, y as f32, w as f32, h as f32)
    });
    gpui_kit::open_window(options, cx, |window, cx| {
        // NSScreenSaverWindowLevel: above the menu bar. Re-framed to the whole display (going
        // borderless lets AppKit resize the window).
        let frame = (display.origin.0, display.origin.1, display.size.0, display.size.1);
        overlay_window(window, 1000, false, frame);
        cx.new(|cx| {
            let focus = cx.focus_handle();
            focus.focus(window, cx);
            Selector { target: Some(target), min, display, rect, drag: None, focus }
        })
    })
    .ok()
    .map(|(handle, _)| {
        clear_root(handle, cx);
        handle
    })
}

impl Selector {
    fn min_pt(&self) -> f32 {
        (self.min as f64 / self.display.scale) as f32
    }

    /// The selection clamped to the display, at least the minimum size.
    fn clamp(&self, (x, y, w, h): Rect) -> Rect {
        let (dw, dh) = (self.display.size.0 as f32, self.display.size.1 as f32);
        let m = self.min_pt();
        let w = w.max(m).min(dw);
        let h = h.max(m).min(dh);
        (x.clamp(0.0, dw - w), y.clamp(0.0, dh - h), w, h)
    }

    fn region(&self) -> Option<Region> {
        let (x, y, w, h) = self.rect?;
        Some(Region::from_points_min(&self.display, (x as f64, y as f64), ((x + w) as f64, (y + h) as f64), self.min))
    }

    /// Which handle (edge directions) is under `p`, if any.
    fn handle_at(&self, p: (f32, f32)) -> Option<(i8, i8)> {
        let (x, y, w, h) = self.rect?;
        let xs = [(x, -1i8), (x + w / 2.0, 0), (x + w, 1)];
        let ys = [(y, -1i8), (y + h / 2.0, 0), (y + h, 1)];
        for &(hx, ex) in &xs {
            for &(hy, ey) in &ys {
                if (ex, ey) != (0, 0) && (p.0 - hx).abs() <= HANDLE && (p.1 - hy).abs() <= HANDLE {
                    return Some((ex, ey));
                }
            }
        }
        None
    }

    fn on_down(&mut self, ev: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let p = (f32::from(ev.position.x), f32::from(ev.position.y));
        self.drag = Some(match (self.handle_at(p), self.rect) {
            (Some((ex, ey)), Some(r)) => Drag::Resize { ex, ey, start: r },
            (None, Some(r)) if p.0 >= r.0 && p.0 <= r.0 + r.2 && p.1 >= r.1 && p.1 <= r.1 + r.3 => {
                Drag::Move { grab: p, start: r }
            }
            _ => Drag::Draw { from: p },
        });
        cx.notify();
    }

    fn on_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.drag else { return };
        if ev.pressed_button != Some(MouseButton::Left) {
            self.drag = None;
            return;
        }
        let p = (f32::from(ev.position.x), f32::from(ev.position.y));
        let r = match drag {
            Drag::Draw { from } => {
                let (x0, x1) = (from.0.min(p.0), from.0.max(p.0));
                let (y0, y1) = (from.1.min(p.1), from.1.max(p.1));
                (x0, y0, x1 - x0, y1 - y0)
            }
            Drag::Move { grab, start } => (start.0 + p.0 - grab.0, start.1 + p.1 - grab.1, start.2, start.3),
            Drag::Resize { ex, ey, start: (x, y, w, h) } => {
                let (mut x0, mut y0, mut x1, mut y1) = (x, y, x + w, y + h);
                match ex {
                    -1 => x0 = p.0.min(x1 - self.min_pt()),
                    1 => x1 = p.0.max(x0 + self.min_pt()),
                    _ => {}
                }
                match ey {
                    -1 => y0 = p.1.min(y1 - self.min_pt()),
                    1 => y1 = p.1.max(y0 + self.min_pt()),
                    _ => {}
                }
                (x0, y0, x1 - x0, y1 - y0)
            }
        };
        // While drawing, show the real (pre-minimum) size; clamp once it's big enough.
        self.rect = Some(match drag {
            Drag::Draw { .. } if r.2 < self.min_pt() || r.3 < self.min_pt() => r,
            _ => self.clamp(r),
        });
        cx.notify();
    }

    fn on_up(&mut self, cx: &mut Context<Self>) {
        if self.drag.take().is_some() {
            self.rect = self.rect.map(|r| self.clamp(r));
            cx.notify();
        }
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let region = self.region();
        self.finish(region, window, cx);
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finish(None, window, cx);
    }

    fn finish(&mut self, region: Option<Region>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.target.take() else { return };
        window.remove_window();
        match target {
            Target::Recording(clipper) => {
                if let Some(r) = &region {
                    crate::region::save_last(r);
                }
                clipper.update(cx, |c, cx| c.region_selected(region, cx));
            }
            // After this window is gone, so it can't be in the shot.
            Target::Screenshot(done) => cx.defer(move |cx| done(region, cx)),
        }
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match ev.keystroke.key.as_str() {
            "escape" => self.cancel(window, cx),
            "enter" if self.rect.is_some() => self.confirm(window, cx),
            _ => {}
        }
    }
}

impl Render for Selector {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let vh = f32::from(window.viewport_size().height);
        let dim = tk::scrim(0.5);
        let shade = || div().absolute().bg(dim);
        let root = div()
            .size_full()
            .relative()
            .track_focus(&self.focus)
            .cursor_crosshair()
            .on_key_down(cx.listener(Self::on_key))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_down))
            .on_mouse_move(cx.listener(Self::on_move))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| this.on_up(cx)));
        let Some((x, y, w, h)) = self.rect else {
            return root.child(shade().inset_0()).child(
                div().absolute().inset_0().flex().items_center().justify_center().child(
                    div()
                        .px_4()
                        .py_2()
                        .rounded_full()
                        .bg(tk::alpha(tk::DARK.paper, 0.92))
                        .text_size(px(tk::T13))
                        .font_family(tk::font())
                        .text_color(rgb(tk::DARK.ink))
                        .child("Drag to select the area to record. Press Esc to cancel."),
                ),
            );
        };
        let region = self.region();
        let label = region.map(|r| r.label()).unwrap_or_default();
        let handles = [(-1i8, -1i8), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)].map(|(ex, ey)| {
            let hx = x + w * (ex as f32 + 1.0) / 2.0;
            let hy = y + h * (ey as f32 + 1.0) / 2.0;
            div()
                .absolute()
                .left(px(hx - HANDLE / 2.0))
                .top(px(hy - HANDLE / 2.0))
                .size(px(HANDLE))
                .rounded(px(3.))
                .bg(rgb(tk::WHITE))
                .border_1()
                .border_color(rgb(tk::DARK.paper))
        });
        // Readout and buttons sit below the selection, or above it near the bottom edge.
        let below = y + h + 56.0 < vh;
        let bar_y = if below { y + h + 10.0 } else { (y - 46.0).max(4.0) };
        let bar = div()
            .absolute()
            .left(px((x + w - 260.0).max(4.0)))
            .top(px(bar_y))
            .w(px(260.))
            .flex()
            .items_center()
            .justify_end()
            .gap_2()
            .child(
                div()
                    .px_3()
                    .py_1p5()
                    .rounded_full()
                    .bg(tk::alpha(tk::DARK.paper, 0.92))
                    .text_size(px(tk::T12))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(tk::DARK.ink))
                    .child(label),
            )
            .child(
                div()
                    .id("sel-cancel")
                    .px_3()
                    .py_1p5()
                    .rounded_full()
                    .bg(tk::alpha(tk::DARK.paper, 0.92))
                    .text_size(px(tk::T12))
                    .text_color(rgb(tk::DARK.ink))
                    .cursor_pointer()
                    .child("Cancel")
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _, window, cx| this.cancel(window, cx))),
            )
            .child(
                div()
                    .id("sel-confirm")
                    .px_3()
                    .py_1p5()
                    .rounded_full()
                    .bg(rgb(tk::DARK.ink))
                    .text_size(px(tk::T12))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(tk::DARK.paper))
                    .cursor_pointer()
                    .child("Confirm")
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _, window, cx| this.confirm(window, cx))),
            );
        // Dim everything around the selection (bands anchored to the window edges).
        root.child(shade().left_0().right_0().top_0().h(px(y.max(0.))))
            .child(shade().left_0().right_0().bottom_0().top(px(y + h)))
            .child(shade().left_0().top(px(y)).h(px(h)).w(px(x.max(0.))))
            .child(shade().right_0().top(px(y)).h(px(h)).left(px(x + w)))
            .child(
                div()
                    .absolute()
                    .left(px(x - 1.0))
                    .top(px(y - 1.0))
                    .w(px(w + 2.0))
                    .h(px(h + 2.0))
                    .border_2()
                    .border_color(rgb(tk::DARK.ink))
                    .cursor_move(),
            )
            .children(handles)
            .when(self.drag.is_none(), |d| d.child(bar))
    }
}

// ---------- recording border ----------

pub struct RegionBorder;

impl Render for RegionBorder {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        // The window is the region grown by `BORDER_GAP`; the outline is in that margin, so
        // it stays outside the recorded area.
        div().size_full().p(px(BORDER_GAP - 3.0)).child(
            // The recording indicator around the region: coral.
            div().size_full().border_2().border_color(tk::alpha(tk::DARK.coral, 0.9)).rounded(px(4.)),
        )
    }
}

const BORDER_GAP: f32 = 4.0;

/// Shows a thin red outline just outside `region` (click-through, above other windows).
pub fn open_border(display: Display, region: Region, cx: &mut App) -> Option<AnyWindowHandle> {
    let gd = cx.find_display(DisplayId::new(display.id as u64))?;
    let db = gd.bounds();
    let (x, y, w, h) = region.to_points(&display);
    let origin: Point<Pixels> = point(db.origin.x + px(x as f32 - BORDER_GAP), db.origin.y + px(y as f32 - BORDER_GAP));
    let bounds = Bounds::new(origin, size(px(w as f32 + 2.0 * BORDER_GAP), px(h as f32 + 2.0 * BORDER_GAP)));
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: None,
        focus: false,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        display_id: Some(gd.id()),
        window_background: WindowBackgroundAppearance::Transparent,
        ..Default::default()
    };
    gpui_kit::open_window(options, cx, |window, cx| {
        let frame = (
            display.origin.0 + x - BORDER_GAP as f64,
            display.origin.1 + y - BORDER_GAP as f64,
            w + 2.0 * BORDER_GAP as f64,
            h + 2.0 * BORDER_GAP as f64,
        );
        overlay_window(window, 3, true, frame); // NSFloatingWindowLevel, click-through
        cx.new(|_| RegionBorder)
    })
    .ok()
    .map(|(handle, _)| {
        clear_root(handle, cx);
        handle
    })
}

/// The kit's root paints the theme background; clear it so only our content shows.
pub(crate) fn clear_root(handle: AnyWindowHandle, cx: &mut App) {
    if let Some(root) = handle.downcast::<gpui_kit::base::Root>() {
        // Redraw: a static window (the border) would otherwise keep its first, opaque frame.
        let _ = root.update(cx, |root, window, cx| {
            root.style().background = Some(gpui_kit::transparent_black().into());
            cx.notify();
            window.refresh();
        });
    }
}

// ---------- AppKit ----------

#[link(name = "objc")]
unsafe extern "C" {
    fn sel_registerName(name: *const c_char) -> *mut c_void;
    fn objc_msgSend();
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn _CGSDefaultConnection() -> i32;
    fn CGSSetConnectionProperty(cid: i32, target: i32, key: *const c_void, value: *const c_void) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFBooleanTrue: *const c_void;
    fn CFStringCreateWithCString(alloc: *const c_void, s: *const c_char, encoding: u32) -> *const c_void;
}

/// Lets Clipper change the mouse pointer while another app is frontmost. The overlays
/// (screenshot cards, selector) never activate Clipper, and without this macOS ignores
/// their hand/resize cursors. The same switch other screenshot tools use.
fn allow_background_cursor() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        let key = CFStringCreateWithCString(std::ptr::null(), c"SetsCursorInBackground".as_ptr(), 0x0800_0100); // UTF-8
        if !key.is_null() {
            let cid = _CGSDefaultConnection();
            CGSSetConnectionProperty(cid, cid, key, kCFBooleanTrue);
        }
    });
}

/// Borderless, no shadow, at window `level`, hidden from screen capture, optionally
/// click-through, placed at `frame` (x, y, w, h in global top-left points).
pub(crate) fn overlay_window(window: &Window, level: isize, click_through: bool, frame: (f64, f64, f64, f64)) {
    allow_background_cursor();
    let Ok(handle) = HasWindowHandle::window_handle(window) else { return };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else { return };
    unsafe {
        let send_id: extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let send_uint: extern "C" fn(*mut c_void, *mut c_void, usize) =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let send_int: extern "C" fn(*mut c_void, *mut c_void, isize) =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let send_bool: extern "C" fn(*mut c_void, *mut c_void, bool) =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let w = send_id(h.ns_view.as_ptr(), sel_registerName(c"window".as_ptr()));
        if w.is_null() {
            return;
        }
        send_uint(w, sel_registerName(c"setStyleMask:".as_ptr()), 1 << 7); // borderless, non-activating
        send_bool(w, sel_registerName(c"setHasShadow:".as_ptr()), false);
        send_int(w, sel_registerName(c"setLevel:".as_ptr()), level);
        // Dev escape hatch for screenshotting the overlays.
        if !(cfg!(debug_assertions) && std::env::var_os("CLIPPER_BAR_CAPTURABLE").is_some()) {
            send_uint(w, sel_registerName(c"setSharingType:".as_ptr()), 0); // NSWindowSharingNone
        }
        if click_through {
            send_bool(w, sel_registerName(c"setIgnoresMouseEvents:".as_ptr()), true);
        }
        // AppKit frames are bottom-left based, measured from the main display's bottom.
        #[repr(C)]
        struct NSRect {
            x: f64,
            y: f64,
            w: f64,
            h: f64,
        }
        let main_h = crate::cursor::displays().first().map_or(frame.3, |d| d.size.1);
        let rect = NSRect { x: frame.0, y: main_h - (frame.1 + frame.3), w: frame.2, h: frame.3 };
        let send_frame: extern "C" fn(*mut c_void, *mut c_void, NSRect, bool) =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        send_frame(w, sel_registerName(c"setFrame:display:".as_ptr()), rect, true);
    }
}
