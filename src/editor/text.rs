//! Text and emoji rasterization through cosmic-text (shaping, font fallback) and swash.
//! One shared font system holds the system fonts plus the bundled Poppins and Noto Color
//! Emoji, so emoji in text and emoji objects go through the same pipeline.

use super::doc::{Align, Rgba, Text};
use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, SwashCache, SwashContent, Weight};
use image::{Rgba as Px, RgbaImage};
use std::sync::{Mutex, OnceLock};

pub const DEFAULT_FAMILY: &str = "Poppins";
pub const EMOJI_FAMILY: &str = "Noto Color Emoji";

const BUNDLED: [&[u8]; 5] = [
    include_bytes!("../../assets/fonts/Poppins-Regular.ttf"),
    include_bytes!("../../assets/fonts/Poppins-Medium.ttf"),
    include_bytes!("../../assets/fonts/Poppins-SemiBold.ttf"),
    include_bytes!("../../assets/fonts/Poppins-Bold.ttf"),
    include_bytes!("../../assets/fonts/NotoColorEmoji.ttf"),
];

struct Fonts {
    system: FontSystem,
    cache: SwashCache,
}

fn fonts() -> &'static Mutex<Fonts> {
    static FONTS: OnceLock<Mutex<Fonts>> = OnceLock::new();
    FONTS.get_or_init(|| {
        let mut db = cosmic_text::fontdb::Database::new();
        db.load_system_fonts();
        for data in BUNDLED {
            db.load_font_data(data.to_vec());
        }
        let system = FontSystem::new_with_locale_and_db("en-US".into(), db);
        Mutex::new(Fonts { system, cache: SwashCache::new() })
    })
}

/// Loads the fonts ahead of first use (it takes a moment).
pub fn warm_up() {
    std::thread::spawn(|| {
        let _ = fonts();
    });
}

/// Font families for the picker: the bundled default first, then the system fonts.
pub fn families() -> Vec<String> {
    let f = fonts().lock().unwrap();
    let mut names: Vec<String> = f
        .system
        .db()
        .faces()
        .filter_map(|face| face.families.first().map(|(n, _)| n.clone()))
        .filter(|n| !n.starts_with('.') && n != DEFAULT_FAMILY && n != EMOJI_FAMILY)
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    names.dedup();
    names.insert(0, DEFAULT_FAMILY.to_string());
    names
}

/// Laid-out lines of text rasterized at `size` px: coverage/color per pixel.
pub struct Raster {
    pub image: RgbaImage,
}

/// Rasterizes `text` at `size` px in `color` (color glyphs keep their own colors), lines
/// aligned within the widest one. Returns `None` for empty text.
pub fn rasterize(text: &str, family: &str, size: f32, weight: u16, color: Rgba, align: Align) -> Option<Raster> {
    if text.is_empty() || size < 1.0 {
        return None;
    }
    let mut guard = fonts().lock().unwrap();
    let Fonts { system, cache } = &mut *guard;
    let line_height = (size * 1.25).ceil();
    let mut buffer = Buffer::new(system, Metrics::new(size, line_height));
    buffer.set_size(None, None);
    let family = if family.is_empty() { DEFAULT_FAMILY } else { family };
    let attrs = Attrs::new().family(Family::Name(family)).weight(Weight(weight));
    buffer.set_text(text, &attrs, Shaping::Advanced, None);
    buffer.shape_until_scroll(system, false);

    let runs: Vec<_> = buffer.layout_runs().map(|run| (run.line_w, run.line_y, run.glyphs.to_vec())).collect();
    let lines = text.split('\n').count().max(runs.len());
    let width = runs.iter().map(|r| r.0).fold(0.0, f32::max).ceil().max(1.0);
    let height = (lines as f32 * line_height).ceil().max(1.0);
    let mut img = RgbaImage::new(width as u32, height as u32);
    for (line_w, line_y, glyphs) in runs {
        let shift = match align {
            Align::Left => 0.0,
            Align::Center => (width - line_w) / 2.0,
            Align::Right => width - line_w,
        };
        for g in glyphs {
            let phys = g.physical((shift, line_y), 1.0);
            let Some(image) = cache.get_image(system, phys.cache_key).as_ref() else { continue };
            let (gx, gy) = (phys.x + image.placement.left, phys.y - image.placement.top);
            let (gw, gh) = (image.placement.width as i32, image.placement.height as i32);
            for yy in 0..gh {
                for xx in 0..gw {
                    let (x, y) = (gx + xx, gy + yy);
                    if x < 0 || y < 0 || x >= img.width() as i32 || y >= img.height() as i32 {
                        continue;
                    }
                    let i = (yy * gw + xx) as usize;
                    let src = match image.content {
                        SwashContent::Mask => {
                            let a = image.data[i];
                            [color[0], color[1], color[2], ((a as u16 * color[3] as u16) / 255) as u8]
                        }
                        SwashContent::Color => {
                            let d = &image.data[i * 4..i * 4 + 4];
                            [d[0], d[1], d[2], d[3]]
                        }
                        SwashContent::SubpixelMask => {
                            let a = image.data[i * 4];
                            [color[0], color[1], color[2], a]
                        }
                    };
                    blend(img.get_pixel_mut(x as u32, y as u32), src);
                }
            }
        }
    }
    Some(Raster { image: img })
}

/// Source-over of straight-alpha `src` onto straight-alpha `dst`.
pub fn blend(dst: &mut Px<u8>, src: [u8; 4]) {
    let sa = src[3] as f32 / 255.0;
    if sa <= 0.0 {
        return;
    }
    let da = dst[3] as f32 / 255.0;
    let oa = sa + da * (1.0 - sa);
    for c in 0..3 {
        let v = (src[c] as f32 * sa + dst[c] as f32 * da * (1.0 - sa)) / oa;
        dst[c] = v.round() as u8;
    }
    dst[3] = (oa * 255.0).round() as u8;
}

/// Padding around a text object's glyphs (room for the pill, outline and shadow).
pub fn padding(t: &Text) -> f32 {
    if t.pill.is_some() { t.size * 0.45 } else { t.size * 0.15 }
}

/// The finished text object at `scale`: pill, shadow, outline and glyphs, sized to the
/// object's laid-out rect × scale.
pub fn render_text(t: &Text, scale: f32) -> Option<RgbaImage> {
    let size = t.size * scale;
    let glyphs = rasterize(&t.text, &t.font, size, t.weight, t.color, t.align)?;
    let pad = padding(t) * scale;
    let (w, h) = ((glyphs.image.width() as f32 + 2.0 * pad).ceil() as u32, (glyphs.image.height() as f32 + 2.0 * pad).ceil() as u32);
    let mut out = RgbaImage::new(w, h);
    if let Some(pill) = t.pill {
        let radius = (h as f32 / 2.0).min(size * 0.6);
        super::render::fill_round_rect_image(&mut out, radius, pill);
    }
    let (ox, oy) = (pad.round() as i64, pad.round() as i64);
    let alpha_of = |img: &RgbaImage| img.pixels().map(|p| p[3]).collect::<Vec<_>>();
    if t.shadow {
        let blur = (size * 0.08).max(1.0);
        let mut sh = image::GrayImage::new(w, h);
        let a = alpha_of(&glyphs.image);
        let off = (size * 0.06).round() as i64;
        for (i, v) in a.iter().enumerate() {
            let (x, y) = ((i as u32 % glyphs.image.width()) as i64 + ox + off, (i as u32 / glyphs.image.width()) as i64 + oy + off);
            if x >= 0 && y >= 0 && (x as u32) < w && (y as u32) < h {
                sh.put_pixel(x as u32, y as u32, image::Luma([*v]));
            }
        }
        let sh = image::imageops::blur(&sh, blur);
        for (x, y, p) in sh.enumerate_pixels() {
            blend(out.get_pixel_mut(x, y), [0, 0, 0, (p[0] as f32 * 0.6) as u8]);
        }
    }
    if let Some(oc) = t.outline {
        let r = (size * 0.06).max(1.0).round() as i64;
        let a = alpha_of(&glyphs.image);
        let gw = glyphs.image.width() as i64;
        let gh = glyphs.image.height() as i64;
        // Dilate the glyph coverage by `r` (disc).
        let mut ring = vec![0u8; (w * h) as usize];
        for gy in 0..gh {
            for gx in 0..gw {
                let v = a[(gy * gw + gx) as usize];
                if v == 0 {
                    continue;
                }
                for dy in -r..=r {
                    for dx in -r..=r {
                        if dx * dx + dy * dy > r * r {
                            continue;
                        }
                        let (x, y) = (gx + ox + dx, gy + oy + dy);
                        if x >= 0 && y >= 0 && x < w as i64 && y < h as i64 {
                            let k = (y * w as i64 + x) as usize;
                            ring[k] = ring[k].max(v);
                        }
                    }
                }
            }
        }
        for (k, v) in ring.iter().enumerate() {
            if *v > 0 {
                let (x, y) = (k as u32 % w, k as u32 / w);
                blend(out.get_pixel_mut(x, y), [oc[0], oc[1], oc[2], ((*v as u16 * oc[3] as u16) / 255) as u8]);
            }
        }
    }
    for (x, y, p) in glyphs.image.enumerate_pixels() {
        let (dx, dy) = (x as i64 + ox, y as i64 + oy);
        if (dx as u32) < w && (dy as u32) < h {
            blend(out.get_pixel_mut(dx as u32, dy as u32), p.0);
        }
    }
    Some(out)
}

/// Laid-out size of a text object at 1× (what its rect's w/h should be).
pub fn measure(t: &Text) -> (f32, f32) {
    let pad = padding(t);
    match rasterize(if t.text.is_empty() { " " } else { &t.text }, &t.font, t.size, t.weight, t.color, t.align) {
        Some(r) => (r.image.width() as f32 + 2.0 * pad, r.image.height() as f32 + 2.0 * pad),
        None => (t.size + 2.0 * pad, t.size * 1.25 + 2.0 * pad),
    }
}

/// An emoji drawn at `px` pixels tall, trimmed to its visible pixels.
pub fn render_emoji(emoji: &str, px: f32) -> Option<RgbaImage> {
    let r = rasterize(emoji, EMOJI_FAMILY, px, 400, [0, 0, 0, 255], Align::Left)?;
    trim(&r.image)
}

/// Crops away fully transparent borders.
pub fn trim(img: &RgbaImage) -> Option<RgbaImage> {
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    for (x, y, p) in img.enumerate_pixels() {
        if p[3] > 0 {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    (x0 <= x1).then(|| image::imageops::crop_imm(img, x0, y0, x1 - x0 + 1, y1 - y0 + 1).to_image())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_renders_glyphs() {
        let r = rasterize("Hello\nWorld", "", 32.0, 700, [255, 0, 0, 255], Align::Center).unwrap();
        assert!(r.image.width() > 60 && r.image.height() >= 80, "{}x{}", r.image.width(), r.image.height());
        let painted = r.image.pixels().filter(|p| p[3] > 128).count();
        assert!(painted > 300, "painted {painted}");
        assert!(r.image.pixels().filter(|p| p[3] > 128).all(|p| p[0] > 200 && p[1] < 60), "glyphs take the color");
        assert!(families().first().map(String::as_str) == Some(DEFAULT_FAMILY));
    }

    #[test]
    fn emoji_renders_in_color() {
        let img = render_emoji("😀", 64.0).expect("emoji");
        assert!(img.width() >= 40 && img.height() >= 40, "{}x{}", img.width(), img.height());
        // Color pixels, not a monochrome mask: the face is yellow.
        let yellow = img.pixels().filter(|p| p[3] > 200 && p[0] > 200 && p[1] > 150 && p[2] < 120).count();
        assert!(yellow > 200, "yellow pixels {yellow}");
    }

    #[test]
    fn styled_text_object() {
        let t = Text {
            rect: super::super::doc::r(0.0, 0.0, 0.0, 0.0),
            text: "Tip 👍".into(),
            font: String::new(),
            size: 40.0,
            weight: 600,
            color: [255, 255, 255, 255],
            align: Align::Left,
            pill: Some([20, 20, 20, 255]),
            outline: Some([0, 0, 0, 255]),
            shadow: true,
        };
        let (w, h) = measure(&t);
        let img = render_text(&t, 1.0).unwrap();
        assert!((img.width() as f32 - w).abs() <= 1.0 && (img.height() as f32 - h).abs() <= 1.0);
        // The pill fills the middle-left; the corner stays clear.
        assert_eq!(img.get_pixel(0, 0)[3], 0);
        assert!(img.get_pixel(4, img.height() / 2)[3] > 200);
        // At 2× it's twice the size.
        let img2 = render_text(&t, 2.0).unwrap();
        assert!((img2.width() as f32 / img.width() as f32 - 2.0).abs() < 0.1);
    }
}
