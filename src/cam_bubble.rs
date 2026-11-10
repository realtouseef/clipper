//! The live webcam bubble shown while recording: a round view of the camera at the
//! bottom-right of the screen, so you can see yourself. It is never in the screen recording
//! (Clipper's windows are left out of the capture); the webcam has its own track and is placed
//! after recording. Drag it anywhere to get it out of the way.
//!
//! It runs its own small camera feed (macOS lets several clients share a camera) and keeps
//! its own frames, so it works while the main window is minimized.

use crate::Clipper;
use crate::ui;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyWindowHandle, App, Entity, FontWeight, MouseButton, MouseDownEvent, MouseMoveEvent, ObjectFit, RenderImage, Window,
    WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, div, hsla, img, point, px, rgb, size,
};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

/// Bubble diameter, and room around it for the shadow.
const D: f32 = 196.0;
const PAD: f32 = 14.0;
const WIN: f32 = D + 2.0 * PAD;
/// Gap to the right edge and above the Dock / control bar.
const RIGHT: f32 = 24.0;
const BOTTOM: f32 = 24.0;
/// Restart a feed that hasn't shown a frame by then.
const NO_FRAME: Duration = Duration::from_secs(4);
/// Wait before retrying a feed, and after the webcam is turned back on.
const RETRY: Duration = Duration::from_secs(2);
const REOPEN_DELAY: Duration = Duration::from_millis(1200);

/// The bubble's camera feed: waiting to (re)open, or running.
enum Feed {
    /// Not running; opens once `until` has passed (`None`: right away).
    Waiting { until: Option<Instant> },
    /// Running since `since` (restarted if no frame arrives in time).
    Running { live: crate::webcam::Live, since: Instant },
}

pub struct CamBubble {
    clipper: Entity<Clipper>,
    device: String,
    mirror: Arc<AtomicBool>,
    feed: Feed,
    frame: Option<Arc<RenderImage>>,
    stale: Vec<Arc<RenderImage>>,
    /// Feeds that failed in a row.
    failures: u32,
    /// While dragging: pointer and window origin (global points) at the press.
    drag: Option<((f64, f64), (f64, f64))>,
    origin: (f64, f64),
}

/// Opens the bubble at the bottom-right of the primary display.
pub fn open(clipper: Entity<Clipper>, device: String, mirror: Arc<AtomicBool>, cx: &mut App) -> Option<AnyWindowHandle> {
    let display = cx.primary_display()?;
    let visible = display.visible_bounds();
    let x = f32::from(visible.origin.x + visible.size.width) - WIN - RIGHT + PAD;
    let y = f32::from(visible.origin.y + visible.size.height) - WIN - BOTTOM + PAD;
    let origin = (x as f64, y as f64);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(gpui_kit::Bounds::new(point(px(x), px(y)), size(px(WIN), px(WIN))))),
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
    let (handle, _) = gpui_kit::open_window(options, cx, move |window, cx| {
        crate::selector::overlay_window(window, 3, false, (origin.0, origin.1, WIN as f64, WIN as f64));
        let entity = cx.new(|cx| {
            cx.observe(&clipper, |_, _, cx| cx.notify()).detach();
            CamBubble { clipper, device, mirror, feed: Feed::Waiting { until: None }, frame: None, stale: Vec::new(), failures: 0, drag: None, origin }
        });
        // Redraw at the feed's rate.
        let weak = entity.downgrade();
        window
            .spawn(cx, async move |cx| {
                loop {
                    cx.background_executor().timer(Duration::from_millis(40)).await;
                    let Some(e) = weak.upgrade() else { break };
                    if cx.update(|_, cx| e.update(cx, |_, cx| cx.notify())).is_err() {
                        break;
                    }
                }
            })
            .detach();
        entity
    })
    .ok()?;
    crate::selector::clear_root(handle, cx);
    Some(handle)
}

pub fn close(handle: AnyWindowHandle, cx: &mut App) {
    let _ = handle.update(cx, |_, window, _| window.remove_window());
}

impl CamBubble {
    /// Keeps the feed running while the recording's webcam is on, and stopped while it's off.
    /// A feed that fails or shows nothing is retried (the recorder may still be reopening the
    /// camera), so turning the webcam off and on again brings the preview back.
    fn sync(&mut self, on: bool) {
        let now = Instant::now();
        if !on {
            self.failures = 0;
            // Back on: give the recorder a moment to reopen the camera first.
            self.feed = Feed::Waiting { until: Some(now + REOPEN_DELAY) };
            if let Some(f) = self.frame.take() {
                self.stale.push(f);
            }
            return;
        }
        match &mut self.feed {
            Feed::Running { live, since } => {
                let stuck = self.frame.is_none() && now - *since > NO_FRAME;
                if live.failed() || stuck {
                    self.failures += 1;
                    self.feed = Feed::Waiting { until: Some(now + RETRY) };
                }
            }
            Feed::Waiting { until } if until.is_some_and(|t| now < t) => {}
            Feed::Waiting { .. } => match crate::webcam::backend().live(&self.device, self.mirror.clone()) {
                Ok(live) => self.feed = Feed::Running { live, since: now },
                Err(_) => {
                    self.failures += 1;
                    self.feed = Feed::Waiting { until: Some(now + RETRY) };
                }
            },
        }
    }
}

impl Render for CamBubble {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let on = self.clipper.read(cx).recorder.as_ref().is_some_and(|r| r.webcam_on());
        self.sync(on);
        let fresh = match &mut self.feed {
            Feed::Running { live, .. } => live.take(),
            Feed::Waiting { .. } => None,
        };
        if let Some(bgra) = fresh {
            let next = Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(bgra)]));
            if let Some(old) = self.frame.replace(next) {
                self.stale.push(old);
            }
        }
        for old in self.stale.drain(..) {
            let _ = window.drop_image(old);
        }

        let label = |text: &'static str| {
            div().size_full().flex().flex_col().items_center().justify_center().gap_2().text_xs().font_weight(FontWeight::MEDIUM).text_color(rgb(ui::MUTED))
                .child(crate::editor::icons::camera().size(px(22.)))
                .child(text)
        };
        let content = match (&self.frame, on) {
            (_, false) => label("Camera off").into_any_element(),
            // The image is rounded itself: element clipping is rectangular.
            (Some(f), true) => img(f.clone()).size_full().rounded_full().object_fit(ObjectFit::Cover).into_any_element(),
            (None, true) if self.failures >= 3 => label("Reconnecting camera…").into_any_element(),
            (None, true) => label("Starting camera…").into_any_element(),
        };
        let dragging = self.drag.is_some();
        div().size_full().p(px(PAD)).child(
            div()
                .id("bubble")
                .size(px(D))
                .rounded_full()
                .overflow_hidden()
                .bg(rgb(ui::SURFACE))
                .border_2()
                .border_color(hsla(0., 0., 1.0, if dragging { 0.5 } else { 0.22 }))
                .shadow(vec![gpui_kit::BoxShadow {
                    color: hsla(0., 0., 0., 0.35),
                    offset: point(px(0.), px(3.)),
                    blur_radius: px(8.),
                    spread_radius: px(0.),
                    inset: false,
                }])
                .cursor(if dragging { gpui_kit::CursorStyle::ClosedHand } else { gpui_kit::CursorStyle::OpenHand })
                .child(content)
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.drag = Some((crate::cursor::cursor_location(), this.origin));
                    cx.notify();
                }))
                .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, window, _| {
                    let Some((start, from)) = this.drag else { return };
                    if ev.pressed_button != Some(MouseButton::Left) {
                        this.drag = None;
                        return;
                    }
                    // Global pointer (the window moves under the local coordinates).
                    let now = crate::cursor::cursor_location();
                    this.origin = (from.0 + now.0 - start.0, from.1 + now.1 - start.1);
                    crate::selector::overlay_window(window, 3, false, (this.origin.0, this.origin.1, WIN as f64, WIN as f64));
                }))
                .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| {
                    this.drag = None;
                    cx.notify();
                })),
        )
    }
}
