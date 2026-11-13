//! The editor window: tools on the left, the canvas in the middle, properties on the right,
//! and Undo/Redo/Zoom/Copy/Save/Export on top.
//!
//! The canvas shows the renderer's output (`render.rs`) at screen resolution, computed on a
//! background thread; while dragging it renders a half-resolution proxy and re-renders at
//! full quality when idle. Selection handles, guides and the crop frame are drawn on top.

use super::canvas::{self, Handle, Map, PointHandle};
use super::doc::*;
use super::export::{self, Format};
use super::render::{self, Cache};
use super::{icons, spline, stickers, text};
use crate::ui;
use gpui_kit::AnimationExt;
use crate::{BG, GREEN, MUTED, ON_GREEN, RAISED, TEXT};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::color_picker::{ColorPickerEvent, ColorPickerState};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::{Disableable, Icon, Selectable, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyWindowHandle, App, Bounds, ClickEvent, Entity, FocusHandle, FontWeight, Global, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ObjectFit, PinchEvent, Pixels, RenderImage, ScrollDelta, ScrollEvent,
    Subscription, TitlebarOptions, Window, WindowBounds, WindowOptions, canvas, div, hsla, img, point, px, rgb, size,
};
use image::RgbaImage;
use std::cell::Cell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

// ---------- tools ----------

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Tool {
    Select,
    Arrow,
    Callout,
    Line,
    Rect,
    RoundRect,
    Ellipse,
    Text,
    Highlight,
    Step,
    Pen,
    Emoji,
    Sticker,
    Redact,
    Crop,
    Background,
    Merge,
}

impl Tool {
    pub const ALL: [Tool; 17] = [
        Tool::Select,
        Tool::Arrow,
        Tool::Callout,
        Tool::Line,
        Tool::Rect,
        Tool::RoundRect,
        Tool::Ellipse,
        Tool::Text,
        Tool::Highlight,
        Tool::Step,
        Tool::Pen,
        Tool::Emoji,
        Tool::Sticker,
        Tool::Redact,
        Tool::Crop,
        Tool::Background,
        Tool::Merge,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Tool::Select => "v",
            Tool::Arrow => "a",
            Tool::Callout => "o",
            Tool::Merge => "m",
            Tool::Line => "l",
            Tool::Rect => "r",
            Tool::RoundRect => "u",
            Tool::Ellipse => "e",
            Tool::Text => "t",
            Tool::Highlight => "h",
            Tool::Step => "n",
            Tool::Pen => "p",
            Tool::Emoji => "j",
            Tool::Sticker => "k",
            Tool::Redact => "b",
            Tool::Crop => "c",
            Tool::Background => "g",
        }
    }

    /// The name under the toolbar icon.
    pub fn short(self) -> &'static str {
        match self {
            Tool::Select => "Select",
            Tool::Arrow => "Arrow",
            Tool::Callout => "Callout",
            Tool::Merge => "Merge",
            Tool::Line => "Line",
            Tool::Rect => "Rectangle",
            Tool::RoundRect => "Rounded",
            Tool::Ellipse => "Ellipse",
            Tool::Text => "Text",
            Tool::Highlight => "Highlight",
            Tool::Step => "Steps",
            Tool::Pen => "Pen",
            Tool::Emoji => "Emoji",
            Tool::Sticker => "Stickers",
            Tool::Redact => "Redact",
            Tool::Crop => "Crop",
            Tool::Background => "Frame",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Tool::Select => "Select and move",
            Tool::Arrow => "Arrow",
            Tool::Callout => "Callout zoom",
            Tool::Merge => "Merge images",
            Tool::Line => "Line",
            Tool::Rect => "Rectangle",
            Tool::RoundRect => "Rounded rectangle",
            Tool::Ellipse => "Ellipse",
            Tool::Text => "Text",
            Tool::Highlight => "Highlighter",
            Tool::Step => "Numbered steps",
            Tool::Pen => "Pen",
            Tool::Emoji => "Emoji",
            Tool::Sticker => "Stickers",
            Tool::Redact => "Hide sensitive info",
            Tool::Crop => "Crop, rotate and flip",
            Tool::Background => "Background and frame",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Tool::Select => icons::select(),
            Tool::Arrow => icons::arrow(),
            Tool::Callout => icons::callout(),
            Tool::Merge => icons::merge(),
            Tool::Line => icons::line(),
            Tool::Rect => icons::rect(),
            Tool::RoundRect => icons::round_rect(),
            Tool::Ellipse => icons::ellipse(),
            Tool::Text => icons::text(),
            Tool::Highlight => icons::highlighter(),
            Tool::Step => icons::step(),
            Tool::Pen => icons::pen(),
            Tool::Emoji => icons::emoji(),
            Tool::Sticker => icons::sticker(),
            Tool::Redact => icons::redact(),
            Tool::Crop => icons::crop(),
            Tool::Background => icons::background(),
        }
    }

    /// Tools that draw by dragging on the canvas.
    fn creates(self) -> bool {
        !matches!(self, Tool::Select | Tool::Crop | Tool::Background | Tool::Merge)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RedactMode {
    Blur,
    Pixelate,
    Solid,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RedactShape {
    Rect,
    Round,
    Ellipse,
    Brush,
}

/// How new callouts (and their source boxes) look.
#[derive(Clone, Debug)]
pub struct CalloutDefaults {
    pub shape: CalloutShape,
    pub scale: f32,
    pub border: Rgba,
    pub width: f32,
    pub radius: f32,
    pub shadow: Option<Shadow>,
    pub src_color: Rgba,
    pub src_width: f32,
    pub src_radius: f32,
    pub src_fill: bool,
    pub src_dashed: bool,
    pub show_source: bool,
    pub connector: bool,
    pub cut: CutMode,
}

impl CalloutDefaults {
    fn for_image(u: f32) -> Self {
        let red = [0xff, 0x3b, 0x30, 255];
        Self {
            shape: CalloutShape::Rect,
            scale: 2.0,
            border: red,
            width: (4.0 * u).round(),
            radius: (14.0 * u).round(),
            shadow: Some(Shadow { blur: (36.0 * u).round(), opacity: 0.35, offset: (14.0 * u).round() }),
            src_color: red,
            src_width: (4.0 * u).round(),
            src_radius: (8.0 * u).round(),
            src_fill: false,
            src_dashed: false,
            show_source: true,
            connector: false,
            cut: CutMode::Copy,
        }
    }
}

/// Settings new objects are made with (and that the panel edits when nothing is selected).
#[derive(Clone, Debug)]
pub struct Defaults {
    pub color: Rgba,
    pub fill: Option<Rgba>,
    pub width: f32,
    pub radius: f32,
    pub head: Head,
    pub double: bool,
    pub arrow_style: ArrowStyle,
    pub roughness: f32,
    pub dashed: bool,
    pub arrow_shadow: bool,
    pub head_size: f32,
    pub callout: CalloutDefaults,
    pub font: String,
    pub size: f32,
    pub weight: u16,
    pub align: Align,
    pub pill: Option<Rgba>,
    pub outline: Option<Rgba>,
    pub shadow: bool,
    pub hl_color: Rgba,
    pub hl_free: bool,
    pub hl_width: f32,
    pub redact: RedactMode,
    pub redact_shape: RedactShape,
    pub strength: f32,
    pub block: f32,
    pub solid: Rgba,
    pub brush: f32,
    pub step: f32,
    pub emoji: String,
    pub sticker: StickerSrc,
}

impl Defaults {
    fn for_image(long: u32) -> Self {
        let u = (long as f32 / 1200.0).max(1.0);
        Self {
            color: [0xff, 0x3b, 0x30, 255],
            fill: None,
            width: (4.0 * u).round(),
            radius: (12.0 * u).round(),
            head: Head::Filled,
            double: false,
            arrow_style: ArrowStyle::Curved,
            roughness: 0.0,
            dashed: false,
            arrow_shadow: false,
            head_size: 0.0,
            callout: CalloutDefaults::for_image(u),
            font: String::new(),
            size: (28.0 * u).round(),
            weight: 700,
            align: Align::Left,
            pill: None,
            outline: None,
            shadow: false,
            hl_color: [0xff, 0xe6, 0x00, 255],
            hl_free: false,
            hl_width: (22.0 * u).round(),
            redact: RedactMode::Pixelate,
            redact_shape: RedactShape::Rect,
            strength: render::min_blur(long),
            block: render::min_block(long),
            solid: [0, 0, 0, 255],
            brush: (36.0 * u).round(),
            step: (18.0 * u).round(),
            emoji: "😀".into(),
            sticker: StickerSrc::Builtin(stickers::BUILTIN[0].name.into()),
        }
    }
}

/// Numeric settings with a slider.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Prop {
    Width,
    Radius,
    FontSize,
    HlWidth,
    Strength,
    Block,
    Brush,
    StepSize,
    Padding,
    Corner,
    ShadowBlur,
    ShadowOpacity,
    ShadowOffset,
    Quality,
    HeadSize,
    Roughness,
    Gap,
    BoardPad,
    ExpandL,
    ExpandT,
    ExpandR,
    ExpandB,
    ImgRadius,
    TrimL,
    TrimT,
    TrimR,
    TrimB,
    CalloutScale,
    CalloutWidth,
    CalloutRadius,
    CShadowBlur,
    CShadowOpacity,
    CShadowOffset,
    SourceWidth,
    SourceRadius,
}


/// Color settings with swatches and a picker.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Slot {
    Stroke,
    Fill,
    Pill,
    Outline,
    Highlight,
    Solid,
    BgSolid,
    Separator,
    CalloutBorder,
    SourceColor,
}

impl Slot {
    pub const ALL: [Slot; 10] = [Slot::Stroke, Slot::Fill, Slot::Pill, Slot::Outline, Slot::Highlight, Slot::Solid, Slot::BgSolid, Slot::Separator, Slot::CalloutBorder, Slot::SourceColor];
}

/// The quick layouts in the Merge panel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Preset {
    Compare,
    SideBySide,
    SingleCallout,
}

// ---------- interaction ----------

#[derive(Clone, Debug)]
enum Drag {
    /// Drawing a new object (already in the document, updated live).
    Create { id: u64, start: P, points: Vec<P> },
    Move { start: P, orig: Vec<Object>, targets: (Vec<f32>, Vec<f32>) },
    Resize { orig: Object, ex: i8, ey: i8 },
    Rotate { orig: Object, start_angle: f32 },
    Point { orig: Object, which: PointHandle },
    /// Moving arrow anchor `i` (an end can glue onto an object).
    Anchor { orig: Object, i: usize },
    /// Moving a tangent handle of anchor `i`.
    Tangent { orig: Object, i: usize, out: bool },
    /// Bending segment `seg` of an arrow from the point at `t`, grabbed at `start`.
    Bend { orig: Object, seg: usize, t: f32, start: P },
    Pan { start: (f32, f32), orig: (f32, f32) },
    /// `ex`/`ey` 0 and `draw` false: moving the crop.
    Crop { orig: R, start: P, ex: i8, ey: i8, draw: bool },
}

struct Toast {
    text: String,
    at: Instant,
    reveal: Option<PathBuf>,
}

const HANDLE: f32 = 9.0;
/// Room the picture may be panned past its edge, in points (the same as around it at Fit).
const PAN_MARGIN: f32 = 24.0;

/// Keeps the picture in view: centered along an axis where it fits, and panned no further
/// than its edge plus a margin where it doesn't. (The canvas used to pan away for ever.)
fn clamp_pan(pan: (f32, f32), k: f32, canvas: (u32, u32), view: (f32, f32)) -> (f32, f32) {
    let axis = |p: f32, size: u32, v: f32| {
        let slack = ((size as f32 * k - v) / 2.0 + PAN_MARGIN).max(0.0);
        p.clamp(-slack, slack)
    };
    (axis(pan.0, canvas.0, view.0), axis(pan.1, canvas.1, view.1))
}

/// A picture resized from `orig` with handle (`ex`, `ey`), keeping its aspect `a`: sized by
/// whichever edge the pointer pulled furthest (`nb` is the free-form rectangle), with the
/// corner or edge opposite the handle held in place. It may reach past the canvas.
fn resize_picture(nb: R, orig: R, ex: i8, ey: i8, a: f32) -> R {
    let (kw, kh) = (nb.w / orig.w.max(1e-3), nb.h / orig.h.max(1e-3));
    let k = match (ex, ey) {
        (0, _) => kh,
        (_, 0) => kw,
        _ => if (kw - 1.0).abs() >= (kh - 1.0).abs() { kw } else { kh },
    };
    let w = (orig.w * k).max(8.0);
    let h = w / a;
    let x = match ex {
        -1 => orig.right() - w,
        1 => orig.x,
        _ => orig.center().x - w / 2.0,
    };
    let y = match ey {
        -1 => orig.bottom() - h,
        1 => orig.y,
        _ => orig.center().y - h / 2.0,
    };
    r(x, y, w, h)
}

/// The resize cursor for a handle on edge `ex`, `ey` (-1 left/top, 1 right/bottom).
fn resize_cursor(ex: i8, ey: i8) -> gpui_kit::CursorStyle {
    use gpui_kit::CursorStyle as C;
    match (ex, ey) {
        (0, _) => C::ResizeUpDown,
        (_, 0) => C::ResizeLeftRight,
        (a, b) if a == b => C::ResizeUpLeftDownRight,
        _ => C::ResizeUpRightDownLeft,
    }
}
/// Selection and handles: the system's selection blue (distinct from the green accent, and
/// visible on screenshots full of green).
const SELECT: u32 = 0x0a84ff;
const PALETTE: [Rgba; 10] = [
    [0xff, 0x3b, 0x30, 255],
    [0xff, 0x9f, 0x0a, 255],
    [0xff, 0xd6, 0x0a, 255],
    [0x30, 0xd1, 0x58, 255],
    [0xa4, 0xf4, 0x7c, 255],
    [0x0a, 0x84, 0xff, 255],
    [0xbf, 0x5a, 0xf2, 255],
    [0xff, 0xff, 0xff, 255],
    [0x8e, 0x8e, 0x93, 255],
    [0x00, 0x00, 0x00, 255],
];

pub struct EditorView {
    pub(super) path: PathBuf,
    base: Arc<RgbaImage>,
    cache: Arc<Cache>,
    pub(super) doc: Document,
    history: History,
    pub(super) tool: Tool,
    pub(super) sel: Option<u64>,
    pub(super) defaults: Defaults,
    drag: Option<Drag>,
    editing: Option<u64>,
    /// The selected arrow's selected anchor (Delete removes it).
    sel_anchor: Option<usize>,
    /// While dragging an arrow end: the object it would attach to and where.
    glue_hint: Option<(u64, P)>,
    guides: (Vec<f32>, Vec<f32>),
    /// None = fit.
    zoom: Option<f32>,
    pan: (f32, f32),
    /// The handle under the pointer (no button down), for its resize cursor.
    hover: Option<Handle>,
    space: bool,
    view: Rc<Cell<Bounds<Pixels>>>,
    image: Option<Arc<RenderImage>>,
    render_state: RenderState,
    /// Render scale of the shown image.
    shown_scale: f32,
    pub(super) export: export::Settings,
    pub(super) export_open: bool,
    toast: Option<Toast>,
    autosave_due: Option<Instant>,
    focus: FocusHandle,
    /// Slider bounds from the last paint, and the slider being dragged.
    slider_bounds: Rc<std::cell::RefCell<HashMap<Prop, Bounds<Pixels>>>>,
    slider_drag: Option<Prop>,
    pub(super) pickers: HashMap<Slot, Entity<ColorPickerState>>,
    pub(super) emoji_query: Entity<InputState>,
    pub(super) emoji_group: usize,
    pub(super) sticker_tab_mine: bool,
    pub(super) my_stickers: Vec<PathBuf>,
    pub(super) thumbs: HashMap<StickerSrc, Arc<RenderImage>>,
    pub(super) custom_w: Entity<InputState>,
    pub(super) custom_h: Entity<InputState>,
    custom_shown: Option<(u32, u32)>,
    pub(super) crop_aspect: Option<f32>,
    pub(super) font_dropdown: Entity<crate::dropdown::Dropdown>,
    fonts: Vec<String>,
    font_shown: Option<String>,
    pub(super) bg_picker: Entity<crate::bg_picker::BgPicker>,
    _subs: Vec<Subscription>,
}

pub fn palette() -> Vec<Rgba> {
    PALETTE.to_vec()
}

/// Background re-rendering of the canvas: one render at a time. Changes while one runs
/// queue exactly one more; a quick low-resolution render made during a drag is redone at
/// full resolution once the drag ends.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) enum RenderState {
    #[default]
    Idle,
    Running {
        /// Rendered at draft resolution (during a drag).
        draft: bool,
        /// Something changed since it started.
        again: bool,
    },
}

impl RenderState {
    /// Something changed. True when a render should start now (none is running).
    pub(super) fn request(&mut self) -> bool {
        match self {
            RenderState::Idle => true,
            RenderState::Running { again, .. } => {
                *again = true;
                false
            }
        }
    }

    pub(super) fn started(&mut self, draft: bool) {
        *self = RenderState::Running { draft, again: false };
    }

    /// The render finished. True when another should start right away: changes came in
    /// meanwhile, or it was a draft and the drag is over.
    pub(super) fn finished(&mut self, dragging: bool) -> bool {
        let more = match *self {
            RenderState::Running { again, draft } => again || (draft && !dragging),
            RenderState::Idle => false,
        };
        *self = RenderState::Idle;
        more
    }

    pub(super) fn busy(&self) -> bool {
        matches!(self, RenderState::Running { .. })
    }
}

struct Open(HashMap<PathBuf, AnyWindowHandle>);
impl Global for Open {}

/// Merges 2–6 pictures into a new board document (`Merge_<time>` in `folder`, with copies
/// of the pictures next to it) and opens it.
pub fn open_merge(paths: &[PathBuf], folder: &std::path::Path, cx: &mut App) -> anyhow::Result<PathBuf> {
    let stamp = crate::recorder::stamp();
    std::fs::create_dir_all(folder)?;
    let path = folder.join(format!("Merge_{stamp}.png"));
    let assets = folder.join(format!("Merge_{stamp}.assets"));
    std::fs::create_dir_all(&assets)?;
    let mut images = Vec::new();
    for (i, src) in paths.iter().take(super::merge::MAX_IMAGES).enumerate() {
        let img = image::open(src).map_err(|e| anyhow::anyhow!("Could not open {}: {e}", src.display()))?;
        let dest = assets.join(format!("{}.png", i + 1));
        img.to_rgba8().save(&dest)?;
        images.push((dest, (img.width(), img.height())));
    }
    let doc = Document::new_board(&images, super::merge::Board::default());
    write_board_preview(&doc, &path)?;
    std::fs::write(Document::sidecar_path(&path), doc.to_json())?;
    open(path.clone(), cx);
    Ok(path)
}

/// A board's picture file is its current render (small): what the recent list and the
/// screenshot cards show. The real originals are its image layers.
fn write_board_preview(doc: &Document, path: &std::path::Path) -> anyhow::Result<()> {
    let (w, h) = doc.layout().canvas;
    let k = (1600.0 / w.max(h).max(1) as f32).min(1.0);
    let img = render::render(doc, &RgbaImage::new(1, 1), k, &Cache::default());
    img.save(path)?;
    Ok(())
}

/// Opens the editor for a screenshot (or brings its window forward).
pub fn open(path: PathBuf, cx: &mut App) {
    if let Some(h) = cx.try_global::<Open>().and_then(|o| o.0.get(&path).copied()) {
        if h.update(cx, |_, window, _| window.activate_window()).is_ok() {
            cx.activate(true);
            return;
        }
    }
    let base = match image::open(&path) {
        Ok(img) => Arc::new(img.to_rgba8()),
        Err(e) => {
            eprintln!("Could not open {}: {e}", path.display());
            return;
        }
    };
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("Screenshot").to_string();
    let bounds = Bounds::centered(None, size(px(1360.), px(860.)), cx);
    let p2 = path.clone();
    let opened = gpui_kit::open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some(format!("Edit — {name}").into()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(18.), px(19.))),
            }),
            is_resizable: true,
            window_min_size: Some(size(px(980.), px(620.))),
            ..Default::default()
        },
        cx,
        move |window, cx| cx.new(|cx| EditorView::new(p2, base, window, cx)),
    );
    if let Ok((handle, view)) = opened {
        if !cx.has_global::<Open>() {
            cx.set_global(Open(HashMap::new()));
        }
        cx.global_mut::<Open>().0.insert(path.clone(), handle);
        let _ = handle.update(cx, |_, window, cx| {
            let view = view.clone();
            window.on_window_should_close(cx, move |_, cx| {
                view.update(cx, |v, _| {
                    v.save_project();
                    if v.doc.board.is_some() {
                        let _ = write_board_preview(&v.doc, &v.path);
                    }
                });
                let path = view.read(cx).path.clone();
                if cx.has_global::<Open>() {
                    cx.global_mut::<Open>().0.remove(&path);
                }
                true
            });
        });
        // Opened from a screenshot card or the menu bar, Clipper isn't the active app: take
        // focus so the editor lands in front of whatever is open, not behind it.
        cx.activate(true);
        let _ = handle.update(cx, |_, window, _| window.activate_window());
    }
}

impl EditorView {
    fn new(path: PathBuf, base: Arc<RgbaImage>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let size = base.dimensions();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        // A merged board's size is its canvas, not its placeholder picture's.
        let mut doc = super::load_doc(&path).filter(|d| d.size == size || d.board.is_some()).unwrap_or_else(|| Document::new(&name, size));
        doc.sync_links();
        // Text boxes follow the fonts on this machine.
        for o in &mut doc.objects {
            if let Kind::Text(t) = &mut o.kind {
                let (w, h) = text::measure(t);
                t.rect.w = w;
                t.rect.h = h;
            }
        }
        let long = size.0.max(size.1);
        let defaults = Defaults::for_image(long);
        let mut subs = Vec::new();

        let mut pickers = HashMap::new();
        for slot in Slot::ALL {
            let state = cx.new(|cx| ColorPickerState::new(window, cx));
            subs.push(cx.subscribe_in(&state, window, move |this: &mut Self, _, ev: &ColorPickerEvent, _, cx| {
                let ColorPickerEvent::Change(Some(c)) = ev else { return };
                let rgba = gpui_kit::Rgba::from(*c);
                let to8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                this.set_color(slot, Some([to8(rgba.r), to8(rgba.g), to8(rgba.b), to8(rgba.a)]), cx);
            }));
            pickers.insert(slot, state);
        }
        let emoji_query = cx.new(|cx| InputState::new(window, cx).placeholder("Search emoji"));
        subs.push(cx.subscribe_in(&emoji_query, window, |_: &mut Self, _, ev: &InputEvent, _, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        }));
        let custom_w = cx.new(|cx| InputState::new(window, cx).placeholder("Width"));
        let custom_h = cx.new(|cx| InputState::new(window, cx).placeholder("Height"));
        for input in [&custom_w, &custom_h] {
            subs.push(cx.subscribe_in(input, window, |this: &mut Self, _, ev: &InputEvent, _, cx| {
                if matches!(ev, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    this.apply_custom_size(cx);
                }
            }));
        }

        let fonts = text::families();
        let font_dropdown = cx.new(|_| crate::dropdown::Dropdown::new("font", "Font"));
        subs.push(cx.subscribe(&font_dropdown, |this: &mut Self, _, ev: &crate::dropdown::DropdownEvent, cx| {
            let crate::dropdown::DropdownEvent::Select(i) = ev;
            let font = if *i == 0 { String::new() } else { this.fonts.get(*i).cloned().unwrap_or_default() };
            this.defaults.font = font.clone();
            this.edit_selected(cx, |o| {
                if let Kind::Text(t) = &mut o.kind {
                    t.font = font
                }
            });
            cx.notify();
        }));
        let portrait = doc.size.1 > doc.size.0;
        let bg_picker = crate::bg_picker::new_picker(doc.frame.background.clone(), portrait, Some(doc.frame.moment), window, cx);
        subs.push(cx.subscribe(&bg_picker, |this: &mut Self, _, ev: &crate::bg_picker::PickerEvent, cx| match ev {
            crate::bg_picker::PickerEvent::Changed(bg) => {
                let bg = bg.clone();
                this.doc_action(|d| d.frame.background = bg, cx);
            }
            crate::bg_picker::PickerEvent::Moment(m) => {
                let m = *m;
                this.doc_action(|d| d.frame.moment = m, cx);
            }
        }));

        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let mut v = Self {
            path,
            base,
            cache: Arc::new(Cache::default()),
            doc,
            history: History::default(),
            tool: Tool::Select,
            sel: None,
            defaults,
            drag: None,
            editing: None,
            sel_anchor: None,
            glue_hint: None,
            guides: (Vec::new(), Vec::new()),
            zoom: None,
            pan: (0.0, 0.0),
            hover: None,
            space: false,
            view: Rc::new(Cell::new(Bounds::default())),
            image: None,
            render_state: RenderState::Idle,
            shown_scale: 0.0,
            export: export::Settings::default(),
            export_open: false,
            toast: None,
            autosave_due: None,
            focus,
            slider_bounds: Rc::new(std::cell::RefCell::new(HashMap::new())),
            slider_drag: None,
            pickers,
            emoji_query,
            emoji_group: 0,
            sticker_tab_mine: false,
            my_stickers: stickers::my_stickers(),
            thumbs: HashMap::new(),
            custom_w,
            custom_h,
            custom_shown: None,
            crop_aspect: None,
            font_dropdown,
            fonts,
            font_shown: None,
            bg_picker,
            _subs: subs,
        };
        v.refresh_thumbs();
        // Dev: open on a tool / with an object selected, for screenshots.
        #[cfg(debug_assertions)]
        {
            if let Ok(t) = std::env::var("CLIPPER_EDIT_TOOL") {
                if let Some(tool) = Tool::ALL.into_iter().find(|x| x.key() == t) {
                    v.tool = tool;
                }
            }
            v.sel = std::env::var("CLIPPER_EDIT_SEL").ok().and_then(|s| s.parse().ok());
            // CLIPPER_PRESET=compare|side|single: apply a merge preset on open.
            let preset = match std::env::var("CLIPPER_PRESET").as_deref() {
                Ok("compare") => Some(Preset::Compare),
                Ok("side") => Some(Preset::SideBySide),
                Ok("single") => Some(Preset::SingleCallout),
                _ => None,
            };
            if let Some(p) = preset {
                cx.defer_in(window, move |v, _, cx| v.apply_preset(p, cx));
            }
        }
        // Timers: autosave, toast expiry, re-render at full quality after a drag.
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(250)).await;
                let ok = this.update(cx, |v, cx| v.tick(cx)).is_ok();
                if !ok {
                    break;
                }
            }
        })
        .detach();
        v
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        if self.autosave_due.is_some_and(|t| Instant::now() >= t) {
            self.save_project();
        }
        if self.toast.as_ref().is_some_and(|t| t.at.elapsed() > Duration::from_secs(4)) {
            self.toast = None;
            cx.notify();
        }
        // The view may have resized since the last render.
        let want = self.render_scale(None);
        if self.image.is_some() && !self.render_state.busy() && (want - self.shown_scale).abs() > 0.02 && self.drag.is_none() {
            self.request_render(cx);
        }
    }

    // ---------- document changes ----------

    /// The document changed: re-render and schedule an autosave.
    fn changed(&mut self, cx: &mut Context<Self>) {
        self.doc.sync_links();
        if self.sel_anchor.is_some() && !matches!(self.selected().map(|o| &o.kind), Some(Kind::Arrow(_))) {
            self.sel_anchor = None;
        }
        // Undo/redo can change the background: keep the picker showing the document's.
        let (bg, moment) = (self.doc.frame.background.clone(), self.doc.frame.moment);
        self.bg_picker.update(cx, |p, cx| {
            p.set_background(bg, cx);
            p.set_moment(moment, cx);
        });
        self.autosave_due = Some(Instant::now() + Duration::from_millis(800));
        self.request_render(cx);
        cx.notify();
    }

    /// Writes the project file next to the original (only once there are edits).
    pub fn save_project(&mut self) {
        self.autosave_due = None;
        if !self.history.can_undo() && !Document::sidecar_path(&self.path).exists() {
            return;
        }
        let _ = std::fs::write(Document::sidecar_path(&self.path), self.doc.to_json());
    }

    fn checkpoint(&mut self) {
        self.history.checkpoint(&self.doc);
    }

    pub(super) fn undo(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.finish_editing();
        if self.history.undo(&mut self.doc) {
            self.fix_selection();
            self.changed(cx);
        }
    }

    pub(super) fn redo(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.finish_editing();
        if self.history.redo(&mut self.doc) {
            self.fix_selection();
            self.changed(cx);
        }
    }

    fn fix_selection(&mut self) {
        if self.sel.is_some_and(|id| self.doc.get(id).is_none()) {
            self.sel = None;
        }
    }

    pub(super) fn set_tool(&mut self, tool: Tool, _window: &mut Window, cx: &mut Context<Self>) {
        self.finish_editing();
        if self.tool == Tool::Crop || tool == Tool::Crop {
            self.zoom = None;
            self.pan = (0.0, 0.0);
        }
        self.tool = tool;
        if tool.creates() || tool == Tool::Crop {
            self.sel = None;
        }
        self.export_open = false;
        self.request_render(cx);
        cx.notify();
    }

    pub(super) fn select(&mut self, id: Option<u64>, _window: &mut Window, cx: &mut Context<Self>) {
        if self.sel != id {
            self.finish_editing();
            self.sel = id;
            cx.notify();
        }
    }

    /// Applies `f` to the selected object (with an undo step).
    pub(super) fn edit_selected(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Object)) {
        let Some(id) = self.sel else { return };
        self.checkpoint();
        if let Some(o) = self.doc.get_mut(id) {
            f(o);
            if let Kind::Text(t) = &mut o.kind {
                let (w, h) = text::measure(t);
                t.rect.w = w;
                t.rect.h = h;
            }
        }
        self.history.discard_if_unchanged(&self.doc);
        self.changed(cx);
    }

    pub(super) fn selected(&self) -> Option<&Object> {
        self.sel.and_then(|id| self.doc.get(id))
    }

    pub(super) fn object_action(&mut self, action: &str, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.sel else { return };
        self.finish_editing();
        self.checkpoint();
        match action {
            "delete" => {
                self.doc.remove(&[id]);
                self.sel = None;
                self.sel_anchor = None;
                self.doc.arrange(None);
            }
            "duplicate" => {
                let off = 20.0 * (self.doc.oriented_size().0.max(self.doc.oriented_size().1) as f32 / 1200.0).max(1.0);
                self.sel = self.doc.duplicate(&[id], off).first().copied();
            }
            "front" => self.doc.reorder(id, Order::Front),
            "back" => self.doc.reorder(id, Order::Back),
            "forward" => self.doc.reorder(id, Order::Forward),
            "backward" => self.doc.reorder(id, Order::Backward),
            "lock" => {
                let locked = self.doc.get(id).is_some_and(|o| o.locked);
                self.doc.set_locked(id, !locked);
            }
            _ => {}
        }
        self.history.discard_if_unchanged(&self.doc);
        self.changed(cx);
    }

    pub(super) fn doc_action(&mut self, f: impl FnOnce(&mut Document), cx: &mut Context<Self>) {
        self.checkpoint();
        f(&mut self.doc);
        self.history.discard_if_unchanged(&self.doc);
        self.changed(cx);
    }

    pub(super) fn apply_custom_size(&mut self, cx: &mut Context<Self>) {
        let w = self.custom_w.read(cx).value().trim().parse::<u32>().ok();
        let h = self.custom_h.read(cx).value().trim().parse::<u32>().ok();
        // On a board the fields size its canvas (Layout); otherwise the frame's output size.
        if self.doc.board.is_some() {
            if let (Some(w), Some(h)) = (w, h) {
                if self.doc.size != (w, h) {
                    self.doc_action(|d| d.set_board_canvas(w as f32, h as f32), cx);
                }
            }
            return;
        }
        if let (Some(w), Some(h)) = (w, h) {
            let size = OutputSize::Custom(w.clamp(16, 16000), h.clamp(16, 16000));
            if self.doc.frame.size != size {
                self.doc_action(|d| d.frame.size = size, cx);
            }
        }
    }

    // ---------- properties ----------

    fn long_side(&self) -> u32 {
        let (w, h) = self.doc.oriented_size();
        w.max(h)
    }

    pub(super) fn prop_get(&self, prop: Prop) -> f32 {
        let d = &self.defaults;
        let f = &self.doc.frame;
        let sel = self.selected().map(|o| &o.kind);
        match prop {
            Prop::Width => match sel {
                Some(Kind::Arrow(a)) => a.width,
                Some(Kind::Line(l)) => l.width,
                Some(Kind::Shape(s)) => s.width,
                Some(Kind::Pen(pn)) => pn.width,
                _ => d.width,
            },
            Prop::Radius => match sel {
                Some(Kind::Shape(s)) => s.radius,
                Some(Kind::Redact(Redact { area: Area::RoundRect(_, r), .. })) => *r,
                _ => d.radius,
            },
            Prop::FontSize => match sel {
                Some(Kind::Text(t)) => t.size,
                _ => d.size,
            },
            Prop::HlWidth => match sel {
                Some(Kind::Highlight(Highlight { mark: Mark::Free { width, .. }, .. })) => *width,
                _ => d.hl_width,
            },
            // What's applied: never below the minimum for this image.
            Prop::Strength => match sel {
                Some(Kind::Redact(Redact { effect: Effect::Blur(s), .. })) => *s,
                _ => d.strength,
            }
            .max(render::min_blur(self.long_side())),
            Prop::Block => match sel {
                Some(Kind::Redact(Redact { effect: Effect::Pixelate(s), .. })) => *s,
                _ => d.block,
            }
            .max(render::min_block(self.long_side())),
            Prop::Brush => match sel {
                Some(Kind::Redact(Redact { area: Area::Brush { width, .. }, .. })) => *width,
                _ => d.brush,
            },
            Prop::StepSize => match sel {
                Some(Kind::Step(s)) => s.radius,
                _ => d.step,
            },
            Prop::Padding => f.padding as f32,
            Prop::Corner => f.radius as f32,
            Prop::ShadowBlur => f.shadow_blur as f32,
            Prop::ShadowOpacity => f.shadow_opacity * 100.0,
            Prop::ShadowOffset => f.shadow_offset as f32,
            Prop::Quality => self.export.quality as f32,
            _ => self.new_prop_get(prop),
        }
    }

    /// The selected callout and its source box (from either one), when one is selected.
    pub(super) fn callout_pair(&self) -> Option<(u64, u64)> {
        match self.selected().map(|o| (o.id, &o.kind)) {
            Some((id, Kind::Callout(c))) => Some((c.source, id)),
            Some((id, Kind::Source(sb))) => Some((id, sb.callout)),
            _ => None,
        }
    }

    pub(super) fn selected_callout(&self) -> Option<&Callout> {
        let (_, c) = self.callout_pair()?;
        match self.doc.get(c).map(|o| &o.kind) {
            Some(Kind::Callout(c)) => Some(c),
            _ => None,
        }
    }

    pub(super) fn selected_source(&self) -> Option<&SourceBox> {
        let (sid, _) = self.callout_pair()?;
        match self.doc.get(sid).map(|o| &o.kind) {
            Some(Kind::Source(sb)) => Some(sb),
            _ => None,
        }
    }

    /// Edits the selected callout pair (undoable), else the defaults for new callouts.
    pub(super) fn edit_callout(&mut self, cx: &mut Context<Self>, f: impl Fn(Option<&mut SourceBox>, Option<&mut Callout>, &mut CalloutDefaults)) {
        f(None, None, &mut self.defaults.callout);
        if let Some((sid, cid)) = self.callout_pair() {
            let mut d = self.defaults.callout.clone();
            self.checkpoint();
            if let Some(Object { kind: Kind::Source(sb), .. }) = self.doc.get_mut(sid) {
                f(Some(sb), None, &mut d);
            }
            if let Some(Object { kind: Kind::Callout(c), .. }) = self.doc.get_mut(cid) {
                f(None, Some(c), &mut d);
            }
            self.history.discard_if_unchanged(&self.doc);
        }
        self.changed(cx);
    }

    fn selected_image(&self) -> Option<&ImageLayer> {
        match self.selected().map(|o| &o.kind) {
            Some(Kind::Image(i)) => Some(i),
            _ => None,
        }
    }

    fn new_prop_get(&self, prop: Prop) -> f32 {
        let d = &self.defaults;
        let arrow = match self.selected().map(|o| &o.kind) {
            Some(Kind::Arrow(a)) => Some(a),
            _ => None,
        };
        let board = self.doc.board.clone().unwrap_or_default();
        let img = self.selected_image();
        let c = self.selected_callout();
        let sb = self.selected_source();
        let cd = &d.callout;
        let trim = |i: usize| {
            img.map_or(0.0, |im| {
                let (w, h) = im.oriented_size();
                [im.crop.x, im.crop.y, w as f32 - im.crop.right(), h as f32 - im.crop.bottom()][i].max(0.0)
            })
        };
        match prop {
            Prop::HeadSize => arrow.map_or(d.head_size, |a| a.head_size).max(0.0).max(if arrow.map_or(d.head_size, |a| a.head_size) == 0.0 { 3.4 } else { 0.0 }),
            Prop::Roughness => arrow.map_or(d.roughness, |a| a.roughness) * 100.0,
            Prop::Gap => board.gap,
            Prop::BoardPad => board.padding,
            Prop::ExpandL => board.expand[0],
            Prop::ExpandT => board.expand[1],
            Prop::ExpandR => board.expand[2],
            Prop::ExpandB => board.expand[3],
            Prop::ImgRadius => img.map_or(0.0, |i| i.radius),
            Prop::TrimL => trim(0),
            Prop::TrimT => trim(1),
            Prop::TrimR => trim(2),
            Prop::TrimB => trim(3),
            Prop::CalloutScale => c.map_or(cd.scale, |c| c.scale),
            Prop::CalloutWidth => c.map_or(cd.width, |c| c.width),
            Prop::CalloutRadius => c.map_or(cd.radius, |c| c.radius),
            Prop::CShadowBlur => c.map_or(cd.shadow, |c| c.shadow).map_or(0.0, |s| s.blur),
            Prop::CShadowOpacity => c.map_or(cd.shadow, |c| c.shadow).map_or(0.0, |s| s.opacity * 100.0),
            Prop::CShadowOffset => c.map_or(cd.shadow, |c| c.shadow).map_or(0.0, |s| s.offset),
            Prop::SourceWidth => sb.map_or(cd.src_width, |s| s.width),
            Prop::SourceRadius => sb.map_or(cd.src_radius, |s| s.radius),
            _ => 0.0,
        }
    }

    /// Applies a slider of the merge, image, callout or arrow settings.
    fn set_new_prop(&mut self, prop: Prop, v: f32, cx: &mut Context<Self>) {
        let board_prop = matches!(prop, Prop::Gap | Prop::BoardPad | Prop::ExpandL | Prop::ExpandT | Prop::ExpandR | Prop::ExpandB);
        if board_prop {
            if let Some(b) = self.doc.board.as_mut() {
                match prop {
                    Prop::Gap => b.gap = v.round(),
                    Prop::BoardPad => b.padding = v.round(),
                    Prop::ExpandL => b.expand[0] = v.round(),
                    Prop::ExpandT => b.expand[1] = v.round(),
                    Prop::ExpandR => b.expand[2] = v.round(),
                    _ => b.expand[3] = v.round(),
                }
                self.doc.arrange(None);
            }
            self.changed(cx);
            return;
        }
        match prop {
            Prop::HeadSize | Prop::Roughness => {
                let value = if prop == Prop::Roughness { v / 100.0 } else { v };
                if prop == Prop::Roughness {
                    self.defaults.roughness = value;
                } else {
                    self.defaults.head_size = value;
                }
                if let Some(Object { kind: Kind::Arrow(a), locked: false, .. }) = self.sel.and_then(|id| self.doc.get_mut(id)) {
                    if prop == Prop::Roughness {
                        a.roughness = value;
                    } else {
                        a.head_size = value;
                    }
                }
            }
            Prop::ImgRadius | Prop::TrimL | Prop::TrimT | Prop::TrimR | Prop::TrimB => {
                if let Some(Object { kind: Kind::Image(im), locked: false, .. }) = self.sel.and_then(|id| self.doc.get_mut(id)) {
                    if prop == Prop::ImgRadius {
                        im.radius = v;
                    } else {
                        // Trims keep the picture's scale; the board re-lays out around it.
                        let (w, h) = im.oriented_size();
                        let (w, h) = (w as f32, h as f32);
                        let sc = im.scale();
                        let mut c = im.crop;
                        let v = v.max(0.0);
                        match prop {
                            Prop::TrimL => c = r(v.min(c.right() - 16.0), c.y, c.right() - v.min(c.right() - 16.0), c.h),
                            Prop::TrimT => c = r(c.x, v.min(c.bottom() - 16.0), c.w, c.bottom() - v.min(c.bottom() - 16.0)),
                            Prop::TrimR => c.w = (w - v - c.x).max(16.0),
                            _ => c.h = (h - v - c.y).max(16.0),
                        }
                        im.crop = c;
                        im.rect = r(im.rect.x, im.rect.y, c.w * sc, c.h * sc);
                    }
                }
                self.doc.arrange(None);
            }
            _ => {
                let v2 = v;
                self.edit_callout_live(prop, v2);
            }
        }
        self.changed(cx);
    }

    /// Callout and source box sliders (no undo step per tick: the slider drag has one).
    fn edit_callout_live(&mut self, prop: Prop, v: f32) {
        let apply = |sb: Option<&mut SourceBox>, c: Option<&mut Callout>, d: &mut CalloutDefaults| {
            let shadow = |s: &mut Option<Shadow>| {
                let mut sh = s.unwrap_or(Shadow { blur: 36.0, opacity: 0.35, offset: 14.0 });
                match prop {
                    Prop::CShadowBlur => sh.blur = v,
                    Prop::CShadowOpacity => sh.opacity = v / 100.0,
                    Prop::CShadowOffset => sh.offset = v,
                    _ => return,
                }
                *s = Some(sh);
            };
            match prop {
                Prop::CalloutScale => d.scale = v,
                Prop::CalloutWidth => d.width = v,
                Prop::CalloutRadius => d.radius = v,
                Prop::SourceWidth => d.src_width = v,
                Prop::SourceRadius => d.src_radius = v,
                _ => shadow(&mut d.shadow),
            }
            if let Some(c) = c {
                match prop {
                    Prop::CalloutScale => c.scale = v.clamp(Callout::MIN_SCALE, Callout::MAX_SCALE),
                    Prop::CalloutWidth => c.width = v,
                    Prop::CalloutRadius => c.radius = v,
                    Prop::CShadowBlur | Prop::CShadowOpacity | Prop::CShadowOffset => shadow(&mut c.shadow),
                    _ => {}
                }
            }
            if let Some(sb) = sb {
                match prop {
                    Prop::SourceWidth => sb.width = v,
                    Prop::SourceRadius => sb.radius = v,
                    _ => {}
                }
            }
        };
        apply(None, None, &mut self.defaults.callout);
        if let Some((sid, cid)) = self.callout_pair() {
            let mut d = self.defaults.callout.clone();
            if let Some(Object { kind: Kind::Source(sb), .. }) = self.doc.get_mut(sid) {
                apply(Some(sb), None, &mut d);
            }
            if let Some(Object { kind: Kind::Callout(c), .. }) = self.doc.get_mut(cid) {
                apply(None, Some(c), &mut d);
            }
        }
    }

    fn set_prop(&mut self, prop: Prop, v: f32, cx: &mut Context<Self>) {
        let long = self.doc.oriented_size().0.max(self.doc.oriented_size().1);
        let d = &mut self.defaults;
        match prop {
            Prop::Width => d.width = v,
            Prop::Radius => d.radius = v,
            Prop::FontSize => d.size = v,
            Prop::HlWidth => d.hl_width = v,
            Prop::Strength => d.strength = v.max(render::min_blur(long)),
            Prop::Block => d.block = v.max(render::min_block(long)),
            Prop::Brush => d.brush = v,
            Prop::StepSize => d.step = v,
            Prop::Padding => self.doc.frame.padding = v.round() as u32,
            Prop::Corner => self.doc.frame.radius = v.round() as u32,
            Prop::ShadowBlur => self.doc.frame.shadow_blur = v.round() as u32,
            Prop::ShadowOpacity => self.doc.frame.shadow_opacity = v / 100.0,
            Prop::ShadowOffset => self.doc.frame.shadow_offset = v.round() as i32,
            Prop::Quality => {
                self.export.quality = v.round() as u8;
                cx.notify();
                return;
            }
            _ => {
                self.set_new_prop(prop, v, cx);
                return;
            }
        }
        let d = self.defaults.clone();
        if let Some(id) = self.sel {
            if let Some(o) = self.doc.get_mut(id).filter(|o| !o.locked) {
                match (&mut o.kind, prop) {
                    (Kind::Arrow(a), Prop::Width) => a.width = v,
                    (Kind::Line(l), Prop::Width) => l.width = v,
                    (Kind::Shape(s), Prop::Width) => s.width = v,
                    (Kind::Pen(pn), Prop::Width) => pn.width = v,
                    (Kind::Shape(s), Prop::Radius) => s.radius = v,
                    (Kind::Redact(Redact { area: Area::RoundRect(_, r), .. }), Prop::Radius) => *r = v,
                    (Kind::Text(t), Prop::FontSize) => {
                        t.size = v;
                        let (w, h) = text::measure(t);
                        t.rect.w = w;
                        t.rect.h = h;
                    }
                    (Kind::Highlight(Highlight { mark: Mark::Free { width, .. }, .. }), Prop::HlWidth) => *width = v,
                    (Kind::Redact(Redact { effect: Effect::Blur(s), .. }), Prop::Strength) => *s = d.strength,
                    (Kind::Redact(Redact { effect: Effect::Pixelate(s), .. }), Prop::Block) => *s = d.block,
                    (Kind::Redact(Redact { area: Area::Brush { width, .. }, .. }), Prop::Brush) => *width = v,
                    (Kind::Step(s), Prop::StepSize) => s.radius = v,
                    _ => {}
                }
            }
        }
        self.changed(cx);
    }

    pub(super) fn color_get(&self, slot: Slot) -> Option<Rgba> {
        let d = &self.defaults;
        let sel = self.selected().map(|o| &o.kind);
        match slot {
            Slot::Stroke => Some(match sel {
                Some(Kind::Arrow(a)) => a.color,
                Some(Kind::Line(l)) => l.color,
                Some(Kind::Shape(s)) => return s.stroke,
                Some(Kind::Text(t)) => t.color,
                Some(Kind::Step(s)) => s.color,
                Some(Kind::Pen(pn)) => pn.color,
                _ => d.color,
            }),
            Slot::Fill => match sel {
                Some(Kind::Shape(s)) => s.fill,
                _ => d.fill,
            },
            Slot::Pill => match sel {
                Some(Kind::Text(t)) => t.pill,
                _ => d.pill,
            },
            Slot::Outline => match sel {
                Some(Kind::Text(t)) => t.outline,
                _ => d.outline,
            },
            Slot::Highlight => Some(match sel {
                Some(Kind::Highlight(h)) => h.color,
                _ => d.hl_color,
            }),
            Slot::Solid => Some(match sel {
                Some(Kind::Redact(Redact { effect: Effect::Solid(c), .. })) => *c,
                _ => d.solid,
            }),
            Slot::BgSolid => match &self.doc.frame.background {
                Background::Solid { color, .. } => Some([color[0], color[1], color[2], 255]),
                _ => None,
            },
            Slot::Separator => self.doc.board.as_ref().and_then(|b| b.separator),
            Slot::CalloutBorder => Some(self.selected_callout().map_or(d.callout.border, |c| c.border)),
            Slot::SourceColor => Some(self.selected_source().map_or(d.callout.src_color, |s| s.color)),
        }
    }

    pub(super) fn set_color(&mut self, slot: Slot, c: Option<Rgba>, cx: &mut Context<Self>) {
        let d = &mut self.defaults;
        match slot {
            Slot::Stroke => {
                if let Some(c) = c {
                    d.color = c;
                }
            }
            Slot::Fill => d.fill = c,
            Slot::Pill => d.pill = c,
            Slot::Outline => d.outline = c,
            Slot::Highlight => {
                if let Some(c) = c {
                    d.hl_color = c;
                }
            }
            Slot::Solid => {
                if let Some(c) = c {
                    d.solid = [c[0], c[1], c[2], 255];
                }
            }
            Slot::BgSolid => {
                if let Some(c) = c {
                    self.doc_action(|doc| doc.frame.background = Background::Solid { color: [c[0], c[1], c[2]], grain: 0.0 }, cx);
                }
                return;
            }
            Slot::Separator => {
                self.doc_action(
                    |doc| {
                        if let Some(b) = doc.board.as_mut() {
                            b.separator = c;
                        }
                    },
                    cx,
                );
                return;
            }
            Slot::CalloutBorder | Slot::SourceColor => {
                let Some(c) = c else { return };
                self.edit_callout(cx, move |sb, co, d| {
                    if slot == Slot::CalloutBorder {
                        d.border = c;
                        if let Some(co) = co {
                            co.border = c;
                        }
                    } else {
                        d.src_color = c;
                        if let Some(sb) = sb {
                            sb.color = c;
                            if let Some(f) = sb.fill.as_mut() {
                                *f = [c[0], c[1], c[2], f[3]];
                            }
                        }
                    }
                });
                return;
            }
        }
        if self.sel.is_some() {
            self.edit_selected(cx, |o| match (&mut o.kind, slot) {
                (Kind::Arrow(a), Slot::Stroke) => a.color = c.unwrap_or(a.color),
                (Kind::Line(l), Slot::Stroke) => l.color = c.unwrap_or(l.color),
                (Kind::Shape(s), Slot::Stroke) => s.stroke = c,
                (Kind::Shape(s), Slot::Fill) => s.fill = c,
                (Kind::Text(t), Slot::Stroke) => t.color = c.unwrap_or(t.color),
                (Kind::Text(t), Slot::Pill) => t.pill = c,
                (Kind::Text(t), Slot::Outline) => t.outline = c,
                (Kind::Step(s), Slot::Stroke) => s.color = c.unwrap_or(s.color),
                (Kind::Pen(pn), Slot::Stroke) => pn.color = c.unwrap_or(pn.color),
                (Kind::Highlight(h), Slot::Highlight) => h.color = c.unwrap_or(h.color),
                (Kind::Redact(Redact { effect: Effect::Solid(s), .. }), Slot::Solid) => {
                    if let Some(c) = c {
                        *s = [c[0], c[1], c[2], 255]
                    }
                }
                _ => {}
            });
        } else {
            cx.notify();
        }
    }

    pub(super) fn sync_font_dropdown(&mut self, font: &str, cx: &mut Context<Self>) {
        if self.font_shown.as_deref() == Some(font) {
            return;
        }
        self.font_shown = Some(font.to_string());
        let i = if font.is_empty() { 0 } else { self.fonts.iter().position(|f| f == font).unwrap_or(0) };
        let items = self.fonts.iter().map(|f| f.clone().into()).collect();
        self.font_dropdown.update(cx, |d, cx| d.set_items(items, Some(i), cx));
    }

    pub(super) fn sync_custom_inputs(&mut self, w: u32, h: u32, window: &mut Window, cx: &mut Context<Self>) {
        if self.custom_shown == Some((w, h)) {
            return;
        }
        self.custom_shown = Some((w, h));
        self.custom_w.update(cx, |s, cx| s.set_value(w.to_string(), window, cx));
        self.custom_h.update(cx, |s, cx| s.set_value(h.to_string(), window, cx));
    }

    pub(super) fn show_toast(&mut self, text: String, cx: &mut Context<Self>) {
        self.toast(text, None, cx);
    }

    /// A slider for `prop`: drag anywhere on it (one undo step per drag).
    pub(super) fn slider_el(&self, prop: Prop, label: &str, cx: &mut Context<Self>) -> gpui_kit::Stateful<gpui_kit::Div> {
        let (min, max, _) = prop_range(prop, self.long_side());
        let v = self.prop_get(prop);
        let shown = match prop {
            Prop::ShadowOpacity | Prop::CShadowOpacity | Prop::Roughness => format!("{}%", v.round()),
            Prop::Quality => format!("{}", v.round()),
            Prop::CalloutScale | Prop::HeadSize => format!("{v:.1}×"),
            _ => format!("{} px", v.round()),
        };
        let store = self.slider_bounds.clone();
        ui::slider(("slider", prop as usize), label.to_string(), shown, (v - min) / (max - min).max(1e-6), self.slider_drag == Some(prop), move |b| {
            store.borrow_mut().insert(prop, b);
        })
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |v, ev: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                v.history.checkpoint(&v.doc);
                v.slider_drag = Some(prop);
                v.slider_to(prop, ev.position.x, cx);
            }),
        )
    }

    fn slider_to(&mut self, prop: Prop, x: Pixels, cx: &mut Context<Self>) {
        let Some(b) = self.slider_bounds.borrow().get(&prop).copied() else { return };
        let (min, max, step) = prop_range(prop, self.long_side());
        let v = (min + ui::slider_fraction(b, x) * (max - min)) / step;
        self.set_prop(prop, (v.round() * step).clamp(min, max), cx);
    }

    fn end_slider_drag(&mut self, cx: &mut Context<Self>) {
        if self.slider_drag.take().is_some() {
            self.history.discard_if_unchanged(&self.doc);
            cx.notify();
        }
    }

    // ---------- rendering ----------

    /// What the canvas shows: the document, or (cropping) the whole image without its frame.
    fn display_doc(&self) -> Document {
        let mut d = self.doc.clone();
        if self.tool == Tool::Crop {
            let (w, h) = d.oriented_size();
            d.crop = r(0.0, 0.0, w as f32, h as f32).union(&d.crop);
            d.frame.enabled = false;
        }
        d
    }

    fn fit_k(&self, out: (u32, u32)) -> f32 {
        let b = self.view.get();
        let (vw, vh) = (f32::from(b.size.width), f32::from(b.size.height));
        if vw < 10.0 || vh < 10.0 {
            return 0.5;
        }
        ((vw - 48.0) / out.0 as f32).min((vh - 48.0) / out.1 as f32).max(0.01)
    }

    fn k(&self, out: (u32, u32)) -> f32 {
        self.zoom.unwrap_or_else(|| self.fit_k(out))
    }

    fn map(&self) -> Map {
        let d = self.display_doc();
        let lay = d.layout();
        let k = self.k(lay.canvas);
        let b = self.view.get();
        let (bx, by, bw, bh) = (f32::from(b.origin.x), f32::from(b.origin.y), f32::from(b.size.width), f32::from(b.size.height));
        let pan = clamp_pan(self.pan, k, lay.canvas, (bw, bh));
        let ox = bx + (bw - lay.canvas.0 as f32 * k) / 2.0 + pan.0;
        let oy = by + (bh - lay.canvas.1 as f32 * k) / 2.0 + pan.1;
        Map { ox, oy, k, crop: d.crop, content: lay.content, lscale: lay.scale }
    }

    /// Render scale for the shown size (`scale_factor` from the window when known).
    /// Pulls the pan back inside `clamp_pan`'s bounds (after panning or zooming), so
    /// overscroll doesn't pile up out of sight.
    fn settle_pan(&mut self) {
        let canvas = self.display_doc().layout().canvas;
        let k = self.k(canvas);
        let b = self.view.get();
        self.pan = clamp_pan(self.pan, k, canvas, (f32::from(b.size.width), f32::from(b.size.height)));
    }

    fn render_scale(&self, scale_factor: Option<f32>) -> f32 {
        let d = self.display_doc();
        let k = self.k(d.layout().canvas);
        let sf = scale_factor.unwrap_or(2.0);
        let full = (k * sf).clamp(0.05, 1.0);
        if self.drag.is_some() { (full * 0.5).max(0.05) } else { full }
    }

    fn request_render(&mut self, cx: &mut Context<Self>) {
        if self.render_state.request() {
            self.start_render(cx);
        }
    }

    fn start_render(&mut self, cx: &mut Context<Self>) {
        self.render_state.started(self.drag.is_some());
        let doc = self.display_doc();
        let (base, cache) = (self.base.clone(), self.cache.clone());
        let s = self.render_scale(None);
        let task = cx.background_executor().spawn(async move {
            let img = render::render(&doc, &base, s, &cache);
            crate::canvas_preview::to_render_image(img)
        });
        cx.spawn(async move |this, cx| {
            let image = task.await;
            let _ = this.update(cx, |v, cx| {
                v.image = Some(image);
                v.shown_scale = s;
                if v.render_state.finished(v.drag.is_some()) {
                    v.start_render(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ---------- output ----------

    fn render_export(&self, scale: f32) -> impl std::future::Future<Output = RgbaImage> + use<> {
        let (doc, base) = (self.doc.clone(), self.base.clone());
        async move { render::render(&doc, &base, scale, &Cache::default()) }
    }

    pub(super) fn copy(&mut self, cx: &mut Context<Self>) {
        let task = cx.background_executor().spawn(self.render_export(1.0));
        cx.spawn(async move |this, cx| {
            let img = task.await;
            let _ = this.update(cx, |v, cx| {
                let msg = match super::copy_image(&img, cx) {
                    Ok(()) => "Copied to the clipboard".to_string(),
                    Err(e) => format!("Copy failed: {e}"),
                };
                v.toast(msg, None, cx);
            });
        })
        .detach();
    }

    fn stem(&self) -> String {
        self.path.file_stem().and_then(|s| s.to_str()).unwrap_or("Screenshot").to_string()
    }

    /// Writes `<name>_edited_<timestamp>.<ext>` next to the original.
    pub(super) fn save(&mut self, cx: &mut Context<Self>) {
        let dir = self.path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let dest = export::output_path(&dir, &self.stem(), &crate::recorder::stamp(), self.export.format);
        self.write_export(dest, cx);
    }

    /// Asks where to save, then writes the export there.
    pub(super) fn export_as(&mut self, cx: &mut Context<Self>) {
        let dir = self.path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let suggested = export::output_path(&dir, &self.stem(), &crate::recorder::stamp(), self.export.format);
        let name = suggested.file_name().and_then(|n| n.to_str()).unwrap_or("export.png").to_string();
        let rx = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(dest))) = rx.await {
                let _ = this.update(cx, |v, cx| v.write_export(dest, cx));
            }
        })
        .detach();
        self.export_open = false;
    }

    fn write_export(&mut self, dest: PathBuf, cx: &mut Context<Self>) {
        self.save_project();
        let settings = self.export;
        let render = self.render_export(settings.scale.clamp(1, 2) as f32);
        let task = cx.background_executor().spawn(async move {
            let img = render.await;
            let bytes = export::encode(&img, settings.format, settings.quality)?;
            std::fs::write(&dest, bytes)?;
            anyhow::Ok(dest)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |v, cx| match result {
                Ok(dest) => {
                    let name = dest.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
                    v.toast(format!("Saved {name}"), Some(dest), cx);
                }
                Err(e) => v.toast(format!("Export failed: {e}"), None, cx),
            });
        })
        .detach();
    }

    fn toast(&mut self, text: String, reveal: Option<PathBuf>, cx: &mut Context<Self>) {
        self.toast = Some(Toast { text, at: Instant::now(), reveal });
        cx.notify();
    }

    // ---------- canvas input ----------

    fn finish_editing(&mut self) {
        let Some(id) = self.editing.take() else { return };
        let empty = matches!(self.doc.get(id), Some(Object { kind: Kind::Text(t), .. }) if t.text.trim().is_empty());
        if empty {
            self.doc.remove(&[id]);
            if self.sel == Some(id) {
                self.sel = None;
            }
        }
        self.autosave_due = Some(Instant::now());
    }

    fn start_editing(&mut self, id: u64, cx: &mut Context<Self>) {
        self.checkpoint();
        self.editing = Some(id);
        self.sel = Some(id);
        cx.notify();
    }

    /// The new object for a creation tool at a point.
    fn new_object(&self, q: P) -> Option<Kind> {
        let d = &self.defaults;
        let z = r(q.x, q.y, 0.0, 0.0);
        Some(match self.tool {
            Tool::Arrow => Kind::Arrow(Arrow {
                from: q,
                to: q,
                double: d.double,
                head: d.head,
                color: d.color,
                width: d.width,
                style: d.arrow_style,
                anchors: spline::fit(&[q, q]),
                roughness: if d.arrow_style == ArrowStyle::Hand { d.roughness.max(0.5) } else { d.roughness },
                seed: self.doc.next_id as u32 * 7919 + 17,
                dashed: d.dashed,
                shadow: d.arrow_shadow,
                head_size: d.head_size,
                ..Default::default()
            }),
            Tool::Callout => {
                let c = &d.callout;
                Kind::Source(SourceBox {
                    rect: z,
                    shape: c.shape,
                    color: c.src_color,
                    width: c.src_width,
                    radius: c.src_radius,
                    fill: c.src_fill.then_some([c.src_color[0], c.src_color[1], c.src_color[2], 36]),
                    dashed: c.src_dashed,
                    visible: c.show_source,
                    callout: 0,
                })
            }
            Tool::Line => Kind::Line(Line { from: q, to: q, color: d.color, width: d.width }),
            Tool::Rect | Tool::RoundRect | Tool::Ellipse => Kind::Shape(Shape {
                kind: match self.tool {
                    Tool::Rect => ShapeKind::Rect,
                    Tool::RoundRect => ShapeKind::RoundRect,
                    _ => ShapeKind::Ellipse,
                },
                rect: z,
                stroke: Some(d.color),
                fill: d.fill,
                width: d.width,
                radius: d.radius,
            }),
            Tool::Highlight => Kind::Highlight(Highlight {
                mark: if d.hl_free { Mark::Free { points: vec![q], width: d.hl_width } } else { Mark::Rect(z) },
                color: d.hl_color,
            }),
            Tool::Pen => Kind::Pen(Pen { points: vec![q], color: d.color, width: d.width }),
            Tool::Redact => Kind::Redact(Redact {
                area: match d.redact_shape {
                    RedactShape::Rect => Area::Rect(z),
                    RedactShape::Round => Area::RoundRect(z, d.radius),
                    RedactShape::Ellipse => Area::Ellipse(z),
                    RedactShape::Brush => Area::Brush { points: vec![q], width: d.brush },
                },
                effect: match d.redact {
                    RedactMode::Blur => Effect::Blur(d.strength),
                    RedactMode::Pixelate => Effect::Pixelate(d.block),
                    RedactMode::Solid => Effect::Solid(d.solid),
                },
            }),
            Tool::Emoji => {
                let s = self.default_item_size();
                Kind::Emoji(Emoji { rect: r(q.x - s / 2.0, q.y - s / 2.0, s, s), emoji: d.emoji.clone() })
            }
            Tool::Sticker => {
                let s = self.default_item_size() * 1.6;
                let a = stickers::aspect(&d.sticker);
                let (w, h) = if a >= 1.0 { (s, s / a) } else { (s * a, s) };
                Kind::Sticker(Sticker { rect: r(q.x - w / 2.0, q.y - h / 2.0, w, h), src: d.sticker.clone() })
            }
            _ => return None,
        })
    }

    fn default_item_size(&self) -> f32 {
        let (w, h) = self.doc.oriented_size();
        (w.max(h) as f32 / 12.0).clamp(48.0, 400.0)
    }

    /// Updates the object being drawn to span `start`–`q`.
    fn update_created(&mut self, id: u64, start: P, q: P, points: &[P], shift: bool) {
        let Some(o) = self.doc.get_mut(id) else { return };
        let end_line = if shift { canvas::snap_angle(start, q) } else { q };
        let end_box = if shift { canvas::square(start, q) } else { q };
        let rect = R::from_points(start, end_box);
        match &mut o.kind {
            Kind::Arrow(a) => {
                if a.style == ArrowStyle::Hand {
                    // Freehand: follow the pen (simplified and fitted on release).
                    a.anchors = spline::fit(&spline::rdp(points, 1.0));
                } else {
                    let bend = if a.style == ArrowStyle::Curved { 0.18 } else { 0.0 };
                    a.anchors = spline::default_curve(start, end_line, bend);
                    if bend == 0.0 {
                        for an in &mut a.anchors {
                            an.c_in = an.p;
                            an.c_out = an.p;
                        }
                    }
                }
                a.sync_ends();
            }
            Kind::Source(sb) => sb.rect = rect,
            Kind::Line(l) => l.to = end_line,
            Kind::Shape(s) => s.rect = rect,
            Kind::Highlight(h) => match &mut h.mark {
                Mark::Rect(r0) => *r0 = rect,
                Mark::Free { points: pts, .. } => *pts = points.to_vec(),
            },
            Kind::Pen(pn) => pn.points = points.to_vec(),
            Kind::Redact(rd) => match &mut rd.area {
                Area::Rect(r0) | Area::RoundRect(r0, _) | Area::Ellipse(r0) => *r0 = rect,
                Area::Brush { points: pts, .. } => *pts = points.to_vec(),
            },
            Kind::Emoji(e) if rect.w > 8.0 => {
                let s = rect.w.max(rect.h);
                e.rect = r(start.x.min(q.x), start.y.min(q.y), s, s);
            }
            Kind::Sticker(st) if rect.w > 8.0 => {
                let a = stickers::aspect(&st.src);
                st.rect = r(rect.x, rect.y, rect.w, rect.w / a);
            }
            _ => {}
        }
    }

    /// The object an arrow end at `q` would attach to (not `skip`, not other arrows): its
    /// id and the glue point on its outline, within a comfortable reach.
    fn find_glue(&self, q: P, skip: u64, map: &Map) -> Option<(u64, f32, f32, P)> {
        let tol = 14.0 / map.unit();
        let mut order: Vec<&Object> = self.doc.objects.iter().filter(|o| o.id != skip && !matches!(o.kind, Kind::Arrow(_))).collect();
        order.sort_by_key(|o| o.kind.level());
        order.into_iter().rev().find_map(|o| {
            if matches!(&o.kind, Kind::Source(sb) if !sb.visible) {
                return None;
            }
            let b = o.outer_bounds();
            if !b.inflate(tol).contains(q) {
                return None;
            }
            let round = matches!(&o.kind, Kind::Callout(Callout { shape: CalloutShape::Circle, .. }) | Kind::Source(SourceBox { shape: CalloutShape::Circle, .. }) | Kind::Step(_))
                || matches!(&o.kind, Kind::Shape(Shape { kind: ShapeKind::Ellipse, .. }));
            let (u, v, at) = edge_point(b, q, round);
            (at.dist(q) <= tol).then_some((o.id, u, v, at))
        })
    }

    /// Adds the callout for source box `source` with the current defaults (and its auto
    /// connector when that's on).
    fn make_callout(&mut self, source: u64) -> Option<u64> {
        let c = self.defaults.callout.clone();
        let gap = (self.long_side() as f32 / 40.0).clamp(24.0, 96.0);
        let callout = Callout {
            source,
            rect: r(0.0, 0.0, 0.0, 0.0),
            scale: c.scale,
            shape: c.shape,
            border: c.border,
            width: c.width,
            radius: c.radius,
            shadow: c.shadow,
            cut: c.cut,
            connector: None,
        };
        let id = self.doc.add_callout(source, callout, gap)?;
        if c.connector {
            self.set_connector(id, true);
        }
        Some(id)
    }

    /// Turns a callout's auto connector (source box → callout arrow) on or off.
    pub(super) fn set_connector(&mut self, callout: u64, on: bool) {
        let Some(Object { kind: Kind::Callout(c), .. }) = self.doc.get(callout) else { return };
        let (source, existing) = (c.source, c.connector);
        match (on, existing) {
            (true, None) => {
                let d = &self.defaults;
                let template = Arrow { color: d.callout.src_color, width: d.callout.src_width.max(d.width), style: ArrowStyle::Curved, head: Head::Filled, ..Default::default() };
                let arrow = self.doc.connect(source, callout, template);
                if let Some(Object { kind: Kind::Callout(c), .. }) = self.doc.get_mut(callout) {
                    c.connector = arrow;
                }
            }
            (false, Some(a)) => {
                self.doc.objects.retain(|o| o.id != a);
                if let Some(Object { kind: Kind::Callout(c), .. }) = self.doc.get_mut(callout) {
                    c.connector = None;
                }
            }
            _ => {}
        }
    }

    /// The "Auto connector" switch: for the selected callout, and for new ones.
    pub(super) fn toggle_connector(&mut self, on: bool, cx: &mut Context<Self>) {
        self.defaults.callout.connector = on;
        if let Some((_, cid)) = self.callout_pair() {
            self.checkpoint();
            self.set_connector(cid, on);
            self.history.discard_if_unchanged(&self.doc);
            self.changed(cx);
        }
        cx.notify();
    }

    /// One-click layouts: two screenshots stacked with a callout on each and an arrow
    /// between the callouts; two side by side; or one callout on the picture.
    pub(super) fn apply_preset(&mut self, preset: Preset, cx: &mut Context<Self>) {
        use super::merge::{BoardLayout, Match};
        let images = self.doc.images().len();
        if preset != Preset::SingleCallout && (self.doc.board.is_none() || images < 2) {
            self.toast("Add a second image first.".into(), None, cx);
            self.choose_images(cx);
            return;
        }
        self.finish_editing();
        self.checkpoint();
        match preset {
            Preset::SideBySide => {
                if let Some(b) = self.doc.board.as_mut() {
                    b.layout = BoardLayout::Horizontal;
                    b.matching = Match::Height;
                }
                self.doc.arrange(None);
            }
            Preset::Compare => {
                if let Some(b) = self.doc.board.as_mut() {
                    b.layout = BoardLayout::Vertical;
                    b.matching = Match::Width;
                    b.gap = b.gap.max(24.0);
                }
                self.doc.arrange(None);
                let firsts: Vec<R> = self.doc.images().iter().take(2).map(|o| o.bounds()).collect();
                let mut callouts = Vec::new();
                for img in firsts {
                    if let Some(c) = self.add_source_and_callout(img) {
                        callouts.push(c);
                    }
                }
                if let [a, b] = callouts[..] {
                    let d = &self.defaults;
                    let template = Arrow { color: d.callout.border, width: d.width.max(4.0), style: ArrowStyle::Curved, head: Head::Filled, ..Default::default() };
                    self.doc.connect(a, b, template);
                }
            }
            Preset::SingleCallout => {
                let area = self.doc.images().first().map(|o| o.bounds()).unwrap_or(self.doc.crop);
                self.sel = self.add_source_and_callout(area);
            }
        }
        self.tool = Tool::Select;
        self.zoom = None;
        self.pan = (0.0, 0.0);
        self.history.discard_if_unchanged(&self.doc);
        self.changed(cx);
    }

    /// A source box in the middle of `area` and its callout.
    fn add_source_and_callout(&mut self, area: R) -> Option<u64> {
        let c = self.defaults.callout.clone();
        let (w, h) = ((area.w * 0.24).max(24.0), (area.h * 0.16).max(24.0));
        let rect = r(area.center().x - w / 2.0, area.center().y - h / 2.0, w, h);
        let id = self.doc.add(Kind::Source(SourceBox {
            rect,
            shape: c.shape,
            color: c.src_color,
            width: c.src_width,
            radius: c.src_radius,
            fill: c.src_fill.then_some([c.src_color[0], c.src_color[1], c.src_color[2], 36]),
            dashed: c.src_dashed,
            visible: c.show_source,
            callout: 0,
        }));
        self.make_callout(id)
    }

    /// Where pictures added to this document are kept: a folder next to its project file.
    fn assets_dir(&self) -> PathBuf {
        let stem = self.path.file_stem().and_then(|s| s.to_str()).unwrap_or("image");
        self.path.with_file_name(format!("{stem}.assets"))
    }

    /// Adds pictures as image layers (copied next to the project so the document keeps
    /// them). A single screenshot becomes a merged board first. At most 6 images.
    pub(super) fn add_images(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let room = super::merge::MAX_IMAGES.saturating_sub(self.doc.images().len().max(if self.doc.board.is_some() { 0 } else { 1 }));
        if room == 0 {
            self.toast(format!("A merge holds up to {} images.", super::merge::MAX_IMAGES), None, cx);
            return;
        }
        let dir = self.assets_dir();
        let mut added = Vec::new();
        for src in paths.into_iter().take(room) {
            let Ok(img) = image::open(&src) else { continue };
            let _ = std::fs::create_dir_all(&dir);
            let dest = dir.join(format!("{}-{}.png", crate::recorder::stamp(), added.len() + self.doc.next_id as usize));
            if img.to_rgba8().save(&dest).is_ok() {
                added.push((dest, (img.width(), img.height())));
            }
        }
        if added.is_empty() {
            self.toast("Couldn't open that as an image.".into(), None, cx);
            return;
        }
        self.checkpoint();
        if self.doc.board.is_none() {
            self.doc.into_board(self.path.clone());
        }
        let mut last = None;
        for (src, size) in added {
            last = Some(self.doc.add_image(src, size));
        }
        self.doc.arrange(None);
        self.sel = last;
        self.tool = Tool::Merge;
        self.zoom = None;
        self.pan = (0.0, 0.0);
        self.changed(cx);
    }

    pub(super) fn choose_images(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui_kit::PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some("Add".into()) });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                let _ = this.update(cx, |v, cx| v.add_images(paths, cx));
            }
        })
        .detach();
    }

    /// ⌘V with a picture on the clipboard: adds it as an image layer.
    fn paste_image(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(item) = cx.read_from_clipboard() else { return false };
        for entry in item.entries() {
            match entry {
                gpui_kit::ClipboardEntry::Image(img) => {
                    let Ok(decoded) = image::load_from_memory(&img.bytes) else { continue };
                    let tmp = std::env::temp_dir().join(format!("clipper-paste-{}.png", img.id));
                    if decoded.save(&tmp).is_ok() {
                        self.add_images(vec![tmp.clone()], cx);
                        let _ = std::fs::remove_file(tmp);
                        return true;
                    }
                }
                gpui_kit::ClipboardEntry::ExternalPaths(paths) => {
                    self.add_images(paths.paths().to_vec(), cx);
                    return true;
                }
                _ => {}
            }
        }
        false
    }

    /// Handles of the selection (unless locked), in screen points.
    fn handles(&self, map: &Map) -> Vec<(Handle, P)> {
        let Some(o) = self.selected().filter(|o| !o.locked) else { return Vec::new() };
        canvas::handles(o, map.unit()).into_iter().map(|(h, q)| (h, map.to_screen(q))).collect()
    }

    fn on_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
        self.export_open = false;
        let s = p(f32::from(ev.position.x), f32::from(ev.position.y));
        if self.space || ev.button == MouseButton::Middle {
            self.drag = Some(Drag::Pan { start: (s.x, s.y), orig: self.pan });
            return;
        }
        if ev.button != MouseButton::Left {
            return;
        }
        let map = self.map();
        let q = map.to_doc(s);
        let shift = ev.modifiers.shift;

        if self.tool == Tool::Crop {
            let c = self.doc.crop;
            let tol = HANDLE / map.unit();
            for (ex, ey) in [(-1i8, -1i8), (0, -1), (1, -1), (1, 0), (1, 1), (0, 1), (-1, 1), (-1, 0)] {
                let hp = p(c.center().x + ex as f32 * c.w / 2.0, c.center().y + ey as f32 * c.h / 2.0);
                if hp.dist(q) <= tol * 1.5 {
                    self.checkpoint();
                    self.drag = Some(Drag::Crop { orig: c, start: q, ex, ey, draw: false });
                    return;
                }
            }
            self.checkpoint();
            let draw = !c.contains(q);
            self.drag = Some(Drag::Crop { orig: c, start: q, ex: 0, ey: 0, draw });
            return;
        }

        // Handles of the selection work in every tool.
        let handle = self.selected().filter(|o| !o.locked).and_then(|o| canvas::handle_at(o, q, map.unit(), HANDLE / map.unit()));
        if let Some(kind) = handle {
            let mut orig = self.selected().cloned().unwrap();
            if let Kind::Arrow(a) = &mut orig.kind {
                a.materialize();
            }
            self.checkpoint();
            // ⌥-click an anchor: smooth ↔ sharp.
            if let (Handle::Anchor(i), true) = (kind, ev.modifiers.alt) {
                if let Some(Object { kind: Kind::Arrow(a), .. }) = self.doc.get_mut(orig.id) {
                    a.materialize();
                    let smooth = !a.anchors[i].smooth || a.anchors[i].c_in == a.anchors[i].p && a.anchors[i].c_out == a.anchors[i].p;
                    spline::set_smooth(&mut a.anchors, i, smooth);
                }
                self.sel_anchor = Some(i);
                self.history.discard_if_unchanged(&self.doc);
                self.changed(cx);
                return;
            }
            self.drag = Some(match kind {
                Handle::Anchor(i) => {
                    self.sel_anchor = Some(i);
                    Drag::Anchor { orig, i }
                }
                Handle::Tangent(i, out) => Drag::Tangent { orig, i, out },
                Handle::Point(which) => Drag::Point { orig, which },
                Handle::Resize(ex, ey) => Drag::Resize { orig, ex, ey },
                Handle::Rotate => {
                    let c = orig.bounds().center();
                    Drag::Rotate { start_angle: (q.y - c.y).atan2(q.x - c.x), orig }
                }
            });
            return;
        }

        // A selected arrow's body: double-click adds an anchor; dragging bends a curve.
        if let Some(o) = self.selected().filter(|o| !o.locked).cloned() {
            if let Kind::Arrow(a) = &o.kind {
                let local = q.rotate(o.bounds().center(), -o.rotation);
                let anchors = a.spline();
                if let Some((seg, t, d, _)) = spline::nearest(&anchors, a.style, local).filter(|n| n.2 <= (8.0 / map.unit()).max(a.width)) {
                    let _ = d;
                    if ev.click_count >= 2 {
                        self.checkpoint();
                        if let Some(Object { kind: Kind::Arrow(a), .. }) = self.doc.get_mut(o.id) {
                            a.materialize();
                            self.sel_anchor = Some(spline::insert(&mut a.anchors, seg, t));
                        }
                        self.changed(cx);
                        return;
                    }
                    if a.style.curved() {
                        let mut orig = o.clone();
                        if let Kind::Arrow(a) = &mut orig.kind {
                            a.materialize();
                        }
                        self.checkpoint();
                        self.drag = Some(Drag::Bend { orig, seg, t, start: local });
                        return;
                    }
                }
            }
        }

        // At least 8 screen points of slack: thin arrows and outlines are easy to grab.
        let tol = 8.0 / map.unit();
        let hit = self.doc.hit(q, tol);
        if hit != self.sel {
            self.sel_anchor = None;
        }
        match self.tool {
            // Merge too: after adding a picture you're in Merge, and clicks there have to
            // pick and move the pictures (it draws nothing).
            Tool::Select | Tool::Background | Tool::Merge => {
                if ev.click_count >= 2 {
                    if let Some(id) = hit.filter(|id| matches!(self.doc.get(*id), Some(Object { kind: Kind::Text(_), locked: false, .. }))) {
                        self.start_editing(id, cx);
                        return;
                    }
                }
                self.finish_editing();
                self.select(hit, window, cx);
                if let Some(id) = hit.filter(|id| self.doc.get(*id).is_some_and(|o| !o.locked)) {
                    self.checkpoint();
                    let orig = self.doc.objects.iter().filter(|o| o.id == id).cloned().collect();
                    let targets = canvas::snap_targets(&self.doc, &[id]);
                    self.drag = Some(Drag::Move { start: q, orig, targets });
                }
            }
            Tool::Text => {
                if let Some(id) = hit.filter(|id| matches!(self.doc.get(*id), Some(Object { kind: Kind::Text(_), locked: false, .. }))) {
                    self.finish_editing();
                    self.start_editing(id, cx);
                    return;
                }
                self.finish_editing();
                self.checkpoint();
                let d = &self.defaults;
                let mut t = Text {
                    rect: r(q.x, q.y, 0.0, 0.0),
                    text: String::new(),
                    font: d.font.clone(),
                    size: d.size,
                    weight: d.weight,
                    color: d.color,
                    align: d.align,
                    pill: d.pill,
                    outline: d.outline,
                    shadow: d.shadow,
                };
                let (w, h) = text::measure(&t);
                t.rect = r(q.x, q.y - h / 2.0, w, h);
                let id = self.doc.add(Kind::Text(t));
                self.editing = Some(id);
                self.sel = Some(id);
                self.changed(cx);
            }
            Tool::Step => {
                self.checkpoint();
                let n = self.doc.next_step();
                let id = self.doc.add(Kind::Step(Step { center: q, radius: self.defaults.step, n, color: self.defaults.color }));
                self.sel = Some(id);
                self.changed(cx);
            }
            _ => {
                let Some(kind) = self.new_object(q) else { return };
                self.finish_editing();
                self.checkpoint();
                let id = self.doc.add(kind);
                self.sel = Some(id);
                self.drag = Some(Drag::Create { id, start: q, points: vec![q] });
                let _ = shift;
                self.changed(cx);
            }
        }
        cx.notify();
    }

    /// The handle under `s` (screen points): the crop frame's in the crop tool, else the
    /// selection's.
    fn handle_under(&self, s: P) -> Option<Handle> {
        let map = self.map();
        let q = map.to_doc(s);
        let tol = HANDLE / map.unit();
        if self.tool == Tool::Crop {
            let c = self.doc.crop;
            return [(-1i8, -1i8), (0, -1), (1, -1), (1, 0), (1, 1), (0, 1), (-1, 1), (-1, 0)]
                .into_iter()
                .find(|(ex, ey)| p(c.center().x + *ex as f32 * c.w / 2.0, c.center().y + *ey as f32 * c.h / 2.0).dist(q) <= tol * 1.5)
                .map(|(ex, ey)| Handle::Resize(ex, ey));
        }
        self.selected().filter(|o| !o.locked).and_then(|o| canvas::handle_at(o, q, map.unit(), tol))
    }

    fn on_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.drag.is_none() {
            let hover = self.handle_under(p(f32::from(ev.position.x), f32::from(ev.position.y)));
            if hover != self.hover {
                self.hover = hover;
                cx.notify();
            }
        }
        let Some(drag) = self.drag.clone() else { return };
        if ev.pressed_button.is_none() {
            return;
        }
        let s = p(f32::from(ev.position.x), f32::from(ev.position.y));
        let map = self.map();
        let q = map.to_doc(s);
        let shift = ev.modifiers.shift;
        match drag {
            Drag::Pan { start, orig } => {
                self.pan = (orig.0 + s.x - start.0, orig.1 + s.y - start.1);
                self.settle_pan();
                cx.notify();
                return;
            }
            Drag::Create { id, start, mut points } => {
                let min = 1.5 / map.unit();
                if points.last().is_none_or(|l: &P| l.dist(q) >= min) {
                    points.push(q);
                }
                self.update_created(id, start, q, &points, shift);
                self.drag = Some(Drag::Create { id, start, points });
            }
            Drag::Move { start, orig, targets } => {
                let ob = orig.iter().map(|o| o.outer_bounds()).reduce(|a, b| a.union(&b)).unwrap_or_default();
                let (dx, dy, gx, gy) = canvas::snap_move(ob, q.x - start.x, q.y - start.y, &targets, 6.0 / map.unit());
                self.guides = (gx.into_iter().collect(), gy.into_iter().collect());
                for o in &orig {
                    if let Some(cur) = self.doc.get_mut(o.id) {
                        *cur = o.clone();
                        cur.translate(dx, dy);
                        // A moved arrow lets go of what it was attached to.
                        if let Kind::Arrow(a) = &mut cur.kind {
                            if dx != 0.0 || dy != 0.0 {
                                a.glue = [None, None];
                            }
                        }
                    }
                }
            }
            Drag::Resize { orig, ex, ey } => {
                let keep_aspect = shift || matches!(orig.kind, Kind::Text(_) | Kind::Emoji(_) | Kind::Step(_));
                let mut nb = canvas::resize(&orig, ex, ey, q, keep_aspect);
                // Pictures keep their aspect, sized from the handle being dragged (the top and
                // bottom handles used to do nothing).
                if let (Kind::Image(im), 0.0) = (&orig.kind, orig.rotation) {
                    nb = resize_picture(nb, orig.bounds(), ex, ey, im.crop.w / im.crop.h.max(1e-3));
                }
                if let Some(cur) = self.doc.get_mut(orig.id) {
                    *cur = orig.clone();
                    cur.set_bounds(nb);
                }
            }
            Drag::Rotate { orig, start_angle } => {
                let c = orig.bounds().center();
                let a = (q.y - c.y).atan2(q.x - c.x);
                let mut deg = orig.rotation + (a - start_angle).to_degrees();
                if shift {
                    deg = (deg / 15.0).round() * 15.0;
                }
                if let Some(cur) = self.doc.get_mut(orig.id) {
                    cur.rotation = deg.rem_euclid(360.0);
                }
            }
            Drag::Point { orig, which } => {
                let c = orig.bounds().center();
                let local = q.rotate(c, -orig.rotation);
                if let Some(cur) = self.doc.get_mut(orig.id) {
                    *cur = orig.clone();
                    match (&mut cur.kind, which) {
                        (Kind::Line(l), PointHandle::From) => l.from = if shift { canvas::snap_angle(l.to, local) } else { local },
                        (Kind::Line(l), PointHandle::To) => l.to = if shift { canvas::snap_angle(l.from, local) } else { local },
                        _ => {}
                    }
                }
            }
            Drag::Anchor { orig, i } => {
                let c = orig.bounds().center();
                let local = q.rotate(c, -orig.rotation);
                let Kind::Arrow(oa) = &orig.kind else { return };
                let last = oa.anchors.len() - 1;
                let end = if i == 0 { Some(0) } else if i == last { Some(1) } else { None };
                // ⇧ keeps the angle to the neighboring anchor in 45° steps.
                let mut target = local;
                if shift {
                    let nb = if i == 0 { oa.anchors[1].p } else { oa.anchors[i - 1].p };
                    target = canvas::snap_angle(nb, local);
                }
                // An end dropped on an object's edge attaches to it.
                let glue = end.and_then(|_| self.find_glue(q, orig.id, &map));
                self.glue_hint = glue.map(|(id, _, _, at)| (id, at));
                if let Some((_, _, _, at)) = glue {
                    target = at.rotate(c, -orig.rotation);
                }
                if let Some(cur) = self.doc.get_mut(orig.id) {
                    *cur = orig.clone();
                    if let Kind::Arrow(a) = &mut cur.kind {
                        let d = (target.x - a.anchors[i].p.x, target.y - a.anchors[i].p.y);
                        a.anchors[i].shift(d.0, d.1);
                        if let Some(e) = end {
                            a.glue[e] = glue.map(|(id, u, v, _)| Glue { id, u, v });
                        }
                        a.sync_ends();
                    }
                }
            }
            Drag::Tangent { orig, i, out } => {
                let local = q.rotate(orig.bounds().center(), -orig.rotation);
                if let Some(cur) = self.doc.get_mut(orig.id) {
                    *cur = orig.clone();
                    if let Kind::Arrow(a) = &mut cur.kind {
                        if out {
                            a.anchors[i].c_out = local;
                        } else {
                            a.anchors[i].c_in = local;
                        }
                        if a.anchors[i].smooth {
                            spline::mirror(&mut a.anchors, i, out);
                        }
                    }
                }
            }
            Drag::Bend { orig, seg, t, start } => {
                let local = q.rotate(orig.bounds().center(), -orig.rotation);
                if let Some(cur) = self.doc.get_mut(orig.id) {
                    *cur = orig.clone();
                    if let Kind::Arrow(a) = &mut cur.kind {
                        spline::bend(&mut a.anchors, seg, t, p(local.x - start.x, local.y - start.y));
                    }
                }
            }
            Drag::Crop { orig, start, ex, ey, draw } => {
                let (ow, oh) = self.doc.oriented_size();
                let aspect = self.crop_aspect;
                let mut c = if draw {
                    R::from_points(start, q)
                } else if ex == 0 && ey == 0 {
                    orig.translate(q.x - start.x, q.y - start.y)
                } else {
                    let (mut x0, mut y0, mut x1, mut y1) = (orig.x, orig.y, orig.right(), orig.bottom());
                    match ex {
                        -1 => x0 = q.x.min(x1 - 8.0),
                        1 => x1 = q.x.max(x0 + 8.0),
                        _ => {}
                    }
                    match ey {
                        -1 => y0 = q.y.min(y1 - 8.0),
                        1 => y1 = q.y.max(y0 + 8.0),
                        _ => {}
                    }
                    r(x0, y0, x1 - x0, y1 - y0)
                };
                if let Some(a) = aspect {
                    let h = c.w / a;
                    c = r(c.x, if ey < 0 { c.bottom() - h } else { c.y }, c.w, h);
                }
                // The crop may grow past the image (canvas resize) up to twice its size.
                let lim = r(-(ow as f32) / 2.0, -(oh as f32) / 2.0, ow as f32 * 2.0, oh as f32 * 2.0);
                let c = c.intersect(&lim).unwrap_or(orig);
                if c.w >= 8.0 && c.h >= 8.0 {
                    self.doc.set_crop(c);
                }
            }
        }
        self.doc.sync_links();
        self.request_render(cx);
        cx.notify();
    }

    fn on_up(&mut self, _: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.take() else { return };
        self.guides = (Vec::new(), Vec::new());
        let map = self.map();
        self.glue_hint = None;
        if let Drag::Create { id, points, .. } = &drag {
            // Freehand: smooth the pen; drop strokes and shapes too small to see.
            if let Some(o) = self.doc.get_mut(*id) {
                if let Kind::Pen(pn) = &mut o.kind {
                    pn.points = smooth(&simplify(&pn.points, 2.0 / map.unit()), 2);
                }
                // A hand-drawn arrow: simplified (Ramer-Douglas-Peucker) and fitted into an
                // editable spline; the head goes on automatically.
                if let Kind::Arrow(a) = &mut o.kind {
                    if a.style == ArrowStyle::Hand && points.len() >= 2 {
                        a.anchors = spline::fit(&spline::rdp(points, 3.0 / map.unit()));
                        a.sync_ends();
                    }
                }
                let b = o.bounds();
                let tiny = match &o.kind {
                    Kind::Emoji(_) | Kind::Sticker(_) => false,
                    Kind::Pen(_) => points.len() < 2,
                    _ => b.w.max(b.h) * map.unit() < 4.0,
                };
                if tiny {
                    self.doc.remove(&[*id]);
                    self.sel = None;
                }
            }
            // A finished source box gets its magnified callout.
            if matches!(self.doc.get(*id).map(|o| &o.kind), Some(Kind::Source(_))) {
                if let Some(c) = self.make_callout(*id) {
                    self.sel = Some(c);
                }
            }
        }
        // A board re-stacks its images after anything moved; its canvas stays as it is
        // (Layout → Canvas sets it, or fits it to the pictures).
        if self.doc.board.is_some() && matches!(drag, Drag::Move { .. } | Drag::Resize { .. } | Drag::Rotate { .. } | Drag::Create { .. }) {
            // A picture sized by hand stays exactly where and how big it was left, even over
            // another one: re-stacking would push its neighbours along (and matching would
            // scale it back). The board switches to Free, which Layout shows.
            if let Drag::Resize { orig: Object { id, kind: Kind::Image(_), .. }, .. } = &drag {
                if let Some(b) = self.doc.board.as_mut() {
                    b.layout = super::merge::BoardLayout::Free;
                    b.matching = super::merge::Match::None;
                }
                // Grown over another picture: it covers that one.
                let mine = self.doc.get(*id).map(|o| o.outer_bounds());
                if mine.is_some_and(|m| self.doc.images().iter().any(|o| o.id != *id && o.outer_bounds().intersect(&m).is_some())) {
                    self.doc.reorder(*id, Order::Front);
                }
            }
            self.doc.restack();
        }
        if !matches!(drag, Drag::Pan { .. }) {
            self.history.discard_if_unchanged(&self.doc);
            self.changed(cx);
        }
        cx.notify();
    }

    fn zoom_by(&mut self, factor: f32, around: Option<P>, cx: &mut Context<Self>) {
        let d = self.display_doc();
        let out = d.layout().canvas;
        let k = self.k(out);
        // No further out than Fit: the picture never shrinks into a sea of empty canvas.
        let nk = (k * factor).max(self.fit_k(out).min(16.0)).min(16.0);
        let b = self.view.get();
        let (bx, by, bw, bh) = (f32::from(b.origin.x), f32::from(b.origin.y), f32::from(b.size.width), f32::from(b.size.height));
        let center = |kk: f32| (bx + (bw - out.0 as f32 * kk) / 2.0, by + (bh - out.1 as f32 * kk) / 2.0);
        let (cx0, cy0) = center(k);
        let origin = (cx0 + self.pan.0, cy0 + self.pan.1);
        let at = around.unwrap_or(p(bx + bw / 2.0, by + bh / 2.0));
        let o = ((at.x - origin.0) / k, (at.y - origin.1) / k);
        let new_origin = (at.x - o.0 * nk, at.y - o.1 * nk);
        let (cx1, cy1) = center(nk);
        self.pan = (new_origin.0 - cx1, new_origin.1 - cy1);
        self.zoom = Some(nk);
        self.settle_pan();
        self.request_render(cx);
        cx.notify();
    }

    pub(super) fn zoom_fit(&mut self, cx: &mut Context<Self>) {
        self.zoom = None;
        self.pan = (0.0, 0.0);
        self.request_render(cx);
        cx.notify();
    }

    pub(super) fn zoom_actual(&mut self, window: &Window, cx: &mut Context<Self>) {
        let k = self.k(self.display_doc().layout().canvas);
        self.zoom_by(1.0 / window.scale_factor() / k, None, cx);
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let key = ks.key.as_str();
        let cmd = ks.modifiers.platform;
        let shift = ks.modifiers.shift;
        // Inline text editing takes the keys first.
        if let Some(id) = self.editing {
            if !cmd {
                let mut changed = true;
                if let Some(Object { kind: Kind::Text(t), .. }) = self.doc.get_mut(id) {
                    match key {
                        "escape" => changed = false,
                        "backspace" => {
                            t.text.pop();
                        }
                        "enter" => t.text.push('\n'),
                        _ => match &ks.key_char {
                            Some(c) if !ks.modifiers.control => t.text.push_str(c),
                            _ => changed = false,
                        },
                    }
                    if changed {
                        let (w, h) = text::measure(t);
                        t.rect.w = w;
                        t.rect.h = h;
                    }
                }
                if key == "escape" {
                    self.finish_editing();
                    self.history.discard_if_unchanged(&self.doc);
                }
                self.changed(cx);
                cx.stop_propagation();
                return;
            }
        }
        if !self.focus.is_focused(window) {
            return;
        }
        let nudge = if shift { 10.0 } else { 1.0 };
        match (cmd, key) {
            (true, "z") if shift => self.redo(window, cx),
            (true, "z") => self.undo(window, cx),
            (true, "d") => self.object_action("duplicate", window, cx),
            (true, "l") => self.object_action("lock", window, cx),
            (true, "]") => self.object_action(if shift { "front" } else { "forward" }, window, cx),
            (true, "[") => self.object_action(if shift { "back" } else { "backward" }, window, cx),
            (true, "c") => self.copy(cx),
            (true, "v") => {
                self.paste_image(cx);
            }
            (true, "s") => self.save(cx),
            (true, "e") => {
                self.export_open = !self.export_open;
                cx.notify();
            }
            (true, "0") => self.zoom_fit(cx),
            (true, "1") => self.zoom_actual(window, cx),
            (true, "=") | (true, "+") => self.zoom_by(1.25, None, cx),
            (true, "-") => self.zoom_by(0.8, None, cx),
            (false, "backspace") | (false, "delete") => {
                // A selected anchor goes first (an arrow keeps at least two).
                let anchor = self.sel_anchor.zip(self.sel).filter(|(_, id)| matches!(self.doc.get(*id).map(|o| &o.kind), Some(Kind::Arrow(a)) if a.anchors.len() > 2));
                if let Some((i, id)) = anchor {
                    self.checkpoint();
                    if let Some(Object { kind: Kind::Arrow(a), .. }) = self.doc.get_mut(id) {
                        spline::remove(&mut a.anchors, i);
                        a.sync_ends();
                    }
                    self.sel_anchor = None;
                    self.changed(cx);
                } else {
                    self.object_action("delete", window, cx);
                }
            }
            (false, "escape") => {
                if self.tool != Tool::Select {
                    self.set_tool(Tool::Select, window, cx);
                } else {
                    self.select(None, window, cx);
                }
            }
            (false, "enter") if self.tool == Tool::Crop => self.set_tool(Tool::Select, window, cx),
            (false, "enter") => {
                if let Some(id) = self.sel.filter(|id| matches!(self.doc.get(*id), Some(Object { kind: Kind::Text(_), .. }))) {
                    self.start_editing(id, cx);
                }
            }
            (false, "space") => {
                self.space = true;
            }
            (false, "left" | "right" | "up" | "down") if ks.modifiers.alt => {
                // Keyboard alternative to dragging a handle: grow/shrink from the bottom-right.
                if let Some(o) = self.selected().filter(|o| !o.locked).cloned() {
                    let (dw, dh) = match key {
                        "left" => (-nudge, 0.0),
                        "right" => (nudge, 0.0),
                        "up" => (0.0, -nudge),
                        _ => (0.0, nudge),
                    };
                    let b = o.bounds();
                    self.checkpoint();
                    if let Some(cur) = self.doc.get_mut(o.id) {
                        cur.set_bounds(r(b.x, b.y, (b.w + dw).max(4.0), (b.h + dh).max(4.0)));
                    }
                    self.history.discard_if_unchanged(&self.doc);
                    self.changed(cx);
                }
            }
            (false, "left" | "right" | "up" | "down") => {
                if let Some(id) = self.sel {
                    let (dx, dy) = match key {
                        "left" => (-nudge, 0.0),
                        "right" => (nudge, 0.0),
                        "up" => (0.0, -nudge),
                        _ => (0.0, nudge),
                    };
                    self.checkpoint();
                    self.doc.translate(&[id], dx, dy);
                    self.history.discard_if_unchanged(&self.doc);
                    self.changed(cx);
                }
            }
            (false, k) => {
                if let Some(tool) = Tool::ALL.into_iter().find(|t| t.key() == k) {
                    self.set_tool(tool, window, cx);
                }
            }
            _ => {}
        }
    }

    // ---------- view ----------

    fn top_bar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let k = self.k(self.display_doc().layout().canvas);
        let pct = (k * window.scale_factor() * 100.0).round();
        let icon_btn = |id: &'static str, icon: Icon, tip: &'static str| Button::new(id).cursor_pointer().ghost().small().icon(icon).tooltip(tip);
        // Related controls share a pill-shaped group.
        let group = || div().flex().items_center().gap(px(2.)).p(px(3.)).rounded_full().bg(rgb(RAISED)).border_1().border_color(ui::hairline());
        div()
            .h(px(56.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(ui::S2))
            .pl(px(88.))
            .pr(px(ui::S4))
            .border_b_1()
            .border_color(ui::hairline())
            .child(
                group()
                    .child(icon_btn("undo", icons::undo(), "Undo  ⌘Z").rounded_full().disabled(!self.history.can_undo()).on_click(cx.listener(|v, _: &ClickEvent, w, cx| v.undo(w, cx))))
                    .child(icon_btn("redo", icons::redo(), "Redo  ⇧⌘Z").rounded_full().disabled(!self.history.can_redo()).on_click(cx.listener(|v, _: &ClickEvent, w, cx| v.redo(w, cx)))),
            )
            .child(
                group()
                    .child(icon_btn("zoom-out", icons::zoom_out(), "Zoom out  ⌘−").rounded_full().on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.zoom_by(0.8, None, cx))))
                    .child(div().w(px(48.)).text_center().text_sm().font_weight(FontWeight::MEDIUM).text_color(rgb(TEXT)).child(format!("{pct}%")))
                    .child(icon_btn("zoom-in", icons::zoom_in(), "Zoom in  ⌘=").rounded_full().on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.zoom_by(1.25, None, cx))))
                    .child(div().w(px(1.)).h(px(16.)).mx(px(2.)).bg(ui::hairline_strong()))
                    .child(Button::new("fit").cursor_pointer().ghost().small().rounded_full().label("Fit").tooltip("Fit  ⌘0").selected(self.zoom.is_none()).on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.zoom_fit(cx))))
                    .child(Button::new("actual").cursor_pointer().ghost().small().rounded_full().label("100%").tooltip("Actual size  ⌘1").on_click(cx.listener(|v, _: &ClickEvent, w, cx| v.zoom_actual(w, cx)))),
            )
            .child(div().flex_1())
            .child(
                div()
                    .id("project-note")
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .mr(px(ui::S2))
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(icons::lock().xsmall())
                    .child("Project file keeps the original. Do not share it.")
                    .tooltip(|w, cx| {
                        gpui_kit::component::tooltip::Tooltip::new("Edits autosave to a .clipper.json project next to the screenshot. Copy, Save and Export always bake redactions in, so exports are safe to share.").build(w, cx)
                    }),
            )
            .child(Button::new("copy").cursor_pointer().ghost().small().rounded_full().icon(icons::copy()).label("Copy").tooltip("Copy image  ⌘C").on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.copy(cx))))
            .child(Button::new("save").cursor_pointer().ghost().small().rounded_full().icon(icons::save()).label("Save").tooltip("Save next to the original  ⌘S").on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.save(cx))))
            .child(Button::new("export").cursor_pointer().primary().small().rounded_full().px(px(ui::S4)).icon(icons::export()).label("Export").tooltip("Export…  ⌘E").on_click(cx.listener(|v, _: &ClickEvent, _, cx| {
                v.export_open = !v.export_open;
                cx.notify();
            })))
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut col = div().w_full().flex().flex_col().items_center().gap(px(1.)).py(px(6.));
        for (i, tool) in Tool::ALL.into_iter().enumerate() {
            if matches!(tool, Tool::Text | Tool::Emoji | Tool::Redact) {
                col = col.child(div().w(px(32.)).h(px(1.)).my(px(3.)).bg(ui::hairline()));
            }
            let on = self.tool == tool;
            let tip = format!("{}  {}", tool.label(), tool.key().to_uppercase());
            // The current tool: accent tint and accent icon (not only a shade change).
            col = col.child(
                div()
                    .id(("tool", i))
                    .w(px(64.))
                    .py(px(4.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(2.))
                    .rounded(px(12.))
                    .cursor_pointer()
                    .when(on, |d| d.bg(ui::accent_tint()).text_color(rgb(GREEN)))
                    .when(!on, |d| d.text_color(rgb(MUTED)).hover(|d| d.bg(rgb(RAISED)).text_color(rgb(TEXT))))
                    .child(tool.icon().size(px(20.)))
                    .child(div().text_size(px(10.)).font_weight(FontWeight::MEDIUM).child(tool.short()))
                    .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx))
                    .on_click(cx.listener(move |v, _: &ClickEvent, w, cx| v.set_tool(tool, w, cx))),
            );
        }
        // Scrolls when the window is shorter than the labelled tools.
        div()
            .id("toolbar")
            .w(px(76.))
            .flex_none()
            .h_full()
            .border_r_1()
            .border_color(ui::hairline())
            .overflow_y_scroll()
            .child(col)
    }

    fn overlay(&self, map: &Map) -> Vec<gpui_kit::AnyElement> {
        let b = self.view.get();
        let (bx, by) = (f32::from(b.origin.x), f32::from(b.origin.y));
        let rel = |q: P| p(q.x - bx, q.y - by);
        let mut out: Vec<gpui_kit::AnyElement> = Vec::new();
        let line = |x: f32, y: f32, w: f32, h: f32, c: u32| div().absolute().left(px(x)).top(px(y)).w(px(w)).h(px(h)).bg(rgb(c)).into_any_element();

        if self.tool == Tool::Crop {
            let c = self.doc.crop;
            let tl = rel(map.to_screen(p(c.x, c.y)));
            let br = rel(map.to_screen(p(c.right(), c.bottom())));
            let (bw, bh) = (f32::from(b.size.width), f32::from(b.size.height));
            let dim = hsla(0., 0., 0., 0.55);
            for (x, y, w, h) in [(0.0, 0.0, bw, tl.y), (0.0, br.y, bw, bh - br.y), (0.0, tl.y, tl.x, br.y - tl.y), (br.x, tl.y, bw - br.x, br.y - tl.y)] {
                out.push(div().absolute().left(px(x)).top(px(y)).w(px(w.max(0.0))).h(px(h.max(0.0))).bg(dim).into_any_element());
            }
            out.push(
                div().absolute().left(px(tl.x)).top(px(tl.y)).w(px(br.x - tl.x)).h(px(br.y - tl.y)).border_2().border_color(rgb(GREEN)).into_any_element(),
            );
            for (ex, ey) in [(-1i8, -1i8), (0, -1), (1, -1), (1, 0), (1, 1), (0, 1), (-1, 1), (-1, 0)] {
                let hx = tl.x + (ex as f32 + 1.0) / 2.0 * (br.x - tl.x);
                let hy = tl.y + (ey as f32 + 1.0) / 2.0 * (br.y - tl.y);
                out.push(handle_dot(hx, hy, true));
            }
            out.push(
                div()
                    .absolute()
                    .left(px(tl.x))
                    .top(px((tl.y - 30.0).max(4.0)))
                    .px_2()
                    .py_1()
                    .rounded(px(6.))
                    .bg(rgb(GREEN))
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(ON_GREEN))
                    .child(format!("{} × {}", c.w.round(), c.h.round()))
                    .into_any_element(),
            );
            return out;
        }

        // Snap guides.
        let c = self.doc.crop;
        for x in &self.guides.0 {
            let a = rel(map.to_screen(p(*x, c.y)));
            let z = rel(map.to_screen(p(*x, c.bottom())));
            out.push(line(a.x, a.y, 1.0, z.y - a.y, 0xff2d92));
        }
        for y in &self.guides.1 {
            let a = rel(map.to_screen(p(c.x, *y)));
            let z = rel(map.to_screen(p(c.right(), *y)));
            out.push(line(a.x, a.y, z.x - a.x, 1.0, 0xff2d92));
        }

        // Images shown larger than their pixels.
        if self.doc.board.is_some() {
            for o in self.doc.images() {
                if let Kind::Image(img) = &o.kind {
                    if img.low_res() {
                        let tl = rel(map.to_screen(p(o.outer_bounds().x, o.outer_bounds().y)));
                        out.push(
                            div()
                                .absolute()
                                .left(px(tl.x + 8.0))
                                .top(px(tl.y + 8.0))
                                .px_2()
                                .py(px(2.))
                                .rounded_full()
                                .bg(hsla(0.11, 0.9, 0.12, 0.85))
                                .text_size(px(11.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(0xffb020))
                                .child("Low resolution")
                                .into_any_element(),
                        );
                    }
                }
            }
        }
        // Where a dragged arrow end will attach.
        if let Some((id, at)) = self.glue_hint {
            if let Some(t) = self.doc.get(id) {
                let b = t.outer_bounds();
                let tl = rel(map.to_screen(p(b.x, b.y)));
                let br = rel(map.to_screen(p(b.right(), b.bottom())));
                out.push(div().absolute().left(px(tl.x - 3.0)).top(px(tl.y - 3.0)).w(px(br.x - tl.x + 6.0)).h(px(br.y - tl.y + 6.0)).rounded(px(4.)).border_2().border_dashed().border_color(rgb(GREEN)).into_any_element());
                let q = rel(map.to_screen(at));
                out.push(div().absolute().left(px(q.x - 7.0)).top(px(q.y - 7.0)).size(px(14.)).rounded_full().bg(rgb(GREEN)).border_2().border_color(rgb(0xffffff)).into_any_element());
            }
        }

        if let Some(o) = self.selected() {
            let ob = o.outer_bounds();
            let tl = rel(map.to_screen(p(ob.x, ob.y)));
            let br = rel(map.to_screen(p(ob.right(), ob.bottom())));
            let editing = self.editing == Some(o.id);
            out.push(
                div()
                    .absolute()
                    .left(px(tl.x - 2.0))
                    .top(px(tl.y - 2.0))
                    .w(px(br.x - tl.x + 4.0))
                    .h(px(br.y - tl.y + 4.0))
                    .border_1()
                    .when(editing, |d| d.border_dashed())
                    .border_color(rgb(if o.locked { MUTED } else { SELECT }))
                    .into_any_element(),
            );
            if o.locked {
                out.push(
                    div()
                        .absolute()
                        .left(px(br.x - 10.0))
                        .top(px(tl.y - 12.0))
                        .p_1()
                        .rounded_full()
                        .bg(rgb(RAISED))
                        .child(icons::lock().xsmall())
                        .into_any_element(),
                );
            }
            if editing {
                // Caret at the end of the last line.
                if let Kind::Text(t) = &o.kind {
                    let pad = text::padding(t);
                    let lines = t.text.split('\n').count().max(1) as f32;
                    let lh = t.size * 1.25;
                    let last = t.text.rsplit('\n').next().unwrap_or("");
                    let lw = text::measure(&Text { text: if last.is_empty() { String::new() } else { last.to_string() }, pill: None, ..t.clone() }).0
                        - 2.0 * text::padding(&Text { pill: None, ..t.clone() });
                    let lw = if last.is_empty() { 0.0 } else { lw };
                    let x0 = match t.align {
                        Align::Left => t.rect.x + pad + lw,
                        Align::Center => t.rect.center().x + lw / 2.0,
                        Align::Right => t.rect.right() - pad,
                    };
                    let y0 = t.rect.y + pad + (lines - 1.0) * lh;
                    let a = rel(map.to_screen(p(x0, y0 + lh * 0.12)));
                    let z = rel(map.to_screen(p(x0, y0 + lh * 0.92)));
                    out.push(line(a.x + 1.0, a.y, 2.0, (z.y - a.y).max(4.0), SELECT));
                }
            }
            // Tangent handles hang off their anchors on thin lines.
            let handles = self.handles(map);
            if let Kind::Arrow(a) = &o.kind {
                let anchors = a.spline();
                let c = o.bounds().center();
                let lines: Vec<(P, P)> = handles
                    .iter()
                    .filter_map(|(h, hp)| match h {
                        Handle::Tangent(i, _) => anchors.get(*i).map(|an| (rel(map.to_screen(an.p.rotate(c, o.rotation))), rel(*hp))),
                        _ => None,
                    })
                    .collect();
                if !lines.is_empty() {
                    out.push(
                        canvas(
                            |_, _, _| {},
                            move |bounds, _, window, _| {
                                let o = bounds.origin;
                                for (a, b) in &lines {
                                    let mut pb = gpui_kit::PathBuilder::stroke(px(1.5));
                                    pb.move_to(gpui_kit::point(o.x + px(a.x), o.y + px(a.y)));
                                    pb.line_to(gpui_kit::point(o.x + px(b.x), o.y + px(b.y)));
                                    if let Ok(path) = pb.build() {
                                        window.paint_path(path, rgb(SELECT));
                                    }
                                }
                            },
                        )
                        .absolute()
                        .inset_0()
                        .into_any_element(),
                    );
                }
            }
            for (kind, hp) in handles {
                let q = rel(hp);
                out.push(match kind {
                    Handle::Anchor(i) => {
                        let on = self.sel_anchor == Some(i);
                        div()
                            .absolute()
                            .left(px(q.x - 6.0))
                            .top(px(q.y - 6.0))
                            .size(px(12.))
                            .rounded(px(3.))
                            .bg(rgb(if on { GREEN } else { 0xffffff }))
                            .border_2()
                            .border_color(rgb(SELECT))
                            .into_any_element()
                    }
                    Handle::Tangent(..) => div()
                        .absolute()
                        .left(px(q.x - 5.0))
                        .top(px(q.y - 5.0))
                        .size(px(10.))
                        .rounded_full()
                        .bg(rgb(SELECT))
                        .border_2()
                        .border_color(rgb(0xffffff))
                        .into_any_element(),
                    Handle::Rotate => div()
                        .absolute()
                        .left(px(q.x - 6.0))
                        .top(px(q.y - 6.0))
                        .size(px(12.))
                        .rounded_full()
                        .bg(rgb(SELECT))
                        .border_2()
                        .border_color(rgb(0xffffff))
                        .into_any_element(),
                    _ => handle_dot(q.x, q.y, false),
                });
            }
        }
        out
    }

    fn export_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let s = self.export;
        let formats = Format::ALL
            .into_iter()
            .map(|f| {
                let on: ui::OnClick = Box::new(cx.listener(move |v, _: &ClickEvent, _, cx| {
                    v.export.format = f;
                    cx.notify();
                }));
                (f.label().into(), s.format == f, on)
            })
            .collect();
        let scales = [1u8, 2]
            .into_iter()
            .map(|k| {
                let on: ui::OnClick = Box::new(cx.listener(move |v, _: &ClickEvent, _, cx| {
                    v.export.scale = k;
                    cx.notify();
                }));
                (format!("{k}×").into(), s.scale == k, on)
            })
            .collect();
        let (w, h) = self.doc.layout().canvas;
        let k = s.scale as u32;
        let body = div()
            .p(px(ui::S4 + 2.0))
            .flex()
            .flex_col()
            .gap(px(ui::S4))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_base().font_weight(FontWeight::SEMIBOLD).text_color(rgb(TEXT)).child("Export"))
                    .child(ui::kbd("⌘E")),
            )
            .child(ui::field("Format", None, ui::segmented("format", formats)))
            .when(s.format != Format::Png, |d| {
                d.child(self.slider_el(Prop::Quality, "Quality", cx))
            })
            .child(ui::field("Scale", Some(format!("{} × {} px", w * k, h * k)), ui::segmented("scale", scales)))
            .child(ui::hint("Metadata stripped. Redactions are baked in, so the file is safe to share."))
            .child(Button::new("export-go").cursor_pointer().primary().rounded_full().w_full().icon(icons::export()).label("Export…").on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.export_as(cx))));
        div()
            .absolute()
            .top(px(60.))
            .right(px(ui::S3))
            .w(px(312.))
            .occlude()
            .child(ui::bezel(ui::R_LG + 4.0, 5.0, body))
            .with_animation("export-menu", gpui_kit::Animation::new(ui::ENTER).with_easing(ui::ease_out), |el, t| el.opacity(t).mt(px((1.0 - t) * -6.0)))
    }
}

fn handle_dot(x: f32, y: f32, green: bool) -> gpui_kit::AnyElement {
    div()
        .absolute()
        .left(px(x - 5.0))
        .top(px(y - 5.0))
        .size(px(10.))
        .rounded(px(3.))
        .bg(rgb(0xffffff))
        .border_2()
        .border_color(rgb(if green { GREEN } else { SELECT }))
        .into_any_element()
}

/// Slider ranges (min, max, step); sizes scale with the image.
fn prop_range(prop: Prop, long: u32) -> (f32, f32, f32) {
    let u = (long as f32 / 1200.0).max(1.0);
    match prop {
        Prop::Width => (1.0, (40.0 * u).round(), 1.0),
        Prop::Radius => (0.0, (80.0 * u).round(), 1.0),
        Prop::FontSize => (8.0, (200.0 * u).round(), 1.0),
        Prop::HlWidth => (4.0, (80.0 * u).round(), 1.0),
        Prop::Strength => (render::min_blur(long), render::min_blur(long) * 4.0, 1.0),
        Prop::Block => (render::min_block(long), render::min_block(long) * 4.0, 1.0),
        Prop::Brush => (8.0, (160.0 * u).round(), 1.0),
        Prop::StepSize => (8.0, (80.0 * u).round(), 1.0),
        Prop::Padding => (0.0, (400.0 * u).round(), 1.0),
        Prop::Corner => (0.0, (120.0 * u).round(), 1.0),
        Prop::ShadowBlur => (0.0, (200.0 * u).round(), 1.0),
        Prop::ShadowOpacity => (0.0, 100.0, 1.0),
        Prop::ShadowOffset => (0.0, (120.0 * u).round(), 1.0),
        Prop::Quality => (10.0, 100.0, 1.0),
        Prop::HeadSize => (1.5, 8.0, 0.1),
        Prop::Roughness => (0.0, 100.0, 1.0),
        Prop::Gap => (0.0, (200.0 * u).round(), 1.0),
        Prop::BoardPad => (0.0, (300.0 * u).round(), 1.0),
        Prop::ExpandL | Prop::ExpandT | Prop::ExpandR | Prop::ExpandB => (0.0, (1600.0 * u).round(), 1.0),
        Prop::ImgRadius => (0.0, (80.0 * u).round(), 1.0),
        Prop::TrimL | Prop::TrimT | Prop::TrimR | Prop::TrimB => (0.0, (long as f32 / 2.0).round(), 1.0),
        Prop::CalloutScale => (Callout::MIN_SCALE, Callout::MAX_SCALE, 0.1),
        Prop::CalloutWidth | Prop::SourceWidth => (0.0, (24.0 * u).round(), 1.0),
        Prop::CalloutRadius | Prop::SourceRadius => (0.0, (80.0 * u).round(), 1.0),
        Prop::CShadowBlur => (0.0, (120.0 * u).round(), 1.0),
        Prop::CShadowOpacity => (0.0, 100.0, 1.0),
        Prop::CShadowOffset => (0.0, (60.0 * u).round(), 1.0),
    }
}

impl Render for EditorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.image.is_none() && !self.render_state.busy() && f32::from(self.view.get().size.width) > 10.0 {
            self.request_render(cx);
        }
        let map = self.map();
        let d = self.display_doc();
        let out = d.layout().canvas;
        let b = self.view.get();
        let (bx, by) = (f32::from(b.origin.x), f32::from(b.origin.y));
        let store = self.view.clone();
        let space = self.space;
        // A handle being dragged, else the one under the pointer.
        let handle = match &self.drag {
            Some(Drag::Resize { ex, ey, .. }) => Some(Handle::Resize(*ex, *ey)),
            Some(Drag::Crop { ex, ey, .. }) if *ex != 0 || *ey != 0 => Some(Handle::Resize(*ex, *ey)),
            Some(Drag::Rotate { .. }) => Some(Handle::Rotate),
            Some(_) => None,
            None => self.hover,
        };
        let cursor_style = if space || matches!(self.drag, Some(Drag::Pan { .. })) {
            gpui_kit::CursorStyle::OpenHand
        } else if let Some(Handle::Resize(ex, ey)) = handle {
            resize_cursor(ex, ey)
        } else if let Some(Handle::Rotate) = handle {
            gpui_kit::CursorStyle::ClosedHand
        } else if handle.is_some() || matches!(self.drag, Some(Drag::Point { .. } | Drag::Anchor { .. } | Drag::Tangent { .. })) {
            gpui_kit::CursorStyle::PointingHand
        } else if self.tool.creates() || self.tool == Tool::Crop {
            gpui_kit::CursorStyle::Crosshair
        } else {
            gpui_kit::CursorStyle::Arrow
        };
        let canvas_el = div()
            .id("canvas")
            .flex_1()
            .relative()
            .overflow_hidden()
            .bg(rgb(BG))
            .cursor(cursor_style)
            .child(canvas(move |b, _, _| store.set(b), |_, _, _, _| {}).absolute().size_full())
            .children(self.image.clone().map(|image| {
                div()
                    .absolute()
                    .left(px(map.ox - bx))
                    .top(px(map.oy - by))
                    .w(px(out.0 as f32 * map.k))
                    .h(px(out.1 as f32 * map.k))
                    .shadow(ui::float_shadow())
                    .child(img(image).size_full().object_fit(ObjectFit::Fill))
            }))
            .children(self.overlay(&map))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::on_down))
            .on_mouse_move(cx.listener(Self::on_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::on_up))
            .on_scroll(cx.listener(|v, ev: &ScrollEvent, w, cx| {
                let at = p(f32::from(ev.position.x), f32::from(ev.position.y));
                match ev.delta {
                    // Trackpad: pan (zoom with ⌘).
                    ScrollDelta::Pixels(d) if !ev.modifiers.platform => {
                        v.pan.0 += f32::from(d.x);
                        v.pan.1 += f32::from(d.y);
                        if v.zoom.is_none() {
                            v.zoom = Some(v.k(v.display_doc().layout().canvas));
                        }
                        v.settle_pan();
                        cx.notify();
                    }
                    ScrollDelta::Pixels(d) => v.zoom_by((1.0 + f32::from(d.y) * 0.01).clamp(0.5, 2.0), Some(at), cx),
                    // Mouse wheel: zoom around the pointer.
                    ScrollDelta::Lines(d) => v.zoom_by(1.1f32.powf(d.y), Some(at), cx),
                }
                let _ = w;
            }))
            .on_pinch(cx.listener(|v, ev: &PinchEvent, _, cx| {
                let at = p(f32::from(ev.position.x), f32::from(ev.position.y));
                v.zoom_by((1.0 + ev.delta).clamp(0.5, 2.0), Some(at), cx);
            }))
            // Drop pictures to merge them in.
            .drag_over::<gpui_kit::ExternalPaths>(|d, _, _, _| d.border_2().border_color(rgb(GREEN)))
            .on_drop(cx.listener(|v, paths: &gpui_kit::ExternalPaths, _, cx| v.add_images(paths.paths().to_vec(), cx)));

        let toast = self.toast.as_ref().map(|t| {
            let reveal = t.reveal.clone();
            let ok = !t.text.contains("failed");
            let id = gpui_kit::ElementId::Name(format!("toast-{:?}", t.at).into());
            div()
                .absolute()
                .bottom(px(ui::S6))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    ui::toast_pill()
                        .child(div().size(px(8.)).rounded_full().bg(rgb(if ok { GREEN } else { crate::RED })))
                        .child(t.text.clone())
                        .children(reveal.map(|path| {
                            Button::new("reveal").cursor_pointer().ghost().xsmall().rounded_full().text_color(rgb(GREEN)).label("Show in Finder").on_click(move |_: &ClickEvent, _, cx| cx.reveal_path(&path))
                        }))
                        .when(self.toast.as_ref().is_some_and(|t| t.reveal.is_none()), |d| d.pr(px(ui::S4)))
                        .with_animation(id, gpui_kit::Animation::new(ui::ENTER).with_easing(ui::ease_out), |el, t| el.opacity(t).mb(px((1.0 - t) * -8.0))),
                )
        });

        div()
            .id("editor")
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .on_mouse_move(cx.listener(|v, ev: &MouseMoveEvent, _, cx| {
                if let Some(prop) = v.slider_drag {
                    if ev.pressed_button == Some(MouseButton::Left) {
                        v.slider_to(prop, ev.position.x, cx);
                    } else {
                        v.end_slider_drag(cx);
                    }
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|v, _: &MouseUpEvent, _, cx| v.end_slider_drag(cx)))
            .on_key_up(cx.listener(|v, ev: &gpui_kit::KeyUpEvent, _, _| {
                if ev.keystroke.key == "space" {
                    v.space = false;
                }
            }))
            .child(self.top_bar(window, cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .relative()
                    .child(self.toolbar(cx))
                    .child(canvas_el)
                    .child(self.panel(window, cx))
                    .children(toast),
            )
            .when(self.export_open, |d| d.child(self.export_menu(cx)))
    }
}

#[cfg(test)]
mod render_state_tests {
    use super::RenderState;

    #[test]
    fn one_render_at_a_time_and_a_final_full_render_after_a_drag() {
        let mut r = RenderState::default();
        assert!(r.request(), "idle: start now");
        r.started(false);
        assert!(r.busy());
        // Changes while rendering queue one more render, not one per change.
        assert!(!r.request());
        assert!(!r.request());
        assert!(r.finished(false), "changed meanwhile: render again");
        assert!(!r.busy());
        r.started(false);
        assert!(!r.finished(false), "nothing changed: done");
        // A draft during a drag is redone at full resolution once the drag ends...
        r.started(true);
        assert!(r.finished(false));
        // ...but not while still dragging (the next change renders another draft).
        r.started(true);
        assert!(!r.finished(true));
    }
}

#[cfg(test)]
mod pan_tests {
    use super::super::doc::r;
    use super::{PAN_MARGIN, clamp_pan, resize_picture};

    #[test]
    fn pictures_resize_from_the_dragged_handle_past_the_canvas() {
        let orig = r(100.0, 100.0, 400.0, 200.0);
        // Bottom-right corner pulled far out: grows past any canvas, top-left held.
        assert_eq!(resize_picture(r(100.0, 100.0, 1500.0, 750.0), orig, 1, 1, 2.0), r(100.0, 100.0, 1500.0, 750.0));
        // Bottom edge: height drives the size, centered horizontally, top held.
        assert_eq!(resize_picture(r(100.0, 100.0, 400.0, 400.0), orig, 0, 1, 2.0), r(-100.0, 100.0, 800.0, 400.0));
        // Top-left corner: bottom-right held.
        assert_eq!(resize_picture(r(-300.0, -100.0, 800.0, 400.0), orig, -1, -1, 2.0), r(-300.0, -100.0, 800.0, 400.0));
    }

    #[test]
    fn the_picture_stays_in_view() {
        // Fits (400 × 300 at 1× in an 800 × 600 view): stays centered.
        assert_eq!(clamp_pan((500.0, -900.0), 1.0, (400, 300), (800.0, 600.0)), (0.0, 0.0));
        // Bigger than the view: up to its edge plus the margin, no further.
        let slack = (2000.0 - 800.0) / 2.0 + PAN_MARGIN;
        assert_eq!(clamp_pan((5000.0, 0.0), 1.0, (2000, 300), (800.0, 600.0)), (slack, 0.0));
        assert_eq!(clamp_pan((-5000.0, 0.0), 1.0, (2000, 300), (800.0, 600.0)), (-slack, 0.0));
        assert_eq!(clamp_pan((100.0, 0.0), 1.0, (2000, 300), (800.0, 600.0)), (100.0, 0.0));
    }
}
