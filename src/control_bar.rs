//! Floating recording control bar: a small always-on-top pill just above the Dock with
//! elapsed time, Stop, Pause/Resume, Mute and (when recording the webcam) Webcam on/off.
//!
//! Keeping it out of the recording: the screen recorder (ScreenCaptureKit) leaves out every
//! Clipper window, and the bar is also marked `NSWindowSharingNone` so screenshots and other
//! capture tools skip it.

use crate::theme as tk;
use crate::{Clipper};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{Disableable, Icon};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyWindowHandle, App, Bounds, ClickEvent, Entity, FontWeight, Window,
    WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, div, point, px, rgb, size,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::ffi::{c_char, c_void};

const W: f32 = 268.;
/// Extra width for the webcam button.
const W_CAM: f32 = 48.;
const H: f32 = 60.;
/// Room around the pill for its shadow (the window itself is transparent).
const INSET: f32 = 6.;
/// Gap between the bar and the top of the Dock (or the bottom of the screen).
const BOTTOM_GAP: f32 = 10.;



/// What the bar shows; read from `Clipper` on every render.
pub struct BarState {
    pub starting: bool,
    pub paused: bool,
    pub muted: bool,
    pub has_audio: bool,
    /// This recording has a webcam, and whether it's on right now.
    pub has_webcam: bool,
    pub webcam_on: bool,
    /// The webcam was on and stopped mid-recording.
    pub cam_lost: bool,
    pub secs: u64,
    pub blink_on: bool,
    pub pause_key: String,
    pub stop_key: String,
}

pub struct ControlBar {
    clipper: Entity<Clipper>,
    /// Opened with room for the webcam button.
    webcam: bool,
}

/// Opens the bar centered above the Dock. `webcam` adds the webcam button.
pub fn open(clipper: Entity<Clipper>, webcam: bool, cx: &mut App) -> Option<AnyWindowHandle> {
    let display = cx.primary_display()?;
    // `visible_bounds` excludes the menu bar and the Dock.
    let area = display.bounds();
    let visible = display.visible_bounds();
    let pill_w = W + if webcam { W_CAM } else { 0. };
    let (w, h) = (pill_w + INSET * 2., H + INSET * 2.);
    let bottom = visible.origin.y + visible.size.height;
    let origin = point(
        visible.origin.x + (visible.size.width - px(w)) / 2.,
        bottom - px(h) - px(BOTTOM_GAP),
    );
    // Distance from the bar's bottom edge to the bottom of the display (AppKit's origin).
    let appkit_y = f64::from(f32::from(area.origin.y + area.size.height - bottom)) + BOTTOM_GAP as f64;
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(origin, size(px(w), px(h))))),
        titlebar: None,
        focus: false,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        display_id: Some(display.id()),
        window_background: WindowBackgroundAppearance::Transparent,
        ..Default::default()
    };
    gpui_kit::open_window(options, cx, |window, cx| {
        // AppKit coordinates: origin at the bottom-left of the primary display.
        let x = f64::from(f32::from(origin.x));
        make_borderless(window, (x, appkit_y));
        // Dev escape hatch for screenshotting the bar itself.
        if !(cfg!(debug_assertions) && std::env::var_os("CLIPPER_BAR_CAPTURABLE").is_some()) {
            exclude_from_capture(window);
        }
        cx.new(|cx| {
            cx.observe(&clipper, |_, _, cx| cx.notify()).detach();
            ControlBar { clipper, webcam }
        })
    })
    .ok()
    .map(|(handle, _)| {
        // Kit's root paints the theme background; clear it so only the pill is visible.
        if let Some(root) = handle.downcast::<gpui_kit::base::Root>() {
            let _ = root.update(cx, |root, _, _| {
                root.style().background = Some(gpui_kit::transparent_black().into());
            });
        }
        handle
    })
}

pub fn close(handle: AnyWindowHandle, cx: &mut App) {
    let _ = handle.update(cx, |_, window, _| window.remove_window());
}

impl Render for ControlBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = self.clipper.read(cx).bar_state();
        let live = !s.starting && !s.paused;

        // The recording indicator: a steady coral dot while recording (no blinking), soft
        // ink while paused or starting.
        let _ = s.blink_on;
        let dot = div().size(px(10.)).rounded_full().bg(rgb(if live { tk::coral() } else { tk::ink_soft() }));
        let time = div()
            .w(px(56.))
            .text_size(px(tk::T15))
            .font_weight(FontWeight::BOLD)
            .font_features(crate::ui::tnum())
            .text_color(rgb(if live { tk::text() } else { tk::ink_soft() }))
            .child(if s.starting { "0:00".to_string() } else { format!("{}:{:02}", s.secs / 60, s.secs % 60) });

        let clipper = self.clipper.clone();
        let pause = icon_button("bar-pause", if s.paused { crate::editor::icons::play() } else { crate::editor::icons::pause() }, cx)
            .tooltip(format!("{}  {}", if s.paused { "Resume" } else { "Pause" }, s.pause_key))
            .disabled(s.starting)
            .on_click(move |_: &ClickEvent, _, cx| clipper.update(cx, |c, cx| c.toggle_pause(cx)));

        let clipper = self.clipper.clone();
        let stop = icon_button("bar-stop", crate::editor::icons::stop(), cx)
            .tooltip(format!("Stop  {}", s.stop_key))
            .on_click(move |_: &ClickEvent, _, cx| clipper.update(cx, |c, cx| c.stop_recording(cx)));

        let clipper = self.clipper.clone();
        let mic_tip = if !s.has_audio {
            "No microphone"
        } else if s.muted {
            "Unmute microphone"
        } else {
            "Mute microphone"
        };
        let mute = icon_button("bar-mute", if s.muted { mic_off_icon() } else { Icon::new(gpui_kit::assets::IconName::Mic) }, cx)
            .tooltip(mic_tip)
            .disabled(!s.has_audio)
            .when(s.muted, |b| b.text_color(rgb(tk::error_text())))
            .on_click(move |_: &ClickEvent, _, cx| clipper.update(cx, |c, cx| c.toggle_mute(cx)));

        let clipper = self.clipper.clone();
        let cam = self.webcam.then(|| {
            let icon = if s.webcam_on { video_icon() } else { video_off_icon() };
            let tip = if !s.has_webcam {
                "No camera available"
            } else if s.webcam_on {
                "Turn webcam off"
            } else {
                "Turn webcam on"
            };
            icon_button("bar-webcam", icon, cx)
                .tooltip(tip)
                .disabled(s.starting || !s.has_webcam)
                .when(!s.webcam_on, |b| b.text_color(rgb(tk::error_text())))
                .on_click(move |_: &ClickEvent, _, cx| clipper.update(cx, |c, cx| c.toggle_webcam(cx)))
        });

        div().size_full().p(px(INSET)).child(
            div()
                .size_full()
                .flex()
                .items_center()
                .gap(px(tk::S1))
                .pl(px(tk::S4 + 2.0))
                .pr(px(tk::S2))
                .rounded_full()
                .bg(rgb(tk::paper()))
                .border_1()
                .border_color(crate::ui::hairline())
                .shadow(crate::ui::float_shadow())
                .text_color(rgb(tk::text()))
                .font_family(tk::font())
                .child(
                    div().flex_1().flex().items_center().gap_2().child(dot).child(time).when(s.cam_lost, |d| {
                        // Non-blocking: screen and audio keep recording.
                        d.child(
                            div()
                                .id("cam-lost")
                                .size(px(8.))
                                .rounded_full()
                                .bg(rgb(tk::warning()))
                                .tooltip(|window, cx| {
                                    gpui_kit::component::tooltip::Tooltip::new(
                                        "Webcam disconnected. Still recording screen and audio.",
                                    )
                                    .build(window, cx)
                                }),
                        )
                    }),
                )
                .child(stop)
                .child(pause)
                .child(mute)
                .children(cam),
        )
    }
}

fn icon_button(id: &'static str, icon: impl Into<Icon>, _: &App) -> Button {
    Button::new(id).cursor_pointer().ghost().rounded_full().size(px(40.)).icon(icon.into())
}

/// Lucide `video` / `video-off` (ISC).
fn video_icon() -> Icon {
    Icon::default().data(include_bytes!("../assets/icons/editor/video.svg"))
}

fn video_off_icon() -> Icon {
    Icon::default().data(include_bytes!("../assets/icons/editor/video-off.svg"))
}

/// Lucide `mic-off` (ISC). Not part of GPUI Kit's default asset bundle, so it's embedded here.
fn mic_off_icon() -> Icon {
    Icon::default().data(include_bytes!("../assets/icons/editor/mic-off.svg"))
}

// ---------- capture exclusion ----------

#[link(name = "objc")]
unsafe extern "C" {
    fn sel_registerName(name: *const c_char) -> *mut c_void;
    fn objc_msgSend();
}

/// Sets `NSWindowSharingNone` on the window, which removes it from screen captures.
fn exclude_from_capture(window: &Window) {
    let Ok(handle) = HasWindowHandle::window_handle(window) else { return };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else { return };
    unsafe {
        let send_id: extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let send_uint: extern "C" fn(*mut c_void, *mut c_void, usize) =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let ns_window = send_id(h.ns_view.as_ptr(), sel_registerName(c"window".as_ptr()));
        if !ns_window.is_null() {
            send_uint(ns_window, sel_registerName(c"setSharingType:".as_ptr()), 0);
        }
    }
}

/// Makes the panel borderless (a titled panel gets an opaque system frame behind our
/// transparent content) and drops the system shadow; the pill draws its own.
fn make_borderless(window: &Window, appkit_origin: (f64, f64)) {
    let Ok(handle) = HasWindowHandle::window_handle(window) else { return };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else { return };
    unsafe {
        let send_id: extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let send_uint: extern "C" fn(*mut c_void, *mut c_void, usize) =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let send_bool: extern "C" fn(*mut c_void, *mut c_void, bool) =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let ns_window = send_id(h.ns_view.as_ptr(), sel_registerName(c"window".as_ptr()));
        if !ns_window.is_null() {
            // NSWindowStyleMaskBorderless | NSWindowStyleMaskNonactivatingPanel
            send_uint(ns_window, sel_registerName(c"setStyleMask:".as_ptr()), 1 << 7);
            send_bool(ns_window, sel_registerName(c"setHasShadow:".as_ptr()), false);
            // Re-place it: AppKit nudged the titled panel up at creation; a borderless one
            // can sit flush at the bottom (inside the strip the recorder crops away).
            #[repr(C)]
            struct NSPoint {
                x: f64,
                y: f64,
            }
            let send_point: extern "C" fn(*mut c_void, *mut c_void, NSPoint) =
                std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let (x, y) = appkit_origin;
            send_point(ns_window, sel_registerName(c"setFrameOrigin:".as_ptr()), NSPoint { x, y });
        }
    }
}
