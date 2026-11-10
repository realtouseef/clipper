//! Shared look for the screenshot windows (editor, cards, Shots tab): design tokens and a few
//! building blocks, so every surface uses the same colors, spacing, radii and motion.
//!
//! Tokens are semantic (what a color is for, not which hex it is). Layout follows a 4/8 px
//! rhythm; radii nest concentrically (inner = outer − padding).

use gpui_kit::prelude::*;
use gpui_kit::{App, ClickEvent, Div, ElementId, FontWeight, Hsla, SharedString, Window, div, hsla, px, rgb};
use std::time::Duration;

pub(crate) use crate::{BG, CARD as SURFACE, HOVER, MUTED, RAISED, TEXT};
#[cfg(test)]
use crate::{GREEN as ACCENT, ON_GREEN as ON_ACCENT};

/// Spacing scale (4/8 rhythm).
pub const S2: f32 = 8.0;
pub const S3: f32 = 12.0;
pub const S4: f32 = 16.0;
pub const S6: f32 = 24.0;

/// Radii.
pub const R_LG: f32 = 18.0;

/// Hairline borders: light at low alpha instead of a flat gray, so they read on any surface.
pub fn hairline() -> Hsla {
    hsla(0.0, 0.0, 1.0, 0.08)
}

pub fn hairline_strong() -> Hsla {
    hsla(0.0, 0.0, 1.0, 0.14)
}

/// Accent at low alpha: the selected tool's background.
pub fn accent_tint() -> Hsla {
    hsla(100.0 / 360.0, 0.85, 0.72, 0.16)
}

/// Soft, wide shadow for floating surfaces.
pub fn float_shadow() -> Vec<gpui_kit::BoxShadow> {
    vec![
        gpui_kit::BoxShadow { color: hsla(0., 0., 0., 0.45), offset: gpui_kit::point(px(0.), px(18.)), blur_radius: px(48.), spread_radius: px(-12.), inset: false },
        gpui_kit::BoxShadow { color: hsla(0., 0., 0., 0.25), offset: gpui_kit::point(px(0.), px(2.)), blur_radius: px(6.), spread_radius: px(0.), inset: false },
    ]
}

/// The house easing: fast out, long settle (≈ cubic-bezier(0.32, 0.72, 0, 1)).
pub fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(4)
}

pub const ENTER: Duration = Duration::from_millis(320);

/// Section label above a group of controls.
pub fn eyebrow(text: impl Into<SharedString>) -> Div {
    div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(rgb(MUTED)).child(text.into())
}

/// A labelled field: label (optionally with a value on the right) over its control.
pub fn field(label: impl Into<SharedString>, value: Option<String>, control: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(S2))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(eyebrow(label))
                .children(value.map(|v| div().text_xs().text_color(rgb(TEXT)).child(v))),
        )
        .child(control)
}

pub type OnClick = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// A segmented control: equal segments in a track; the selected one is a light pill (clear
/// at a glance, without relying on a subtle shade).
pub fn segmented(id: impl Into<ElementId>, items: Vec<(SharedString, bool, OnClick)>) -> Div {
    let id: ElementId = id.into();
    let small = items.len() > 3;
    // Four or more: size each to its label so none truncates; fewer: equal widths.
    let many = items.len() > 3;
    let mut track = div().flex().w_full().overflow_hidden().p(px(3.)).gap(px(if many { 0. } else { 2. })).rounded_full().bg(rgb(RAISED)).border_1().border_color(hairline());
    for (i, (label, selected, on)) in items.into_iter().enumerate() {
        track = track.child(
            div()
                .id(ElementId::NamedChild(std::sync::Arc::new(id.clone()), format!("seg{i}").into()))
                .when(many, |d| d.flex_auto().px(px(4.)))
                .when(!many, |d| d.flex_1().min_w_0().px(px(S2)))
                .h(px(28.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .cursor_pointer()
                .when(small, |d| d.text_xs())
                .when(!small, |d| d.text_sm())
                .font_weight(FontWeight::MEDIUM)
                .when(selected, |d| d.bg(rgb(TEXT)).text_color(rgb(BG)))
                .when(!selected, |d| d.text_color(rgb(MUTED)).hover(|d| d.bg(rgb(HOVER)).text_color(rgb(TEXT))))
                .child(div().truncate().child(label))
                .on_click(move |e, w, cx| on(e, w, cx)),
        );
    }
    track
}

/// Inset of the slider's position bar from the field's ends.
pub const SLIDER_INSET: f32 = 12.0;

/// A slider as one field: label on the left, value on the right, and a thin bar marking the
/// position over a faint fill. Press anywhere and drag. `on_bounds` receives the field's
/// bounds each frame; turn a pointer x into a fraction with `slider_fraction`.
pub fn slider(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    value: impl Into<SharedString>,
    fraction: f32,
    active: bool,
    on_bounds: impl Fn(gpui_kit::Bounds<gpui_kit::Pixels>) + 'static,
) -> gpui_kit::Stateful<Div> {
    let f = fraction.clamp(0.0, 1.0);
    div()
        .id(id)
        .relative()
        .h(px(42.))
        .w_full()
        .rounded(px(14.))
        .bg(rgb(RAISED))
        .border_1()
        .border_color(if active { hairline_strong() } else { hairline() })
        .overflow_hidden()
        .cursor_pointer()
        .child(gpui_kit::canvas(move |b, _, _| on_bounds(b), |_, _, _, _| {}).absolute().size_full())
        // Faint fill up to the value.
        // GPUI clips children to a square box, so the fill rounds its own ends to match the field.
        .child(
            div()
                .absolute()
                .left_0()
                .top_0()
                .bottom_0()
                .w(gpui_kit::relative(f))
                .rounded_l(px(13.))
                .when(f > 0.98, |d| d.rounded_r(px(13.)))
                .bg(hsla(0., 0., 1.0, if active { 0.08 } else { 0.05 })),
        )
        // The position bar, kept inside the field's ends; drawn under the text.
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(SLIDER_INSET))
                .right(px(SLIDER_INSET))
                .child(
                    div()
                        .absolute()
                        .top(px(10.))
                        .bottom(px(10.))
                        .left(gpui_kit::relative(f))
                        .ml(px(-1.5))
                        .w(px(3.))
                        .rounded_full()
                        .bg(hsla(0., 0., 1.0, if active { 1.0 } else { 0.85 })),
                ),
        )
        .child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_between()
                .pl(px(S4))
                .pr(px(S6 + 4.0))
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(rgb(MUTED)).child(label.into()))
                .child(div().text_sm().font_weight(FontWeight::BOLD).text_color(rgb(TEXT)).child(value.into())),
        )
}

/// Pointer x → 0..=1 along a slider whose bounds are `b`.
pub fn slider_fraction(b: gpui_kit::Bounds<gpui_kit::Pixels>, x: gpui_kit::Pixels) -> f32 {
    let w = f32::from(b.size.width) - 2.0 * SLIDER_INSET;
    if w <= 0.0 {
        return 0.0;
    }
    ((f32::from(x) - f32::from(b.origin.x) - SLIDER_INSET) / w).clamp(0.0, 1.0)
}

/// A floating card: an outer shell (hairline, faint fill, small padding) around an inner core
/// with a top highlight; the inner radius follows the outer one.
pub fn bezel(outer_radius: f32, pad: f32, inner: impl IntoElement) -> Div {
    div()
        .p(px(pad))
        .rounded(px(outer_radius))
        .bg(hsla(0., 0., 1.0, 0.05))
        .border_1()
        .border_color(hairline())
        .shadow(float_shadow())
        .child(
            div()
                .size_full()
                .rounded(px((outer_radius - pad).max(2.0)))
                .overflow_hidden()
                .bg(rgb(SURFACE))
                .border_t_1()
                .border_color(hsla(0., 0., 1.0, 0.10))
                .child(inner),
        )
}

/// A keyboard shortcut chip.
pub fn kbd(text: impl Into<SharedString>) -> Div {
    div()
        .px(px(6.))
        .h(px(20.))
        .flex()
        .items_center()
        .rounded(px(5.))
        .bg(rgb(RAISED))
        .border_1()
        .border_color(hairline_strong())
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(rgb(TEXT))
        .child(text.into())
}

/// A shortcut row: what it does, and the keys.
pub fn shortcut(what: &'static str, keys: impl Into<SharedString>) -> Div {
    div().flex().items_center().justify_between().child(div().text_sm().text_color(rgb(MUTED)).child(what)).child(kbd(keys))
}

/// A short notice pill (toasts).
pub fn toast_pill() -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(S3))
        .pl(px(S4))
        .pr(px(S2))
        .h(px(40.))
        .rounded_full()
        .bg(hsla(240. / 360., 0.04, 0.06, 0.96))
        .border_1()
        .border_color(hairline_strong())
        .shadow(float_shadow())
        .text_sm()
        .text_color(rgb(TEXT))
}

/// Explanatory text under controls.
pub fn hint(text: impl Into<SharedString>) -> Div {
    div().text_xs().line_height(px(18.)).text_color(rgb(MUTED)).child(text.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easing_is_monotonic_and_settles() {
        assert_eq!(ease_out(0.0), 0.0);
        assert_eq!(ease_out(1.0), 1.0);
        let mut last = 0.0;
        for i in 1..=20 {
            let v = ease_out(i as f32 / 20.0);
            assert!(v >= last);
            last = v;
        }
        // Fast out: most of the way there at the halfway point.
        assert!(ease_out(0.5) > 0.9);
    }

    /// Text and muted text meet WCAG AA (4.5:1) on every surface they sit on.
    #[test]
    fn text_contrast() {
        fn lum(c: u32) -> f64 {
            let ch = |v: u32| {
                let s = v as f64 / 255.0;
                if s <= 0.03928 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
            };
            0.2126 * ch(c >> 16 & 255) + 0.7152 * ch(c >> 8 & 255) + 0.0722 * ch(c & 255)
        }
        let ratio = |a: u32, b: u32| {
            let (x, y) = (lum(a).max(lum(b)), lum(a).min(lum(b)));
            (x + 0.05) / (y + 0.05)
        };
        for surface in [BG, SURFACE, RAISED] {
            assert!(ratio(TEXT, surface) >= 4.5);
            assert!(ratio(MUTED, surface) >= 4.5, "muted on {surface:06x}: {}", ratio(MUTED, surface));
        }
        assert!(ratio(ON_ACCENT, ACCENT) >= 4.5);
        assert!(ratio(BG, TEXT) >= 4.5, "selected segment");
    }
}
