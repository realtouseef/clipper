//! The app's one background system, used by the recorder preview, every export (MP4, GIF,
//! WebP) and the screenshot editor. No UI code here.
//!
//! - `Background`: what is shown (solid color, static gradient, animated gradient, image).
//! - `render`: the renderer, a pure function of (background, size, time).
//! - `presets` / `generators`: the five built-in gradients and the harmony generator.
//! - `library`: the user's images, favorites, saved gradients, watched folder.
//! - `fit`: cover / contain and focal-point crop math.

pub mod color;
pub mod fit;
pub mod generators;
pub mod library;
pub mod presets;
pub mod render;

pub use color::Rgb;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Stop {
    /// 0..=1 along the gradient.
    pub pos: f32,
    pub color: Rgb,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub enum GradientKind {
    /// CSS convention: 0° = bottom → top, 90° = left → right.
    Linear { angle: f32 },
    /// Circle from (cx, cy) (fractions of the canvas) out to the farthest corner.
    Radial { cx: f32, cy: f32 },
    /// Sweep around (cx, cy), starting at `angle` (0° = up, clockwise).
    Conic { cx: f32, cy: f32, angle: f32 },
    /// A grid of colors (row-major, `cols`×`rows`) blended smoothly; stops are unused.
    Mesh { cols: u8, rows: u8, colors: Vec<Rgb> },
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Gradient {
    pub kind: GradientKind,
    pub stops: Vec<Stop>,
    /// Film grain against banding, 0..=100.
    #[serde(default)]
    pub grain: f32,
}

impl Gradient {
    pub fn linear(angle: f32, colors: &[Rgb]) -> Self {
        let n = colors.len().max(2) - 1;
        let stops = colors.iter().enumerate().map(|(i, &c)| Stop { pos: i as f32 / n as f32, color: c }).collect();
        Self { kind: GradientKind::Linear { angle }, stops, grain: 0.0 }
    }

    /// The colors that make it up (stops, or the mesh's points).
    pub fn colors(&self) -> Vec<Rgb> {
        match &self.kind {
            GradientKind::Mesh { colors, .. } => colors.clone(),
            _ => self.stops.iter().map(|s| s.color).collect(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum AnimStyle {
    /// Soft blobs drifting on sine paths over a base color.
    Aurora,
    /// A linear gradient whose angle turns.
    Rotate,
    /// A radial gradient breathing in and out.
    Pulse,
    /// Bands rippling across a linear gradient.
    Wave,
}

impl AnimStyle {
    pub const ALL: [AnimStyle; 4] = [AnimStyle::Aurora, AnimStyle::Rotate, AnimStyle::Pulse, AnimStyle::Wave];
    pub fn label(self) -> &'static str {
        match self {
            AnimStyle::Aurora => "Aurora",
            AnimStyle::Rotate => "Slow rotate",
            AnimStyle::Pulse => "Pulse",
            AnimStyle::Wave => "Wave",
        }
    }
}

/// An animated gradient: a deterministic function of timeline time that loops every
/// `period` seconds.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Animated {
    pub style: AnimStyle,
    /// The palette (2..=6 colors; the first is the base for Aurora).
    pub colors: Vec<Rgb>,
    /// Whole cycles per loop (1..=4), so the loop stays seamless.
    pub speed: f32,
    /// 0..=1
    pub intensity: f32,
    /// Softness, 0..=1.
    pub blur: f32,
    /// 0..=100
    pub grain: f32,
    /// Loop length in seconds, 4..=20.
    pub period: f32,
    /// Holds the first frame.
    #[serde(default)]
    pub frozen: bool,
}

impl Default for Animated {
    fn default() -> Self {
        Self {
            style: AnimStyle::Aurora,
            colors: vec![[0x0b, 0x10, 0x2a], [0x6a, 0x5a, 0xf9], [0x2c, 0xe0, 0xc4], [0xf2, 0x5c, 0xc0]],
            speed: 1.0,
            intensity: 0.7,
            blur: 0.6,
            grain: 12.0,
            period: 8.0,
            frozen: false,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Hash, Debug)]
pub enum ImageSource {
    /// A bundled image (index into `style::WALLPAPERS`).
    Builtin(usize),
    /// An image in the library (by id).
    Library(String),
    /// A file outside the library.
    File(PathBuf),
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Fit {
    #[default]
    Cover,
    /// The whole image, with a blurred copy filling the rest.
    Contain,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct ImageBg {
    pub source: ImageSource,
    pub fit: Fit,
    /// What stays visible when cropped (fractions of the image).
    pub focal: (f32, f32),
    /// 1.0..=3.0
    pub zoom: f32,
    /// Extra shift, -1..=1 of the free room each way.
    pub pan: (f32, f32),
    /// 0..=100
    pub blur: f32,
    /// Black overlay, 0..=0.8.
    pub darken: f32,
    /// 0..=100
    pub grain: f32,
}

impl ImageBg {
    pub fn new(source: ImageSource) -> Self {
        Self { source, fit: Fit::Cover, focal: (0.5, 0.5), zoom: 1.0, pan: (0.0, 0.0), blur: 0.0, darken: 0.0, grain: 0.0 }
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub enum Background {
    Solid { color: Rgb, #[serde(default)] grain: f32 },
    Gradient(Gradient),
    Animated(Animated),
    Image(ImageBg),
}

impl Default for Background {
    fn default() -> Self {
        Background::Gradient(presets::builtin_gradient(0))
    }
}

impl Background {
    pub fn is_animated(&self) -> bool {
        matches!(self, Background::Animated(a) if !a.frozen)
    }

}
