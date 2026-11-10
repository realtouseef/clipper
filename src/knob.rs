//! The settings sliders (see `ui::slider` for the look). Press anywhere on one and drag; the
//! value follows the pointer, even outside the field.

use crate::Clipper;
use gpui_kit::prelude::*;
use gpui_kit::{Bounds, Div, MouseButton, MouseDownEvent, Pixels, Point};
use std::cell::Cell;
use std::rc::Rc;

/// Every slider in the settings panel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Knob {
    Deadzone,
    Smoothing,
    Cursor,
    Padding,
    Radius,
    ShadowBlur,
    ShadowOpacity,
    ZoomIn,
    ZoomOut,
    CamSize,
    CamMargin,
    CamBorder,
    Split,
    ScreenZoom,
    CapSize,
    CapOutline,
    PillOpacity,
    Threads,
    ClickSize,
    ClickDuration,
    WebpQuality,
    SizeLimit,
}

impl Knob {
    pub const COUNT: usize = 22;

    /// (min, max, step).
    fn range(self) -> (f32, f32, f32) {
        match self {
            Knob::Deadzone => (0.0, 0.3, 0.01),
            Knob::Smoothing => (0.02, 0.6, 0.01),
            Knob::Cursor => (0.5, 3.0, 0.1),
            Knob::Padding => (0.0, 200.0, 1.0),
            Knob::Radius => (0.0, 120.0, 1.0),
            Knob::ShadowBlur => (0.0, 120.0, 1.0),
            Knob::ShadowOpacity => (0.0, 1.0, 0.05),
            Knob::ZoomIn | Knob::ZoomOut => (0.1, 1.5, 0.05),
            Knob::CamSize => (0.10, 0.40, 0.01),
            Knob::CamMargin => (0.0, 120.0, 2.0),
            Knob::CamBorder => (0.0, 16.0, 1.0),
            Knob::Split => (crate::layout::MIN_SPLIT, crate::layout::MAX_SPLIT, 0.01),
            Knob::ScreenZoom => (1.0, 2.5, 0.05),
            Knob::CapSize => (crate::captions::render::SIZES.0, crate::captions::render::SIZES.1, 1.0),
            Knob::CapOutline => (0.0, 12.0, 0.5),
            Knob::PillOpacity => (0.1, 1.0, 0.05),
            Knob::Threads => (1.0, crate::captions::transcribe::max_threads() as f32, 1.0),
            Knob::ClickSize => (24.0, 120.0, 2.0),
            Knob::ClickDuration => (0.2, 1.0, 0.05),
            Knob::WebpQuality => (0.0, 100.0, 1.0),
            Knob::SizeLimit => (1.0, 50.0, 1.0),
        }
    }

    fn get(self, c: &Clipper) -> f32 {
        match self {
            Knob::Deadzone => c.settings.deadzone as f32,
            Knob::Smoothing => c.settings.smoothing as f32,
            Knob::Cursor => c.style.cursor_scale,
            Knob::Padding => c.style.padding as f32,
            Knob::Radius => c.style.radius as f32,
            Knob::ShadowBlur => c.style.shadow_blur as f32,
            Knob::ShadowOpacity => c.style.shadow_opacity,
            Knob::ZoomIn => c.motion.zoom_in as f32,
            Knob::ZoomOut => c.motion.zoom_out as f32,
            Knob::CamSize => c.layout.cam_size,
            Knob::CamMargin => c.layout.cam_margin as f32,
            Knob::CamBorder => c.layout.cam_border as f32,
            Knob::Split => c.layout.split,
            Knob::ScreenZoom => c.layout.zoom,
            Knob::CapSize => c.cap_style.size,
            Knob::CapOutline => c.cap_style.outline,
            Knob::PillOpacity => c.cap_style.pill_opacity,
            Knob::Threads => c.cap.threads as f32,
            Knob::ClickSize => c.style.click.size,
            Knob::ClickDuration => c.style.click.duration as f32,
            Knob::WebpQuality => c.export_opts.webp_opts.quality,
            Knob::SizeLimit => c.export_opts.limit_mb,
        }
    }

    fn set(self, c: &mut Clipper, v: f32) {
        match self {
            Knob::Deadzone => c.settings.deadzone = v as f64,
            Knob::Smoothing => c.settings.smoothing = v as f64,
            Knob::Cursor => c.style.cursor_scale = v,
            Knob::Padding => c.style.padding = v.round() as u32,
            Knob::Radius => c.style.radius = v.round() as u32,
            Knob::ShadowBlur => c.style.shadow_blur = v.round() as u32,
            Knob::ShadowOpacity => c.style.shadow_opacity = v,
            Knob::ZoomIn => c.motion.zoom_in = v as f64,
            Knob::ZoomOut => c.motion.zoom_out = v as f64,
            Knob::CamSize => c.layout.cam_size = v,
            Knob::CamMargin => c.layout.cam_margin = (v.round() as u32 / 2) * 2,
            Knob::CamBorder => c.layout.cam_border = v.round() as u32,
            Knob::Split => c.layout.split = v,
            Knob::ScreenZoom => c.layout.zoom = v,
            Knob::CapSize => c.cap_style.size = v,
            Knob::CapOutline => c.cap_style.outline = v,
            Knob::PillOpacity => c.cap_style.pill_opacity = v,
            Knob::Threads => c.cap.threads = v.round() as usize,
            Knob::ClickSize => c.style.click.size = v,
            Knob::ClickDuration => c.style.click.duration = v as f64,
            Knob::WebpQuality => c.export_opts.webp_opts.quality = v,
            Knob::SizeLimit => c.export_opts.limit_mb = v,
        }
    }

    fn display(self, v: f32) -> String {
        match self {
            Knob::Deadzone | Knob::ShadowOpacity => format!("{:.0}%", v * 100.0),
            Knob::Smoothing | Knob::ZoomIn | Knob::ZoomOut => format!("{v:.2} s"),
            Knob::Cursor => format!("{v:.1}×"),
            Knob::Padding | Knob::Radius | Knob::ShadowBlur | Knob::CamMargin | Knob::CamBorder => {
                format!("{v:.0} px")
            }
            Knob::CamSize | Knob::Split => format!("{:.0}%", v * 100.0),
            Knob::ScreenZoom => format!("{v:.2}×"),
            Knob::CapSize => format!("{v:.0} px"),
            Knob::CapOutline => format!("{v:.1} px"),
            Knob::PillOpacity => format!("{:.0}%", v * 100.0),
            Knob::Threads => format!("{v:.0} of {}", crate::captions::transcribe::max_threads()),
            Knob::ClickSize => format!("{v:.0} pt"),
            Knob::ClickDuration => format!("{:.0} ms", v * 1000.0),
            Knob::WebpQuality => format!("{v:.0}"),
            Knob::SizeLimit => format!("{v:.0} MB"),
        }
    }
}

/// Track bounds of every knob from the last paint, for mapping the pointer to a value.
pub type KnobBounds = Rc<[Cell<Bounds<Pixels>>; Knob::COUNT]>;

pub fn new_bounds() -> KnobBounds {
    Rc::new(std::array::from_fn(|_| Cell::new(Bounds::default())))
}

impl Clipper {
    /// Sets `k` from a pointer position over its track.
    pub(crate) fn knob_to(&mut self, k: Knob, pos: Point<Pixels>) {
        let b = self.knob_bounds[k as usize].get();
        if f32::from(b.size.width) <= 0.0 {
            return;
        }
        let f = crate::ui::slider_fraction(b, pos.x);
        let (min, max, step) = k.range();
        let v = ((min + f * (max - min)) / step).round() * step;
        let v = v.clamp(min, max);
        if (v - k.get(self)).abs() > f32::EPSILON {
            k.set(self, v);
            self.recompute();
        }
    }

    /// The slider for `k`.
    pub(crate) fn knob(&self, k: Knob, label: &'static str, cx: &mut Context<Self>) -> gpui_kit::Stateful<Div> {
        let (min, max, _) = k.range();
        let v = k.get(self);
        let f = (v - min) / (max - min);
        let active = self.gesture == crate::timeline::Gesture::Knob(k);
        let shared = self.knob_bounds.clone();
        crate::ui::slider(("knob", k as usize), label, k.display(v), f, active, move |b| shared[k as usize].set(b))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.gesture = crate::timeline::Gesture::Knob(k);
                    this.knob_to(k, ev.position);
                    cx.notify();
                }),
            )
    }
}
