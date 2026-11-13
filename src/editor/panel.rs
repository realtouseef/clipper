//! The editor's properties panel: settings for the selected object, or for the current tool
//! when nothing is selected.

use super::doc::*;
use super::stickers;
use super::view::{EditorView, Prop, RedactMode, RedactShape, Slot, Tool};
use crate::ui::{self, S2, S4, S6, hairline};
use crate::{CARD, MUTED, RAISED, TEXT};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::color_picker::ColorPicker;
use gpui_kit::component::input::Input;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Selectable, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, ClickEvent, Div, FontWeight, ObjectFit, PathPromptOptions, Window, div, img, px, rgb};

/// A labelled settings row.
pub fn row(label: &str, content: impl IntoElement) -> Div {
    ui::field(label.to_string(), None, content)
}

/// A sub-heading with a hairline above it.
fn section(title: &str) -> Div {
    div()
        .pt(px(S4))
        .border_t_1()
        .border_color(hairline())
        .text_sm()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(TEXT))
        .child(title.to_string())
}

fn hex(c: Rgba) -> u32 {
    ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | c[2] as u32
}

/// What the panel shows: the selection's kind, else the tool.
fn context(v: &EditorView) -> Tool {
    match v.selected().map(|o| &o.kind) {
        Some(Kind::Arrow(_)) => Tool::Arrow,
        Some(Kind::Image(_)) => Tool::Merge,
        Some(Kind::Source(_) | Kind::Callout(_)) => Tool::Callout,
        Some(Kind::Line(_)) => Tool::Line,
        Some(Kind::Shape(s)) => match s.kind {
            ShapeKind::Rect => Tool::Rect,
            ShapeKind::RoundRect => Tool::RoundRect,
            ShapeKind::Ellipse => Tool::Ellipse,
        },
        Some(Kind::Text(_)) => Tool::Text,
        Some(Kind::Highlight(_)) => Tool::Highlight,
        Some(Kind::Step(_)) => Tool::Step,
        Some(Kind::Pen(_)) => Tool::Pen,
        Some(Kind::Emoji(_)) => Tool::Emoji,
        Some(Kind::Sticker(_)) => Tool::Sticker,
        Some(Kind::Redact(_)) => Tool::Redact,
        None => v.tool,
    }
}

impl EditorView {
    fn slider(&self, prop: Prop, label: &str, cx: &mut Context<Self>) -> gpui_kit::Stateful<Div> {
        let v = self.prop_get(prop);
        let _ = v;
        self.slider_el(prop, label, cx)
    }

    fn swatches(&self, slot: Slot, allow_none: bool, palette: &[Rgba], cx: &mut Context<Self>) -> Div {
        let current = self.color_get(slot);
        let mut grid = div().flex().flex_wrap().gap(px(6.)).items_center();
        if allow_none {
            let on = current.is_none();
            grid = grid.child(
                div()
                    .id(("none", slot as usize))
                    .size(px(26.))
                    .rounded(px(7.))
                    .border_2()
                    .border_color(if on { gpui_kit::Hsla::from(rgb(TEXT)) } else { ui::hairline_strong() })
                    .bg(rgb(CARD))
                    .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new("None").build(w, cx))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .cursor_pointer()
                    .child("∅")
                    .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| v.set_color(slot, None, cx))),
            );
        }
        for (i, c) in palette.iter().copied().enumerate() {
            let on = current.is_some_and(|k| k[..3] == c[..3]);
            grid = grid.child(
                // Selected: a light ring with a gap, readable on any swatch color.
                div()
                    .id(("sw", slot as usize * 100 + i))
                    .size(px(26.))
                    .p(px(2.))
                    .rounded(px(8.))
                    .border_2()
                    .border_color(if on { gpui_kit::Hsla::from(rgb(TEXT)) } else { gpui_kit::transparent_black() })
                    .cursor_pointer()
                    .child(div().size_full().rounded(px(5.)).bg(rgb(hex(c))).border_1().border_color(ui::hairline()))
                    .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| {
                        let keep_alpha = v.color_get(slot).map(|k| k[3]).filter(|a| *a < 255 && slot != Slot::Solid);
                        v.set_color(slot, Some([c[0], c[1], c[2], keep_alpha.unwrap_or(c[3])]), cx)
                    })),
            );
        }
        grid.child(
            div()
                .id(("custom", slot as usize))
                .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new("Custom color").build(w, cx))
                .child(ColorPicker::new(&self.pickers[&slot]).cursor_pointer().small().accessibility_label("Custom color")),
        )
    }

    fn seg<T: Copy + PartialEq + 'static>(
        &self,
        id: &'static str,
        options: &[(T, &'static str)],
        current: T,
        cx: &mut Context<Self>,
        on: fn(&mut EditorView, T, &mut Window, &mut Context<EditorView>),
    ) -> Div {
        let items = options
            .iter()
            .copied()
            .map(|(val, label)| {
                let handler: ui::OnClick = Box::new(cx.listener(move |v, _: &ClickEvent, w, cx| on(v, val, w, cx)));
                (label.into(), val == current, handler)
            })
            .collect();
        ui::segmented(id, items)
    }

    fn switch(&self, id: &'static str, label: &'static str, on: bool, cx: &mut Context<Self>, f: fn(&mut EditorView, bool, &mut Context<EditorView>)) -> Div {
        let this = cx.entity().downgrade();
        div().flex().items_center().justify_between().child(div().text_sm().child(label)).child(
            Switch::new(id).cursor_pointer().checked(on).on_click(move |checked: &bool, _, cx| {
                let checked = *checked;
                let _ = this.update(cx, |v, cx| f(v, checked, cx));
            }),
        )
    }

    fn hint(text: &str) -> Div {
        ui::hint(text.to_string())
    }

    pub(super) fn panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ctx = context(self);
        let mut col = div().flex().flex_col().gap(px(S6)).px(px(S4 + 4.0)).py(px(S4 + 2.0));
        let locked = self.selected().is_some_and(|o| o.locked);
        let kind = if self.selected().is_some() { "Selected" } else { "Tool" };
        col = col.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(div().text_xs().text_color(rgb(MUTED)).child(if locked { "Selected · locked" } else { kind }))
                        .child(div().text_base().font_weight(FontWeight::SEMIBOLD).child(ctx.label())),
                )
                .child(ui::kbd(ctx.key().to_uppercase())),
        );
        let body: Vec<AnyElement> = match ctx {
            Tool::Select => vec![
                Self::hint("Click an object to edit it. Drag the handles to resize or rotate.").into_any_element(),
                div()
                    .flex()
                    .flex_col()
                    .gap(px(S2 + 2.0))
                    .child(ui::shortcut("Keep proportions", "⇧ drag"))
                    .child(ui::shortcut("Nudge", "← → ↑ ↓"))
                    .child(ui::shortcut("Resize by keyboard", "⌥ ← → ↑ ↓"))
                    .child(ui::shortcut("Duplicate", "⌘D"))
                    .child(ui::shortcut("Bring forward / back", "⌘] ⌘["))
                    .child(ui::shortcut("Lock", "⌘L"))
                    .child(ui::shortcut("Pan", "Space drag"))
                    .child(ui::shortcut("Zoom", "Scroll · Pinch"))
                    .child(ui::shortcut("Fit / 100%", "⌘0 ⌘1"))
                    .into_any_element(),
            ],
            Tool::Arrow => self.arrow_panel(cx),
            Tool::Callout => self.callout_panel(cx),
            Tool::Merge => self.merge_panel(cx),
            Tool::Line | Tool::Pen => vec![
                row("Color", self.swatches(Slot::Stroke, false, &super::view::palette(), cx)).into_any_element(),
                self.slider(Prop::Width, "Thickness", cx).into_any_element(),
            ],
            Tool::Rect | Tool::RoundRect | Tool::Ellipse => {
                let mut v = vec![
                    row("Stroke", self.swatches(Slot::Stroke, true, &super::view::palette(), cx)).into_any_element(),
                    row("Fill", self.swatches(Slot::Fill, true, &super::view::palette(), cx)).into_any_element(),
                    self.slider(Prop::Width, "Thickness", cx).into_any_element(),
                ];
                if ctx == Tool::RoundRect {
                    v.push(self.slider(Prop::Radius, "Corner radius", cx).into_any_element());
                }
                v
            }
            Tool::Text => self.text_panel(cx),
            Tool::Highlight => {
                let free = match self.selected().map(|o| &o.kind) {
                    Some(Kind::Highlight(h)) => matches!(h.mark, Mark::Free { .. }),
                    _ => self.defaults.hl_free,
                };
                let mut v = vec![
                    row(
                        "Shape",
                        self.seg("hl", &[(false, "Rectangle"), (true, "Freehand")], free, cx, |v, on, _, cx| {
                            v.defaults.hl_free = on;
                            cx.notify();
                        }),
                    )
                    .into_any_element(),
                    row(
                        "Color",
                        self.swatches(
                            Slot::Highlight,
                            false,
                            &[[0xff, 0xe6, 0x00, 255], [0x7c, 0xf4, 0x7c, 255], [0xff, 0x8a, 0xd8, 255], [0x7c, 0xd4, 0xff, 255], [0xff, 0xb3, 0x47, 255]],
                            cx,
                        ),
                    )
                    .into_any_element(),
                ];
                if free {
                    v.push(self.slider(Prop::HlWidth, "Marker width", cx).into_any_element());
                }
                v.push(Self::hint("A translucent marker: text underneath stays readable.").into_any_element());
                v
            }
            Tool::Step => vec![
                row("Color", self.swatches(Slot::Stroke, false, &super::view::palette(), cx)).into_any_element(),
                self.slider(Prop::StepSize, "Size", cx).into_any_element(),
                Self::hint(&format!("Click to place step {}.", self.doc.next_step())).into_any_element(),
            ],
            Tool::Emoji => self.emoji_panel(cx),
            Tool::Sticker => self.sticker_panel(cx),
            Tool::Redact => self.redact_panel(cx),
            Tool::Crop => self.crop_panel(cx),
            Tool::Background => self.background_panel(window, cx),
        };
        col = col.children(body);
        if let Some(o) = self.selected() {
            let locked = o.locked;
            let act = |id: &'static str, icon: gpui_kit::component::Icon, tip: &'static str| Button::new(id).cursor_pointer().ghost().small().icon(icon).tooltip(tip);
            col = col.child(
                ui::field(
                    "Arrange",
                    None,
                    div()
                        .flex()
                        .gap(px(2.))
                        .p(px(3.))
                        .rounded(px(12.))
                        .bg(rgb(RAISED))
                        .border_1()
                        .border_color(hairline())
                        .child(act("dup", super::icons::duplicate(), "Duplicate  ⌘D").on_click(cx.listener(|v, _: &ClickEvent, w, cx| v.object_action("duplicate", w, cx))))
                        .child(act("front", super::icons::front(), "Bring to front  ⇧⌘]").on_click(cx.listener(|v, _: &ClickEvent, w, cx| v.object_action("front", w, cx))))
                        .child(act("fwd", super::icons::forward(), "Bring forward  ⌘]").on_click(cx.listener(|v, _: &ClickEvent, w, cx| v.object_action("forward", w, cx))))
                        .child(act("bwd", super::icons::backward(), "Send backward  ⌘[").on_click(cx.listener(|v, _: &ClickEvent, w, cx| v.object_action("backward", w, cx))))
                        .child(act("back", super::icons::back(), "Send to back  ⇧⌘[").on_click(cx.listener(|v, _: &ClickEvent, w, cx| v.object_action("back", w, cx))))
                        .child(
                            act("lock", if locked { super::icons::unlock() } else { super::icons::lock() }, if locked { "Unlock  ⌘L" } else { "Lock  ⌘L" })
                                .on_click(cx.listener(|v, _: &ClickEvent, w, cx| v.object_action("lock", w, cx))),
                        )
                ),
            );
            // Destructive, so apart from the rest and in the danger color.
            if !locked {
                col = col.child(
                    Button::new("del").cursor_pointer()
                        .danger()
                        .small()
                        .w_full()
                        .icon(super::icons::trash())
                        .label("Delete  ⌫")
                        .on_click(cx.listener(|v, _: &ClickEvent, w, cx| v.object_action("delete", w, cx))),
                );
            }
        }
        div()
            .id("panel")
            .w(px(300.))
            .flex_none()
            .h_full()
            .border_l_1()
            .border_color(hairline())
            .bg(rgb(CARD))
            .overflow_y_scrollbar()
            .child(col)
    }

    fn arrow_panel(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let d = &self.defaults;
        let sel = match self.selected().map(|o| &o.kind) {
            Some(Kind::Arrow(a)) => Some(a.clone()),
            _ => None,
        };
        let (style, head, double, dashed, shadow) = sel.as_ref().map_or((d.arrow_style, d.head, d.double, d.dashed, d.arrow_shadow), |a| (a.style, a.head, a.double, a.dashed, a.shadow));
        let styles: Vec<(ArrowStyle, &'static str)> = ArrowStyle::ALL.iter().map(|s| (*s, if *s == ArrowStyle::Hand { "Hand" } else { s.label() })).collect();
        let mut v = vec![
            row(
                "Type",
                self.seg("arrow-style", &styles, style, cx, |v, st, _, cx| {
                    v.defaults.arrow_style = st;
                    v.edit_selected(cx, |o| {
                        if let Kind::Arrow(a) = &mut o.kind {
                            a.materialize();
                            a.style = st;
                            if st == ArrowStyle::Hand && a.roughness == 0.0 {
                                a.roughness = 0.5;
                            }
                        }
                    });
                    cx.notify();
                }),
            )
            .into_any_element(),
            row("Color", self.swatches(Slot::Stroke, false, &super::view::palette(), cx)).into_any_element(),
            self.slider(Prop::Width, "Thickness", cx).into_any_element(),
            row(
                "Head",
                self.seg("head", &[(Head::Filled, "Filled"), (Head::Open, "Open"), (Head::None, "None")], head, cx, |v, h, _, cx| {
                    v.defaults.head = h;
                    v.edit_selected(cx, |o| {
                        if let Kind::Arrow(a) = &mut o.kind {
                            a.head = h
                        }
                    });
                    cx.notify();
                }),
            )
            .into_any_element(),
            self.slider(Prop::HeadSize, "Head size", cx).into_any_element(),
            self.switch("double", "Double ended", double, cx, |v, on, cx| {
                v.defaults.double = on;
                v.edit_selected(cx, |o| {
                    if let Kind::Arrow(a) = &mut o.kind {
                        a.double = on
                    }
                });
                cx.notify();
            })
            .into_any_element(),
            self.switch("dashed", "Dashed", dashed, cx, |v, on, cx| {
                v.defaults.dashed = on;
                v.edit_selected(cx, |o| {
                    if let Kind::Arrow(a) = &mut o.kind {
                        a.dashed = on
                    }
                });
                cx.notify();
            })
            .into_any_element(),
            self.switch("arrow-shadow", "Shadow", shadow, cx, |v, on, cx| {
                v.defaults.arrow_shadow = on;
                v.edit_selected(cx, |o| {
                    if let Kind::Arrow(a) = &mut o.kind {
                        a.shadow = on
                    }
                });
                cx.notify();
            })
            .into_any_element(),
            self.slider(Prop::Roughness, "Roughness", cx).into_any_element(),
        ];
        if sel.is_some() {
            v.push(
                Button::new("reroll")
                    .cursor_pointer()
                    .secondary()
                    .small()
                    .w_full()
                    .icon(super::icons::rotate_left())
                    .label("Reroll wobble")
                    .on_click(cx.listener(|v, _: &ClickEvent, _, cx| {
                        v.edit_selected(cx, |o| {
                            if let Kind::Arrow(a) = &mut o.kind {
                                a.seed = a.seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                            }
                        });
                    }))
                    .into_any_element(),
            );
        }
        v.push(
            Self::hint(if style == ArrowStyle::Hand {
                "Draw the arrow freehand; it becomes an editable curve. Drag the curve to bend it, double-click to add a point, ⌥-click a point for smooth or sharp, ⌫ removes it. Drop an end on an object to attach it."
            } else {
                "Drag the curve to bend it, or the round handles to shape it. Double-click to add a point, ⌥-click a point for smooth or sharp, ⌫ removes it. Drop an end on an object to attach it; ⇧ keeps 45° angles."
            })
            .into_any_element(),
        );
        v
    }

    fn callout_panel(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let cd = self.defaults.callout.clone();
        let c = self.selected_callout().cloned();
        let sb = self.selected_source().cloned();
        let shape = c.as_ref().map_or(cd.shape, |c| c.shape);
        let shadow = c.as_ref().map_or(cd.shadow, |c| c.shadow).is_some();
        let show = sb.as_ref().map_or(cd.show_source, |s| s.visible);
        let tint = sb.as_ref().map_or(cd.src_fill, |s| s.fill.is_some());
        let dashed = sb.as_ref().map_or(cd.src_dashed, |s| s.dashed);
        let cut = c.as_ref().map_or(cd.cut, |c| c.cut);
        let mut v = vec![
            section("Callout").into_any_element(),
            row(
                "Shape",
                self.seg("c-shape", &[(CalloutShape::Rect, "Rounded rect"), (CalloutShape::Circle, "Circle")], shape, cx, |v, sh, _, cx| {
                    v.edit_callout(cx, move |sb, c, d| {
                        d.shape = sh;
                        if let Some(c) = c {
                            c.shape = sh;
                        }
                        if let Some(sb) = sb {
                            sb.shape = sh;
                        }
                    });
                }),
            )
            .into_any_element(),
            self.slider(Prop::CalloutScale, "Zoom", cx).into_any_element(),
            row("Border", self.swatches(Slot::CalloutBorder, false, &super::view::palette(), cx)).into_any_element(),
            self.slider(Prop::CalloutWidth, "Border width", cx).into_any_element(),
        ];
        if shape == CalloutShape::Rect {
            v.push(self.slider(Prop::CalloutRadius, "Corner radius", cx).into_any_element());
        }
        v.push(
            self.switch("c-shadow", "Shadow", shadow, cx, |v, on, cx| {
                v.edit_callout(cx, move |_, c, d| {
                    let sh = if on { Some(Shadow { blur: 36.0, opacity: 0.35, offset: 14.0 }) } else { None };
                    d.shadow = sh;
                    if let Some(c) = c {
                        c.shadow = sh;
                    }
                });
            })
            .into_any_element(),
        );
        if shadow {
            v.push(self.slider(Prop::CShadowBlur, "Shadow blur", cx).into_any_element());
            v.push(self.slider(Prop::CShadowOpacity, "Shadow opacity", cx).into_any_element());
            v.push(self.slider(Prop::CShadowOffset, "Shadow offset", cx).into_any_element());
        }
        v.push(section("Source highlight").into_any_element());
        v.push(
            self.switch("c-show", "Show source highlight", show, cx, |v, on, cx| {
                v.edit_callout(cx, move |sb, _, d| {
                    d.show_source = on;
                    if let Some(sb) = sb {
                        sb.visible = on;
                    }
                });
            })
            .into_any_element(),
        );
        v.push(row("Color", self.swatches(Slot::SourceColor, false, &super::view::palette(), cx)).into_any_element());
        v.push(self.slider(Prop::SourceWidth, "Thickness", cx).into_any_element());
        if shape == CalloutShape::Rect {
            v.push(self.slider(Prop::SourceRadius, "Corner radius", cx).into_any_element());
        }
        v.push(
            self.switch("c-tint", "Tint inside", tint, cx, |v, on, cx| {
                v.edit_callout(cx, move |sb, _, d| {
                    d.src_fill = on;
                    if let Some(sb) = sb {
                        sb.fill = on.then_some([sb.color[0], sb.color[1], sb.color[2], 36]);
                    }
                });
            })
            .into_any_element(),
        );
        v.push(
            self.switch("c-dashed", "Dashed", dashed, cx, |v, on, cx| {
                v.edit_callout(cx, move |sb, _, d| {
                    d.src_dashed = on;
                    if let Some(sb) = sb {
                        sb.dashed = on;
                    }
                });
            })
            .into_any_element(),
        );
        v.push(section("Options").into_any_element());
        let connector = c.as_ref().map_or(cd.connector, |c| c.connector.is_some());
        v.push(
            self.switch("c-conn", "Auto connector", connector, cx, |v, on, cx| {
                v.toggle_connector(on, cx);
            })
            .into_any_element(),
        );
        v.push(
            row(
                "Source area",
                self.seg("c-cut", &[(CutMode::Copy, "Keep"), (CutMode::Blank, "Blank"), (CutMode::Blur, "Blur")], cut, cx, |v, m, _, cx| {
                    v.edit_callout(cx, move |_, c, d| {
                        d.cut = m;
                        if let Some(c) = c {
                            c.cut = m;
                        }
                    });
                }),
            )
            .into_any_element(),
        );
        if let (Some(c), Some(sb)) = (&c, &sb) {
            let detail = super::render::detail_at(&self.doc, sb.rect);
            if c.scale / detail > 2.0 {
                v.push(Self::hint("Low resolution: this zoom goes past the picture's own pixels, so it looks soft.").into_any_element());
            }
        }
        v.push(Self::hint("Drag a box over the part to magnify (⇧ for a square or circle). Move the source box and the callout follows; move the callout anywhere.").into_any_element());
        v
    }

    fn merge_panel(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut v: Vec<AnyElement> = Vec::new();
        let add = Button::new("add-image")
            .cursor_pointer()
            .secondary()
            .small()
            .w_full()
            .icon(super::icons::add_image())
            .label("Add image…")
            .on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.choose_images(cx)));
        let presets = div()
            .flex()
            .flex_col()
            .gap(px(S2))
            .child(Button::new("p-compare").cursor_pointer().secondary().small().w_full().label("Compare two screenshots").on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.apply_preset(super::view::Preset::Compare, cx))))
            .child(Button::new("p-side").cursor_pointer().secondary().small().w_full().label("Side by side").on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.apply_preset(super::view::Preset::SideBySide, cx))))
            .child(Button::new("p-single").cursor_pointer().secondary().small().w_full().label("Single callout").on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.apply_preset(super::view::Preset::SingleCallout, cx))));
        let Some(board) = self.doc.board.clone() else {
            v.push(Self::hint("Add more pictures to merge them with this screenshot: stack them, put them side by side or place them freely. Drop files on the canvas or paste with ⌘V too.").into_any_element());
            v.push(add.into_any_element());
            v.push(section("Presets").into_any_element());
            v.push(presets.into_any_element());
            return v;
        };
        use super::merge::{BoardAlign, BoardLayout, Match};
        let n = self.doc.images().len();
        v.push(section(&format!("Layout · {n} image{}", if n == 1 { "" } else { "s" })).into_any_element());
        v.push(
            self.seg("b-layout", &[(BoardLayout::Vertical, "Vertical"), (BoardLayout::Horizontal, "Side by side"), (BoardLayout::Free, "Free")], board.layout, cx, |v, l, _, cx| {
                v.doc_action(
                    |d| {
                        if let Some(b) = d.board.as_mut() {
                            b.layout = l;
                        }
                        d.arrange(None);
                    },
                    cx,
                );
                v.zoom_fit(cx);
            })
            .into_any_element(),
        );
        if board.layout != BoardLayout::Free {
            v.push(self.slider(Prop::Gap, "Gap", cx).into_any_element());
            v.push(
                row(
                    "Align",
                    self.seg("b-align", &[(BoardAlign::Start, "Start"), (BoardAlign::Center, "Center"), (BoardAlign::End, "End")], board.align, cx, |v, a, _, cx| {
                        v.doc_action(
                            |d| {
                                if let Some(b) = d.board.as_mut() {
                                    b.align = a;
                                }
                                d.arrange(None);
                            },
                            cx,
                        );
                    }),
                )
                .into_any_element(),
            );
            v.push(
                row(
                    "Match edges",
                    self.seg("b-match", &[(Match::None, "Off"), (Match::Width, "Width"), (Match::Height, "Height")], board.matching, cx, |v, m, _, cx| {
                        v.doc_action(
                            |d| {
                                if let Some(b) = d.board.as_mut() {
                                    b.matching = m;
                                }
                                d.arrange(None);
                            },
                            cx,
                        );
                    }),
                )
                .into_any_element(),
            );
            v.push(row("Separator", self.swatches(Slot::Separator, true, &super::view::palette(), cx)).into_any_element());
            v.push(
                Button::new("b-swap")
                    .cursor_pointer()
                    .secondary()
                    .small()
                    .w_full()
                    .label("Swap order")
                    .on_click(cx.listener(|v, _: &ClickEvent, _, cx| {
                        v.doc_action(
                            |d| {
                                let horizontal = d.board.as_ref().is_some_and(|b| b.layout == BoardLayout::Horizontal);
                                let mut ids: Vec<(u64, f32)> = d.images().iter().map(|o| (o.id, if horizontal { o.bounds().x } else { o.bounds().y })).collect();
                                ids.sort_by(|a, b| a.1.total_cmp(&b.1));
                                let order: Vec<u64> = ids.into_iter().rev().map(|(id, _)| id).collect();
                                d.arrange(Some(&order));
                            },
                            cx,
                        );
                    }))
                    .into_any_element(),
            );
        } else {
            v.push(Self::hint("Drag the images anywhere; the canvas grows to fit.").into_any_element());
        }
        v.push(add.into_any_element());
        v.push(section("Canvas").into_any_element());
        v.push(self.slider(Prop::BoardPad, "Padding", cx).into_any_element());
        v.push(self.slider(Prop::ExpandL, "Extra left", cx).into_any_element());
        v.push(self.slider(Prop::ExpandR, "Extra right", cx).into_any_element());
        v.push(self.slider(Prop::ExpandT, "Extra top", cx).into_any_element());
        v.push(self.slider(Prop::ExpandB, "Extra bottom", cx).into_any_element());
        v.push(
            Button::new("b-bg")
                .cursor_pointer()
                .ghost()
                .small()
                .w_full()
                .icon(super::icons::background())
                .label("Background…")
                .on_click(cx.listener(|v, _: &ClickEvent, w, cx| v.set_tool(Tool::Background, w, cx)))
                .into_any_element(),
        );
        if let Some(Kind::Image(img)) = self.selected().map(|o| o.kind.clone()) {
            v.push(section("Selected image").into_any_element());
            if img.low_res() {
                v.push(
                    div()
                        .px_2()
                        .py_1()
                        .rounded(px(8.))
                        .bg(rgb(0x4a3410))
                        .text_xs()
                        .text_color(rgb(0xffb020))
                        .child(format!("Low resolution: shown at {:.1}× its pixels", img.scale()))
                        .into_any_element(),
                );
            }
            v.push(self.slider(Prop::ImgRadius, "Corner radius", cx).into_any_element());
            v.push(
                self.switch("img-shadow", "Shadow", img.shadow, cx, |v, on, cx| {
                    v.edit_selected(cx, |o| {
                        if let Kind::Image(i) = &mut o.kind {
                            i.shadow = on;
                        }
                    });
                })
                .into_any_element(),
            );
            v.push(self.slider(Prop::TrimL, "Crop left", cx).into_any_element());
            v.push(self.slider(Prop::TrimR, "Crop right", cx).into_any_element());
            v.push(self.slider(Prop::TrimT, "Crop top", cx).into_any_element());
            v.push(self.slider(Prop::TrimB, "Crop bottom", cx).into_any_element());
        }
        v.push(section("Presets").into_any_element());
        v.push(presets.into_any_element());
        v
    }

    fn text_panel(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let (weight, align, shadow, font) = match self.selected().map(|o| &o.kind) {
            Some(Kind::Text(t)) => (t.weight, t.align, t.shadow, t.font.clone()),
            _ => (self.defaults.weight, self.defaults.align, self.defaults.shadow, self.defaults.font.clone()),
        };
        self.sync_font_dropdown(&font, cx);
        vec![
            row("Font", self.font_dropdown.clone()).into_any_element(),
            self.slider(Prop::FontSize, "Size", cx).into_any_element(),
            row(
                "Weight",
                self.seg("weight", &[(400u16, "Regular"), (500, "Medium"), (600, "Semi"), (700, "Bold")], weight, cx, |v, w, _, cx| {
                    v.defaults.weight = w;
                    v.edit_selected(cx, |o| {
                        if let Kind::Text(t) = &mut o.kind {
                            t.weight = w
                        }
                    });
                    cx.notify();
                }),
            )
            .into_any_element(),
            row("Color", self.swatches(Slot::Stroke, false, &super::view::palette(), cx)).into_any_element(),
            row(
                "Alignment",
                self.seg("align", &[(Align::Left, "Left"), (Align::Center, "Center"), (Align::Right, "Right")], align, cx, |v, a, _, cx| {
                    v.defaults.align = a;
                    v.edit_selected(cx, |o| {
                        if let Kind::Text(t) = &mut o.kind {
                            t.align = a
                        }
                    });
                    cx.notify();
                }),
            )
            .into_any_element(),
            row("Background pill", self.swatches(Slot::Pill, true, &super::view::palette(), cx)).into_any_element(),
            row("Outline", self.swatches(Slot::Outline, true, &super::view::palette(), cx)).into_any_element(),
            self.switch("shadow", "Shadow", shadow, cx, |v, on, cx| {
                v.defaults.shadow = on;
                v.edit_selected(cx, |o| {
                    if let Kind::Text(t) = &mut o.kind {
                        t.shadow = on
                    }
                });
                cx.notify();
            })
            .into_any_element(),
            Self::hint("Click the canvas to add text and type. Double-click text to edit it; Esc finishes.").into_any_element(),
        ]
    }

    fn emoji_panel(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let query = self.emoji_query.read(cx).value().trim().to_lowercase();
        let groups: Vec<emojis::Group> = emojis::Group::iter().collect();
        let mut tabs = div().flex().flex_wrap().gap_1();
        for (i, g) in groups.iter().enumerate() {
            let first = g.emojis().next().map(|e| e.as_str().to_string()).unwrap_or_default();
            tabs = tabs.child(
                Button::new(("emoji-group", i)).cursor_pointer()
                    .ghost()
                    .small()
                    .selected(query.is_empty() && self.emoji_group == i)
                    .tooltip(format!("{g:?}"))
                    .child(first)
                    .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| {
                        v.emoji_group = i;
                        cx.notify();
                    })),
            );
        }
        let list: Vec<&'static emojis::Emoji> = if query.is_empty() {
            groups.get(self.emoji_group).map(|g| g.emojis().collect()).unwrap_or_default()
        } else {
            emojis::iter()
                .filter(|e| e.name().contains(&query) || e.shortcodes().any(|s| s.contains(&query)))
                .take(240)
                .collect()
        };
        let current = match self.selected().map(|o| &o.kind) {
            Some(Kind::Emoji(e)) => e.emoji.clone(),
            _ => self.defaults.emoji.clone(),
        };
        let mut grid = div().flex().flex_wrap().gap(px(2.));
        for (i, e) in list.into_iter().take(400).enumerate() {
            let s = e.as_str().to_string();
            let on = s == current;
            grid = grid.child(
                div()
                    .id(("emoji", i))
                    .size(px(34.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(8.))
                    .text_xl()
                    .cursor_pointer()
                    .when(on, |d| d.bg(rgb(RAISED)))
                    .hover(|d| d.bg(rgb(RAISED)))
                    .child(s.clone())
                    .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| {
                        v.defaults.emoji = s.clone();
                        let s2 = s.clone();
                        if v.sel.is_some() {
                            v.edit_selected(cx, |o| {
                                if let Kind::Emoji(e) = &mut o.kind {
                                    e.emoji = s2
                                }
                            });
                        }
                        cx.notify();
                    })),
            );
        }
        vec![
            Input::new(&self.emoji_query).small().into_any_element(),
            tabs.into_any_element(),
            grid.into_any_element(),
            Self::hint("Pick an emoji, then click (or drag) on the canvas to place it.").into_any_element(),
        ]
    }

    pub(super) fn refresh_thumbs(&mut self) {
        let mut srcs: Vec<StickerSrc> = stickers::BUILTIN.iter().map(|b| StickerSrc::Builtin(b.name.into())).collect();
        srcs.extend(self.my_stickers.iter().cloned().map(StickerSrc::File));
        for src in srcs {
            if self.thumbs.contains_key(&src) {
                continue;
            }
            let a = stickers::aspect(&src);
            let (w, h) = if a >= 1.0 { (112, (112.0 / a) as u32) } else { ((112.0 * a) as u32, 112) };
            if let Some(img) = stickers::render(&src, w.max(1), h.max(1)) {
                self.thumbs.insert(src, crate::canvas_preview::to_render_image(img));
            }
        }
    }

    fn sticker_panel(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        self.refresh_thumbs();
        let current = match self.selected().map(|o| &o.kind) {
            Some(Kind::Sticker(s)) => s.src.clone(),
            _ => self.defaults.sticker.clone(),
        };
        let srcs: Vec<StickerSrc> = if self.sticker_tab_mine {
            self.my_stickers.iter().cloned().map(StickerSrc::File).collect()
        } else {
            stickers::BUILTIN.iter().map(|b| StickerSrc::Builtin(b.name.into())).collect()
        };
        let category = |src: &StickerSrc| match src {
            StickerSrc::Builtin(n) => stickers::BUILTIN.iter().find(|b| b.name == n).map(|b| b.category).unwrap_or(""),
            StickerSrc::File(_) => "",
        };
        let mut grid = div().flex().flex_wrap().gap(px(6.));
        let mut last_cat = "";
        for (i, src) in srcs.into_iter().enumerate() {
            let cat = category(&src);
            if cat != last_cat && stickers::CATEGORIES.contains(&cat) {
                last_cat = cat;
                grid = grid.child(div().w_full().pt_1().text_xs().text_color(rgb(MUTED)).child(cat));
            }
            let on = src == current;
            let thumb = self.thumbs.get(&src).cloned();
            grid = grid.child(
                div()
                    .id(("sticker", i))
                    .size(px(60.))
                    .p(px(6.))
                    .rounded(px(10.))
                    .border_2()
                    .border_color(if on { gpui_kit::Hsla::from(rgb(TEXT)) } else { ui::hairline() })
                    .bg(rgb(0x202023))
                    .cursor_pointer()
                    .children(thumb.map(|t| img(t).size_full().rounded(px(8.)).object_fit(ObjectFit::Contain)))
                    .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| {
                        v.defaults.sticker = src.clone();
                        let s2 = src.clone();
                        if v.sel.is_some() {
                            v.edit_selected(cx, |o| {
                                if let Kind::Sticker(s) = &mut o.kind {
                                    let a = stickers::aspect(&s2);
                                    s.rect.h = s.rect.w / a;
                                    s.src = s2
                                }
                            });
                        }
                        cx.notify();
                    })),
            );
        }
        let tabs = self.seg("st-tab", &[(false, "Built-in"), (true, "My Stickers")], self.sticker_tab_mine, cx, |v, mine, _, cx| {
            v.sticker_tab_mine = mine;
            cx.notify();
        });
        let mut out = vec![tabs.into_any_element(), grid.into_any_element()];
        if self.sticker_tab_mine {
            if self.my_stickers.is_empty() {
                out.push(Self::hint("Imported stickers are kept here for every screenshot.").into_any_element());
            }
            out.push(
                Button::new("import-sticker").cursor_pointer()
                    .secondary()
                    .small()
                    .icon(super::icons::folder())
                    .label("Import sticker…")
                    .on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                        let rx = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some("Import".into()) });
                        cx.spawn(async move |this, cx| {
                            let Ok(Ok(Some(paths))) = rx.await else { return };
                            let _ = this.update(cx, |v, cx| {
                                let mut errors = Vec::new();
                                for p in paths {
                                    if let Err(e) = stickers::import(&p) {
                                        errors.push(e.to_string());
                                    }
                                }
                                v.my_stickers = stickers::my_stickers();
                                v.refresh_thumbs();
                                if let Some(e) = errors.first() {
                                    v.show_toast(e.clone(), cx);
                                }
                                cx.notify();
                            });
                        })
                        .detach();
                    }))
                    .into_any_element(),
            );
            out.push(Self::hint("PNG, SVG or WebP.").into_any_element());
        } else {
            out.push(Self::hint("Pick a sticker, then click (or drag) on the canvas.").into_any_element());
        }
        out
    }

    fn redact_panel(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let (mode, shape) = match self.selected().map(|o| &o.kind) {
            Some(Kind::Redact(r)) => (
                match r.effect {
                    Effect::Blur(_) => RedactMode::Blur,
                    Effect::Pixelate(_) => RedactMode::Pixelate,
                    Effect::Solid(_) => RedactMode::Solid,
                },
                match r.area {
                    Area::Rect(_) => RedactShape::Rect,
                    Area::RoundRect(..) => RedactShape::Round,
                    Area::Ellipse(_) => RedactShape::Ellipse,
                    Area::Brush { .. } => RedactShape::Brush,
                },
            ),
            _ => (self.defaults.redact, self.defaults.redact_shape),
        };
        let mut v = vec![
            row(
                "Effect",
                self.seg("rmode", &[(RedactMode::Blur, "Blur"), (RedactMode::Pixelate, "Pixelate"), (RedactMode::Solid, "Solid")], mode, cx, |v, m, _w, cx| {
                    v.defaults.redact = m;
                    let d = v.defaults.clone();
                    v.edit_selected(cx, |o| {
                        if let Kind::Redact(r) = &mut o.kind {
                            r.effect = match m {
                                RedactMode::Blur => Effect::Blur(d.strength),
                                RedactMode::Pixelate => Effect::Pixelate(d.block),
                                RedactMode::Solid => Effect::Solid(d.solid),
                            }
                        }
                    });
                    cx.notify();
                }),
            )
            .into_any_element(),
            row(
                "Shape",
                self.seg(
                    "rshape",
                    &[(RedactShape::Rect, "Box"), (RedactShape::Round, "Rounded"), (RedactShape::Ellipse, "Oval"), (RedactShape::Brush, "Brush")],
                    shape,
                    cx,
                    |v, s, _, cx| {
                        v.defaults.redact_shape = s;
                        cx.notify();
                    },
                ),
            )
            .into_any_element(),
        ];
        match mode {
            RedactMode::Blur => v.push(self.slider(Prop::Strength, "Strength", cx).into_any_element()),
            RedactMode::Pixelate => v.push(self.slider(Prop::Block, "Block size", cx).into_any_element()),
            RedactMode::Solid => v.push(row("Color", self.swatches(Slot::Solid, false, &super::view::palette(), cx)).into_any_element()),
        }
        if shape == RedactShape::Brush {
            v.push(self.slider(Prop::Brush, "Brush size", cx).into_any_element());
        }
        if shape == RedactShape::Round {
            v.push(self.slider(Prop::Radius, "Corner radius", cx).into_any_element());
        }
        v.push(
            Self::hint("Strength has a minimum for this image size so text can't be recovered. Redactions are baked into every copy, save and export.")
                .into_any_element(),
        );
        v
    }

    fn crop_panel(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let presets: [(Option<f32>, &'static str); 5] =
            [(None, "Free"), (Some(1.0), "1:1"), (Some(4.0 / 5.0), "4:5"), (Some(16.0 / 9.0), "16:9"), (Some(9.0 / 16.0), "9:16")];
        let mut seg = div().flex().flex_wrap().gap_1();
        for (i, (a, label)) in presets.into_iter().enumerate() {
            seg = seg.child(
                Button::new(("aspect", i)).cursor_pointer().secondary().small().label(label).selected(self.crop_aspect == a).on_click(cx.listener(move |v, _: &ClickEvent, _, cx| {
                    v.crop_aspect = a;
                    if let Some(a) = a {
                        v.doc_action(|d| d.crop_to_aspect(a), cx);
                    }
                    cx.notify();
                })),
            );
        }
        let c = self.doc.crop;
        let ib = |id: &'static str, icon: gpui_kit::component::Icon, tip: &'static str| Button::new(id).cursor_pointer().secondary().small().icon(icon).tooltip(tip);
        vec![
            row("Aspect", seg).into_any_element(),
            row(
                "Rotate and flip",
                div()
                    .flex()
                    .gap_1()
                    .child(ib("rot-l", super::icons::rotate_left(), "Rotate left").on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.doc_action(|d| d.rotate_image(false), cx))))
                    .child(ib("rot-r", super::icons::rotate_right(), "Rotate right").on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.doc_action(|d| d.rotate_image(true), cx))))
                    .child(ib("flip-h", super::icons::flip_h(), "Flip horizontally").on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.doc_action(|d| d.flip(true), cx))))
                    .child(ib("flip-v", super::icons::flip_v(), "Flip vertically").on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.doc_action(|d| d.flip(false), cx)))),
            )
            .into_any_element(),
            div().text_sm().child(format!("{} × {} px", c.w.round(), c.h.round())).into_any_element(),
            div()
                .flex()
                .gap_2()
                .child(Button::new("crop-reset").cursor_pointer().secondary().small().label("Reset").on_click(cx.listener(|v, _: &ClickEvent, _, cx| {
                    v.crop_aspect = None;
                    v.doc_action(|d| d.reset_crop(), cx)
                })))
                .child(Button::new("crop-done").cursor_pointer().primary().small().label("Done  ↵").on_click(cx.listener(|v, _: &ClickEvent, w, cx| v.set_tool(Tool::Select, w, cx))))
                .into_any_element(),
            Self::hint("Drag the frame or its handles. Drag past the image edge to add canvas around it.").into_any_element(),
        ]
    }

    fn background_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let f = self.doc.frame.clone();
        // A merged picture always has its background, filling the canvas around the images;
        // the space around them is the board's padding (Layout).
        if self.doc.board.is_some() {
            return vec![
                Self::hint("The background fills the canvas around the pictures. Set the space around them in Layout.").into_any_element(),
                self.bg_picker.clone().into_any_element(),
            ];
        }
        let mut v = vec![self
            .switch("bg-on", "Background", f.enabled, cx, |v, on, cx| v.doc_action(|d| d.frame.enabled = on, cx))
            .into_any_element()];
        if !f.enabled {
            v.push(Self::hint("Turn on to add padding, a background, rounded corners, a shadow or a window frame.").into_any_element());
            return v;
        }
        // The app's shared background picker (same library and presets as the recorder).
        v.push(self.bg_picker.clone().into_any_element());
        v.push(section("Frame").into_any_element());
        v.push(self.slider(Prop::Padding, "Padding", cx).into_any_element());
        v.push(self.slider(Prop::Corner, "Corner radius", cx).into_any_element());
        v.push(self.slider(Prop::ShadowBlur, "Shadow blur", cx).into_any_element());
        v.push(self.slider(Prop::ShadowOpacity, "Shadow opacity", cx).into_any_element());
        v.push(self.slider(Prop::ShadowOffset, "Shadow offset", cx).into_any_element());
        v.push(self.switch("inset", "Inset border", f.inset, cx, |v, on, cx| v.doc_action(|d| d.frame.inset = on, cx)).into_any_element());
        v.push(
            row(
                "Window",
                self.seg("win", &[(WindowStyle::None, "None"), (WindowStyle::Mac, "macOS"), (WindowStyle::Browser, "Browser")], f.window, cx, |v, s, _, cx| {
                    v.doc_action(|d| d.frame.window = s, cx)
                }),
            )
            .into_any_element(),
        );
        let size_opts: [(OutputSize, &'static str); 6] = [
            (OutputSize::Auto, "Auto"),
            (OutputSize::Aspect(1, 1), "1:1"),
            (OutputSize::Aspect(4, 5), "4:5"),
            (OutputSize::Aspect(16, 9), "16:9"),
            (OutputSize::Aspect(9, 16), "9:16"),
            (OutputSize::Custom(0, 0), "Custom"),
        ];
        let current = match f.size {
            OutputSize::Custom(..) => OutputSize::Custom(0, 0),
            s => s,
        };
        v.push(
            row(
                "Output size",
                self.seg("out-size", &size_opts, current, cx, |v, s, _, cx| {
                    let s = match s {
                        OutputSize::Custom(..) => {
                            let (w, h) = v.doc.layout().canvas;
                            OutputSize::Custom(w, h)
                        }
                        s => s,
                    };
                    v.doc_action(|d| d.frame.size = s, cx)
                }),
            )
            .into_any_element(),
        );
        if let OutputSize::Custom(w, h) = f.size {
            self.sync_custom_inputs(w, h, window, cx);
            v.push(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(Input::new(&self.custom_w).small())
                    .child("×")
                    .child(Input::new(&self.custom_h).small())
                    .into_any_element(),
            );
            v.push(Self::hint("The screenshot is centered and fitted inside. Press Enter to apply.").into_any_element());
        }
        let (w, h) = self.doc.layout().canvas;
        v.push(div().text_xs().text_color(rgb(MUTED)).child(format!("Output {w} × {h} px")).into_any_element());
        v
    }
}
