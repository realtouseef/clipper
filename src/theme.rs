//! Light and dark palettes, and which one is showing.
//!
//! Every UI color comes from here through a semantic token (`tk::text()`, `tk::card()`, …),
//! never a raw hex in a view, so a theme switch repaints the whole app. The choice is
//! System / Light / Dark; System follows macOS and updates live. The kit's component theme
//! (buttons, switches, sliders, popovers) is mapped from the same palette in `apply`.
//!
//! Surfaces that always sit on dark pixels (the recording pill, the region selector over the
//! screen, video overlays) use `DARK` directly so they read the same in both themes.

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::{App, Hsla, WindowAppearance, hsla, px, rgb};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

/// What the user picked in Settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Appearance {
    System,
    Light,
    Dark,
}

impl Appearance {
    pub const ALL: [Appearance; 3] = [Appearance::System, Appearance::Light, Appearance::Dark];

    pub fn label(self) -> &'static str {
        match self {
            Appearance::System => "System",
            Appearance::Light => "Light",
            Appearance::Dark => "Dark",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Appearance::System => "system",
            Appearance::Light => "light",
            Appearance::Dark => "dark",
        }
    }

    fn parse(s: &str) -> Appearance {
        match s.trim() {
            "light" => Appearance::Light,
            "dark" => Appearance::Dark,
            _ => Appearance::System,
        }
    }
}

/// One theme's semantic colors (0xRRGGBB). Names follow the design spec: paper, mist, ink,
/// ink-soft, stone and coral, plus a few derived tones for states.
#[derive(Debug)]
pub struct Palette {
    /// App canvas and header.
    pub paper: u32,
    /// Panels, rails, the stage and input fills.
    pub mist: u32,
    /// Controls sitting on mist (fields, tracks, inactive chips).
    pub raised: u32,
    /// Hover on mist.
    pub hover: u32,
    pub pressed: u32,
    /// Primary text and the selected state (ink pill, ink underline).
    pub ink: u32,
    /// Secondary text; meets 4.5:1 on paper, mist and hover.
    pub ink_soft: u32,
    /// Borders and dividers.
    pub stone: u32,
    /// Record, Export and the recording indicator only.
    pub coral: u32,
    /// Coral behind a label (darker in light mode so white text keeps 4.5:1).
    pub coral_fill: u32,
    pub coral_hover: u32,
    /// Text and icons on coral.
    pub on_coral: u32,
    pub success: u32,
    pub warning: u32,
    pub error: u32,
    /// Error as text (the error hue, shifted to keep 4.5:1).
    pub error_text: u32,
    /// Warning as text.
    pub warning_text: u32,
    /// Soft fill behind warnings.
    pub warning_bg: u32,
    /// Zoom blocks on the timeline when not selected.
    pub zoom_bg: u32,
    pub zoom_line: u32,
    /// Strength of the one floating shadow.
    pub shadow: f32,
}

pub const DARK: Palette = Palette {
    paper: 0x17131a,
    mist: 0x231d27,
    raised: 0x2c2530,
    hover: 0x2e2733,
    pressed: 0x3a323d,
    ink: 0xf4f1f5,
    ink_soft: 0xa79faa,
    stone: 0x3a323d,
    coral: 0xff5a72,
    coral_fill: 0xff5a72,
    coral_hover: 0xff7387,
    on_coral: 0x17131a,
    success: 0x1f9d6b,
    warning: 0xe8a317,
    error: 0xd92d3f,
    error_text: 0xff8a95,
    warning_text: 0xf0b941,
    warning_bg: 0x3a2d14,
    zoom_bg: 0x352d39,
    zoom_line: 0x4f4553,
    shadow: 1.0,
};

pub const LIGHT: Palette = Palette {
    paper: 0xffffff,
    mist: 0xf1f0ee,
    raised: 0xffffff,
    hover: 0xe6e4e1,
    pressed: 0xdad7d3,
    ink: 0x261d27,
    ink_soft: 0x6b636c,
    stone: 0xcfcbc7,
    coral: 0xf23a55,
    coral_fill: 0xdc2c48,
    coral_hover: 0xc92640,
    on_coral: 0xffffff,
    success: 0x1f9d6b,
    warning: 0xe8a317,
    error: 0xd92d3f,
    error_text: 0xb8202f,
    warning_text: 0x8a5a00,
    warning_bg: 0xfbefd2,
    zoom_bg: 0xe4e0dc,
    zoom_line: 0xbdb7b2,
    shadow: 0.35,
};

/// Fixed colors that are not theme dependent: the selection handles drawn over the user's
/// pixels, and plain white/black for marks that must read on any image.
pub const WHITE: u32 = 0xffffff;
pub const BLACK: u32 = 0x000000;
/// Selection blue drawn on the user's image (handles, marquee); content-facing, not chrome.
pub const SELECTION: u32 = 0x0a84ff;

/// Snap guides drawn over the user's image in the editor (content-facing, not chrome).
pub const GUIDE: u32 = 0xff2d92;
/// Text over video frames (the preview's loading note).
pub const ON_VIDEO: u32 = 0xc7c7cc;

/// Swatches the user picks for their content (not interface colors). They live here so the
/// views hold no hex values.
pub mod content {
    /// Click ripple colors.
    pub const RIPPLE: [u32; 5] = [0xa4f47c, 0xffffff, 0xfacc15, 0x22d3ee, 0xf472b6];
    /// Caption text and pill colors.
    pub const CAPTION_TEXT: [u32; 6] = [0xffffff, 0xfacc15, 0xa4f47c, 0x22d3ee, 0xf472b6, 0x0a0a0b];
    pub const CAPTION_PILL: [u32; 5] = [0x000000, 0x18181a, 0xffffff, 0x1e3a8a, 0x7c3aed];
    /// Webcam border colors.
    pub const CAM_BORDER: [u32; 5] = [0xffffff, 0x0a0a0b, 0xa4f47c, 0x22d3ee, 0xf472b6];
    /// Solid backgrounds.
    pub const SOLIDS: [[u8; 3]; 14] = [
        [0xf2, 0xf2, 0xf4],
        [0xff, 0xff, 0xff],
        [0x0a, 0x0a, 0x0b],
        [0x1c, 0x1c, 0x1f],
        [0xa4, 0xf4, 0x7c],
        [0x30, 0xd1, 0x58],
        [0x0a, 0x84, 0xff],
        [0x5e, 0x5c, 0xe6],
        [0xbf, 0x5a, 0xf2],
        [0xff, 0x37, 0x5f],
        [0xff, 0x9f, 0x0a],
        [0xff, 0xd6, 0x0a],
        [0xfb, 0xe2, 0xd5],
        [0xd8, 0xe7, 0xf5],
    ];
    /// Annotation colors in the image editor (arrows, shapes, text).
    pub const ANNOTATION: [[u8; 4]; 10] = [
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
    /// Highlighter colors.
    pub const HIGHLIGHT: [[u8; 4]; 5] = [[0xff, 0xe6, 0x00, 255], [0x7c, 0xf4, 0x7c, 255], [0xff, 0x8a, 0xd8, 255], [0x7c, 0xd4, 0xff, 255], [0xff, 0xb3, 0x47, 255]];
    /// The star on a favorite image.
    pub const FAVORITE: u32 = 0xffd60a;
}

// ---------- type, radius, spacing, motion ----------

/// Type scale (Figtree). Body text is 13 or 15; headings 18 to 32.
pub const T12: f32 = 12.0;
pub const T13: f32 = 13.0;
pub const T15: f32 = 15.0;
pub const T18: f32 = 18.0;
pub const T24: f32 = 24.0;
pub const T32: f32 = 32.0;

/// The UI font, and the fallback when it can't load.
pub const FONT: &str = "Figtree";
pub const FONT_FALLBACK: &str = ".SystemUIFont";

/// Radius hierarchy: stage and library cards, floating panels and dialogs, inputs and
/// small controls. Pills use `rounded_full`.
pub const R_STAGE: f32 = 24.0;
pub const R_PANEL: f32 = 16.0;
pub const R_CONTROL: f32 = 8.0;

/// Spacing on a 4 px base.
pub const S1: f32 = 4.0;
pub const S2: f32 = 8.0;
pub const S3: f32 = 12.0;
pub const S4: f32 = 16.0;
pub const S6: f32 = 24.0;

/// Fixed sizes of the shell.
pub const HEADER_H: f32 = 56.0;
pub const PANEL_W: f32 = 320.0;
pub const PILL_H: f32 = 56.0;
pub const TOOL_BTN: f32 = 44.0;
pub const ICON: f32 = 20.0;
pub const CONTROL_H: f32 = 32.0;
/// Left inset of the header, clear of the traffic lights.
pub const TRAFFIC_INSET: f32 = 84.0;
/// Smallest window the layout is designed for.
pub const MIN_W: f32 = 1100.0;
pub const MIN_H: f32 = 680.0;

/// A new window's size: 1360 × 860, or `CLIPPER_SIZE=WxH` in debug builds (to check the
/// layout at the minimum size).
pub fn initial_size() -> gpui_kit::Size<gpui_kit::Pixels> {
    #[cfg(debug_assertions)]
    if let Some((w, h)) = std::env::var("CLIPPER_SIZE").ok().and_then(|v| {
        let (w, h) = v.split_once('x')?;
        Some((w.parse::<f32>().ok()?, h.parse::<f32>().ok()?))
    }) {
        return gpui_kit::size(px(w.max(MIN_W)), px(h.max(MIN_H)));
    }
    gpui_kit::size(px(1360.), px(860.))
}

/// Motion: only in answer to a user action.
pub const LIFT_MS: u64 = 120;
pub const PANEL_MS: u64 = 180;
pub const TOAST_MS: u64 = 180;
/// Card hover lift.
pub const LIFT: f32 = 2.0;

static REDUCE_MOTION: AtomicBool = AtomicBool::new(false);

/// macOS "Reduce motion": read once off the UI thread at startup.
pub fn reduce_motion() -> bool {
    REDUCE_MOTION.load(Ordering::Relaxed)
}

fn read_reduce_motion() {
    std::thread::spawn(|| {
        let on = std::process::Command::new("defaults")
            .args(["read", "com.apple.universalaccess", "reduceMotion"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "1")
            .unwrap_or(false);
        REDUCE_MOTION.store(on, Ordering::Relaxed);
    });
}

/// A duration, or zero when the user asked for less motion.
pub fn motion(ms: u64) -> std::time::Duration {
    std::time::Duration::from_millis(if reduce_motion() { 1 } else { ms })
}

static LIGHT_ON: AtomicBool = AtomicBool::new(false);
static PREF: AtomicU8 = AtomicU8::new(0);
/// The appearance last forced on the window chrome (0 none, 1 light, 2 dark, 255 never set),
/// so `apply` only touches it on a change (a change notifies appearance observers).
static FORCED: AtomicU8 = AtomicU8::new(255);

/// The palette showing now.
pub fn palette() -> &'static Palette {
    if LIGHT_ON.load(Ordering::Relaxed) { &LIGHT } else { &DARK }
}

pub fn is_light() -> bool {
    LIGHT_ON.load(Ordering::Relaxed)
}

macro_rules! tokens {
    ($($name:ident),*) => { $( #[inline] pub fn $name() -> u32 { palette().$name } )* };
}
tokens!(
    paper, mist, raised, hover, pressed, ink_soft, stone, coral, coral_fill, coral_hover, on_coral,
    success, warning, error, error_text, warning_text, warning_bg, zoom_bg, zoom_line
);

/// Primary text (ink).
#[inline]
pub fn text() -> u32 {
    palette().ink
}

/// The selected state: an ink fill (pill) or ink underline/outline.
#[inline]
pub fn selected() -> u32 {
    palette().ink
}

/// Text and icons on the selected ink fill.
#[inline]
pub fn on_selected() -> u32 {
    palette().paper
}

/// Ink at `alpha`: hairlines, faint washes and scrims that follow the theme.
pub fn ink(alpha: f32) -> Hsla {
    let c: Hsla = rgb(palette().ink).into();
    Hsla { a: alpha, ..c }
}

/// A color token at `alpha`.
pub fn alpha(hex: u32, a: f32) -> Hsla {
    let c: Hsla = rgb(hex).into();
    Hsla { a, ..c }
}

/// A drop-shadow color at `alpha`, softened in light mode.
pub fn shadow(alpha: f32) -> Hsla {
    hsla(0., 0., 0., alpha * palette().shadow)
}

/// The one soft elevation, for floating elements only (tool pill, popovers, toasts).
pub fn float_shadow() -> Vec<gpui_kit::BoxShadow> {
    vec![
        gpui_kit::BoxShadow { color: shadow(0.28), offset: gpui_kit::point(px(0.), px(8.)), blur_radius: px(24.), spread_radius: px(-4.), inset: false },
        gpui_kit::BoxShadow { color: shadow(0.12), offset: gpui_kit::point(px(0.), px(1.)), blur_radius: px(3.), spread_radius: px(0.), inset: false },
    ]
}

/// The bottom scrim on a hovered library card (always dark: it sits on the user's pixels).
pub fn scrim(alpha: f32) -> Hsla {
    hsla(0., 0., 0., alpha)
}

/// Text on a dark scrim over the user's pixels.
pub fn on_scrim() -> u32 {
    WHITE
}

pub fn preference() -> Appearance {
    match PREF.load(Ordering::Relaxed) {
        1 => Appearance::Light,
        2 => Appearance::Dark,
        _ => Appearance::System,
    }
}

fn store(a: Appearance) {
    PREF.store(
        match a {
            Appearance::System => 0,
            Appearance::Light => 1,
            Appearance::Dark => 2,
        },
        Ordering::Relaxed,
    );
}

fn prefs_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join("Library/Application Support/Clipper/appearance")
}

/// Startup: read the saved choice (`CLIPPER_THEME=light|dark|system` overrides it in debug
/// builds) and apply it.
pub fn init(cx: &mut App) {
    let saved = std::fs::read_to_string(prefs_path()).map(|s| Appearance::parse(&s)).unwrap_or(Appearance::System);
    #[cfg(debug_assertions)]
    let saved = std::env::var("CLIPPER_THEME").map(|s| Appearance::parse(&s)).unwrap_or(saved);
    store(saved);
    read_reduce_motion();
    load_font(cx);
    apply(cx);
}

static FONT_OK: AtomicBool = AtomicBool::new(false);

/// Registers the bundled Figtree (SIL OFL 1.1, see Credits). If it can't load, the system
/// sans is used.
fn load_font(cx: &mut App) {
    let faces: Vec<std::borrow::Cow<'static, [u8]>> = vec![
        std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/Figtree-Medium.ttf")),
        std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/Figtree-SemiBold.ttf")),
        std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/Figtree-Bold.ttf")),
    ];
    FONT_OK.store(cx.text_system().add_fonts(faces).is_ok(), Ordering::Relaxed);
}

/// The UI font family in use.
pub fn font() -> &'static str {
    if FONT_OK.load(Ordering::Relaxed) { FONT } else { FONT_FALLBACK }
}

/// The user picked an appearance: save it and repaint.
pub fn set_preference(a: Appearance, cx: &mut App) {
    store(a);
    let path = prefs_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, a.key());
    apply(cx);
}

/// macOS switched light/dark: follow it when the choice is System.
pub fn system_changed(cx: &mut App) {
    if preference() == Appearance::System {
        apply(cx);
    }
}

/// Resolve the choice to light or dark, set the window chrome to match, and map the palette
/// onto the kit's theme (through `Theme::update`, so component tokens pick it up).
pub fn apply(cx: &mut App) {
    let forced = match preference() {
        Appearance::System => 0,
        Appearance::Light => 1,
        Appearance::Dark => 2,
    };
    if FORCED.swap(forced, Ordering::Relaxed) != forced {
        cx.set_window_appearance(match forced {
            1 => Some(WindowAppearance::Light),
            2 => Some(WindowAppearance::Dark),
            _ => None,
        });
    }
    let light = match forced {
        1 => true,
        2 => false,
        _ => matches!(cx.window_appearance(), WindowAppearance::Light | WindowAppearance::VibrantLight),
    };
    LIGHT_ON.store(light, Ordering::Relaxed);
    let p = palette();
    let c = |hex: u32| -> Hsla { rgb(hex).into() };
    Theme::change(if light { ThemeMode::Light } else { ThemeMode::Dark }, None, cx);
    Theme::update(cx, |theme| {
        theme.radius = px(R_CONTROL);
        theme.radius_lg = px(R_PANEL);
        theme.shadow = false;
        theme.font_family = font().into();
        theme.font_size = px(T13);
        theme.focus_ring = true;
        let t = &mut theme.colors;
        t.background = c(p.paper);
        t.foreground = c(p.ink);
        t.border = c(p.stone);
        t.input = c(p.stone);
        t.ring = c(p.ink);
        t.muted = c(p.mist);
        t.muted_foreground = c(p.ink_soft);
        // Primary = the selected/confirm state: an ink pill with paper text. Coral is
        // reserved for Record, Export and the recording dot (see `ui::coral_button`).
        t.primary = c(p.ink);
        t.primary_hover = alpha(p.ink, 0.88);
        t.primary_active = alpha(p.ink, 0.8);
        t.primary_foreground = c(p.paper);
        t.secondary = c(p.mist);
        t.secondary_hover = c(p.hover);
        t.secondary_active = c(p.pressed);
        t.secondary_foreground = c(p.ink);
        t.accent = c(p.hover);
        t.accent_foreground = c(p.ink);
        t.popover = c(p.paper);
        t.popover_foreground = c(p.ink);
        t.list = c(p.paper);
        t.list_hover = c(p.mist);
        t.list_active = c(p.hover);
        t.list_active_border = c(p.stone);
        t.button_primary = c(p.ink);
        t.button_primary_hover = alpha(p.ink, 0.88);
        t.button_primary_active = alpha(p.ink, 0.8);
        t.button_primary_foreground = c(p.paper);
        t.button_secondary = c(p.mist);
        t.button_secondary_hover = c(p.hover);
        t.button_secondary_active = c(p.pressed);
        t.button_secondary_foreground = c(p.ink);
        // Danger = coral: Record, Export and destructive confirms only.
        t.button_danger = c(p.coral_fill);
        t.button_danger_hover = c(p.coral_hover);
        t.button_danger_active = c(p.coral_hover);
        t.button_danger_foreground = c(p.on_coral);
        t.danger = c(p.error);
        t.danger_foreground = c(p.paper);
        t.success = c(p.success);
        t.warning = c(p.warning);
        t.slider_bar = c(p.ink);
        t.slider_thumb = c(p.paper);
        t.switch = c(p.pressed);
        t.switch_thumb = c(p.paper);
        t.progress_bar = c(p.ink);
        t.tab_bar = c(p.mist);
        t.tab_bar_segmented = c(p.mist);
        t.tab = c(p.mist);
        t.tab_active = c(p.ink);
        t.tab_foreground = c(p.ink_soft);
        t.tab_active_foreground = c(p.paper);
        t.scrollbar_thumb = c(p.stone);
        t.scrollbar_thumb_hover = c(p.ink_soft);
    });
    cx.refresh_windows();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lum(c: u32) -> f64 {
        let ch = |v: u32| {
            let s = v as f64 / 255.0;
            if s <= 0.03928 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * ch(c >> 16 & 255) + 0.7152 * ch(c >> 8 & 255) + 0.0722 * ch(c & 255)
    }

    fn ratio(a: u32, b: u32) -> f64 {
        let (x, y) = (lum(a).max(lum(b)), lum(a).min(lum(b)));
        (x + 0.05) / (y + 0.05)
    }

    /// WCAG AA in both themes: text 4.5:1 on every surface it sits on, labels on the ink
    /// and coral fills 4.5:1, error/warning text 4.5:1 on paper.
    #[test]
    fn both_themes_meet_contrast() {
        for (name, p) in [("dark", &DARK), ("light", &LIGHT)] {
            for surface in [p.paper, p.mist, p.raised, p.hover] {
                assert!(ratio(p.ink, surface) >= 4.5, "{name}: ink on {surface:06x}");
                assert!(ratio(p.ink_soft, surface) >= 4.5, "{name}: ink-soft on {surface:06x}: {:.2}", ratio(p.ink_soft, surface));
            }
            assert!(ratio(p.paper, p.ink) >= 4.5, "{name}: selected pill");
            assert!(ratio(p.on_coral, p.coral_fill) >= 4.5, "{name}: label on coral {:.2}", ratio(p.on_coral, p.coral_fill));
            assert!(ratio(p.on_coral, p.coral_hover) >= 4.5, "{name}: label on coral hover");
            for t in [p.error_text, p.warning_text] {
                assert!(ratio(t, p.paper) >= 4.5, "{name}: {t:06x} on paper: {:.2}", ratio(t, p.paper));
                assert!(ratio(t, p.mist) >= 4.5, "{name}: {t:06x} on mist: {:.2}", ratio(t, p.mist));
            }
            assert!(ratio(p.coral, p.paper) >= 3.0, "{name}: recording dot on paper");
        }
    }

    /// The spec's fixed hexes.
    #[test]
    fn palette_matches_the_spec() {
        assert_eq!((LIGHT.paper, LIGHT.mist, LIGHT.ink, LIGHT.ink_soft, LIGHT.stone, LIGHT.coral), (0xffffff, 0xf1f0ee, 0x261d27, 0x6b636c, 0xcfcbc7, 0xf23a55));
        assert_eq!((DARK.paper, DARK.mist, DARK.ink, DARK.ink_soft, DARK.stone, DARK.coral), (0x17131a, 0x231d27, 0xf4f1f5, 0xa79faa, 0x3a323d, 0xff5a72));
    }

    #[test]
    fn appearance_round_trips() {
        for a in Appearance::ALL {
            assert_eq!(Appearance::parse(a.key()), a);
        }
        assert_eq!(Appearance::parse("garbage"), Appearance::System);
    }
}
