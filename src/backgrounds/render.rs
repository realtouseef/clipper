//! The background renderer: (background, size, time) in, pixels out. Deterministic: the same
//! inputs give the same pixels. Animated backgrounds depend only on timeline time `t` (never
//! the wall clock) and loop every `period` seconds (all motion is sin/cos of whole multiples
//! of 2π·t/period). They are rendered small (`low_size`) and upscaled; grain goes on after.

use super::color::{self, Lab, Rgb};
use super::fit;
use super::{AnimStyle, Animated, Background, Fit, Gradient, GradientKind, ImageBg, ImageSource};
use image::imageops::{self, FilterType};
use image::{Rgb as Px, RgbImage, Rgba, RgbaImage};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

/// Short side of the low-resolution animated frame.
pub const LOW_SHORT: u32 = 256;

/// The full background at `w`×`h` for timeline time `t`. `full` = export quality (original
/// images, high-quality scaling); otherwise previews use image proxies.
pub fn render(bg: &Background, w: u32, h: u32, t: f64, full: bool) -> RgbImage {
    let (w, h) = (w.max(1), h.max(1));
    match bg {
        Background::Solid { color, grain } => {
            let mut img = RgbImage::from_pixel(w, h, Px(*color));
            apply_grain(&mut img, *grain);
            img
        }
        Background::Gradient(g) => {
            let mut img = gradient(g, w, h);
            apply_grain(&mut img, g.grain);
            img
        }
        Background::Animated(a) => {
            let (lw, lh) = low_size(w, h);
            let low = animated_frame(a, lw, lh, t);
            let mut img = imageops::resize(&low, w, h, FilterType::Triangle);
            apply_grain(&mut img, a.grain);
            img
        }
        Background::Image(ib) => image_bg(ib, w, h, full),
    }
}

/// Size of the low-resolution animated frame for a `w`×`h` output (short side 256).
pub fn low_size(w: u32, h: u32) -> (u32, u32) {
    let k = LOW_SHORT as f32 / w.min(h).max(1) as f32;
    if k >= 1.0 {
        return (w.max(1), h.max(1));
    }
    (((w as f32 * k).round() as u32).max(2), ((h as f32 * k).round() as u32).max(2))
}

// ---------- gradients ----------

/// Gradient position 0..=1 for pixel centers, per kind (all but mesh).
fn position(kind: &GradientKind, w: u32, h: u32) -> impl Fn(f32, f32) -> f32 + '_ {
    let (wf, hf) = (w as f32, h as f32);
    move |x: f32, y: f32| match kind {
        GradientKind::Linear { angle } => {
            let (s, c) = angle.to_radians().sin_cos();
            let (dx, dy) = (s, -c);
            let len = (wf * s).abs() + (hf * c).abs();
            ((x - wf / 2.0) * dx + (y - hf / 2.0) * dy) / len.max(1e-6) + 0.5
        }
        GradientKind::Radial { cx, cy } => {
            let (px0, py0) = (cx * wf, cy * hf);
            let r = [(0.0, 0.0), (wf, 0.0), (0.0, hf), (wf, hf)]
                .iter()
                .map(|(qx, qy)| ((qx - px0).powi(2) + (qy - py0).powi(2)).sqrt())
                .fold(0.0, f32::max);
            ((x - px0).powi(2) + (y - py0).powi(2)).sqrt() / r.max(1e-6)
        }
        GradientKind::Conic { cx, cy, angle } => {
            let a = (x - cx * wf).atan2(-(y - cy * hf)).to_degrees();
            ((a - angle) / 360.0).rem_euclid(1.0)
        }
        GradientKind::Mesh { .. } => 0.0,
    }
}

pub fn gradient(g: &Gradient, w: u32, h: u32) -> RgbImage {
    if let GradientKind::Mesh { cols, rows, colors } = &g.kind {
        return mesh(*cols as usize, *rows as usize, colors, w, h);
    }
    let lut = color::lut(&g.stops.iter().map(|s| (s.pos, s.color)).collect::<Vec<_>>());
    let pos = position(&g.kind, w, h);
    RgbImage::from_fn(w, h, |x, y| {
        let t = pos(x as f32 + 0.5, y as f32 + 0.5).clamp(0.0, 1.0);
        Px(lut[(t * 255.0).round() as usize])
    })
}

fn smooth(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// A grid of colors blended bilinearly (smoothstepped) in OKLab.
fn mesh(cols: usize, rows: usize, colors: &[Rgb], w: u32, h: u32) -> RgbImage {
    let (cols, rows) = (cols.max(2), rows.max(2));
    let labs: Vec<Lab> = (0..cols * rows).map(|i| color::to_lab(colors.get(i).copied().unwrap_or([0, 0, 0]))).collect();
    RgbImage::from_fn(w, h, |x, y| {
        let u = (x as f32 + 0.5) / w as f32 * (cols - 1) as f32;
        let v = (y as f32 + 0.5) / h as f32 * (rows - 1) as f32;
        let (i, j) = ((u as usize).min(cols - 2), (v as usize).min(rows - 2));
        let (fu, fv) = (smooth(u - i as f32), smooth(v - j as f32));
        let at = |c: usize, r: usize| labs[r * cols + c];
        let top = color::mix(at(i, j), at(i + 1, j), fu);
        let bottom = color::mix(at(i, j + 1), at(i + 1, j + 1), fu);
        Px(color::to_rgb(color::mix(top, bottom, fv)))
    })
}

// ---------- animated ----------

/// The animated background's low-resolution frame at timeline time `t`.
pub fn animated_frame(a: &Animated, w: u32, h: u32, t: f64) -> RgbImage {
    let period = a.period.clamp(4.0, 20.0) as f64;
    let t = if a.frozen { 0.0 } else { t };
    let phase = (std::f64::consts::TAU * t / period) as f32;
    let k = a.speed.round().clamp(1.0, 4.0);
    let colors: Vec<Rgb> = if a.colors.is_empty() { vec![[0, 0, 0], [80, 80, 80]] } else { a.colors.clone() };
    let even: Vec<(f32, Rgb)> = {
        let n = colors.len().max(2) - 1;
        colors.iter().enumerate().map(|(i, &c)| (i as f32 / n as f32, c)).collect()
    };
    let (wf, hf) = (w as f32, h as f32);
    let intensity = a.intensity.clamp(0.0, 1.0);
    match a.style {
        AnimStyle::Aurora => {
            let base = color::to_lab(colors[0]);
            let blobs = (3.0 + (intensity * 3.0).round()) as usize;
            let r = (0.25 + 0.35 * a.blur.clamp(0.0, 1.0)) * wf.max(hf);
            let alpha = 0.45 + 0.5 * intensity;
            // Gaussians are separable: per-blob row and column factors.
            let layers: Vec<(Lab, Vec<f32>, Vec<f32>)> = (0..blobs)
                .map(|i| {
                    let col = if colors.len() > 1 { colors[1 + i % (colors.len() - 1)] } else { colors[0] };
                    let fi = i as f32;
                    let mx = k * (1 + i % 2) as f32;
                    let my = k * (1 + (i + 1) % 2) as f32;
                    let cx = (0.5 + 0.34 * (mx * phase + fi * 1.7).sin()) * wf;
                    let cy = (0.5 + 0.34 * (my * phase + fi * 2.3).cos()) * hf;
                    let ex = (0..w).map(|x| (-((x as f32 + 0.5 - cx) / r).powi(2)).exp()).collect();
                    let ey = (0..h).map(|y| (-((y as f32 + 0.5 - cy) / r).powi(2)).exp()).collect();
                    (color::to_lab(col), ex, ey)
                })
                .collect();
            RgbImage::from_fn(w, h, |x, y| {
                let mut c = base;
                for (col, ex, ey) in &layers {
                    c = color::mix(c, *col, (alpha * ex[x as usize] * ey[y as usize]).min(1.0));
                }
                Px(color::to_rgb(c))
            })
        }
        AnimStyle::Rotate => {
            let angle = 135.0 + 360.0 * k * (t / period).fract() as f32;
            gradient(&Gradient { kind: GradientKind::Linear { angle }, stops: stops(&even), grain: 0.0 }, w, h)
        }
        AnimStyle::Pulse => {
            let lut = color::lut(&even);
            let s = 1.0 + 0.3 * intensity * (k * phase).sin();
            let r = (wf * wf + hf * hf).sqrt() / 2.0 * s;
            RgbImage::from_fn(w, h, |x, y| {
                let d = ((x as f32 + 0.5 - wf / 2.0).powi(2) + (y as f32 + 0.5 - hf / 2.0).powi(2)).sqrt();
                Px(lut[((d / r).clamp(0.0, 1.0) * 255.0).round() as usize])
            })
        }
        AnimStyle::Wave => {
            let lut = color::lut(&even);
            let (s, c) = 120f32.to_radians().sin_cos();
            let (dx, dy) = (s, -c);
            let len = (wf * s).abs() + (hf * c).abs();
            let amp = 0.04 + 0.12 * intensity;
            RgbImage::from_fn(w, h, |x, y| {
                let (px0, py0) = (x as f32 + 0.5 - wf / 2.0, y as f32 + 0.5 - hf / 2.0);
                let u = (px0 * dx + py0 * dy) / len + 0.5;
                let v = (-px0 * dy + py0 * dx) / len + 0.5;
                let off = amp * (std::f32::consts::TAU * 1.2 * v + k * phase).sin();
                Px(lut[((u + off).clamp(0.0, 1.0) * 255.0).round() as usize])
            })
        }
    }
}

fn stops(even: &[(f32, Rgb)]) -> Vec<super::Stop> {
    even.iter().map(|&(pos, color)| super::Stop { pos, color }).collect()
}

// ---------- grain ----------

/// Deterministic per-pixel noise in -1..=1.
fn noise(x: u32, y: u32) -> f32 {
    let mut h = x.wrapping_mul(0x8da6_b343) ^ y.wrapping_mul(0xd816_3841) ^ 0x9e37_79b9;
    h ^= h >> 13;
    h = h.wrapping_mul(0x5bd1_e995);
    h ^= h >> 15;
    (h & 0xffff) as f32 / 32767.5 - 1.0
}

fn amplitude(strength: f32) -> f32 {
    strength.clamp(0.0, 100.0) / 100.0 * 14.0
}

pub fn apply_grain(img: &mut RgbImage, strength: f32) {
    let a = amplitude(strength);
    if a <= 0.0 {
        return;
    }
    for (x, y, p) in img.enumerate_pixels_mut() {
        let n = noise(x, y) * a;
        for c in &mut p.0 {
            *c = (*c as f32 + n).round().clamp(0.0, 255.0) as u8;
        }
    }
}

/// The same grain as a layer to draw over an upscaled frame (white or black with alpha).
pub fn grain_layer(w: u32, h: u32, strength: f32) -> RgbaImage {
    let a = amplitude(strength);
    RgbaImage::from_fn(w, h, |x, y| {
        let n = noise(x, y);
        let v = if n >= 0.0 { 255 } else { 0 };
        Rgba([v, v, v, (n.abs() * a).round().min(255.0) as u8])
    })
}

pub fn grain_of(bg: &Background) -> f32 {
    match bg {
        Background::Solid { grain, .. } => *grain,
        Background::Gradient(g) => g.grain,
        Background::Animated(a) => a.grain,
        Background::Image(i) => i.grain,
    }
}

// ---------- images ----------

type ImageCache = Mutex<HashMap<(PathBuf, bool), Option<Arc<RgbImage>>>>;

fn cache() -> &'static ImageCache {
    static C: OnceLock<ImageCache> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The image for a source: the original (`full`) or the preview proxy. `None` if missing.
pub fn load_image(src: &ImageSource, full: bool) -> Option<Arc<RgbImage>> {
    let path = match src {
        ImageSource::Builtin(i) => {
            return Some(Arc::new(crate::style::wallpaper_image(*i).pixels(full).clone()));
        }
        ImageSource::Library(id) => {
            let lib = super::library::global().lock().unwrap();
            if full { lib.original(id)? } else { lib.proxy(id) }
        }
        ImageSource::File(p) => p.clone(),
    };
    let mut c = cache().lock().unwrap();
    if c.len() > 24 {
        c.clear();
    }
    c.entry((path.clone(), full)).or_insert_with(|| image::open(&path).ok().map(|i| Arc::new(i.to_rgb8()))).clone()
}

/// Pixel size of the original image (for the low-resolution check).
pub fn original_size(src: &ImageSource) -> Option<(u32, u32)> {
    match src {
        ImageSource::Builtin(i) => Some(crate::style::wallpaper_image(*i).pixels(true).dimensions()),
        ImageSource::Library(id) => super::library::global().lock().unwrap().get(id).map(|e| (e.width, e.height)),
        ImageSource::File(p) => image::image_dimensions(p).ok(),
    }
}

/// Fit modes, adjustments and grain for an image background.
pub fn image_bg(ib: &ImageBg, w: u32, h: u32, full: bool) -> RgbImage {
    let Some(src) = load_image(&ib.source, full) else {
        // Missing file: a neutral placeholder until it's relinked.
        return RgbImage::from_fn(w, h, |x, y| if (x / 24 + y / 24) % 2 == 0 { Px([42, 42, 46]) } else { Px([36, 36, 40]) });
    };
    let filter = if full { FilterType::CatmullRom } else { FilterType::Triangle };
    let short = w.min(h) as f32;
    let cover = |focal, zoom, pan| {
        let (x, y, cw, ch) = fit::cover_crop(src.dimensions(), (w, h), focal, zoom, pan);
        let part = imageops::crop_imm(&*src, x.round() as u32, y.round() as u32, (cw.round() as u32).max(1), (ch.round() as u32).max(1)).to_image();
        imageops::resize(&part, w, h, filter)
    };
    let mut img = match ib.fit {
        Fit::Cover => cover(ib.focal, ib.zoom, ib.pan),
        Fit::Contain => {
            // A blurred, slightly darker cover fills the empty sides.
            let mut fill = imageops::fast_blur(&cover((0.5, 0.5), 1.0, (0.0, 0.0)), short * 0.05);
            for p in fill.pixels_mut() {
                for c in &mut p.0 {
                    *c = (*c as f32 * 0.8) as u8;
                }
            }
            let (x, y, cw, ch) = fit::contain_rect(src.dimensions(), (w, h));
            let fg = imageops::resize(&*src, (cw.round() as u32).max(1), (ch.round() as u32).max(1), filter);
            imageops::overlay(&mut fill, &fg, x.round() as i64, y.round() as i64);
            fill
        }
    };
    if ib.blur > 0.0 {
        img = imageops::fast_blur(&img, ib.blur.clamp(0.0, 100.0) / 100.0 * short * 0.03);
    }
    if ib.darken > 0.0 {
        let k = 1.0 - ib.darken.clamp(0.0, 0.8);
        for p in img.pixels_mut() {
            for c in &mut p.0 {
                *c = (*c as f32 * k).round() as u8;
            }
        }
    }
    apply_grain(&mut img, ib.grain);
    img
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use super::*;

    fn diff(a: &RgbImage, b: &RgbImage) -> u8 {
        a.pixels().zip(b.pixels()).flat_map(|(p, q)| (0..3).map(move |k| (p[k] as i16 - q[k] as i16).unsigned_abs() as u8)).max().unwrap_or(0)
    }

    #[test]
    fn animated_loops_seamlessly_and_is_deterministic() {
        for preset in presets::animated() {
            let a = &preset.anim;
            let (w, h) = low_size(1080, 1920);
            let start = animated_frame(a, w, h, 0.0);
            let end = animated_frame(a, w, h, a.period as f64);
            assert!(diff(&start, &end) <= 1, "{}: frame at T differs from frame at 0", preset.name);
            // Same t, same pixels.
            assert_eq!(animated_frame(a, w, h, 2.5), animated_frame(a, w, h, 2.5));
            // It actually moves.
            assert!(diff(&start, &animated_frame(a, w, h, a.period as f64 / 4.0)) > 3, "{} doesn't move", preset.name);
            // t and t + T are the same frame (loop at any time).
            assert!(diff(&animated_frame(a, w, h, 1.3), &animated_frame(a, w, h, 1.3 + a.period as f64)) <= 1);
        }
        // Frozen holds the first frame.
        let mut a = Animated::default();
        a.frozen = true;
        assert_eq!(animated_frame(&a, 64, 64, 0.0), animated_frame(&a, 64, 64, 3.0));
    }

    #[test]
    fn full_render_is_deterministic_and_sized() {
        let bg = Background::Animated(Animated::default());
        let a = render(&bg, 360, 640, 1.0, false);
        assert_eq!(a.dimensions(), (360, 640));
        assert_eq!(a, render(&bg, 360, 640, 1.0, false));
        assert_eq!(low_size(1080, 1920), (256, 455));
        assert_eq!(low_size(100, 50), (100, 50), "never upscaled");
    }

    #[test]
    fn gradient_kinds_and_oklab() {
        let g = Gradient::linear(90.0, &[[255, 0, 0], [0, 0, 255]]);
        let img = gradient(&g, 256, 8);
        // Left to right: red at the left edge, blue at the right.
        assert!(img.get_pixel(0, 4)[0] > 240 && img.get_pixel(255, 4)[2] > 240);
        let radial = Gradient { kind: GradientKind::Radial { cx: 0.5, cy: 0.5 }, ..g.clone() };
        let img = gradient(&radial, 101, 101);
        assert!(img.get_pixel(50, 50)[0] > 240, "center is the first stop");
        let conic = Gradient { kind: GradientKind::Conic { cx: 0.5, cy: 0.5, angle: 0.0 }, ..g.clone() };
        let img = gradient(&conic, 101, 101);
        assert_ne!(img.get_pixel(60, 10), img.get_pixel(40, 10), "a seam at the start angle");
        let mesh = Gradient { kind: GradientKind::Mesh { cols: 2, rows: 2, colors: vec![[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 255]] }, ..g };
        let img = gradient(&mesh, 64, 64);
        assert!(img.get_pixel(0, 0)[0] > 230 && img.get_pixel(63, 63)[2] > 230);
        // Grain is deterministic and bounded.
        let mut a = RgbImage::from_pixel(32, 32, Px([128, 128, 128]));
        let mut b = a.clone();
        apply_grain(&mut a, 50.0);
        apply_grain(&mut b, 50.0);
        assert_eq!(a, b);
        assert!(a.pixels().all(|p| (p[0] as i16 - 128).abs() <= 8));
        assert!(a.pixels().any(|p| p[0] != 128));
    }

    #[test]
    fn images_never_distort() {
        let dir = std::env::temp_dir().join(format!("clipper-bgimg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A 400×200 image: left half red, right half blue, a green dot at (300, 100).
        let mut src = RgbImage::from_fn(400, 200, |x, _| if x < 200 { Px([255, 0, 0]) } else { Px([0, 0, 255]) });
        for y in 95..105 {
            for x in 295..305 {
                src.put_pixel(x, y, Px([0, 255, 0]));
            }
        }
        let p = dir.join("i.png");
        src.save(&p).unwrap();
        let mut ib = ImageBg::new(ImageSource::File(p.clone()));
        // Cover into a square, focal on the green dot: it is centered.
        ib.focal = (0.75, 0.5);
        let img = image_bg(&ib, 100, 100, true);
        assert!(img.get_pixel(50, 50)[1] > 200, "focal point in the middle");
        // Contain into a tall output: the image keeps 2:1, centered; above it is the blurred fill.
        ib.fit = Fit::Contain;
        let img = image_bg(&ib, 100, 200, true);
        assert!(img.get_pixel(10, 100)[0] > 200 && img.get_pixel(90, 100)[2] > 200);
        // Darken and missing files.
        ib.fit = Fit::Cover;
        ib.darken = 0.5;
        let dark = image_bg(&ib, 100, 100, true);
        assert!(dark.get_pixel(10, 10)[2] < 140);
        let missing = image_bg(&ImageBg::new(ImageSource::File(dir.join("gone.png"))), 50, 50, true);
        assert_eq!(missing.dimensions(), (50, 50));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
