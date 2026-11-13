//! Canvas styling: background, rounded screen rect with drop shadow, and the cursor sprite.
//!
//! Everything here renders to plain RGBA buffers at any scale, so the live preview and the
//! export share one implementation. The output is composed in three static images plus video:
//!   1. `backdrop`: background with the screen rect's shadow baked in (opaque).
//!   2. the cropped screen video, placed at the inner rect.
//!   3. `corners`: backdrop pixels outside the rounded corners, drawn over the video to round it.
//!   4. the cursor sprite, on top of everything.

use anyhow::{Result, anyhow};
use image::imageops::{self, FilterType};
use image::{ImageBuffer, Luma, Rgb, RgbImage, Rgba, RgbaImage};
use std::path::PathBuf;
use std::sync::OnceLock;
use crate::layout::{CamFrame, Frame, Rect};
#[cfg(test)]
use crate::layout::Layout;

pub type Color = [u8; 3];

const fn hex(v: u32) -> Color {
    [(v >> 16) as u8, (v >> 8) as u8, v as u8]
}

/// Direction of a gradient preset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    /// CSS convention: 0 = bottom→top, 90 = left→right.
    Linear(f32),
    /// Circle from (x, y) (fractions of the canvas) out to the farthest corner.
    Radial(f32, f32),
}

/// A built-in gradient: color stops at positions 0..=1 along `shape`. `id` is the preset's
/// id since it first shipped (favorites refer to it).
pub struct Gradient {
    pub id: &'static str,
    pub name: &'static str,
    pub shape: Shape,
    pub stops: &'static [(f32, Color)],
}

macro_rules! g {
    ($id:literal, $name:literal, $shape:expr, [$(($p:expr, $c:expr)),+ $(,)?]) => {
        Gradient { id: $id, name: $name, shape: $shape, stops: &[$(($p, hex($c))),+] }
    };
}

use Shape::Linear;

/// The five built-in gradient presets: one light, three colorful, one dark. (The others
/// that used to ship live on in `assets/gradients/legacy.tsv` for migration only.)
pub const GRADIENTS: [Gradient; 5] = [
    g!("app:11", "Mint sky", Linear(120.0), [(0.0, 0x84fab0), (1.0, 0x8fd3f4)]),
    g!("app:16", "Mango", Linear(90.0), [(0.0, 0xfa709a), (1.0, 0xfee140)]),
    g!("app:15", "Orchid", Linear(0.0), [(0.0, 0xc471f5), (1.0, 0xfa71cd)]),
    g!("app:12", "Ocean", Linear(90.0), [(0.0, 0x4facfe), (1.0, 0x00f2fe)]),
    g!("app:19", "Midnight", Linear(180.0), [(0.0, 0x334155), (1.0, 0x020617)]),
];

/// A built-in background image, bundled into the app (`assets/backgrounds`).
pub struct Wallpaper {
    pub name: &'static str,
    bytes: &'static [u8],
}

pub const WALLPAPERS: [Wallpaper; 50] = [
    Wallpaper { name: "Groovy", bytes: include_bytes!("../assets/backgrounds/01-groovy.jpg") },
    Wallpaper { name: "Oil slick", bytes: include_bytes!("../assets/backgrounds/02-oil-slick.jpg") },
    Wallpaper { name: "Glow", bytes: include_bytes!("../assets/backgrounds/03-glow.jpg") },
    Wallpaper { name: "Iridescent", bytes: include_bytes!("../assets/backgrounds/04-iridescent.jpg") },
    Wallpaper { name: "Night stop", bytes: include_bytes!("../assets/backgrounds/05-night-stop.jpg") },
    Wallpaper { name: "Smiles", bytes: include_bytes!("../assets/backgrounds/06-smiles.jpg") },
    Wallpaper { name: "Clouds", bytes: include_bytes!("../assets/backgrounds/07-clouds.jpg") },
    Wallpaper { name: "Dusk clouds", bytes: include_bytes!("../assets/backgrounds/08-dusk-clouds.jpg") },
    Wallpaper { name: "Checker", bytes: include_bytes!("../assets/backgrounds/09-checker.jpg") },
    Wallpaper { name: "Bloom", bytes: include_bytes!("../assets/backgrounds/10-bloom.jpg") },
    Wallpaper { name: "Blossom sky", bytes: include_bytes!("../assets/backgrounds/11-blossom-sky.jpg") },
    Wallpaper { name: "Alley", bytes: include_bytes!("../assets/backgrounds/12-alley.jpg") },
    Wallpaper { name: "Stardust", bytes: include_bytes!("../assets/backgrounds/13-stardust.jpg") },
    Wallpaper { name: "Pool", bytes: include_bytes!("../assets/backgrounds/14-pool.jpg") },
    Wallpaper { name: "Cat", bytes: include_bytes!("../assets/backgrounds/15-cat.jpg") },
    Wallpaper { name: "Sky", bytes: include_bytes!("../assets/backgrounds/16-sky.jpg") },
    Wallpaper { name: "Doodles", bytes: include_bytes!("../assets/backgrounds/17-doodles.jpg") },
    Wallpaper { name: "Mosaic", bytes: include_bytes!("../assets/backgrounds/18-mosaic.jpg") },
    Wallpaper { name: "Critters", bytes: include_bytes!("../assets/backgrounds/19-critters.jpg") },
    Wallpaper { name: "Grid", bytes: include_bytes!("../assets/backgrounds/20-grid.jpg") },
    Wallpaper { name: "Squiggle", bytes: include_bytes!("../assets/backgrounds/21-squiggle.jpg") },
    Wallpaper { name: "Starry", bytes: include_bytes!("../assets/backgrounds/22-starry.jpg") },
    Wallpaper { name: "Pixel pastel", bytes: include_bytes!("../assets/backgrounds/23-pixel-pastel.jpg") },
    Wallpaper { name: "Thunder pink", bytes: include_bytes!("../assets/backgrounds/24-thunder-pink.jpg") },
    Wallpaper { name: "Swirl", bytes: include_bytes!("../assets/backgrounds/25-swirl.jpg") },
    Wallpaper { name: "Neon blobs", bytes: include_bytes!("../assets/backgrounds/26-neon-blobs.jpg") },
    Wallpaper { name: "Moss", bytes: include_bytes!("../assets/backgrounds/27-moss.jpg") },
    Wallpaper { name: "Violet maze", bytes: include_bytes!("../assets/backgrounds/28-violet-maze.jpg") },
    Wallpaper { name: "Green lines", bytes: include_bytes!("../assets/backgrounds/29-green-lines.jpg") },
    Wallpaper { name: "Coral scribble", bytes: include_bytes!("../assets/backgrounds/30-coral-scribble.jpg") },
    Wallpaper { name: "Violet scribble", bytes: include_bytes!("../assets/backgrounds/31-violet-scribble.jpg") },
    Wallpaper { name: "Pink doodle", bytes: include_bytes!("../assets/backgrounds/32-pink-doodle.jpg") },
    Wallpaper { name: "Night clouds", bytes: include_bytes!("../assets/backgrounds/33-night-clouds.jpg") },
    Wallpaper { name: "Paint streaks", bytes: include_bytes!("../assets/backgrounds/34-paint-streaks.jpg") },
    Wallpaper { name: "Lime scribble", bytes: include_bytes!("../assets/backgrounds/35-lime-scribble.jpg") },
    Wallpaper { name: "Magenta scribble", bytes: include_bytes!("../assets/backgrounds/36-magenta-scribble.jpg") },
    Wallpaper { name: "Plum shards", bytes: include_bytes!("../assets/backgrounds/37-plum-shards.jpg") },
    Wallpaper { name: "Jelly tiles", bytes: include_bytes!("../assets/backgrounds/38-jelly-tiles.jpg") },
    Wallpaper { name: "Pink pebbles", bytes: include_bytes!("../assets/backgrounds/39-pink-pebbles.jpg") },
    Wallpaper { name: "Green doodle", bytes: include_bytes!("../assets/backgrounds/40-green-doodle.jpg") },
    Wallpaper { name: "Liquid rainbow", bytes: include_bytes!("../assets/backgrounds/41-liquid-rainbow.jpg") },
    Wallpaper { name: "Orange squiggle", bytes: include_bytes!("../assets/backgrounds/42-orange-squiggle.jpg") },
    Wallpaper { name: "Retro waves", bytes: include_bytes!("../assets/backgrounds/43-retro-waves.jpg") },
    Wallpaper { name: "Gold squiggle", bytes: include_bytes!("../assets/backgrounds/44-gold-squiggle.jpg") },
    Wallpaper { name: "Contours", bytes: include_bytes!("../assets/backgrounds/45-contours.jpg") },
    Wallpaper { name: "Old town", bytes: include_bytes!("../assets/backgrounds/46-old-town.jpg") },
    Wallpaper { name: "Cat pile", bytes: include_bytes!("../assets/backgrounds/47-cat-pile.jpg") },
    Wallpaper { name: "Seaside", bytes: include_bytes!("../assets/backgrounds/48-seaside.jpg") },
    Wallpaper { name: "Fire clouds", bytes: include_bytes!("../assets/backgrounds/49-fire-clouds.jpg") },
    Wallpaper { name: "Purple doodle", bytes: include_bytes!("../assets/backgrounds/50-purple-doodle.jpg") },
];

/// The decoded built-in image `i` (decoded once, on first use).
pub fn wallpaper_image(i: usize) -> &'static BgImage {
    static DECODED: OnceLock<Vec<OnceLock<BgImage>>> = OnceLock::new();
    let i = i.min(WALLPAPERS.len() - 1);
    let slots = DECODED.get_or_init(|| WALLPAPERS.iter().map(|_| OnceLock::new()).collect());
    slots[i].get_or_init(|| {
        let wp = &WALLPAPERS[i];
        BgImage::from_bytes(wp.name, wp.bytes).expect("bundled background decodes")
    })
}

/// Pixel size of built-in image `i`, read from its header (no full decode).
pub fn wallpaper_size(i: usize) -> (u32, u32) {
    let wp = &WALLPAPERS[i.min(WALLPAPERS.len() - 1)];
    image::ImageReader::new(std::io::Cursor::new(wp.bytes))
        .with_guessed_format()
        .ok()
        .and_then(|r| r.into_dimensions().ok())
        .unwrap_or((1, 1))
}

/// Built-in image `i` cover-fitted to `w`×`h`.
pub fn render_wallpaper(i: usize, w: u32, h: u32) -> RgbImage {
    cover(wallpaper_image(i), w, h, false)
}

/// A user-chosen background photo. `small` is a downscaled copy for fast preview rendering.
pub struct BgImage {
    pub path: PathBuf,
    full: RgbImage,
    small: RgbImage,
}

impl BgImage {

    /// A bundled image.
    pub fn from_bytes(name: &str, bytes: &[u8]) -> Result<Self> {
        let full = image::load_from_memory(bytes).map_err(|e| anyhow!("Could not decode {name}: {e}"))?.to_rgb8();
        Ok(Self::new(PathBuf::from(name), full))
    }

    fn new(path: PathBuf, full: RgbImage) -> Self {
        let (w, h) = full.dimensions();
        let k = (1400.0 / w.max(h) as f32).min(1.0);
        let small = imageops::resize(
            &full,
            ((w as f32 * k).round() as u32).max(1),
            ((h as f32 * k).round() as u32).max(1),
            FilterType::Triangle,
        );
        Self { path, full, small }
    }

    /// The full image, or its ≤1400 px copy for previews.
    pub fn pixels(&self, full: bool) -> &RgbImage {
        if full { &self.full } else { &self.small }
    }

    #[allow(dead_code)]
    pub fn name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
    }
}

/// Canvas styling for a recording. The background itself is the shared
/// `backgrounds::Background` (also used by the screenshot editor).
#[derive(Clone)]
pub struct Style {
    pub bg: crate::backgrounds::Background,
    /// In output pixels.
    pub padding: u32,
    pub radius: u32,
    pub shadow_blur: u32,
    /// 0..=1
    pub shadow_opacity: f32,
    /// Cursor sprite size; 1.0 = the native macOS arrow size. 0.5..=3.0.
    pub cursor_scale: f32,
    /// Ripple at each click.
    pub click: crate::clicks::ClickStyle,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            bg: crate::backgrounds::Background::default(),
            padding: 48,
            radius: 32,
            shadow_blur: 48,
            shadow_opacity: 0.35,
            cursor_scale: 1.0,
            click: crate::clicks::ClickStyle::default(),
        }
    }
}

/// Static layers for one render scale.
pub struct Backdrop {
    /// Background + shadow, fully opaque.
    pub bg: RgbaImage,
    /// Backdrop pixels outside the rounded corners (transparent elsewhere). Its alpha is the
    /// corner mask.
    pub corners: RgbaImage,
    /// What the backdrop adds over the plain background (shadow, empty webcam region), for
    /// drawing over an animated background frame by frame.
    pub overlay: RgbaImage,
}

/// Fill of a split-layout webcam region before (or without) webcam frames.
pub const CAM_EMPTY: Color = hex(0x18181a);

/// Renders the static layers for `frame` at `scale` (1.0 = export): the background with
/// the screen rect's shadow (and a split webcam region's), and the corner overlay that rounds
/// both regions.
pub fn render_backdrop(style: &Style, frame: &Frame, scale: f32) -> Backdrop {
    let (cw, ch) = frame.canvas;
    let w = ((cw as f32 * scale).round() as u32).max(1);
    let h = ((ch as f32 * scale).round() as u32).max(1);
    let mut bg = fill_background(style, w, h, scale >= 1.0);
    let mut overlay = RgbaImage::new(w, h);

    let round = |r: Rect, radius: u32| {
        RoundRect::new(r.x as f32 * scale, r.y as f32 * scale, r.w as f32 * scale, r.h as f32 * scale, radius as f32 * scale)
    };
    let mut rects = vec![round(frame.screen, style.radius)];
    let split_cam = frame.cam.filter(|c| !c.floating).map(|c| round(c.rect, c.radius));
    rects.extend(split_cam);

    // Drop shadow: blurred rect coverage, slightly offset downward.
    let blur = style.shadow_blur as f32 * scale;
    if style.shadow_opacity > 0.0 {
        let offset = blur * 0.25;
        let shifted: Vec<RoundRect> = rects.iter().map(|r| RoundRect { y0: r.y0 + offset, ..*r }).collect();
        let mask: ImageBuffer<Luma<f32>, Vec<f32>> = ImageBuffer::from_fn(w, h, |x, y| {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            Luma([shifted.iter().map(|r| r.coverage(px, py)).fold(0.0, f32::max)])
        });
        let mask = if blur > 0.5 { imageops::fast_blur(&mask, blur / 2.0) } else { mask };
        for (x, y, p) in bg.enumerate_pixels_mut() {
            let a = style.shadow_opacity * mask.get_pixel(x, y)[0].clamp(0.0, 1.0);
            for c in &mut p.0 {
                *c = (*c as f32 * (1.0 - a)).round() as u8;
            }
            overlay.put_pixel(x, y, Rgba([0, 0, 0, (a * 255.0).round() as u8]));
        }
    }

    let mut corners = RgbaImage::new(w, h);
    for (i, rect) in rects.iter().enumerate() {
        let (x0, y0) = (rect.x0.floor() as u32, rect.y0.floor() as u32);
        let x1 = ((rect.x0 + rect.w).ceil() as u32).min(w);
        let y1 = ((rect.y0 + rect.h).ceil() as u32).min(h);
        for y in y0..y1 {
            for x in x0..x1 {
                let cov = rect.coverage(x as f32 + 0.5, y as f32 + 0.5);
                if cov < 1.0 {
                    let Rgb([r, g, b]) = *bg.get_pixel(x, y);
                    corners.put_pixel(x, y, Rgba([r, g, b, ((1.0 - cov) * 255.0).round() as u8]));
                }
            }
        }
        // The split webcam region is dark until webcam frames cover it.
        if i == 1 {
            for y in y0..y1 {
                for x in x0..x1 {
                    let cov = rect.coverage(x as f32 + 0.5, y as f32 + 0.5);
                    if cov > 0.0 {
                        let p = bg.get_pixel_mut(x, y);
                        for (c, e) in p.0.iter_mut().zip(CAM_EMPTY) {
                            *c = (*c as f32 * (1.0 - cov) + e as f32 * cov).round() as u8;
                        }
                        let o = overlay.get_pixel_mut(x, y);
                        let a = (o[3] as f32).max(cov * 255.0).round() as u8;
                        *o = Rgba([CAM_EMPTY[0], CAM_EMPTY[1], CAM_EMPTY[2], a]);
                    }
                }
            }
        }
    }

    let bg = image::DynamicImage::ImageRgb8(bg).to_rgba8();
    Backdrop { bg, corners, overlay }
}

/// The floating webcam's frame: drop shadow, border ring and a dark inner fill, drawn under
/// the webcam video. `offset` is where the image's top-left sits relative to the webcam
/// rect's top-left, in scaled pixels (negative: the shadow reaches outside the rect).
pub struct CamSprite {
    pub image: RgbaImage,
    pub offset: (i32, i32),
}

pub fn render_cam_frame(cam: &CamFrame, border_color: Color, shadow: bool, scale: f32) -> CamSprite {
    let d = cam.rect.w as f32 * scale;
    let blur = if shadow { (d * 0.10).max(1.0) } else { 0.0 };
    let m = (blur * 2.0).ceil() as u32;
    let size = d.ceil() as u32 + 2 * m;
    let outer = RoundRect::new(m as f32, m as f32, d, d, cam.radius as f32 * scale);
    let b = cam.border.min(cam.rect.w / 4) as f32 * scale;
    let inner = RoundRect::new(m as f32 + b, m as f32 + b, d - 2.0 * b, d - 2.0 * b, cam.inner_radius() as f32 * scale);
    let shadow_mask: Option<ImageBuffer<Luma<f32>, Vec<f32>>> = shadow.then(|| {
        let shifted = RoundRect { y0: outer.y0 + blur * 0.3, ..outer };
        let mask = ImageBuffer::from_fn(size, size, |x, y| Luma([shifted.coverage(x as f32 + 0.5, y as f32 + 0.5)]));
        imageops::fast_blur(&mask, blur / 2.0)
    });
    let image = RgbaImage::from_fn(size, size, |x, y| {
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        let mut p = [0.0f32; 4];
        if let Some(mask) = &shadow_mask {
            over(&mut p, [0.0; 3], 0.45 * mask.get_pixel(x, y)[0]);
        }
        let (co, ci) = (outer.coverage(px, py), inner.coverage(px, py));
        over(&mut p, border_color.map(|c| c as f32 / 255.0), co);
        over(&mut p, CAM_EMPTY.map(|c| c as f32 / 255.0), ci);
        Rgba([
            (p[0] * 255.0).round() as u8,
            (p[1] * 255.0).round() as u8,
            (p[2] * 255.0).round() as u8,
            (p[3] * 255.0).round() as u8,
        ])
    });
    CamSprite { image, offset: (-(m as i32), -(m as i32)) }
}

/// Alpha mask (white = visible) for the floating webcam video, the size of its inner rect.
pub fn render_cam_mask(cam: &CamFrame) -> image::GrayImage {
    let r = cam.inner();
    let rr = RoundRect::new(0.0, 0.0, r.w as f32, r.h as f32, cam.inner_radius() as f32);
    image::GrayImage::from_fn(r.w, r.h, |x, y| Luma([(rr.coverage(x as f32 + 0.5, y as f32 + 0.5) * 255.0).round() as u8]))
}

/// The background at timeline time 0 (animated backgrounds are drawn per frame elsewhere).
pub(crate) fn fill_background(style: &Style, w: u32, h: u32, full_quality: bool) -> RgbImage {
    crate::backgrounds::render::render(&style.bg, w, h, 0.0, full_quality)
}

/// Scales the image to cover `w`×`h` and crops the center.
fn cover(img: &BgImage, w: u32, h: u32, full_quality: bool) -> RgbImage {
    let need = |src: &RgbImage| (w as f32 / src.width() as f32).max(h as f32 / src.height() as f32);
    let src = if !full_quality && need(&img.small) <= 1.0 { &img.small } else { &img.full };
    let k = need(src);
    let sw = ((src.width() as f32 * k).ceil() as u32).max(w);
    let sh = ((src.height() as f32 * k).ceil() as u32).max(h);
    let filter = if full_quality { FilterType::CatmullRom } else { FilterType::Triangle };
    let scaled = imageops::resize(src, sw, sh, filter);
    imageops::crop_imm(&scaled, (sw - w) / 2, (sh - h) / 2, w, h).to_image()
}

#[derive(Clone, Copy)]
struct RoundRect {
    x0: f32,
    y0: f32,
    w: f32,
    h: f32,
    r: f32,
}

impl RoundRect {
    fn new(x0: f32, y0: f32, w: f32, h: f32, r: f32) -> Self {
        Self { x0, y0, w, h, r: r.min(w.min(h) / 2.0).max(0.0) }
    }

    /// Anti-aliased coverage (0..=1) of the pixel centered at (x, y).
    fn coverage(&self, x: f32, y: f32) -> f32 {
        let (hw, hh) = (self.w / 2.0, self.h / 2.0);
        let qx = (x - self.x0 - hw).abs() - (hw - self.r);
        let qy = (y - self.y0 - hh).abs() - (hh - self.r);
        let outside = qx.max(0.0).hypot(qy.max(0.0));
        let d = outside + qx.max(qy).min(0.0) - self.r;
        (0.5 - d).clamp(0.0, 1.0)
    }
}

// ---------- cursor ----------

/// Arrow outline in display points, tip (the hotspot) at the origin.
const ARROW: [(f32, f32); 7] =
    [(0.0, 0.0), (0.0, 16.5), (4.0, 12.8), (6.8, 19.2), (9.5, 18.0), (6.8, 11.9), (11.9, 11.9)];
const ARROW_MARGIN: f32 = 3.0;
const ARROW_OUTLINE: f32 = 1.3;

pub struct CursorSprite {
    pub image: RgbaImage,
    /// Pixel offset of the hotspot inside `image`.
    pub hotspot: f32,
}

/// Renders the arrow at `px_per_pt` image pixels per display point.
pub fn render_cursor(px_per_pt: f32) -> CursorSprite {
    let s = px_per_pt.max(0.05);
    let w = ((12.0 + 2.0 * ARROW_MARGIN) * s).ceil() as u32;
    let h = ((19.5 + 2.0 * ARROW_MARGIN) * s).ceil() as u32;
    let image = RgbaImage::from_fn(w, h, |x, y| {
        let px = (x as f32 + 0.5) / s - ARROW_MARGIN;
        let py = (y as f32 + 0.5) / s - ARROW_MARGIN;
        let d = polygon_sdf(px, py);
        // Soft shadow under the outline.
        let ds = polygon_sdf(px, py - 0.7) - ARROW_OUTLINE;
        let mut out = [0.0f32; 4];
        over(&mut out, [0.0; 3], 0.28 * (1.0 - ds / 1.8).clamp(0.0, 1.0));
        over(&mut out, [1.0; 3], (0.5 - (d - ARROW_OUTLINE) * s).clamp(0.0, 1.0));
        over(&mut out, [0.0; 3], (0.5 - d * s).clamp(0.0, 1.0));
        Rgba(out.map(|c| (c * 255.0).round() as u8))
    });
    CursorSprite { image, hotspot: ARROW_MARGIN * s }
}

/// Straight-alpha "over" compositing of a flat color onto `dst`.
fn over(dst: &mut [f32; 4], c: [f32; 3], a: f32) {
    let out_a = a + dst[3] * (1.0 - a);
    if out_a > 0.0 {
        for i in 0..3 {
            dst[i] = (c[i] * a + dst[i] * dst[3] * (1.0 - a)) / out_a;
        }
    }
    dst[3] = out_a;
}

/// Signed distance to the arrow polygon (negative inside), in points.
fn polygon_sdf(x: f32, y: f32) -> f32 {
    let v = &ARROW;
    let mut d = f32::MAX;
    let mut inside = false;
    let mut j = v.len() - 1;
    for i in 0..v.len() {
        let (ax, ay) = v[i];
        let (bx, by) = v[j];
        let (ex, ey) = (bx - ax, by - ay);
        let (wx, wy) = (x - ax, y - ay);
        let t = ((wx * ex + wy * ey) / (ex * ex + ey * ey)).clamp(0.0, 1.0);
        d = d.min((wx - ex * t).hypot(wy - ey * t));
        if (ay > y) != (by > y) && x < ax + (y - ay) * (bx - ax) / (by - ay) {
            inside = !inside;
        }
        j = i;
    }
    if inside { -d } else { d }
}

/// Where the cursor sprite goes on the canvas (output pixels), or `None` when the hotspot is
/// outside the camera crop. `crop` is (x, y, w, h) in video pixels; `screen` is the screen
/// content rect on the canvas.
pub fn cursor_position(cursor: (f64, f64), crop: (u32, u32, u32, u32), screen: Rect) -> Option<(f64, f64)> {
    let inner = (screen.w as f64, screen.h as f64);
    let (cx, cy) = cursor;
    let (x, y, w, h) = (crop.0 as f64, crop.1 as f64, crop.2 as f64, crop.3 as f64);
    if cx < x || cy < y || cx >= x + w || cy >= y + h {
        return None;
    }
    Some((screen.x as f64 + (cx - x) * inner.0 / w, screen.y as f64 + (cy - y) * inner.1 / h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inner_rect_and_layers() {
        let style = Style::default();
        let canvas = (1080, 1920);
        let frame = Layout::default().frame(&style, false);
        assert_eq!((frame.screen.w, frame.screen.h), (984, 1824));
        let b = render_backdrop(&style, &frame, 1.0);
        assert_eq!(b.bg.dimensions(), canvas);
        // Inner rect center: no corner overlay. Exact corner: fully backdrop.
        assert_eq!(b.corners.get_pixel(540, 960)[3], 0);
        assert_eq!(b.corners.get_pixel(48, 48)[3], 255);
        assert_eq!(b.corners.get_pixel(10, 10)[3], 0);
        // Shadow darkens just below the rect relative to the far top.
        let lum = |p: &Rgba<u8>| p[0] as u32 + p[1] as u32 + p[2] as u32;
        let solid = Style { bg: crate::backgrounds::Background::Solid { color: hex(0xf2f2f4), grain: 0.0 }, ..Style::default() };
        let b = render_backdrop(&solid, &frame, 1.0);
        assert!(lum(b.bg.get_pixel(540, 1900)) < lum(b.bg.get_pixel(540, 2)));
    }

    #[test]
    fn image_background_covers() {
        // Wide red|blue image: cover-fit on a tall canvas keeps the center seam, crops the sides.
        let path = std::env::temp_dir().join(format!("clipper-bg-{}.png", std::process::id()));
        RgbImage::from_fn(400, 100, |x, _| if x < 200 { Rgb([255, 0, 0]) } else { Rgb([0, 0, 255]) })
            .save(&path)
            .unwrap();
        let src = crate::backgrounds::ImageSource::File(path.clone());
        let style = Style { bg: crate::backgrounds::Background::Image(crate::backgrounds::ImageBg::new(src)), shadow_opacity: 0.0, ..Style::default() };
        let b = render_backdrop(&style, &Layout::default().frame(&style, false), 1.0);
        assert_eq!(b.bg.get_pixel(5, 5)[0], 255);
        assert_eq!(b.bg.get_pixel(1075, 5)[2], 255);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn gradients_and_wallpapers_render() {
        // "Night fade" (a removed preset, from the legacy table) runs bottom→top: pale pink at
        // the bottom, deep blue at the top.
        let nf = crate::backgrounds::render::gradient(&crate::backgrounds::presets::legacy("app:1").unwrap().1, 10, 100);
        assert!(nf.get_pixel(5, 99)[0] > 200 && nf.get_pixel(5, 0)[2] > nf.get_pixel(5, 0)[0]);
        for (i, wp) in WALLPAPERS.iter().enumerate() {
            let img = render_wallpaper(i, 27, 48);
            assert_eq!(img.dimensions(), (27, 48), "{}", wp.name);
            // Real pictures, not a flat fill.
            let (a, b) = (img.get_pixel(2, 2).0, img.get_pixel(24, 45).0);
            let flat = img.pixels().all(|p| p.0 == a) && a == b;
            assert!(!flat, "{} is a flat color", wp.name);
        }
        // A built-in image background renders the picture.
        let src = crate::backgrounds::ImageSource::Builtin(6);
        let s = Style { bg: crate::backgrounds::Background::Image(crate::backgrounds::ImageBg::new(src)), ..Style::default() };
        let b = render_backdrop(&s, &Layout::default().frame(&s, false), 0.1);
        assert_ne!(b.bg.get_pixel(2, 2).0[..3], [0xf2, 0xf2, 0xf4]);
    }

    /// A split layout rounds and shades the webcam region too; a floating webcam gets its own
    /// sprite with a border ring and a mask that's clear outside the circle.
    #[test]
    fn webcam_layers() {
        use crate::layout::{Corner, Placement, Shape, Side};
        let style = Style { bg: crate::backgrounds::Background::Solid { color: hex(0xf2f2f4), grain: 0.0 }, shadow_opacity: 0.0, ..Style::default() };
        let split = Layout { placement: Placement::Split(Side::Top), ..Layout::default() }.frame(&style, true);
        let b = render_backdrop(&style, &split, 1.0);
        let cam = split.cam.unwrap().rect;
        assert_eq!(b.bg.get_pixel(cam.x + cam.w / 2, cam.y + cam.h / 2).0[..3], CAM_EMPTY);
        assert_eq!(b.corners.get_pixel(cam.x, cam.y)[3], 255);
        assert_eq!(b.corners.get_pixel(cam.x + cam.w / 2, cam.y + cam.h / 2)[3], 0);

        let float = Layout { placement: Placement::Float(Shape::Circle, Corner::TopLeft), ..Layout::default() }
            .frame(&style, true)
            .cam
            .unwrap();
        let mask = render_cam_mask(&float);
        assert_eq!(mask.dimensions(), (float.inner().w, float.inner().h));
        assert_eq!(mask.get_pixel(0, 0)[0], 0);
        assert_eq!(mask.get_pixel(mask.width() / 2, mask.height() / 2)[0], 255);
        let sprite = render_cam_frame(&float, [255, 255, 255], true, 1.0);
        let (ox, oy) = (-sprite.offset.0 as u32, -sprite.offset.1 as u32);
        // Border ring at the top of the circle, dark fill inside.
        let top = sprite.image.get_pixel(ox + float.rect.w / 2, oy + 1);
        assert!(top[0] > 200 && top[3] > 200, "{top:?}");
        assert_eq!(sprite.image.get_pixel(ox + float.rect.w / 2, oy + float.rect.h / 2).0[..3], CAM_EMPTY);
    }

    #[test]
    fn cursor_sprite() {
        let c = render_cursor(2.0);
        let hs = c.hotspot as u32;
        // Just inside the tip is opaque black; far corner is transparent.
        let p = c.image.get_pixel(hs + 1, hs + 4);
        assert!(p[3] > 200 && p[0] < 40);
        assert_eq!(c.image.get_pixel(c.image.width() - 1, 0)[3], 0);
    }
}

