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
            Appearance::System => "Auto",
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

/// One theme's semantic colors (0xRRGGBB).
#[derive(Debug)]
pub struct Palette {
    /// Window background.
    pub bg: u32,
    /// Cards: preview, settings panel, timeline.
    pub card: u32,
    /// Controls on a card: buttons, rows, chips, tracks.
    pub raised: u32,
    pub hover: u32,
    pub pressed: u32,
    pub text: u32,
    /// Secondary text; also meets 4.5:1 on every surface.
    pub muted: u32,
    /// Between text and muted: list rows, inactive-but-readable items.
    pub text_soft: u32,
    pub line: u32,
    /// Primary fill: main action, active tab, switches, zoom blocks.
    pub green: u32,
    pub green_hover: u32,
    pub green_pressed: u32,
    /// Text and icons on a green fill.
    pub on_green: u32,
    /// Green used as text, outline or icon on a surface (the fill is too light for that on white).
    pub green_ink: u32,
    pub red: u32,
    /// Soft red disc behind the stop square / danger buttons.
    pub red_deep: u32,
    pub red_deep_hover: u32,
    /// Warnings (size limit, low resolution) as text.
    pub warn: u32,
    pub warn_bg: u32,
    /// Error messages as text.
    pub error: u32,
    /// Zoom blocks on the timeline when not selected.
    pub zoom_bg: u32,
    pub zoom_line: u32,
    /// Lightness of overlays drawn at low alpha (hairlines, washes): white on dark, black on light.
    pub ink: f32,
    /// Strength of drop shadows.
    pub shadow: f32,
}

pub const DARK: Palette = Palette {
    bg: 0x0a0a0b,
    card: 0x18181a,
    raised: 0x262629,
    hover: 0x313135,
    pressed: 0x3d3d42,
    text: 0xf4f4f5,
    muted: 0x9a9aa0,
    text_soft: 0xd4d4d8,
    line: 0x2a2a2e,
    green: 0xa4f47c,
    green_hover: 0xb9f89b,
    green_pressed: 0x8ee163,
    on_green: 0x0a0a0b,
    green_ink: 0xa4f47c,
    red: 0xff453a,
    red_deep: 0x3a1412,
    red_deep_hover: 0x4d1a17,
    warn: 0xffb020,
    warn_bg: 0x4a3410,
    error: 0xffb4a8,
    zoom_bg: 0x233119,
    zoom_line: 0x3d5a2c,
    ink: 1.0,
    shadow: 1.0,
};

/// Light: cool off-white window, white cards, soft gray controls. The lime stays the brand
/// fill (black text on it); a deep green carries it as text and outlines.
pub const LIGHT: Palette = Palette {
    bg: 0xefeff2,
    card: 0xffffff,
    raised: 0xf2f2f5,
    hover: 0xe7e7ec,
    pressed: 0xdcdce2,
    text: 0x111113,
    muted: 0x5c5c66,
    text_soft: 0x34343b,
    line: 0xe2e2e7,
    green: 0x9dec72,
    green_hover: 0x8fe260,
    green_pressed: 0x80d550,
    on_green: 0x0a0a0b,
    green_ink: 0x2f7a12,
    red: 0xd92d20,
    red_deep: 0xfde7e5,
    red_deep_hover: 0xfbd3cf,
    warn: 0xa15c00,
    warn_bg: 0xfdf0d3,
    error: 0xb42318,
    zoom_bg: 0xe4f8d8,
    zoom_line: 0x9bd77a,
    ink: 0.0,
    shadow: 0.45,
};

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
    bg, card, raised, hover, pressed, text, muted, text_soft, line, green, on_green, green_ink,
    red, warn, warn_bg, error, zoom_bg, zoom_line
);

/// An overlay at `alpha`: white on dark, black on light (hairlines, faint washes, outlines).
pub fn ink(alpha: f32) -> Hsla {
    hsla(0., 0., palette().ink, alpha)
}

/// A drop-shadow color at `alpha`, softened in light mode.
pub fn shadow(alpha: f32) -> Hsla {
    hsla(0., 0., 0., alpha * palette().shadow)
}

/// Accent at low alpha: selected tool or block background.
pub fn green_tint(alpha: f32) -> Hsla {
    let c: Hsla = rgb(green_ink()).into();
    Hsla { a: alpha, ..c }
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
    apply(cx);
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
        theme.radius = px(crate::R_MID);
        theme.radius_lg = px(crate::R_BIG);
        theme.shadow = false;
        let t = &mut theme.colors;
        t.background = c(p.bg);
        t.foreground = c(p.text);
        t.border = c(p.line);
        t.input = c(p.line);
        t.ring = c(p.green_ink);
        t.muted = c(p.raised);
        t.muted_foreground = c(p.muted);
        t.primary = c(p.green);
        t.primary_hover = c(p.green_hover);
        t.primary_active = c(p.green_pressed);
        t.primary_foreground = c(p.on_green);
        t.secondary = c(p.raised);
        t.secondary_hover = c(p.hover);
        t.secondary_active = c(p.pressed);
        t.secondary_foreground = c(p.text);
        t.accent = c(p.hover);
        t.accent_foreground = c(p.text);
        t.popover = c(if light { p.card } else { p.raised });
        t.popover_foreground = c(p.text);
        t.list = c(if light { p.card } else { p.raised });
        t.list_hover = c(p.hover);
        t.list_active = c(p.pressed);
        t.list_active_border = c(p.pressed);
        // Buttons: primary = green pill, secondary = soft control, danger = stop.
        t.button_primary = c(p.green);
        t.button_primary_hover = c(p.green_hover);
        t.button_primary_active = c(p.green_pressed);
        t.button_primary_foreground = c(p.on_green);
        t.button_secondary = c(p.raised);
        t.button_secondary_hover = c(p.hover);
        t.button_secondary_active = c(p.pressed);
        t.button_secondary_foreground = c(p.text);
        t.button_danger = c(p.red_deep);
        t.button_danger_hover = c(p.red_deep_hover);
        t.button_danger_active = c(p.red_deep_hover);
        t.button_danger_foreground = c(p.red);
        t.slider_bar = c(p.green);
        t.slider_thumb = c(if light { p.card } else { p.text });
        t.switch = c(p.pressed);
        t.switch_thumb = c(if light { p.card } else { p.text });
        t.progress_bar = c(p.green);
        t.tab_bar = c(p.card);
        t.tab_bar_segmented = c(p.raised);
        t.tab = c(p.raised);
        t.tab_active = c(if light { p.card } else { p.bg });
        t.tab_foreground = c(p.muted);
        t.tab_active_foreground = c(p.text);
        t.scrollbar_thumb = c(p.pressed);
        t.scrollbar_thumb_hover = c(p.muted);
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

    /// WCAG AA in both themes: body and secondary text 4.5:1 on every surface they sit on,
    /// green/red/warn as text 4.5:1 on cards, outlines 3:1.
    #[test]
    fn both_themes_meet_contrast() {
        for (name, p) in [("dark", &DARK), ("light", &LIGHT)] {
            for surface in [p.bg, p.card, p.raised, p.hover] {
                assert!(ratio(p.text, surface) >= 4.5, "{name}: text on {surface:06x}");
                assert!(ratio(p.muted, surface) >= 4.5, "{name}: muted on {surface:06x}: {:.2}", ratio(p.muted, surface));
                assert!(ratio(p.text_soft, surface) >= 4.5, "{name}: text_soft on {surface:06x}");
            }
            assert!(ratio(p.on_green, p.green) >= 4.5, "{name}: on green");
            assert!(ratio(p.on_green, p.green_pressed) >= 4.5, "{name}: on pressed green");
            assert!(ratio(p.bg, p.text) >= 4.5, "{name}: selected segment");
            for ink in [p.green_ink, p.error, p.warn] {
                assert!(ratio(ink, p.card) >= 4.5, "{name}: {ink:06x} on card: {:.2}", ratio(ink, p.card));
            }
            assert!(ratio(p.red, p.card) >= 3.0, "{name}: red marks on card");
            assert!(ratio(p.red, p.red_deep) >= 3.0, "{name}: stop square on its disc");
        }
    }

    #[test]
    fn appearance_round_trips() {
        for a in Appearance::ALL {
            assert_eq!(Appearance::parse(a.key()), a);
        }
        assert_eq!(Appearance::parse("garbage"), Appearance::System);
    }
}
