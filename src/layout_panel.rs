//! The Layout tab: output format (with the platforms each one suits and its exact size),
//! the webcam (on/off, device, mirror, live preview), and the webcam placement picker.

use crate::theme as tk;
use crate::knob::Knob;
use crate::layout::{Corner, Placement, Ratio, Res, Shape, Side};
use crate::{Clipper, Stage, button, grid, hex_color, color_hex, note, section, tip_owned};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{ClickEvent, Div, FontWeight, ObjectFit, div, img, px, rgb};

/// Webcam border colors.
const BORDER_COLORS: [u32; 5] = [0xffffff, 0x0a0a0b, 0xa4f47c, 0x22d3ee, 0xf472b6];

impl Clipper {
    pub(crate) fn layout_panel(&self, cx: &mut Context<Self>) -> Div {
        let mut body = div()
            .flex()
            .flex_col()
            .gap_5()
            .child(section("Format", self.format_section(cx)))
            .child(section("Webcam", self.webcam_section(cx)))
            .child(section("Webcam layout", self.placement_section(cx)));
        if let Some(extra) = self.placement_settings(cx) {
            body = body.child(section("Webcam style", extra));
        }
        body.child(section(
            "Screen",
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(self.knob(Knob::ScreenZoom, "Screen zoom", cx))
                .child(note("Above 1× the camera follows the cursor even when the screen already fits the frame.")),
        ))
    }

    fn set_layout(&mut self, f: impl FnOnce(&mut crate::layout::Layout), cx: &mut Context<Self>) {
        f(&mut self.layout);
        self.recompute();
        cx.notify();
    }

    /// Two format cards (9:16 and 16:9) with the platforms they suit and the exact output
    /// size at the bottom, then the resolution.
    fn format_section(&self, cx: &mut Context<Self>) -> Div {
        let res = self.layout.res;
        let cards = Ratio::ALL.map(|ratio| {
            let selected = self.layout.ratio == ratio;
            let (w, h) = crate::layout::canvas(ratio, res);
            // A little frame in the format's shape.
            let (fw, fh) = if ratio == Ratio::Portrait { (14.0, 24.0) } else { (26.0, 15.0) };
            div()
                .id(("ratio", ratio as usize))
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_2()
                .p_3()
                .rounded(px(16.))
                .bg(rgb(tk::raised()))
                .border_2()
                .border_color(rgb(if selected { tk::text() } else { tk::raised() }))
                .cursor_pointer()
                .when(!selected, |d| d.hover(|s| s.bg(rgb(tk::hover()))))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_layout(|l| l.set_ratio(ratio), cx)))
                .child(
                    div()
                        .h(px(26.))
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .w(px(fw))
                                .h(px(fh))
                                .rounded(px(4.))
                                .border_2()
                                .border_color(rgb(if selected { tk::green_ink() } else { tk::muted() })),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(ratio.title()))
                                .child(div().text_xs().text_color(rgb(tk::muted())).child(ratio.label())),
                        ),
                )
                .child(
                    div()
                        .w_full()
                        .whitespace_normal()
                        .text_xs()
                        .text_color(rgb(tk::muted()))
                        .line_height(px(16.))
                        .child(format!("For {}", ratio.platforms())),
                )
                .child(div().mt_auto().pt_1().border_t_1().border_color(rgb(tk::line())).text_xs().font_weight(FontWeight::BOLD).text_color(
                    rgb(if selected { tk::green_ink() } else { tk::text() }),
                ).child(format!("{w} × {h}")))
        });
        let resolution = crate::ui::segmented(
            "res",
            Res::ALL
                .map(|r| {
                    let on: crate::ui::OnClick = Box::new(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_layout(|l| l.res = r, cx)));
                    (r.label().into(), r == res, on)
                })
                .into_iter()
                .collect(),
        );
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().flex().flex_col().gap_2().children(cards))
            .child(resolution)
    }

    fn webcam_section(&self, cx: &mut Context<Self>) -> Div {
        let idle = matches!(self.stage, Stage::Idle);
        let on = self.webcam.enabled;
        let toggle = Switch::new("webcam-on").cursor_pointer()
            .checked(on)
            .small()
            .disabled(!idle)
            .on_click(cx.listener(|this, v: &bool, _, cx| {
                this.webcam.enabled = *v;
                this.restart_live();
                if *v && this.layout.placement == Placement::None {
                    this.layout.placement = Placement::FALLBACK;
                }
                this.recompute();
                cx.notify();
            }));
        let mirror = Switch::new("webcam-mirror").cursor_pointer()
            .checked(self.webcam.mirror)
            .small()
            .label("Mirror")
            .on_click(cx.listener(|this, v: &bool, _, cx| {
                this.webcam.mirror = *v;
                this.live_mirror.store(*v, std::sync::atomic::Ordering::Relaxed);
                this.load_cam_frames(cx);
                cx.notify();
            }));
        let preview = div()
            .w_full()
            .h(px(150.))
            .rounded(px(16.))
            .bg(rgb(tk::card()))
            .border_1()
            .border_color(rgb(tk::line()))
            .overflow_hidden()
            .flex()
            .items_center()
            .justify_center()
            .text_xs()
            .text_color(rgb(tk::muted()))
            .map(|d| match &self.live_frame {
                Some(frame) => d.child(img(frame.frame.clone()).size_full().object_fit(ObjectFit::Cover).rounded(px(16.))),
                None if self.live.failed() => d.child("Camera unavailable"),
                None => d.child("Starting camera…"),
            });
        let devices_note = if self.cam_devices.is_empty() { Some("No camera found.") } else { None };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Record webcam"))
                    .child(toggle),
            )
            .when(on, |d| {
                d.child(self.cam_select.clone())
                .child(mirror)
                .when(idle, |d| d.child(preview))
            })
            .when_some(devices_note.filter(|_| on), |d, n| d.child(note(n)))
            .when_some(self.cam_notice.clone(), |d, n| d.child(note(&n)))
            .when(!on && self.recording.as_ref().is_none_or(|r| r.webcam.is_none()), |d| {
                d.child(note("Recorded as its own track, so you can change the layout after recording."))
            })
    }

    /// Thumbnail tiles of every placement valid for the current ratio.
    fn placement_section(&self, cx: &mut Context<Self>) -> Div {
        let ratio = self.layout.ratio;
        let current = self.layout.placement.for_ratio(ratio);
        let (tw, th) = if ratio == Ratio::Portrait { (30.0, 54.0) } else { (54.0, 30.0) };
        let tiles = Placement::options(ratio).into_iter().enumerate().map(|(i, p)| {
            let selected = p == current;
            div()
                .id(("placement", i))
                .h(px(70.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(12.))
                .bg(rgb(tk::raised()))
                .border_2()
                .border_color(rgb(if selected { tk::text() } else { tk::raised() }))
                .cursor_pointer()
                .when(!selected, |d| d.hover(|s| s.bg(rgb(tk::hover()))))
                .tooltip(tip_owned(p.label()))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    // A placement tile puts the webcam back in that corner.
                    this.set_layout(
                        |l| {
                            l.placement = p;
                            l.cam_pos = None;
                        },
                        cx,
                    );
                }))
                .child(thumb(p, tw, th, selected, self.layout.split))
        });
        let has = self.has_webcam();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(grid(4).children(tiles))
            .child(note(&match current {
                _ if !has => "Turn on the webcam to see it in the layout.".to_string(),
                Placement::Split(_) => format!("{} · screen gets the rest", current.label()),
                p => p.label(),
            }))
    }

    /// Size, margin and border for a floating webcam; split share and swap for a split.
    fn placement_settings(&self, cx: &mut Context<Self>) -> Option<Div> {
        match self.layout.placement.for_ratio(self.layout.ratio) {
            Placement::None => None,
            Placement::Float(..) => {
                let colors = div().flex().gap_2().children(BORDER_COLORS.iter().enumerate().map(|(i, &v)| {
                    let selected = color_hex(self.layout.cam_border_color) == v;
                    div()
                        .id(("cam-border", i))
                        .size(px(28.))
                        .p(px(2.))
                        .rounded(px(9.))
                        .border_2()
                        .border_color(rgb(if selected { tk::text() } else { tk::card() }))
                        .cursor_pointer()
                        .child(div().size_full().rounded(px(6.)).border_1().border_color(rgb(tk::pressed())).bg(rgb(v)))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.layout.cam_border_color = hex_color(v);
                            cx.notify();
                        }))
                }));
                let shadow = Switch::new("cam-shadow").cursor_pointer()
                    .checked(self.layout.cam_shadow)
                    .small()
                    .label("Shadow")
                    .on_click(cx.listener(|this, v: &bool, _, cx| {
                        this.layout.cam_shadow = *v;
                        cx.notify();
                    }));
                Some(
                    div()
                        .flex()
                        .flex_col()
                        .gap_4()
                        .child(self.knob(Knob::CamSize, "Size", cx))
                        .child(self.knob(Knob::CamMargin, "Margin", cx))
                        .child(self.knob(Knob::CamBorder, "Border", cx))
                        .child(div().flex().items_center().justify_between().child(div().text_sm().child("Border color")).child(colors))
                        .child(shadow),
                )
            }
            Placement::Split(_) => Some(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(self.knob(Knob::Split, "Webcam size", cx))
                    .child(note("Share of the frame the webcam takes. Lower it to keep the focus on the screen."))
                    .child(
                        button("swap-sides", "Swap sides", cx)
                            .small()
                            .w_full()
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.set_layout(|l| l.placement = l.placement.swapped(), cx);
                            })),
                    ),
            ),
        }
    }
}

/// A mini canvas showing where the webcam goes.
fn thumb(p: Placement, w: f32, h: f32, selected: bool, split: f32) -> Div {
    let accent = rgb(if selected { tk::green_ink() } else { tk::muted() });
    let screen = rgb(tk::pressed());
    let canvas = div().relative().w(px(w)).h(px(h)).rounded(px(5.)).bg(rgb(tk::card())).border_1().border_color(rgb(tk::line()));
    let pad = 3.0;
    match p {
        Placement::None => canvas.child(
            div().absolute().left(px(pad)).top(px(pad)).w(px(w - 2.0 * pad)).h(px(h - 2.0 * pad)).rounded(px(3.)).bg(screen),
        )
        .child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .child(div().w(px(w.min(h) * 0.7)).h(px(2.)).bg(accent).rounded_full()),
        ),
        Placement::Float(shape, corner) => {
            let d = w.min(h) * 0.38;
            let m = 3.0;
            let (x, y) = match corner {
                Corner::TopLeft => (m, m),
                Corner::TopRight => (w - m - d - 2.0, m),
                Corner::BottomLeft => (m, h - m - d - 2.0),
                Corner::BottomRight => (w - m - d - 2.0, h - m - d - 2.0),
            };
            canvas
                .child(div().absolute().left(px(pad)).top(px(pad)).w(px(w - 2.0 * pad)).h(px(h - 2.0 * pad)).rounded(px(3.)).bg(screen))
                .child(
                    div()
                        .absolute()
                        .left(px(x))
                        .top(px(y))
                        .size(px(d))
                        .bg(accent)
                        .border_1()
                        .border_color(rgb(tk::on_green()))
                        .map(|d2| if shape == Shape::Circle { d2.rounded_full() } else { d2.rounded(px(d * 0.25)) }),
                )
        }
        Placement::Split(side) => {
            let vertical = matches!(side, Side::Top | Side::Bottom);
            let inner = (w - 2.0 * pad, h - 2.0 * pad);
            let (cam_len, screen_len) = if vertical {
                let a = (inner.1 - 2.0) * split;
                (a, inner.1 - 2.0 - a)
            } else {
                let a = (inner.0 - 2.0) * split;
                (a, inner.0 - 2.0 - a)
            };
            let cam_first = matches!(side, Side::Top | Side::Left);
            let block = |len: f32, color| {
                let d = div().rounded(px(3.)).bg(color);
                if vertical { d.w_full().h(px(len)) } else { d.h_full().w(px(len)) }
            };
            let (a, b) = if cam_first {
                (block(cam_len, accent), block(screen_len, screen))
            } else {
                (block(screen_len, screen), block(cam_len, accent))
            };
            canvas.child(
                div()
                    .absolute()
                    .left(px(pad))
                    .top(px(pad))
                    .w(px(inner.0))
                    .h(px(inner.1))
                    .flex()
                    .when(vertical, |d| d.flex_col())
                    .gap(px(2.))
                    .child(a)
                    .child(b),
            )
        }
    }
}
