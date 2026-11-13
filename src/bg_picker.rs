//! The background picker, shared by the recorder (Scene tab) and the screenshot editor
//! (Background panel): a segmented control (Gradient, Color, Image) over the one background
//! system in `backgrounds`. Gradient shows the five presets, a Custom swatch that opens the
//! gradient editor, the user's saved gradients ("Mine") and an Animate toggle. It emits `PickerEvent::Changed` with the new background; the owner
//! stores it and re-renders.

use crate::theme as tk;
use crate::backgrounds::generators::{self, Harmony};
use crate::backgrounds::library::{self, Filter};
use crate::backgrounds::presets;
use crate::backgrounds::{self, AnimStyle, Animated, Background, Fit, Gradient, GradientKind, ImageBg, ImageSource, Rgb, Stop};
use crate::ui;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Sizable, tooltip::Tooltip};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, Bounds, ClickEvent, DispatchPhase, Div, Entity, EventEmitter, ExternalPaths, FontWeight, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ObjectFit, PathPromptOptions, Pixels, RenderImage, SharedString, Subscription,
    Window, div, img, px, rgb,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};

pub enum PickerEvent {
    Changed(Background),
    /// The editor's still frame of an animated background moved.
    Moment(f32),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    /// Static or animated gradients.
    Gradient,
    Color,
    Image,
}

impl Tab {
    const ALL: [Tab; 3] = [Tab::Gradient, Tab::Color, Tab::Image];
    fn label(self) -> &'static str {
        match self {
            Tab::Gradient => "Gradient",
            Tab::Color => "Color",
            Tab::Image => "Image",
        }
    }
    fn of(bg: &Background) -> Tab {
        match bg {
            Background::Gradient(_) | Background::Animated(_) => Tab::Gradient,
            Background::Image(_) => Tab::Image,
            Background::Solid { .. } => Tab::Color,
        }
    }
}

/// Swatch size in the Gradient tab: three to a row in the 320 px panel.
const SWATCH_H: f32 = 72.0;

/// Numeric settings with a slider.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
enum Knob {
    Angle,
    Grain,
    Hue,
    Speed,
    Intensity,
    Blur,
    Period,
    Moment,
    Zoom,
    PanX,
    PanY,
    ImageBlur,
    Darken,
    /// The selected stop's (or the solid's) color in OKLCh.
    ColorHue,
    ColorLight,
    ColorChroma,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Drag {
    Knob(Knob),
    Stop(usize),
    Focal,
}

const SOLIDS: [Rgb; 14] = tk::content::SOLIDS;

/// Thumbnails, shared by every picker (keyed by preset/image id).
fn thumbs() -> &'static Mutex<HashMap<String, Arc<RenderImage>>> {
    static T: OnceLock<Mutex<HashMap<String, Arc<RenderImage>>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(HashMap::new()))
}

fn thumb(key: &str, make: impl FnOnce() -> Option<image::RgbImage>) -> Option<Arc<RenderImage>> {
    if let Some(t) = thumbs().lock().unwrap().get(key) {
        return Some(t.clone());
    }
    let img = make()?;
    let t = crate::canvas_preview::to_render_image(image::DynamicImage::ImageRgb8(img).to_rgba8());
    thumbs().lock().unwrap().insert(key.to_string(), t.clone());
    Some(t)
}

pub struct BgPicker {
    bg: Background,
    tab: Tab,
    /// Output is 9:16 (portrait images first) or 16:9.
    portrait: bool,
    /// The editor shows a "moment" slider for animated backgrounds.
    moment: Option<f32>,
    /// The gradient editor is open (the Custom swatch).
    custom_open: bool,
    image_filter: Option<Filter>,
    hue: f32,
    harmony: Harmony,
    seed: u64,
    stop: usize,
    selected_image: Option<String>,
    drag: Option<Drag>,
    bounds: Rc<RefCell<HashMap<&'static str, Bounds<Pixels>>>>,
    rename: Entity<InputState>,
    tags: Entity<InputState>,
    /// Picks the edited color (stop, mesh corner or solid).
    color: Entity<ColorPickerState>,
    error: Option<String>,
    /// Last background shown in the rename/tag fields.
    fields_for: Option<String>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<PickerEvent> for BgPicker {}

impl BgPicker {
    pub fn new(bg: Background, portrait: bool, moment: Option<f32>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let rename = cx.new(|cx| InputState::new(window, cx).placeholder("Name"));
        let tags = cx.new(|cx| InputState::new(window, cx).placeholder("Tags, comma separated"));
        let color = cx.new(|cx| ColorPickerState::new(window, cx));
        let mut subs = vec![];
        subs.push(cx.subscribe_in(&rename, window, |this: &mut Self, input, ev: &InputEvent, _, cx| {
            if matches!(ev, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                if let Some(id) = this.selected_image.clone() {
                    let name = input.read(cx).value().to_string();
                    if !name.trim().is_empty() {
                        library::global().lock().unwrap().rename(&id, &name);
                        cx.notify();
                    }
                }
            }
        }));
        subs.push(cx.subscribe_in(&tags, window, |this: &mut Self, input, ev: &InputEvent, _, cx| {
            if matches!(ev, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                if let Some(id) = this.selected_image.clone() {
                    let tags = input.read(cx).value().split(',').map(str::to_string).collect();
                    library::global().lock().unwrap().set_tags(&id, tags);
                    cx.notify();
                }
            }
        }));
        subs.push(cx.subscribe_in(&color, window, |this: &mut Self, _, ev: &ColorPickerEvent, _, cx| {
            if let ColorPickerEvent::Change(Some(c)) = ev {
                let c = gpui_kit::Rgba::from(*c);
                let to8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                this.set_edited_color([to8(c.r), to8(c.g), to8(c.b)], cx);
            }
        }));
        Self {
            tab: Tab::of(&bg),
            bg,
            portrait,
            moment,
            custom_open: false,
            image_filter: None,
            hue: 200.0,
            harmony: Harmony::Analogous,
            seed: 1,
            stop: 0,
            selected_image: None,
            drag: None,
            bounds: Rc::new(RefCell::new(HashMap::new())),
            rename,
            tags,
            color,
            error: None,
            fields_for: None,
            _subs: subs,
        }
    }

    /// The owner changed the background (undo, another window): show it.
    pub fn set_background(&mut self, bg: Background, cx: &mut Context<Self>) {
        if self.bg != bg {
            // A different kind of background: show its tab.
            if std::mem::discriminant(&self.bg) != std::mem::discriminant(&bg) {
                self.tab = Tab::of(&bg);
            }
            self.bg = bg;
            cx.notify();
        }
    }

    pub fn set_portrait(&mut self, portrait: bool, cx: &mut Context<Self>) {
        if self.portrait != portrait {
            self.portrait = portrait;
            cx.notify();
        }
    }

    pub fn set_moment(&mut self, moment: f32, cx: &mut Context<Self>) {
        if self.moment.is_some() {
            self.moment = Some(moment);
            cx.notify();
        }
    }

    fn set(&mut self, bg: Background, cx: &mut Context<Self>) {
        if let Background::Image(ImageBg { source: ImageSource::Library(id), .. }) = &bg {
            library::global().lock().unwrap().touch(id);
        }
        self.bg = bg.clone();
        cx.emit(PickerEvent::Changed(bg));
        cx.notify();
    }

    fn edit_gradient(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Gradient)) {
        if let Background::Gradient(mut g) = self.bg.clone() {
            f(&mut g);
            self.set(Background::Gradient(g), cx);
        }
    }

    fn edit_anim(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Animated)) {
        if let Background::Animated(mut a) = self.bg.clone() {
            f(&mut a);
            self.set(Background::Animated(a), cx);
        }
    }

    fn edit_image(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut ImageBg)) {
        if let Background::Image(mut i) = self.bg.clone() {
            f(&mut i);
            self.set(Background::Image(i), cx);
        }
    }

    // ---------- sliders ----------

    fn range(&self, k: Knob) -> (f32, f32, f32) {
        match k {
            Knob::Angle => (0.0, 360.0, 1.0),
            Knob::Grain => (0.0, 100.0, 1.0),
            Knob::Hue => (0.0, 360.0, 1.0),
            Knob::Speed => (1.0, 4.0, 1.0),
            Knob::Intensity | Knob::Blur => (0.0, 100.0, 1.0),
            Knob::Period => (4.0, 20.0, 1.0),
            Knob::Moment => (0.0, match &self.bg {
                Background::Animated(a) => a.period,
                _ => 8.0,
            }, 0.1),
            Knob::Zoom => (1.0, 3.0, 0.05),
            Knob::PanX | Knob::PanY => (-100.0, 100.0, 1.0),
            Knob::ImageBlur => (0.0, 100.0, 1.0),
            Knob::Darken => (0.0, 80.0, 1.0),
            Knob::ColorHue => (0.0, 360.0, 1.0),
            Knob::ColorLight => (0.0, 100.0, 1.0),
            Knob::ColorChroma => (0.0, 37.0, 0.5),
        }
    }

    fn get(&self, k: Knob) -> f32 {
        match (&self.bg, k) {
            (Background::Gradient(g), Knob::Angle) => match g.kind {
                GradientKind::Linear { angle } | GradientKind::Conic { angle, .. } => angle,
                _ => 0.0,
            },
            (Background::Gradient(g), Knob::Grain) => g.grain,
            (Background::Solid { grain, .. }, Knob::Grain) => *grain,
            (Background::Animated(a), Knob::Grain) => a.grain,
            (Background::Image(i), Knob::Grain) => i.grain,
            (_, Knob::Hue) => self.hue,
            (Background::Animated(a), Knob::Speed) => a.speed,
            (Background::Animated(a), Knob::Intensity) => a.intensity * 100.0,
            (Background::Animated(a), Knob::Blur) => a.blur * 100.0,
            (Background::Animated(a), Knob::Period) => a.period,
            (_, Knob::Moment) => self.moment.unwrap_or(0.0),
            (Background::Image(i), Knob::Zoom) => i.zoom,
            (Background::Image(i), Knob::PanX) => i.pan.0 * 100.0,
            (Background::Image(i), Knob::PanY) => i.pan.1 * 100.0,
            (Background::Image(i), Knob::ImageBlur) => i.blur,
            (Background::Image(i), Knob::Darken) => i.darken * 100.0,
            (_, Knob::ColorHue | Knob::ColorLight | Knob::ColorChroma) => {
                let (l, c, h) = backgrounds::color::lch(self.edited_color().unwrap_or([128, 128, 128]));
                match k {
                    Knob::ColorHue => h,
                    Knob::ColorLight => l * 100.0,
                    _ => c * 100.0,
                }
            }
            _ => 0.0,
        }
    }

    fn put(&mut self, k: Knob, v: f32, cx: &mut Context<Self>) {
        match k {
            Knob::Angle => self.edit_gradient(cx, |g| match &mut g.kind {
                GradientKind::Linear { angle } | GradientKind::Conic { angle, .. } => *angle = v,
                _ => {}
            }),
            Knob::Grain => match self.bg.clone() {
                Background::Gradient(mut g) => {
                    g.grain = v;
                    self.set(Background::Gradient(g), cx);
                }
                Background::Solid { color, .. } => self.set(Background::Solid { color, grain: v }, cx),
                Background::Animated(mut a) => {
                    a.grain = v;
                    self.set(Background::Animated(a), cx);
                }
                Background::Image(mut i) => {
                    i.grain = v;
                    self.set(Background::Image(i), cx);
                }
            },
            Knob::Hue => {
                self.hue = v;
                let angle = match &self.bg {
                    Background::Gradient(Gradient { kind: GradientKind::Linear { angle }, .. }) => *angle,
                    _ => 135.0,
                };
                self.set(Background::Gradient(generators::gradient(v, self.harmony, angle)), cx);
            }
            Knob::Speed => self.edit_anim(cx, |a| a.speed = v),
            Knob::Intensity => self.edit_anim(cx, |a| a.intensity = v / 100.0),
            Knob::Blur => self.edit_anim(cx, |a| a.blur = v / 100.0),
            Knob::Period => self.edit_anim(cx, |a| a.period = v),
            Knob::Moment => {
                self.moment = Some(v);
                cx.emit(PickerEvent::Moment(v));
                cx.notify();
            }
            Knob::Zoom => self.edit_image(cx, |i| i.zoom = v),
            Knob::PanX => self.edit_image(cx, |i| i.pan.0 = v / 100.0),
            Knob::PanY => self.edit_image(cx, |i| i.pan.1 = v / 100.0),
            Knob::ImageBlur => self.edit_image(cx, |i| i.blur = v),
            Knob::Darken => self.edit_image(cx, |i| i.darken = v / 100.0),
            Knob::ColorHue | Knob::ColorLight | Knob::ColorChroma => {
                let (mut l, mut c, mut h) = backgrounds::color::lch(self.edited_color().unwrap_or([128, 128, 128]));
                match k {
                    Knob::ColorHue => h = v,
                    Knob::ColorLight => l = v / 100.0,
                    _ => c = v / 100.0,
                }
                self.set_edited_color(backgrounds::color::from_lch(l, c, h), cx);
            }
        }
    }

    fn knob(&self, k: Knob, label: &'static str, cx: &mut Context<Self>) -> gpui_kit::Stateful<Div> {
        let (min, max, _) = self.range(k);
        let v = self.get(k);
        let shown = match k {
            Knob::Angle | Knob::Hue | Knob::ColorHue => format!("{}°", v.round()),
            Knob::ColorLight => format!("{}%", v.round()),
            Knob::ColorChroma => format!("{:.1}", v),
            Knob::Speed => format!("{}×", v.round()),
            Knob::Period => format!("{} s", v.round()),
            Knob::Moment => format!("{v:.1} s"),
            Knob::Zoom => format!("{v:.2}×"),
            Knob::Darken => format!("{}%", v.round()),
            _ => format!("{}", v.round()),
        };
        let key: &'static str = match k {
            Knob::Angle => "angle",
            Knob::Grain => "grain",
            Knob::Hue => "hue",
            Knob::Speed => "speed",
            Knob::Intensity => "intensity",
            Knob::Blur => "blur",
            Knob::Period => "period",
            Knob::Moment => "moment",
            Knob::Zoom => "zoom",
            Knob::PanX => "panx",
            Knob::PanY => "pany",
            Knob::ImageBlur => "iblur",
            Knob::Darken => "darken",
            Knob::ColorHue => "chue",
            Knob::ColorLight => "clight",
            Knob::ColorChroma => "cchroma",
        };
        let store = self.bounds.clone();
        ui::slider(("bgknob", k as usize), label, shown, (v - min) / (max - min).max(1e-6), self.drag == Some(Drag::Knob(k)), move |b| {
            store.borrow_mut().insert(key, b);
        })
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                this.drag = Some(Drag::Knob(k));
                this.knob_to(k, key, ev.position.x, cx);
            }),
        )
    }

    fn knob_to(&mut self, k: Knob, key: &'static str, x: Pixels, cx: &mut Context<Self>) {
        let Some(b) = self.bounds.borrow().get(key).copied() else { return };
        let (min, max, step) = self.range(k);
        let v = ((min + ui::slider_fraction(b, x) * (max - min)) / step).round() * step;
        self.put(k, v.clamp(min, max), cx);
    }

    /// Follows the pointer for any drag in progress, even outside the picker.
    fn drag_to(&mut self, ev: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(drag) = self.drag else { return };
        if ev.pressed_button != Some(MouseButton::Left) {
            self.drag = None;
            cx.notify();
            return;
        }
        match drag {
            Drag::Knob(k) => {
                let key = match k {
                    Knob::Angle => "angle",
                    Knob::Grain => "grain",
                    Knob::Hue => "hue",
                    Knob::Speed => "speed",
                    Knob::Intensity => "intensity",
                    Knob::Blur => "blur",
                    Knob::Period => "period",
                    Knob::Moment => "moment",
                    Knob::Zoom => "zoom",
                    Knob::PanX => "panx",
                    Knob::PanY => "pany",
                    Knob::ImageBlur => "iblur",
                    Knob::Darken => "darken",
                    Knob::ColorHue => "chue",
                    Knob::ColorLight => "clight",
                    Knob::ColorChroma => "cchroma",
                };
                self.knob_to(k, key, ev.position.x, cx);
            }
            Drag::Stop(i) => {
                let Some(b) = self.bounds.borrow().get("stops").copied() else { return };
                let f = ((f32::from(ev.position.x) - f32::from(b.origin.x)) / f32::from(b.size.width).max(1.0)).clamp(0.0, 1.0);
                self.edit_gradient(cx, |g| {
                    if let Some(s) = g.stops.get_mut(i) {
                        s.pos = f;
                    }
                });
            }
            Drag::Focal => self.focal_to(ev.position, cx),
        }
    }

    /// The color the color sliders edit: the selected gradient stop / mesh corner, or the
    /// solid color.
    fn edited_color(&self) -> Option<Rgb> {
        match &self.bg {
            Background::Gradient(g) => match &g.kind {
                GradientKind::Mesh { colors, .. } => colors.get(self.stop).copied(),
                _ => g.stops.get(self.stop).map(|s| s.color),
            },
            Background::Solid { color, .. } => Some(*color),
            _ => None,
        }
    }

    fn set_edited_color(&mut self, c: Rgb, cx: &mut Context<Self>) {
        let i = self.stop;
        match self.bg.clone() {
            Background::Gradient(_) => self.edit_gradient(cx, |g| match &mut g.kind {
                GradientKind::Mesh { colors, .. } => {
                    if let Some(x) = colors.get_mut(i) {
                        *x = c;
                    }
                }
                _ => {
                    if let Some(s) = g.stops.get_mut(i) {
                        s.color = c;
                    }
                }
            }),
            Background::Solid { grain, .. } => self.set(Background::Solid { color: c, grain }, cx),
            _ => {}
        }
    }

    /// Hue, lightness and chroma sliders for the edited color (OKLCh, so lightness stays
    /// even as the hue changes).
    fn color_knobs(&self, cx: &mut Context<Self>) -> Div {
        let c = self.edited_color().unwrap_or([128, 128, 128]);
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().size(px(18.)).rounded(px(5.)).bg(rgb(hex(c))).border_1().border_color(ui::hairline()))
                    .child(ui::eyebrow(format!("#{:06X}", hex(c))))
                    .child(div().flex_1())
                    .child(
                        div()
                            .id("pick-color")
                            .tooltip(|w, cx| Tooltip::new("Pick a color").build(w, cx))
                            .child(ColorPicker::new(&self.color).cursor_pointer().small().accessibility_label("Pick a color")),
                    ),
            )
            .child(self.knob(Knob::ColorHue, "Hue", cx))
            .child(self.knob(Knob::ColorLight, "Lightness", cx))
            .child(self.knob(Knob::ColorChroma, "Chroma", cx))
    }

    fn focal_to(&mut self, p: gpui_kit::Point<Pixels>, cx: &mut Context<Self>) {
        let Some(b) = self.bounds.borrow().get("focal").copied() else { return };
        let fx = ((f32::from(p.x) - f32::from(b.origin.x)) / f32::from(b.size.width).max(1.0)).clamp(0.0, 1.0);
        let fy = ((f32::from(p.y) - f32::from(b.origin.y)) / f32::from(b.size.height).max(1.0)).clamp(0.0, 1.0);
        self.edit_image(cx, |i| i.focal = (fx, fy));
    }

    // ---------- tabs ----------

    /// A gradient swatch: a large rendered preview, ink outline when selected.
    fn grad_swatch(&self, id: &str, name: String, g: &Gradient, on: bool, cx: &mut Context<Self>, click: impl Fn(&mut Self, &mut Context<Self>) + 'static) -> AnyElement {
        let t = thumb(&format!("g:{id}:{:?}", g.stops), || Some(backgrounds::render::gradient(g, 200, 144)));
        ui::swatch(SharedString::from(format!("gs-{id}")), on, div().size_full().children(t.map(|t| img(t).size_full().object_fit(ObjectFit::Cover))))
            .h(px(SWATCH_H))
            .tooltip(move |w, cx| Tooltip::new(name.clone()).build(w, cx))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| click(this, cx)))
            .into_any_element()
    }

    fn gradients_tab(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        // The colors on show (static or animated); a swatch is selected when they match.
        let (current, anim) = match &self.bg {
            Background::Gradient(g) => (Some(g.clone()), None),
            Background::Animated(a) => (None, Some(a.clone())),
            _ => (None, None),
        };
        let matches = |g: &Gradient| match (&current, &anim) {
            (Some(c), _) => c == g,
            (_, Some(a)) => {
                let mut x = a.colors.clone();
                let mut y = g.colors();
                x.sort();
                y.sort();
                x == y
            }
            _ => false,
        };
        let pick = |g: Gradient| {
            move |this: &mut Self, cx: &mut Context<Self>| {
                this.stop = 0;
                this.custom_open = false;
                match &this.bg {
                    Background::Animated(_) => {
                        let mut c = g.colors();
                        // Aurora's first color is the base: start from the darkest.
                        c.sort_by(|x, y| backgrounds::color::lch(*x).0.total_cmp(&backgrounds::color::lch(*y).0));
                        this.edit_anim(cx, |a| a.colors = c);
                    }
                    _ => this.set(Background::Gradient(g.clone()), cx),
                }
            }
        };
        let mut grid = div().grid().grid_cols(3).gap(px(ui::S2));
        let mut any_selected = false;
        for p in presets::gradients() {
            let on = !self.custom_open && matches(&p.gradient);
            any_selected |= on;
            grid = grid.child(self.grad_swatch(&p.id, p.name.clone(), &p.gradient, on, cx, pick(p.gradient.clone())));
        }
        let mine: Vec<library::SavedGradient> = library::global().lock().unwrap().saved_gradients().to_vec();
        let mine_selected = mine.iter().any(|g| matches(&g.gradient));
        // Custom: the current gradient when it's none of the swatches, else a plus.
        let custom_on = self.custom_open || (current.is_some() && !any_selected && !mine_selected);
        let custom_fill = match (&current, custom_on) {
            (Some(g), true) => thumb(&format!("custom:{:?}", g.stops), || Some(backgrounds::render::gradient(g, 200, 144))).map(|t| img(t).size_full().object_fit(ObjectFit::Cover).into_any_element()),
            _ => None,
        };
        grid = grid.child(
            ui::swatch("gs-custom", custom_on, div().size_full().relative().bg(rgb(tk::raised())).children(custom_fill).child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(2.))
                    .text_size(px(tk::T12))
                    .font_weight(FontWeight::SEMIBOLD)
                    .when(!custom_on || current.is_none(), |d| d.child(crate::editor::icons::plus().size(px(18.))).child("Custom"))
                    .when(custom_on && current.is_some(), |d| d.child(div().px(px(ui::S2)).py(px(2.)).rounded_full().bg(rgb(tk::paper())).child("Custom"))),
            ))
            .h(px(SWATCH_H))
            .tooltip(|w, cx| Tooltip::new("Edit your own gradient").build(w, cx))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.custom_open = !this.custom_open;
                // Start from what's showing.
                let start = match &this.bg {
                    Background::Gradient(_) => None,
                    Background::Animated(a) => Some(Gradient::linear(135.0, &a.colors)),
                    _ => Some(presets::gradients()[0].gradient.clone()),
                };
                if let (true, Some(g)) = (this.custom_open, start) {
                    this.set(Background::Gradient(g), cx);
                }
                cx.notify();
            })),
        );
        let mut out = vec![grid.into_any_element()];
        if !mine.is_empty() {
            let mut mg = div().grid().grid_cols(3).gap(px(ui::S2));
            for g in &mine {
                let on = !self.custom_open && matches(&g.gradient);
                mg = mg.child(self.grad_swatch(&g.id, g.name.clone(), &g.gradient, on, cx, pick(g.gradient.clone())));
            }
            out.push(ui::field("Mine", None, mg).into_any_element());
        }
        // Animate: the same colors, moving with the timeline.
        let animated = anim.is_some();
        let this = cx.entity().downgrade();
        out.push(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(div().text_size(px(tk::T13)).font_weight(FontWeight::MEDIUM).child("Animate"))
                .child(Switch::new("animate").cursor_pointer().checked(animated).on_click(move |on: &bool, _, cx| {
                    let on = *on;
                    let _ = this.update(cx, |p, cx| p.set_animated(on, cx));
                }))
                .into_any_element(),
        );
        match (anim, current) {
            (Some(a), _) => out.extend(self.anim_settings(&a, cx)),
            (None, Some(g)) if self.custom_open => {
                out.extend(self.gradient_editor(&g, cx));
                out.extend(self.generator(cx));
            }
            _ => {}
        }
        out
    }

    /// Animate on: the current gradient's colors as an animated background; off: back to a
    /// static gradient of the same colors.
    fn set_animated(&mut self, on: bool, cx: &mut Context<Self>) {
        match (&self.bg, on) {
            (Background::Gradient(g), true) => {
                let mut c = g.colors();
                c.sort_by(|x, y| backgrounds::color::lch(*x).0.total_cmp(&backgrounds::color::lch(*y).0));
                c.truncate(6);
                self.custom_open = false;
                self.set(Background::Animated(Animated { colors: c, ..Animated::default() }), cx);
            }
            (Background::Animated(a), false) => {
                let g = presets::gradients().iter().map(|p| &p.gradient).find(|g| {
                    let (mut x, mut y) = (g.colors(), a.colors.clone());
                    x.sort();
                    y.sort();
                    x == y
                });
                let g = g.cloned().unwrap_or_else(|| Gradient::linear(135.0, &a.colors));
                self.set(Background::Gradient(g), cx);
            }
            _ => {}
        }
    }

    fn gradient_editor(&mut self, g: &Gradient, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let kind_ix = match g.kind {
            GradientKind::Linear { .. } => 0,
            GradientKind::Radial { .. } => 1,
            GradientKind::Conic { .. } => 2,
            GradientKind::Mesh { .. } => 3,
        };
        let kinds = ["Linear", "Radial", "Conic", "Mesh"]
            .into_iter()
            .enumerate()
            .map(|(i, l)| {
                let on: ui::OnClick = Box::new(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.stop = 0;
                    this.edit_gradient(cx, |g| {
                        let colors = g.colors();
                        g.kind = match i {
                            0 => GradientKind::Linear { angle: 135.0 },
                            1 => GradientKind::Radial { cx: 0.5, cy: 0.5 },
                            2 => GradientKind::Conic { cx: 0.5, cy: 0.5, angle: 0.0 },
                            _ => {
                                let mut c = colors.clone();
                                while c.len() < 4 {
                                    c.push(colors[c.len() % colors.len().max(1)]);
                                }
                                c.truncate(4);
                                GradientKind::Mesh { cols: 2, rows: 2, colors: c }
                            }
                        };
                        if i < 3 && g.stops.len() < 2 {
                            *g = Gradient { kind: g.kind.clone(), ..Gradient::linear(0.0, &colors) };
                        }
                    });
                }));
                (l.into(), i == kind_ix, on)
            })
            .collect();
        let mut out = vec![
            div().pt(px(ui::S2)).text_size(px(tk::T13)).font_weight(FontWeight::SEMIBOLD).child("Edit gradient").into_any_element(),
            ui::segmented("gkind", kinds).into_any_element(),
        ];
        if matches!(g.kind, GradientKind::Linear { .. } | GradientKind::Conic { .. }) {
            out.push(self.knob(Knob::Angle, "Angle", cx).into_any_element());
        }
        match &g.kind {
            GradientKind::Mesh { colors, .. } => {
                // The four corner colors; click one to edit it.
                let mut grid = div().flex().flex_wrap().gap(px(6.));
                for (i, c) in colors.iter().enumerate() {
                    let on = self.stop == i;
                    grid = grid.child(
                        div()
                            .id(("mesh", i))
                            .w(px(60.))
                            .h(px(32.))
                            .rounded(px(8.))
                            .bg(rgb(hex(*c)))
                            .border_2()
                            .border_color(if on { gpui_kit::Hsla::from(rgb(tk::text())) } else { ui::hairline() })
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.stop = i;
                                cx.notify();
                            })),
                    );
                }
                out.push(ui::field("Corner colors", None, grid).into_any_element());
            }
            _ => out.push(self.stops_bar(g, cx).into_any_element()),
        }
        out.push(ui::field(if matches!(g.kind, GradientKind::Mesh { .. }) { "Corner color" } else { "Stop color" }, None, self.color_knobs(cx)).into_any_element());
        let stop_ops = div()
            .flex()
            .items_center()
            .gap_2()
            .when(!matches!(g.kind, GradientKind::Mesh { .. }) && g.stops.len() > 2, |d| {
                d.child(Button::new("rm-stop").cursor_pointer().ghost().xsmall().text_color(rgb(tk::error_text())).label("Remove stop").on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    let i = this.stop;
                    this.edit_gradient(cx, |g| {
                        if g.stops.len() > 2 && i < g.stops.len() {
                            g.stops.remove(i);
                        }
                    });
                    this.stop = 0;
                })))
            });
        out.push(stop_ops.into_any_element());
        out.push(self.knob(Knob::Grain, "Grain", cx).into_any_element());
        out
    }

    /// The gradient as a bar with draggable stop handles; click the bar to add a stop.
    fn stops_bar(&mut self, g: &Gradient, cx: &mut Context<Self>) -> Div {
        let strip = thumb(&format!("strip:{:?}", g.stops), || Some(backgrounds::render::gradient(&Gradient { kind: GradientKind::Linear { angle: 90.0 }, ..g.clone() }, 256, 8)));
        let store = self.bounds.clone();
        let mut bar = div()
            .id("stops")
            .relative()
            .h(px(28.))
            .w_full()
            .rounded(px(8.))
            .border_1()
            .border_color(ui::hairline())
            .cursor_pointer()
            .children(strip.map(|s| img(s).size_full().rounded(px(8.)).object_fit(ObjectFit::Fill)))
            .child(gpui_kit::canvas(move |b, _, _| {
                store.borrow_mut().insert("stops", b);
            }, |_, _, _, _| {}).absolute().size_full())
            .on_mouse_down(MouseButton::Left, cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                // Add a stop where clicked, colored like the gradient there.
                let Some(b) = this.bounds.borrow().get("stops").copied() else { return };
                let f = ((f32::from(ev.position.x) - f32::from(b.origin.x)) / f32::from(b.size.width).max(1.0)).clamp(0.0, 1.0);
                let mut idx = 0;
                this.edit_gradient(cx, |g| {
                    let lut = backgrounds::color::lut(&g.stops.iter().map(|s| (s.pos, s.color)).collect::<Vec<_>>());
                    g.stops.push(Stop { pos: f, color: lut[(f * 255.0).round() as usize] });
                    idx = g.stops.len() - 1;
                });
                this.stop = idx;
                this.drag = Some(Drag::Stop(idx));
            }));
        for (i, s) in g.stops.iter().enumerate() {
            let on = self.stop == i;
            bar = bar.child(
                div()
                    .id(("stop", i))
                    .absolute()
                    .top(px(-4.))
                    .left(gpui_kit::relative(s.pos))
                    .ml(px(-7.))
                    .w(px(14.))
                    .h(px(36.))
                    .rounded(px(5.))
                    .border_2()
                    .border_color(rgb(if on { tk::text() } else { tk::stone() }))
                    .bg(rgb(hex(s.color)))
                    .shadow_md()
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.stop = i;
                        this.drag = Some(Drag::Stop(i));
                        cx.notify();
                    })),
            );
        }
        ui::field("Stops", Some("Click to add, drag to move".into()), bar)
    }

    fn generator(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let harmonies = Harmony::ALL
            .into_iter()
            .map(|h| {
                let on: ui::OnClick = Box::new(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.harmony = h;
                    let hue = this.hue;
                    this.set(Background::Gradient(generators::gradient(hue, h, 135.0)), cx);
                }));
                (h.label().into(), self.harmony == h, on)
            })
            .collect();
        vec![
            div().pt(px(ui::S2)).text_size(px(tk::T13)).font_weight(FontWeight::SEMIBOLD).child("Generate").into_any_element(),
            self.knob(Knob::Hue, "Base hue", cx).into_any_element(),
            ui::segmented("harmony", harmonies).into_any_element(),
            div()
                .flex()
                .gap_2()
                .child(Button::new("randomize").cursor_pointer().secondary().small().rounded_full().flex_1().label("Randomize").on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.seed = this.seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407) ^ std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(7);
                    let (g, hue, harmony) = generators::random(this.seed);
                    this.hue = hue;
                    this.harmony = harmony;
                    this.stop = 0;
                    this.set(Background::Gradient(g), cx);
                })))
                .child(Button::new("save-preset").cursor_pointer().secondary().small().rounded_full().flex_1().label("Save to Mine").on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    if let Background::Gradient(g) = &this.bg {
                        let n = library::global().lock().unwrap().saved_gradients().len() + 1;
                        library::global().lock().unwrap().save_gradient(&format!("Custom {n}"), g.clone());
                        this.custom_open = false;
                        cx.notify();
                    }
                })))
                .into_any_element(),
        ]
    }

    /// Style, speed and the rest, while Animate is on.
    fn anim_settings(&mut self, a: &Animated, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut out = Vec::new();
        let styles = AnimStyle::ALL
            .into_iter()
            .map(|s| {
                let on: ui::OnClick = Box::new(cx.listener(move |this, _: &ClickEvent, _, cx| this.edit_anim(cx, |a| a.style = s)));
                (s.label().into(), a.style == s, on)
            })
            .collect();
        out.push(ui::field("Style", None, ui::segmented("astyle", styles)).into_any_element());
        out.push(self.knob(Knob::Speed, "Speed", cx).into_any_element());
        out.push(ui::hint("Animated backgrounds move with the recording's timeline, so pausing freezes them and exports match the preview.").into_any_element());
        out.push(self.knob(Knob::Intensity, "Intensity", cx).into_any_element());
        out.push(self.knob(Knob::Blur, "Softness", cx).into_any_element());
        out.push(self.knob(Knob::Grain, "Grain", cx).into_any_element());
        out.push(self.knob(Knob::Period, "Loop length", cx).into_any_element());
        let this = cx.entity().downgrade();
        out.push(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(div().text_size(px(tk::T13)).child("Freeze motion"))
                .child(Switch::new("freeze").cursor_pointer().checked(a.frozen).on_click(move |on: &bool, _, cx| {
                    let on = *on;
                    let _ = this.update(cx, |v, cx| v.edit_anim(cx, |a| a.frozen = on));
                }))
                .into_any_element(),
        );
        if self.moment.is_some() {
            out.push(self.knob(Knob::Moment, "Moment", cx).into_any_element());
            out.push(ui::hint("Screenshots are still images: pick the moment of the loop to show.").into_any_element());
        }
        out
    }

    fn images_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let _ = window;
        let filter = self.image_filter;
        let lib = library::global().lock().unwrap();
        // Built-in images and their orientation (read from the file headers).
        let builtin_all: Vec<(usize, backgrounds::fit::Orientation)> = (0..crate::style::WALLPAPERS.len())
            .map(|i| {
                let (w, h) = crate::style::wallpaper_size(i);
                (i, backgrounds::fit::Orientation::of(w, h))
            })
            .collect();
        // Orientation tabs only show when they'd list something.
        let has = |f: Filter, o: backgrounds::fit::Orientation| !lib.ordered(f, true).is_empty() || builtin_all.iter().any(|(_, b)| *b == o);
        let available: Vec<Filter> = [(Filter::Portrait, backgrounds::fit::Orientation::Portrait), (Filter::Landscape, backgrounds::fit::Orientation::Landscape), (Filter::Square, backgrounds::fit::Orientation::Square)]
            .into_iter()
            .filter(|(f, o)| has(*f, *o))
            .map(|(f, _)| f)
            .chain([Filter::Favorites])
            .collect();
        let filter = filter.filter(|f| available.contains(f));
        let entries: Vec<library::ImageEntry> = lib.ordered(filter.unwrap_or(Filter::All), self.portrait).into_iter().cloned().collect();
        let recent: Vec<library::ImageEntry> = lib.recent().into_iter().cloned().collect();
        let favs: Vec<String> = lib.images().iter().filter(|e| lib.is_favorite(&e.id)).map(|e| e.id.clone()).collect();
        let thumb_paths: HashMap<String, std::path::PathBuf> = lib.images().iter().map(|e| (e.id.clone(), lib.thumb(&e.id))).collect();
        let missing: Vec<String> = lib.images().iter().filter(|e| lib.missing(&e.id)).map(|e| e.id.clone()).collect();
        let watched = lib.watched().map(|p| p.display().to_string());
        drop(lib);
        let current = match &self.bg {
            Background::Image(i) => Some(i.clone()),
            _ => None,
        };

        let filters = {
            let mut items: Vec<(SharedString, bool, ui::OnClick)> = Vec::new();
            let auto: ui::OnClick = Box::new(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.image_filter = None;
                cx.notify();
            }));
            items.push(("Auto".into(), filter.is_none(), auto));
            for f in available.iter().copied() {
                let on: ui::OnClick = Box::new(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.image_filter = Some(f);
                    cx.notify();
                }));
                items.push((if f == Filter::Favorites { "★".into() } else { f.label().into() }, filter == Some(f), on));
            }
            ui::segmented("ifilter", items)
        };

        let tile = |id: String, src: ImageSource, t: Option<Arc<RenderImage>>, on: bool, badge: Option<&'static str>, fav: bool, i: usize, cx: &mut Context<Self>| {
            div()
                .id(SharedString::from(format!("img-{i}-{id}")))
                .relative()
                .w(px(56.))
                .h(px(42.))
                .rounded(px(9.))
                .overflow_hidden()
                .bg(rgb(tk::raised()))
                .border_2()
                .border_color(if on { gpui_kit::Hsla::from(rgb(tk::text())) } else { ui::hairline() })
                .cursor_pointer()
                .children(t.map(|t| img(t).size_full().rounded(px(7.)).object_fit(ObjectFit::Cover)))
                .when(fav, |d| d.child(div().absolute().top(px(1.)).right(px(3.)).text_size(px(tk::T12)).text_color(rgb(tk::content::FAVORITE)).child("★")))
                .when_some(badge, |d, b| d.child(div().absolute().inset_0().flex().items_center().justify_center().text_size(px(tk::T12)).text_color(rgb(tk::ink_soft())).child(b)))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    let keep = match &this.bg {
                        Background::Image(i) => Some(i.clone()),
                        _ => None,
                    };
                    let mut ib = ImageBg::new(src.clone());
                    if let Some(k) = keep {
                        ib.fit = k.fit;
                        ib.blur = k.blur;
                        ib.darken = k.darken;
                        ib.grain = k.grain;
                    }
                    this.selected_image = match &src {
                        ImageSource::Library(id) => Some(id.clone()),
                        _ => None,
                    };
                    this.set(Background::Image(ib), cx);
                }))
        };
        let is_on = |src: &ImageSource| current.as_ref().is_some_and(|c| &c.source == src);

        let mut grid = div().flex().flex_wrap().gap(px(6.));
        let mut n = 0;
        for e in &entries {
            let src = ImageSource::Library(e.id.clone());
            let path = thumb_paths.get(&e.id).cloned();
            let t = thumb(&format!("i:{}", e.id), || image::open(path?).ok().map(|i| i.to_rgb8()));
            let badge = missing.contains(&e.id).then_some("Missing");
            grid = grid.child(tile(e.id.clone(), src.clone(), t, is_on(&src), badge, favs.contains(&e.id), n, cx));
            n += 1;
        }
        // Built-in images, filtered and ordered by their own orientation like library images.
        let mut builtins: Vec<(usize, backgrounds::fit::Orientation)> = builtin_all
            .into_iter()
            .filter(|(_, o)| match filter {
                None | Some(Filter::All) => true,
                Some(Filter::Landscape) => *o == backgrounds::fit::Orientation::Landscape,
                Some(Filter::Portrait) => *o == backgrounds::fit::Orientation::Portrait,
                Some(Filter::Square) => *o == backgrounds::fit::Orientation::Square,
                Some(Filter::Favorites) => false,
            })
            .collect();
        let portrait = self.portrait;
        builtins.sort_by_key(|(_, o)| match (o, portrait) {
            (backgrounds::fit::Orientation::Portrait, true) | (backgrounds::fit::Orientation::Landscape, false) => 0,
            (backgrounds::fit::Orientation::Square, _) => 1,
            _ => 2,
        });
        for (i, o) in builtins {
            let src = ImageSource::Builtin(i);
            let t = thumb(&format!("w:{i}"), || {
                let (w, h) = if o == backgrounds::fit::Orientation::Portrait { (84, 112) } else { (112, 84) };
                Some(crate::style::render_wallpaper(i, w, h))
            });
            grid = grid.child(tile(format!("w{i}"), src.clone(), t, is_on(&src), None, false, n, cx));
            n += 1;
        }

        let drop_zone = div()
            .id("drop")
            .w_full()
            .px(px(20.))
            .py(px(18.))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(6.))
            .text_center()
            .rounded(px(12.))
            .border_1()
            .border_dashed()
            .border_color(ui::hairline_strong())
            .text_size(px(tk::T12))
            .text_color(rgb(tk::ink_soft()))
            .cursor_pointer()
            .drag_over::<ExternalPaths>(|s, _, _, _| s.bg(ui::accent_tint()).border_color(rgb(tk::selected())))
            .child(div().text_size(px(tk::T13)).text_color(rgb(tk::text())).child("Import images"))
            .child("Drop PNG, JPG or WebP here, or click to choose")
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| this.import(paths.paths().to_vec(), cx)))
            .on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                let rx = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some("Import".into()) });
                cx.spawn(async move |this, cx| {
                    if let Ok(Ok(Some(paths))) = rx.await {
                        let _ = this.update(cx, |v, cx| v.import(paths, cx));
                    }
                })
                .detach();
            }));

        let watch = div()
            .flex()
            .items_center()
            .gap_2()
            .child(div().flex_1().min_w_0().text_size(px(tk::T12)).text_color(rgb(tk::ink_soft())).truncate().child(match (&watched, library::scanning()) {
                (Some(w), true) => format!("Adding images from {w}..."),
                (Some(w), false) => format!("Watching {w}"),
                (None, _) => "No watched folder".into(),
            }))
            .child(Button::new("watch").cursor_pointer().ghost().xsmall().label(if watched.is_some() { "Change" } else { "Watch a folder" }).on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                let rx = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Watch".into()) });
                cx.spawn(async move |this, cx| {
                    if let Ok(Ok(Some(paths))) = rx.await {
                        let _ = this.update(cx, |_, cx| {
                            library::global().lock().unwrap().set_watched(paths.into_iter().next());
                            cx.notify();
                        });
                        // Import off the UI thread; redraw as images arrive.
                        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                        let d = done.clone();
                        std::thread::spawn(move || {
                            // A periodic scan may be running for the old folder: wait it out.
                            while library::scanning() {
                                std::thread::sleep(std::time::Duration::from_millis(100));
                            }
                            library::scan_watched_global();
                            d.store(true, std::sync::atomic::Ordering::SeqCst);
                        });
                        loop {
                            cx.background_executor().timer(std::time::Duration::from_millis(250)).await;
                            let finished = done.load(std::sync::atomic::Ordering::SeqCst);
                            if this.update(cx, |_, cx| cx.notify()).is_err() || finished {
                                break;
                            }
                        }
                    }
                })
                .detach();
            })))
            .when(watched.is_some(), |d| {
                d.child(Button::new("unwatch").cursor_pointer().ghost().xsmall().label("Stop").on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                    library::global().lock().unwrap().set_watched(None);
                    cx.notify();
                })))
            });

        let mut out = vec![drop_zone.into_any_element(), watch.into_any_element()];
        if let Some(e) = &self.error {
            out.push(div().text_size(px(tk::T12)).text_color(rgb(tk::error_text())).child(e.clone()).into_any_element());
        }
        if !recent.is_empty() {
            let mut row = div().flex().gap(px(6.));
            for (i, e) in recent.iter().take(5).enumerate() {
                let src = ImageSource::Library(e.id.clone());
                let path = thumb_paths.get(&e.id).cloned();
                let t = thumb(&format!("i:{}", e.id), || image::open(path?).ok().map(|i| i.to_rgb8()));
                row = row.child(tile(e.id.clone(), src.clone(), t, is_on(&src), None, false, 1000 + i, cx));
            }
            out.push(ui::field("Recent", None, row).into_any_element());
        }
        out.push(filters.into_any_element());
        if n == 0 {
            let msg = if filter == Some(Filter::Favorites) { "No favorites yet. Import an image, select it and tap ☆ Favorite." } else { "No images here yet. Import some above." };
            out.push(ui::hint(msg).into_any_element());
        } else {
            out.push(grid.into_any_element());
        }
        if let Some(ib) = current {
            out.extend(self.image_settings(&ib, &missing, cx));
        }
        out
    }

    fn import(&mut self, paths: Vec<std::path::PathBuf>, cx: &mut Context<Self>) {
        self.error = None;
        let mut last = None;
        for p in paths {
            match library::global().lock().unwrap().import(&p) {
                Ok(e) => last = Some(e.id),
                Err(e) => self.error = Some(e.to_string()),
            }
        }
        if let Some(id) = last {
            self.selected_image = Some(id.clone());
            self.set(Background::Image(ImageBg::new(ImageSource::Library(id))), cx);
        }
        cx.notify();
    }

    fn image_settings(&mut self, ib: &ImageBg, missing: &[String], cx: &mut Context<Self>) -> Vec<AnyElement> {
        let lib_id = match &ib.source {
            ImageSource::Library(id) => Some(id.clone()),
            _ => None,
        };
        let mut out = vec![div().pt(px(ui::S2)).text_size(px(tk::T13)).font_weight(FontWeight::SEMIBOLD).child("Adjust image").into_any_element()];
        if let Some((w, h)) = backgrounds::render::original_size(&ib.source) {
            out.push(ui::hint(format!("{}, {w} × {h} px", backgrounds::fit::Orientation::of(w, h).label())).into_any_element());
        }
        // Low resolution badge.
        if let Some(size) = backgrounds::render::original_size(&ib.source) {
            let out_size = if self.portrait { (1080, 1920) } else { (1920, 1080) };
            if backgrounds::fit::low_resolution(size, out_size, ib.fit == Fit::Cover, ib.zoom) {
                out.push(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().px(px(8.)).py(px(2.)).rounded_full().bg(rgb(tk::warning_bg())).text_size(px(tk::T12)).text_color(rgb(tk::warning_text())).child("Low resolution"))
                        .child(div().text_size(px(tk::T12)).text_color(rgb(tk::ink_soft())).child(format!("{} × {} px", size.0, size.1)))
                        .into_any_element(),
                );
            }
        }
        let fits = [(Fit::Cover, "Cover"), (Fit::Contain, "Contain")]
            .into_iter()
            .map(|(f, l)| {
                let on: ui::OnClick = Box::new(cx.listener(move |this, _: &ClickEvent, _, cx| this.edit_image(cx, |i| i.fit = f)));
                (l.into(), ib.fit == f, on)
            })
            .collect();
        out.push(ui::field("Fit", None, ui::segmented("fit", fits)).into_any_element());
        if ib.fit == Fit::Cover {
            // Focal point: click or drag on the image.
            let key = match &ib.source {
                ImageSource::Library(id) => format!("i:{id}"),
                ImageSource::Builtin(i) => format!("w:{i}"),
                ImageSource::File(p) => format!("f:{}", p.display()),
            };
            let t = thumbs().lock().unwrap().get(&key).cloned();
            let store = self.bounds.clone();
            let (fx, fy) = ib.focal;
            out.push(
                ui::field(
                    "Focal point",
                    Some("click what should stay visible".into()),
                    div()
                        .id("focal")
                        .relative()
                        .w_full()
                        .h(px(150.))
                        .rounded(px(10.))
                        .overflow_hidden()
                        .bg(rgb(tk::raised()))
                        .cursor_pointer()
                        .children(t.map(|t| img(t).size_full().rounded(px(10.)).object_fit(ObjectFit::Fill)))
                        .child(gpui_kit::canvas(move |b, _, _| {
                            store.borrow_mut().insert("focal", b);
                        }, |_, _, _, _| {}).absolute().size_full())
                        .child(
                            div()
                                .absolute()
                                .left(gpui_kit::relative(fx))
                                .top(gpui_kit::relative(fy))
                                .ml(px(-9.))
                                .mt(px(-9.))
                                .size(px(18.))
                                .rounded_full()
                                .border_2()
                                .border_color(rgb(tk::WHITE))
                                .bg(tk::scrim(0.3))
                                .shadow_md(),
                        )
                        .on_mouse_down(MouseButton::Left, cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                            this.drag = Some(Drag::Focal);
                            this.focal_to(ev.position, cx);
                        })),
                )
                .into_any_element(),
            );
            out.push(self.knob(Knob::Zoom, "Zoom", cx).into_any_element());
            out.push(self.knob(Knob::PanX, "Pan horizontal", cx).into_any_element());
            out.push(self.knob(Knob::PanY, "Pan vertical", cx).into_any_element());
        }
        out.push(self.knob(Knob::ImageBlur, "Blur", cx).into_any_element());
        out.push(self.knob(Knob::Darken, "Darken", cx).into_any_element());
        out.push(self.knob(Knob::Grain, "Grain", cx).into_any_element());

        if let Some(id) = lib_id {
            let fav = library::global().lock().unwrap().is_favorite(&id);
            let (id2, id3, id4) = (id.clone(), id.clone(), id.clone());
            out.push(div().pt(px(ui::S2)).text_size(px(tk::T13)).font_weight(FontWeight::SEMIBOLD).child("In your library").into_any_element());
            out.push(Input::new(&self.rename).small().into_any_element());
            out.push(Input::new(&self.tags).small().into_any_element());
            let mut row = div()
                .flex()
                .gap_2()
                .child(Button::new("fav-img").cursor_pointer().secondary().small().label(if fav { "★ Favorite" } else { "☆ Favorite" }).on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                    library::global().lock().unwrap().toggle_favorite(&id2);
                    cx.notify();
                })));
            if missing.contains(&id) {
                row = row.child(Button::new("relink").cursor_pointer().secondary().small().label("Relink").on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                    let id = id3.clone();
                    let rx = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Relink".into()) });
                    cx.spawn(async move |this, cx| {
                        if let Ok(Ok(Some(paths))) = rx.await {
                            if let Some(p) = paths.into_iter().next() {
                                let _ = this.update(cx, |v, cx| {
                                    if let Err(e) = library::global().lock().unwrap().relink(&id, &p) {
                                        v.error = Some(e.to_string());
                                    }
                                    thumbs().lock().unwrap().remove(&format!("i:{id}"));
                                    cx.notify();
                                });
                            }
                        }
                    })
                    .detach();
                })));
            }
            row = row.child(div().flex_1()).child(Button::new("del-img").cursor_pointer().ghost().small().text_color(rgb(tk::error_text())).label("Delete").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                library::global().lock().unwrap().delete(&id4);
                thumbs().lock().unwrap().remove(&format!("i:{id4}"));
                this.selected_image = None;
                // Fall back to the first built-in image.
                this.set(Background::Image(ImageBg::new(ImageSource::Builtin(0))), cx);
            })));
            out.push(row.into_any_element());
        }
        out
    }

    fn colors_tab(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let current = match &self.bg {
            Background::Solid { color, .. } => Some(*color),
            _ => None,
        };
        let grain = self.get(Knob::Grain);
        let mut grid = div().flex().flex_wrap().gap(px(6.)).items_center();
        for (i, c) in SOLIDS.iter().copied().enumerate() {
            let on = current == Some(c);
            grid = grid.child(
                div()
                    .id(("solid", i))
                    .size(px(34.))
                    .p(px(3.))
                    .rounded(px(10.))
                    .border_2()
                    .border_color(if on { gpui_kit::Hsla::from(rgb(tk::text())) } else { gpui_kit::transparent_black() })
                    .cursor_pointer()
                    .child(div().size_full().rounded(px(7.)).bg(rgb(hex(c))).border_1().border_color(ui::hairline()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set(Background::Solid { color: c, grain }, cx))),
            );
        }
        let mut out = vec![grid.into_any_element()];
        if current.is_some() {
            out.push(ui::field("Custom color", None, self.color_knobs(cx)).into_any_element());
            out.push(self.knob(Knob::Grain, "Grain", cx).into_any_element());
        }
        out
    }
}

fn hex(c: Rgb) -> u32 {
    ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | c[2] as u32
}

impl Render for BgPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Keep the rename/tag fields on the selected library image.
        let lib_id = match &self.bg {
            Background::Image(ImageBg { source: ImageSource::Library(id), .. }) => Some(id.clone()),
            _ => None,
        };
        if lib_id != self.fields_for {
            self.fields_for = lib_id.clone();
            if let Some(id) = &lib_id {
                if let Some(e) = library::global().lock().unwrap().get(id).cloned() {
                    self.rename.update(cx, |s, cx| s.set_value(e.name.clone(), window, cx));
                    self.tags.update(cx, |s, cx| s.set_value(e.tags.join(", "), window, cx));
                }
            }
        }
        let tabs = ui::segmented(
            "bg-tabs",
            Tab::ALL
                .into_iter()
                .map(|t| {
                    let on: ui::OnClick = Box::new(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.tab = t;
                        cx.notify();
                    }));
                    (t.label().into(), self.tab == t, on)
                })
                .collect(),
        );
        let body = match self.tab {
            Tab::Gradient => self.gradients_tab(cx),
            Tab::Image => self.images_tab(window, cx),
            Tab::Color => self.colors_tab(cx),
        };
        // While dragging a slider, stop or focal point, follow the pointer anywhere.
        let dragging = self.drag.is_some();
        let me = cx.entity().downgrade();
        let tracker = gpui_kit::canvas(|_, _, _| {}, move |_, _, window, _| {
            if !dragging {
                return;
            }
            let (m1, m2) = (me.clone(), me.clone());
            window.on_mouse_event(move |ev: &MouseMoveEvent, phase, _, cx| {
                if phase == DispatchPhase::Bubble {
                    let _ = m1.update(cx, |v, cx| v.drag_to(ev, cx));
                }
            });
            window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                if phase == DispatchPhase::Bubble {
                    let _ = m2.update(cx, |v, cx| {
                        v.drag = None;
                        cx.notify();
                    });
                }
            });
        })
        .absolute()
        .size_0();
        div()
            .relative()
            .flex()
            .flex_col()
            .gap(px(ui::S4))
            .child(tabs)
            .children(body)
            .child(
                div().child(
                    Button::new("credits").cursor_pointer().ghost().xsmall().rounded_full().text_color(rgb(tk::ink_soft())).label("Credits and licenses").on_click(|_: &ClickEvent, _, cx| crate::credits::open(cx)),
                ),
            )
            .child(tracker)
    }
}

/// Opens the picker as an entity (convenience for owners).
pub fn new_picker<T: 'static>(bg: Background, portrait: bool, moment: Option<f32>, window: &mut Window, cx: &mut Context<T>) -> Entity<BgPicker> {
    cx.new(|cx| BgPicker::new(bg, portrait, moment, window, cx))
}
