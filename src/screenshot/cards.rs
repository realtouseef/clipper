//! Thumbnail cards after a capture: a small always-on-top window at the bottom-left of the
//! screen. Newest at the bottom, at most 4 shown (older ones collapse into "+N"). Each card
//! auto-dismisses after 8 s (paused while hovered); the file stays saved. Delete moves the
//! file aside with a 5 s Undo toast, then to the Trash. The window is hidden from screen
//! capture, so it never shows up in the next screenshot.

use super::library::{self, Pending};
use crate::editor::{self, icons};
use crate::region::Display;
use crate::ui;
use crate::{CARD, GREEN, MUTED, TEXT};
use gpui_kit::AnimationExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{Sizable, tooltip::Tooltip};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyWindowHandle, App, ClickEvent, Entity, FontWeight, Global, ObjectFit, RenderImage, Window, WindowBackgroundAppearance,
    WindowBounds, WindowKind, WindowOptions, div, img, point, px, rgb, size,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

const W: f32 = 264.0;
const CARD_W: f32 = 240.0;
const IMAGE_H: f32 = 138.0;
const ACTIONS_H: f32 = 44.0;
/// Image + action row + the shell's padding and border.
const CARD_H: f32 = IMAGE_H + ACTIONS_H + 12.0;
const GAP: f32 = 10.0;
const BADGE_H: f32 = 28.0;
const TOAST_H: f32 = 56.0;
const MAX_SHOWN: usize = 4;
const LIFETIME: Duration = Duration::from_secs(8);
const UNDO: Duration = Duration::from_secs(5);
/// Clear of the Dock.
const BOTTOM: f64 = 96.0;

struct Card {
    path: PathBuf,
    thumb: Option<Arc<RenderImage>>,
    /// Time left while not hovered.
    left: Duration,
    hovered: bool,
}

pub struct Cards {
    cards: Vec<Card>,
    toast: Option<(Pending, Instant)>,
    display: Display,
    last_tick: Instant,
    error: Option<(String, Instant)>,
}

struct Open(Option<(AnyWindowHandle, Entity<Cards>)>);
impl Global for Open {}

fn thumbnail(path: &PathBuf) -> Option<Arc<RenderImage>> {
    let img = image::open(path).ok()?;
    let t = img.thumbnail((CARD_W * 2.0) as u32, (CARD_H * 2.0) as u32).to_rgba8();
    Some(crate::canvas_preview::to_render_image(t))
}

/// Adds a card for a new screenshot (opening the window if needed).
pub fn show(path: PathBuf, display: Display, cx: &mut App) {
    let card = Card { thumb: thumbnail(&path), path, left: LIFETIME, hovered: false };
    open_with(vec![card], None, display, cx);
}

/// Shows why a screenshot failed, where its card would have been.
pub fn show_error(msg: String, display: Display, cx: &mut App) {
    open_with(Vec::new(), Some(msg), display, cx);
}

fn open_with(new_cards: Vec<Card>, error: Option<String>, display: Display, cx: &mut App) {
    let error = error.map(|m| (m, Instant::now()));
    if let Some((_, cards)) = cx.try_global::<Open>().and_then(|o| o.0.clone()) {
        cards.update(cx, |c, cx| {
            c.cards.extend(new_cards);
            if error.is_some() {
                c.error = error;
            }
            c.display = display;
            cx.notify();
        });
        relayout(cx);
        return;
    }
    let n = new_cards.len();
    let has_error = error.is_some();
    let entity_cards = new_cards;
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(gpui_kit::Bounds::new(point(px(0.), px(0.)), size(px(W), px(CARD_H + 2.0 * GAP))))),
        titlebar: None,
        focus: false,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        window_background: WindowBackgroundAppearance::Transparent,
        ..Default::default()
    };
    let opened = gpui_kit::open_window(options, cx, move |window, cx| {
        let entity = cx.new(|_| Cards { cards: entity_cards, toast: None, display, last_tick: Instant::now(), error });
        crate::selector::overlay_window(window, 25, false, frame(&display, height_of(n, has_error)));
        start_ticker(entity.clone(), window, cx);
        entity
    });
    if let Ok((handle, entity)) = opened {
        crate::selector::clear_root(handle, cx);
        cx.set_global(Open(Some((handle, entity))));
    }
}

/// "Screenshot_20261107-212538" → "Screenshot · 21:25" (the capture time).
fn time_label(stem: &str) -> String {
    let stamp = stem.trim_start_matches("Screenshot_");
    match stamp.split_once('-') {
        Some((_, t)) if t.len() >= 4 && t[..4].chars().all(|c| c.is_ascii_digit()) => format!("Screenshot · {}:{}", &t[..2], &t[2..4]),
        _ => "Screenshot".to_string(),
    }
}

fn height_of(cards: usize, toast: bool) -> f64 {
    let shown = cards.min(MAX_SHOWN);
    let mut h = GAP + shown as f32 * (CARD_H + GAP);
    if cards > MAX_SHOWN {
        h += BADGE_H + GAP;
    }
    if toast {
        h += TOAST_H + GAP;
    }
    h.max(GAP * 2.0) as f64
}

fn frame(d: &Display, h: f64) -> (f64, f64, f64, f64) {
    (d.origin.0 + 12.0, d.origin.1 + d.size.1 - BOTTOM - h, W as f64, h)
}

/// Resizes the window to its cards, or closes it when nothing is left.
fn relayout(cx: &mut App) {
    let Some((handle, cards)) = cx.try_global::<Open>().and_then(|o| o.0.clone()) else { return };
    let (n, toast, display, err) = {
        let c = cards.read(cx);
        (c.cards.len(), c.toast.is_some(), c.display, c.error.is_some())
    };
    if n == 0 && !toast && !err {
        cx.set_global(Open(None));
        let _ = handle.update(cx, |_, window, _| window.remove_window());
        return;
    }
    let h = height_of(n, toast || err);
    let _ = handle.update(cx, |_, window, _| crate::selector::overlay_window(window, 25, false, frame(&display, h)));
}

fn start_ticker(entity: Entity<Cards>, window: &mut Window, cx: &mut App) {
    let weak = entity.downgrade();
    window
        .spawn(cx, async move |cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(200)).await;
                let Some(entity) = weak.upgrade() else { break };
                let changed = cx.update(|_, cx| entity.update(cx, |c, cx| c.tick(cx))).unwrap_or(false);
                if changed {
                    let _ = cx.update(|_, cx| relayout(cx));
                }
            }
        })
        .detach();
}

impl Cards {
    /// Advances the timers; true if the layout changed.
    fn tick(&mut self, cx: &mut Context<Self>) -> bool {
        let now = Instant::now();
        let dt = now - self.last_tick;
        self.last_tick = now;
        let before = (self.cards.len(), self.toast.is_some(), self.error.is_some());
        for c in self.cards.iter_mut().filter(|c| !c.hovered) {
            c.left = c.left.saturating_sub(dt);
        }
        self.cards.retain(|c| !c.left.is_zero());
        if self.toast.as_ref().is_some_and(|(_, at)| at.elapsed() >= UNDO) {
            let (p, _) = self.toast.take().unwrap();
            library::finish(&p);
        }
        if self.error.as_ref().is_some_and(|(_, at)| at.elapsed() >= UNDO) {
            self.error = None;
        }
        let after = (self.cards.len(), self.toast.is_some(), self.error.is_some());
        if before != after {
            cx.notify();
        }
        before != after
    }

    fn remove(&mut self, path: &PathBuf, cx: &mut Context<Self>) {
        self.cards.retain(|c| &c.path != path);
        cx.notify();
        cx.defer(relayout);
    }

    fn edit(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.remove(&path, cx);
        cx.defer(move |cx| editor::view::open(path, cx));
    }

    fn delete(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        // A second delete during the undo window commits the first.
        if let Some((p, _)) = self.toast.take() {
            library::finish(&p);
        }
        match library::delete(&path) {
            Ok(p) => self.toast = Some((p, Instant::now())),
            Err(e) => self.error = Some((format!("Could not delete: {e}"), Instant::now())),
        }
        self.remove(&path, cx);
    }

    fn undo(&mut self, cx: &mut Context<Self>) {
        if let Some((p, _)) = self.toast.take() {
            match library::undo(&p) {
                Ok(()) => {
                    let card = Card { thumb: thumbnail(&p.original), path: p.original.clone(), left: LIFETIME, hovered: false };
                    self.cards.push(card);
                }
                Err(e) => self.error = Some((format!("Could not restore: {e}"), Instant::now())),
            }
        }
        cx.notify();
        cx.defer(relayout);
    }

    fn copy(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let result = editor::baked(&path).and_then(|img| editor::copy_image(&img, cx));
        if let Err(e) = result {
            self.error = Some((e.to_string(), Instant::now()));
            cx.notify();
            cx.defer(relayout);
        }
    }

    fn save_as(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("Screenshot.png").to_string();
        let rx = cx.prompt_for_new_path(&dir, Some(&name));
        let weak = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let Ok(Ok(Some(dest))) = rx.await else { return };
            let result = editor::baked(&path).and_then(|img| {
                let format = match dest.extension().and_then(|e| e.to_str()).map(str::to_lowercase).as_deref() {
                    Some("jpg") | Some("jpeg") => editor::export::Format::Jpg,
                    Some("webp") => editor::export::Format::Webp,
                    _ => editor::export::Format::Png,
                };
                Ok(std::fs::write(&dest, editor::export::encode(&img, format, 92)?)?)
            });
            if let Err(e) = result {
                let _ = weak.update(cx, |c, cx| {
                    c.error = Some((e.to_string(), Instant::now()));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn card(&self, i: usize, c: &Card, cx: &mut Context<Self>) -> impl IntoElement {
        let path = c.path.clone();
        let (p1, p2, p3, p4, p5, p6) = (path.clone(), path.clone(), path.clone(), path.clone(), path.clone(), path.clone());
        let name = time_label(path.file_stem().and_then(|n| n.to_str()).unwrap_or(""));
        let icon_btn = |id: String, icon: gpui_kit::component::Icon, tip: &'static str| {
            Button::new(gpui_kit::SharedString::from(id)).cursor_pointer().ghost().xsmall().rounded_full().icon(icon).tooltip(tip)
        };
        // Image on top, actions in their own row: readable whatever the screenshot shows.
        let image = div()
            .id(("card-image", i))
            .h(px(IMAGE_H))
            .w_full()
            .overflow_hidden().rounded_t(px(ui::R_LG - 5.0))
            .bg(rgb(0x0e0e10))
            .cursor_pointer()
            .children(c.thumb.clone().map(|t| img(t).size_full().rounded_t(px(ui::R_LG - 5.0)).object_fit(ObjectFit::Cover)))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.edit(p1.clone(), cx)));
        let actions = div()
            .h(px(ACTIONS_H))
            .px(px(ui::S2))
            .flex()
            .items_center()
            .gap(px(2.))
            .border_t_1()
            .border_color(ui::hairline())
            .child(
                Button::new(("edit", i)).cursor_pointer().primary().xsmall().rounded_full().px(px(ui::S3)).icon(icons::edit()).label("Edit").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    this.edit(p2.clone(), cx)
                })),
            )
            .child(div().flex_1().min_w_0().px(px(ui::S2)).text_xs().text_color(rgb(MUTED)).truncate().child(name))
            .child(icon_btn(format!("copy-{i}"), icons::copy(), "Copy to clipboard").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                cx.stop_propagation();
                this.copy(p3.clone(), cx)
            })))
            .child(icon_btn(format!("save-{i}"), icons::save(), "Save as…").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                cx.stop_propagation();
                this.save_as(p4.clone(), cx)
            })))
            // Destructive: last, and red.
            .child(
                icon_btn(format!("delete-{i}"), icons::trash(), "Delete (undo for 5 s)").text_color(rgb(crate::RED)).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    this.delete(p5.clone(), cx)
                })),
            );
        div()
            .id(("card", i))
            .w(px(CARD_W))
            .h(px(CARD_H))
            .on_hover(cx.listener(move |this, h: &bool, _, cx| {
                if let Some(c) = this.cards.iter_mut().find(|c| c.path == p6) {
                    c.hovered = *h;
                    cx.notify();
                }
            }))
            .child(ui::bezel(ui::R_LG, 5.0, div().size_full().flex().flex_col().child(image).child(actions)).size_full())
            .with_animation(
                gpui_kit::ElementId::Name(format!("card-in-{}", path.display()).into()),
                gpui_kit::Animation::new(ui::ENTER).with_easing(ui::ease_out),
                |el, t| el.opacity(t),
            )
    }
}

impl Render for Cards {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let hidden = self.cards.len().saturating_sub(MAX_SHOWN);
        let shown: Vec<usize> = (hidden..self.cards.len()).collect();
        let mut col = div().size_full().flex().flex_col().justify_end().items_center().gap(px(GAP)).pb(px(GAP));
        if hidden > 0 {
            col = col.child(
                div()
                    .h(px(BADGE_H))
                    .px(px(ui::S3))
                    .flex()
                    .items_center()
                    .rounded_full()
                    .bg(rgb(CARD))
                    .border_1()
                    .border_color(ui::hairline_strong())
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(TEXT))
                    .child(format!("+{hidden} more")),
            );
        }
        for i in shown {
            let el = self.card(i, &self.cards[i], cx).into_any_element();
            col = col.child(el);
        }
        if let Some((_, at)) = &self.toast {
            let left = UNDO.saturating_sub(at.elapsed()).as_secs() + 1;
            col = col.child(
                ui::toast_pill()
                    .w(px(CARD_W))
                    .justify_between()
                    .child(format!("Screenshot deleted · {left}s"))
                    .child(
                        Button::new("undo").cursor_pointer()
                            .ghost()
                            .small()
                            .rounded_full()
                            .text_color(rgb(GREEN))
                            .label("Undo")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.undo(cx))),
                    ),
            );
        } else if let Some((msg, _)) = &self.error {
            col = col.child(
                ui::toast_pill()
                    .id("card-error")
                    .w(px(CARD_W))
                    .h_auto()
                    .py(px(ui::S2))
                    .pr(px(ui::S4))
                    .items_start()
                    .child(div().mt(px(5.)).size(px(8.)).flex_none().rounded_full().bg(rgb(crate::RED)))
                    .child(div().text_xs().line_height(px(16.)).child(msg.clone()))
                    .tooltip({
                        let m = msg.clone();
                        move |window, cx| Tooltip::new(m.clone()).build(window, cx)
                    }),
            );
        }
        col
    }
}
