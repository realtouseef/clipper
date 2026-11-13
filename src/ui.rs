//! Shared components for every window: buttons, pill tabs, segmented control, chips, input
//! field, tooltip, popover, toast, dialog and swatch. Every value comes from `theme` tokens;
//! views compose these instead of styling raw divs.
//!
//! Rules carried here so views can't drift: selected = ink fill with paper text (or a 2 px
//! ink underline), hover = mist, coral only through `coral_button` (Record, Export) and the
//! recording dot, and one soft shadow for floating surfaces.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::prelude::*;
use gpui_kit::AnimationExt;
use gpui_kit::{App, ClickEvent, Div, ElementId, FontWeight, Hsla, SharedString, Stateful, Window, div, px, rgb};

use crate::theme as tk;

pub use crate::theme::{R_CONTROL, R_PANEL, S1, S2, S3, S4, S6};

/// Floating panels and dialogs.
pub const R_LG: f32 = tk::R_PANEL;

/// Dividers and borders (stone).
pub fn hairline() -> Hsla {
    rgb(tk::stone()).into()
}

/// A stronger edge (focused field, active control).
pub fn hairline_strong() -> Hsla {
    tk::ink(0.35)
}

/// Selected background for list rows and tools: the ink fill.
pub fn accent_tint() -> Hsla {
    rgb(tk::selected()).into()
}

/// The one floating elevation.
pub fn float_shadow() -> Vec<gpui_kit::BoxShadow> {
    tk::float_shadow()
}

/// Keyboard focus ring: 2 px ink with a 2 px paper gap.
pub fn focus_ring() -> Vec<gpui_kit::BoxShadow> {
    vec![
        gpui_kit::BoxShadow { color: rgb(tk::paper()).into(), offset: gpui_kit::point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(2.), inset: false },
        gpui_kit::BoxShadow { color: rgb(tk::text()).into(), offset: gpui_kit::point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(4.), inset: false },
    ]
}

/// Makes a custom control reachable by Tab and activatable by Return/Space, with the focus ring.
pub fn focusable(el: Stateful<Div>) -> Stateful<Div> {
    el.tab_index(0).focus_visible(|s| s.shadow(focus_ring()))
}

/// Fast out, long settle.
pub fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(4)
}

/// Panel/popover entrance, shortened to nothing under Reduce motion.
pub fn enter() -> gpui_kit::Animation {
    gpui_kit::Animation::new(tk::motion(tk::PANEL_MS)).with_easing(ease_out)
}

// ---------- text ----------

/// A plain section heading (sentence case, no caps, no tracking).
pub fn heading(text: impl Into<SharedString>) -> Div {
    div().text_size(px(tk::T13)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(tk::text())).child(text.into())
}

/// Small label above a control.
pub fn eyebrow(text: impl Into<SharedString>) -> Div {
    div().text_size(px(tk::T12)).font_weight(FontWeight::MEDIUM).text_color(rgb(tk::ink_soft())).child(text.into())
}

/// Explanatory text under controls (kept to about 60 characters a line).
pub fn hint(text: impl Into<SharedString>) -> Div {
    div().max_w(px(60.0 * 6.2)).text_size(px(tk::T12)).line_height(px(18.)).text_color(rgb(tk::ink_soft())).child(text.into())
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
                .children(value.map(|v| div().text_size(px(tk::T12)).text_color(rgb(tk::text())).child(v))),
        )
        .child(control)
}

/// A panel section: heading, body, and a stone divider above every section but the first.
pub fn section(title: impl Into<SharedString>, first: bool, body: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(S3))
        .when(!first, |d| d.pt(px(S4)).border_t_1().border_color(hairline()))
        .child(heading(title))
        .child(body)
}

/// A thin vertical divider between groups in a bar.
pub fn vdivider(h: f32) -> Div {
    div().w(px(1.)).h(px(h)).flex_none().bg(hairline())
}

/// The `tnum` OpenType feature: digits of equal width, so timecodes don't jitter.
pub fn tnum() -> gpui_kit::FontFeatures {
    gpui_kit::FontFeatures(std::sync::Arc::new(vec![("tnum".into(), 1)]))
}

/// Tabular numerals for timecodes.
pub fn timecode(text: impl Into<SharedString>) -> Div {
    div().font_features(tnum()).child(text.into())
}

// ---------- buttons ----------

pub type OnClick = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// Coral pill: Record and Export only.
pub fn coral_button(id: impl Into<ElementId>) -> Button {
    Button::new(id).cursor_pointer().danger().rounded_full().h(px(tk::CONTROL_H + 4.0)).px(px(S4)).gap(px(S2)).font_weight(FontWeight::SEMIBOLD)
}

/// Ink pill: the confirm action of a dialog, "Suggest zooms".
pub fn primary_button(id: impl Into<ElementId>) -> Button {
    Button::new(id).cursor_pointer().primary().rounded_full().h(px(tk::CONTROL_H)).px(px(S4)).gap(px(S2)).font_weight(FontWeight::SEMIBOLD)
}

/// Mist pill: secondary actions.
pub fn secondary_button(id: impl Into<ElementId>) -> Button {
    Button::new(id).cursor_pointer().secondary().rounded_full().h(px(tk::CONTROL_H)).px(px(S4)).gap(px(S2)).font_weight(FontWeight::MEDIUM)
}

/// No fill until hovered.
pub fn ghost_button(id: impl Into<ElementId>) -> Button {
    Button::new(id).cursor_pointer().ghost().rounded_full().h(px(tk::CONTROL_H)).px(px(S3)).gap(px(S2)).font_weight(FontWeight::MEDIUM)
}

/// A round icon-only button with a tooltip (label and shortcut).
pub fn icon_button(id: impl Into<ElementId>, icon: gpui_kit::component::Icon, tip: impl Into<SharedString>) -> Button {
    Button::new(id).cursor_pointer().ghost().rounded_full().size(px(36.)).icon(icon.size(px(tk::ICON))).tooltip(tip)
}

// ---------- tabs, segmented, chips ----------

/// One pill in a tab row or filter row: ink fill when selected, mist on hover.
pub fn pill(id: impl Into<ElementId>, label: impl Into<SharedString>, selected: bool) -> Stateful<Div> {
    focusable(
        div()
            .id(id)
            .h(px(tk::CONTROL_H))
            .px(px(S3 - 1.0))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .rounded_full()
            .cursor_pointer()
            .text_size(px(tk::T13))
            .font_weight(FontWeight::SEMIBOLD)
            .when(selected, |d| d.bg(rgb(tk::selected())).text_color(rgb(tk::on_selected())))
            .when(!selected, |d| d.text_color(rgb(tk::text())).hover(|d| d.bg(rgb(tk::hover())))),
    )
    .child(label.into())
}

/// A row of pill tabs (panel tabs, library filters).
pub fn pill_tabs(id: impl Into<ElementId>, items: Vec<(SharedString, bool, OnClick)>) -> Div {
    let id: ElementId = id.into();
    let mut row = div().flex().flex_wrap().items_center().gap(px(2.));
    for (i, (label, selected, on)) in items.into_iter().enumerate() {
        row = row.child(pill(ElementId::NamedChild(std::sync::Arc::new(id.clone()), format!("tab{i}").into()), label, selected).on_click(move |e, w, cx| on(e, w, cx)));
    }
    row
}

/// A row of pill tabs that share the width equally (the settings panel's tabs).
pub fn pill_tabs_fill(id: impl Into<ElementId>, items: Vec<(SharedString, bool, OnClick)>) -> Div {
    let id: ElementId = id.into();
    let mut row = div().flex().w_full().items_center().gap(px(2.));
    for (i, (label, selected, on)) in items.into_iter().enumerate() {
        row = row.child(
            pill(ElementId::NamedChild(std::sync::Arc::new(id.clone()), format!("tab{i}").into()), label, selected)
                .flex_1()
                .min_w_0()
                .px(px(2.))
                .on_click(move |e, w, cx| on(e, w, cx)),
        );
    }
    row
}

/// A chip: a small pill, mist when off, ink when on.
#[allow(dead_code)] // part of the shared set; not every window uses it
pub fn chip(id: impl Into<ElementId>, label: impl Into<SharedString>, selected: bool) -> Stateful<Div> {
    focusable(
        div()
            .id(id)
            .h(px(28.))
            .px(px(S3))
            .flex()
            .items_center()
            .rounded_full()
            .cursor_pointer()
            .text_size(px(tk::T12))
            .font_weight(FontWeight::SEMIBOLD)
            .when(selected, |d| d.bg(rgb(tk::selected())).text_color(rgb(tk::on_selected())))
            .when(!selected, |d| d.bg(rgb(tk::mist())).text_color(rgb(tk::text())).hover(|d| d.bg(rgb(tk::hover())))),
    )
    .child(label.into())
}

/// A segmented control: equal segments in a mist track; the selected one is an ink pill.
pub fn segmented(id: impl Into<ElementId>, items: Vec<(SharedString, bool, OnClick)>) -> Div {
    let id: ElementId = id.into();
    let many = items.len() > 3;
    let mut track = div().flex().w_full().overflow_hidden().p(px(3.)).gap(px(2.)).rounded_full().bg(rgb(tk::raised())).border_1().border_color(hairline());
    for (i, (label, selected, on)) in items.into_iter().enumerate() {
        track = track.child(
            focusable(
                div()
                    .id(ElementId::NamedChild(std::sync::Arc::new(id.clone()), format!("seg{i}").into()))
                    .when(many, |d| d.flex_auto().px(px(S1)))
                    .when(!many, |d| d.flex_1().min_w_0().px(px(S2)))
                    .h(px(28.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .cursor_pointer()
                    .text_size(px(if many { tk::T12 } else { tk::T13 }))
                    .font_weight(FontWeight::SEMIBOLD)
                    .when(selected, |d| d.bg(rgb(tk::selected())).text_color(rgb(tk::on_selected())))
                    .when(!selected, |d| d.text_color(rgb(tk::ink_soft())).hover(|d| d.bg(rgb(tk::hover())).text_color(rgb(tk::text())))),
            )
            .child(div().truncate().child(label))
            .on_click(move |e, w, cx| on(e, w, cx)),
        );
    }
    track
}

// ---------- slider ----------

/// Inset of the slider's position bar from the field's ends.
pub const SLIDER_INSET: f32 = 12.0;

/// A slider as one field: label on the left, value on the right, and a thin ink bar marking
/// the position over a faint fill. Press anywhere and drag. `on_bounds` receives the field's
/// bounds each frame; turn a pointer x into a fraction with `slider_fraction`.
pub fn slider(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    value: impl Into<SharedString>,
    fraction: f32,
    active: bool,
    on_bounds: impl Fn(gpui_kit::Bounds<gpui_kit::Pixels>) + 'static,
) -> Stateful<Div> {
    let f = fraction.clamp(0.0, 1.0);
    div()
        .id(id)
        .relative()
        .h(px(40.))
        .w_full()
        .rounded(px(R_CONTROL))
        .bg(rgb(tk::raised()))
        .border_1()
        .border_color(if active { hairline_strong() } else { hairline() })
        .overflow_hidden()
        .cursor_pointer()
        .child(gpui_kit::canvas(move |b, _, _| on_bounds(b), |_, _, _, _| {}).absolute().size_full())
        .child(
            div()
                .absolute()
                .left_0()
                .top_0()
                .bottom_0()
                .w(gpui_kit::relative(f))
                .rounded_l(px(R_CONTROL - 1.0))
                .when(f > 0.98, |d| d.rounded_r(px(R_CONTROL - 1.0)))
                .bg(tk::ink(if active { 0.10 } else { 0.06 })),
        )
        .child(
            div().absolute().top_0().bottom_0().left(px(SLIDER_INSET)).right(px(SLIDER_INSET)).child(
                div()
                    .absolute()
                    .top(px(10.))
                    .bottom(px(10.))
                    .left(gpui_kit::relative(f))
                    .ml(px(-1.))
                    .w(px(2.))
                    .rounded_full()
                    .bg(tk::ink(if active { 0.9 } else { 0.5 })),
            ),
        )
        .child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_between()
                .pl(px(S3))
                .pr(px(S6))
                .child(div().text_size(px(tk::T13)).font_weight(FontWeight::MEDIUM).text_color(rgb(tk::ink_soft())).child(label.into()))
                .child(div().text_size(px(tk::T13)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(tk::text())).font_features(tnum()).child(value.into())),
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

// ---------- input ----------

/// The look of a text field shell (inputs, search): mist fill, 8 px radius, stone edge.
#[allow(dead_code)] // part of the shared set; not every window uses it
pub fn input_shell() -> Div {
    div().h(px(tk::CONTROL_H + 4.0)).px(px(S3)).flex().items_center().gap(px(S2)).rounded(px(R_CONTROL)).bg(rgb(tk::mist())).border_1().border_color(hairline())
}

// ---------- floating surfaces ----------

/// A floating surface: paper, 16 px radius, a stone edge and the one soft shadow.
pub fn popover() -> Div {
    div().rounded(px(R_PANEL)).bg(rgb(tk::paper())).border_1().border_color(hairline()).shadow(float_shadow()).text_color(rgb(tk::text()))
}

/// Kept for the call sites that wrap their body in a floating card.
pub fn bezel(_outer_radius: f32, _pad: f32, inner: impl IntoElement) -> Div {
    popover().overflow_hidden().child(inner)
}

/// One row of a popover menu.
pub fn menu_item(id: impl Into<ElementId>, icon: Option<gpui_kit::component::Icon>, label: impl Into<SharedString>) -> Stateful<Div> {
    focusable(
        div()
            .id(id)
            .h(px(tk::CONTROL_H + 4.0))
            .px(px(S3))
            .flex()
            .items_center()
            .gap(px(S2 + 2.0))
            .rounded(px(R_CONTROL))
            .cursor_pointer()
            .text_size(px(tk::T13))
            .font_weight(FontWeight::MEDIUM)
            .text_color(rgb(tk::text()))
            .hover(|d| d.bg(rgb(tk::mist()))),
    )
    .children(icon.map(|i| i.size(px(18.)).text_color(rgb(tk::ink_soft()))))
    .child(label.into())
}

/// A menu: a popover with rows.
pub fn menu(width: f32) -> Div {
    popover().w(px(width)).p(px(S1 + 2.0)).flex().flex_col().gap(px(2.))
}

/// A keyboard shortcut chip.
pub fn kbd(text: impl Into<SharedString>) -> Div {
    div()
        .px(px(6.))
        .h(px(22.))
        .flex()
        .items_center()
        .rounded(px(6.))
        .bg(rgb(tk::mist()))
        .border_1()
        .border_color(hairline())
        .text_size(px(tk::T12))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(tk::text()))
        .child(text.into())
}

/// A shortcut row: what it does, and the keys.
pub fn shortcut(what: &'static str, keys: impl Into<SharedString>) -> Div {
    div().flex().items_center().justify_between().gap(px(S4)).child(div().text_size(px(tk::T13)).text_color(rgb(tk::text())).child(what)).child(kbd(keys))
}

/// A short notice pill (toasts): paper, pill radius, floating shadow.
pub fn toast_pill() -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(S3))
        .pl(px(S4))
        .pr(px(S2))
        .h(px(44.))
        .rounded_full()
        .bg(rgb(tk::paper()))
        .border_1()
        .border_color(hairline())
        .shadow(float_shadow())
        .text_size(px(tk::T13))
        .font_weight(FontWeight::MEDIUM)
        .text_color(rgb(tk::text()))
}

/// A toast anchored to the bottom centre of its parent, sliding in.
pub fn toast_layer(id: impl Into<ElementId>, pill: Div) -> Div {
    div().absolute().bottom(px(S6)).left_0().right_0().flex().justify_center().child(
        pill.with_animation(id, gpui_kit::Animation::new(tk::motion(tk::TOAST_MS)).with_easing(ease_out), |el, t| el.opacity(t).mb(px((1.0 - t) * -8.0))),
    )
}

/// A modal dialog over a scrim: title, a sentence, and its actions (confirm last).
pub fn dialog(title: impl Into<SharedString>, body: impl Into<SharedString>, actions: Vec<Button>, on_dismiss: OnClick) -> Div {
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .child(div().id("dialog-scrim").absolute().inset_0().bg(tk::scrim(0.32)).on_click(move |e, w, cx| on_dismiss(e, w, cx)))
        .child(
            popover()
                .occlude()
                .w(px(400.))
                .p(px(S6))
                .flex()
                .flex_col()
                .gap(px(S3))
                .child(div().text_size(px(tk::T18)).font_weight(FontWeight::BOLD).child(title.into()))
                .child(div().text_size(px(tk::T13)).line_height(px(20.)).text_color(rgb(tk::ink_soft())).child(body.into()))
                .child(div().pt(px(S3)).flex().justify_end().gap(px(S2)).children(actions))
                .with_animation("dialog-in", enter(), |el, t| el.opacity(t)),
        )
}

// ---------- swatch ----------

/// A color or gradient swatch: 8 px radius; selected gets a 2 px ink outline with a 2 px
/// paper gap. `fill` is the swatch's own content (a color or a gradient image).
pub fn swatch(id: impl Into<ElementId>, selected: bool, fill: impl IntoElement) -> Stateful<Div> {
    focusable(
        div()
            .id(id)
            .p(px(2.))
            .rounded(px(R_CONTROL + 4.0))
            .border_2()
            .border_color(if selected { gpui_kit::Hsla::from(rgb(tk::selected())) } else { tk::ink(0.0) })
            .cursor_pointer()
            .hover(|d| d.border_color(if selected { rgb(tk::selected()).into() } else { hairline() })),
    )
    .child(div().size_full().rounded(px(R_CONTROL)).overflow_hidden().border_1().border_color(tk::ink(0.08)).child(fill))
}

/// A skeleton placeholder: a flat mist rectangle (no shimmer).
#[allow(dead_code)] // part of the shared set; not every window uses it
pub fn skeleton() -> Div {
    div().bg(rgb(tk::mist()))
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
        assert!(ease_out(0.5) > 0.9);
    }
}
