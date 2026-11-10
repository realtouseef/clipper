//! The renderer: document + original image in, pixels out. The live canvas (at a reduced
//! proxy scale) and every export use this same function, so they match.
//!
//! Order, bottom to top: the oriented image (or, on a merged board, its background and the
//! image layers) → annotations and redactions in list order (a redaction bakes the pixels
//! beneath it: everything under its area is replaced) → source boxes → callouts → arrows →
//! background frame around the cropped result. Callouts sample a full-resolution render of
//! the levels below the source boxes, so redactions always show in them.
//! `Cache` only memoizes intermediate results for one original image; output never depends
//! on it.

use super::doc::*;
use super::{stickers, text};
use image::RgbaImage;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use tiny_skia::{
    BlendMode, Color, FillRule, FilterQuality, LineCap, LineJoin, Mask, Paint, PathBuilder, Pixmap, PixmapPaint, Shader,
    Stroke, StrokeDash, Transform,
};

/// Memoized pieces for one original image (shared between renders; thread-safe).
#[derive(Default)]
pub struct Cache {
    /// Oriented original at a scale: (orient, scale bits) → pixmap.
    base: Mutex<Vec<((Orient, u32), Arc<Pixmap>)>>,
    redact: Mutex<HashMap<u64, Arc<Patch>>>,
    stickers: Mutex<HashMap<(StickerSrc, u32, u32), Arc<RgbaImage>>>,
    emoji: Mutex<HashMap<(String, u32), Option<Arc<RgbaImage>>>>,
    /// Background with the window's shadow, for one frame setup and scale.
    backdrop: Mutex<Vec<(u64, Arc<Pixmap>)>>,
    texts: Mutex<HashMap<u64, Option<Arc<RgbaImage>>>>,
    /// Image layers: (file, orientation, pre-shrink scale bits) → pixmap.
    layers: Mutex<Vec<((std::path::PathBuf, Orient, u32), Option<Arc<Pixmap>>)>>,
    /// Board backgrounds: key → pixmap.
    board_bg: Mutex<Vec<(u64, Arc<Pixmap>)>>,
    /// Magnified callout pixels: key → image at output size.
    callouts: Mutex<HashMap<u64, Arc<RgbaImage>>>,
}

fn key_of(v: &impl serde::Serialize, s: f32) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(v).unwrap_or_default().hash(&mut h);
    s.to_bits().hash(&mut h);
    h.finish()
}

/// A baked redaction: the pixels of its region after the effect.
struct Patch {
    x: u32,
    y: u32,
    w: u32,
    pixels: Vec<u8>,
    mask: Vec<bool>,
}

/// Smallest pixelate block for an image whose long side is `long` pixels.
pub fn min_block(long: u32) -> f32 {
    (long as f32 / 120.0).max(16.0)
}

/// Smallest blur strength for an image whose long side is `long` pixels.
pub fn min_blur(long: u32) -> f32 {
    (long as f32 / 100.0).max(16.0)
}

pub fn render(doc: &Document, base: &RgbaImage, scale: f32, cache: &Cache) -> RgbaImage {
    pixmap_to_image(&render_pixmap(doc, base, scale, cache))
}

pub fn render_pixmap(doc: &Document, base: &RgbaImage, scale: f32, cache: &Cache) -> Pixmap {
    let content = render_content(doc, base, scale, cache);
    if !doc.frame.enabled {
        return content;
    }
    compose_frame(doc, &content, scale, cache)
}

// ---------- content ----------

fn oriented(base: &RgbaImage, o: Orient) -> RgbaImage {
    use image::imageops;
    let mut img = match o.quarter % 4 {
        1 => imageops::rotate90(base),
        2 => imageops::rotate180(base),
        3 => imageops::rotate270(base),
        _ => base.clone(),
    };
    if o.flip_h {
        imageops::flip_horizontal_in_place(&mut img);
    }
    if o.flip_v {
        imageops::flip_vertical_in_place(&mut img);
    }
    img
}

/// The oriented original, pre-shrunk when drawn below 1× (sharper and faster than
/// sampling the full image). Returns it and its scale relative to oriented pixels.
fn base_pixmap(doc: &Document, base: &RgbaImage, scale: f32, cache: &Cache) -> (Arc<Pixmap>, f32) {
    let k = if scale < 0.999 { scale } else { 1.0 };
    let key = (doc.orient, k.to_bits());
    let mut slots = cache.base.lock().unwrap();
    if let Some((_, pm)) = slots.iter().find(|(kk, _)| *kk == key) {
        return (pm.clone(), k);
    }
    let img = oriented(base, doc.orient);
    let img = if k < 1.0 {
        let (w, h) = (((img.width() as f32 * k).round() as u32).max(1), ((img.height() as f32 * k).round() as u32).max(1));
        image::imageops::resize(&img, w, h, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let pm = Arc::new(image_to_pixmap(&img));
    // A few scales: dragging switches between the proxy and full quality.
    if slots.len() >= 3 {
        slots.remove(0);
    }
    slots.push((key, pm.clone()));
    (pm, k)
}

fn render_content(doc: &Document, base: &RgbaImage, s: f32, cache: &Cache) -> Pixmap {
    let crop = doc.crop;
    let mut pm = render_lower(doc, base, s, cache);
    let view = Transform::from_row(s, 0.0, 0.0, s, -crop.x * s, -crop.y * s);
    let long = doc.oriented_size().0.max(doc.oriented_size().1);
    let at = |o: &Object| view.pre_concat(Transform::from_rotate_at(o.rotation, o.bounds().center().x, o.bounds().center().y));
    let source = |id: u64| doc.get(id).and_then(|o| if let Kind::Source(sb) = &o.kind { Some((o, sb)) } else { None });

    // Callouts sample the levels so far (with every redaction) before anything is cut out.
    let callouts: Vec<(&Object, &Callout, &Object, &SourceBox)> = doc
        .objects
        .iter()
        .filter_map(|o| if let Kind::Callout(c) = &o.kind { source(c.source).map(|(so, sb)| (o, c, so, sb)) } else { None })
        .collect();
    let patches: Vec<Option<Arc<RgbaImage>>> = callouts.iter().map(|(_, c, _, sb)| callout_patch(doc, base, sb.rect, c, s, cache)).collect();
    for (_, c, so, sb) in &callouts {
        let area = match sb.shape {
            CalloutShape::Rect => Area::RoundRect(sb.rect, sb.radius),
            CalloutShape::Circle => Area::Ellipse(sb.rect),
        };
        match c.cut {
            CutMode::Copy => {}
            CutMode::Blank => {
                if let Some(path) = area_path(&area) {
                    pm.fill_path(&path, &paint([242, 242, 244, 255]), FillRule::Winding, at(so), None);
                }
            }
            CutMode::Blur => {
                let rd = Redact { area, effect: Effect::Blur(min_blur(long) * 1.5) };
                let patch = bake_redaction(&pm, &rd, at(so), s, long, crop);
                apply_patch(&mut pm, &patch);
            }
        }
    }
    for o in doc.objects.iter() {
        if let Kind::Source(sb) = &o.kind {
            if sb.visible {
                draw_source(&mut pm, sb, at(o));
            }
        }
    }
    for ((o, c, _, _), patch) in callouts.iter().zip(&patches) {
        draw_callout(&mut pm, c, patch.as_deref(), at(o), s);
    }
    for o in doc.objects.iter() {
        if let Kind::Arrow(a) = &o.kind {
            draw_arrow(&mut pm, a, at(o), s);
        }
    }
    pm
}

/// Everything below the source boxes: the image (or a board's background and image
/// layers), then annotations and redactions in list order.
fn render_lower(doc: &Document, base: &RgbaImage, s: f32, cache: &Cache) -> Pixmap {
    let crop = doc.crop;
    let (w, h) = (((crop.w * s).round() as u32).max(1), ((crop.h * s).round() as u32).max(1));
    let mut pm = Pixmap::new(w, h).expect("canvas size");
    let view = Transform::from_row(s, 0.0, 0.0, s, -crop.x * s, -crop.y * s);
    if let Some(board) = &doc.board {
        draw_board_background(&mut pm, doc, board, view, s, cache);
    } else {
        let (img, k) = base_pixmap(doc, base, s, cache);
        let t = Transform::from_row(s / k, 0.0, 0.0, s / k, -crop.x * s, -crop.y * s);
        let quality = if (s / k - 1.0).abs() < 1e-4 { FilterQuality::Nearest } else { FilterQuality::Bilinear };
        pm.draw_pixmap(0, 0, img.as_ref().as_ref(), &PixmapPaint { quality, ..Default::default() }, t, None);
    }
    let at = |o: &Object| view.pre_concat(Transform::from_rotate_at(o.rotation, o.bounds().center().x, o.bounds().center().y));
    for o in doc.objects.iter() {
        if let Kind::Image(img) = &o.kind {
            draw_layer(&mut pm, img, at(o), s, cache);
        }
    }
    let long = doc.oriented_size().0.max(doc.oriented_size().1);
    for (i, o) in doc.objects.iter().enumerate() {
        if o.kind.level() != 1 {
            continue;
        }
        let m = at(o);
        match &o.kind {
            Kind::Redact(rd) => {
                let key = redact_key(doc, i, s);
                let cached = cache.redact.lock().unwrap().get(&key).cloned();
                let patch = cached.unwrap_or_else(|| {
                    let patch = Arc::new(bake_redaction(&pm, rd, m, s, long, crop));
                    let mut map = cache.redact.lock().unwrap();
                    if map.len() > 128 {
                        map.clear();
                    }
                    map.insert(key, patch.clone());
                    patch
                });
                apply_patch(&mut pm, &patch);
            }
            kind => draw_object(&mut pm, kind, o, m, s, cache),
        }
    }
    pm
}

/// What a redaction at index `i` bakes over: the image or board, every image layer, and
/// the lower-level objects before it.
fn redact_key(doc: &Document, i: usize, s: f32) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let below: Vec<&Object> = doc.objects.iter().enumerate().filter(|(j, o)| o.kind.level() == 0 || (o.kind.level() == 1 && *j <= i)).map(|(_, o)| o).collect();
    serde_json::to_string(&below).unwrap_or_default().hash(&mut h);
    serde_json::to_string(&(doc.crop, doc.orient, doc.size, &doc.board)).unwrap_or_default().hash(&mut h);
    if doc.board.is_some() {
        serde_json::to_string(&doc.frame.background).unwrap_or_default().hash(&mut h);
    }
    s.to_bits().hash(&mut h);
    h.finish()
}

/// A board's canvas: the frame background across it, and the separator fill in the gaps
/// between stacked images.
fn draw_board_background(pm: &mut Pixmap, doc: &Document, board: &super::merge::Board, view: Transform, s: f32, cache: &Cache) {
    let (w, h) = (pm.width(), pm.height());
    let key = key_of(&(&doc.frame.background, doc.frame.moment, w, h), s);
    let cached = cache.board_bg.lock().unwrap().iter().find(|(k, _)| *k == key).map(|(_, p)| p.clone());
    let bg = cached.unwrap_or_else(|| {
        let img = crate::backgrounds::render::render(&doc.frame.background, w, h, doc.frame.moment as f64, s >= 1.0);
        let mut bpm = Pixmap::new(w, h).expect("canvas");
        for (px, src) in bpm.data_mut().chunks_exact_mut(4).zip(img.pixels()) {
            px.copy_from_slice(&[src[0], src[1], src[2], 255]);
        }
        let bpm = Arc::new(bpm);
        let mut slots = cache.board_bg.lock().unwrap();
        if slots.len() >= 3 {
            slots.remove(0);
        }
        slots.push((key, bpm.clone()));
        bpm
    });
    pm.draw_pixmap(0, 0, bg.as_ref().as_ref(), &PixmapPaint::default(), Transform::identity(), None);
    if let Some(c) = board.separator {
        let rects: Vec<R> = doc.images().iter().map(|o| o.bounds()).collect();
        let mut sorted = rects.clone();
        match board.layout {
            super::merge::BoardLayout::Vertical => sorted.sort_by(|a, b| a.y.total_cmp(&b.y)),
            _ => sorted.sort_by(|a, b| a.x.total_cmp(&b.x)),
        }
        for g in super::merge::gaps(&sorted, board.layout) {
            if let Some(path) = rect_path(g) {
                pm.fill_path(&path, &paint(c), FillRule::Winding, view, None);
            }
        }
    }
}

/// An image layer's picture, oriented, pre-shrunk to `k` (≤ 1) for sharp downscaling.
fn layer_pixmap(src: &std::path::Path, orient: Orient, k: f32, cache: &Cache) -> Option<Arc<Pixmap>> {
    let key = (src.to_path_buf(), orient, k.to_bits());
    if let Some((_, pm)) = cache.layers.lock().unwrap().iter().find(|(kk, _)| *kk == key) {
        return pm.clone();
    }
    let pm = image::open(src).ok().map(|img| {
        let img = oriented(&img.to_rgba8(), orient);
        let img = if k < 0.999 {
            let (w, h) = (((img.width() as f32 * k).round() as u32).max(1), ((img.height() as f32 * k).round() as u32).max(1));
            image::imageops::resize(&img, w, h, image::imageops::FilterType::Triangle)
        } else {
            img
        };
        Arc::new(image_to_pixmap(&img))
    });
    let mut slots = cache.layers.lock().unwrap();
    if slots.len() >= 12 {
        slots.remove(0);
    }
    slots.push((key, pm.clone()));
    pm
}

/// Draws an image layer: its crop scaled uniformly into its rect, rounded, with a shadow.
fn draw_layer(pm: &mut Pixmap, img: &ImageLayer, m: Transform, s: f32, cache: &Cache) {
    let rect = img.rect;
    let Some(path) = round_rect_path(rect, img.radius) else { return };
    if img.shadow {
        draw_shadow(pm, &path, m, Shadow { blur: 36.0, opacity: 0.35, offset: 14.0 }, s);
    }
    let sc = img.scale();
    // Pre-shrink in steps of 1/4 so zooming reuses a few cached sizes.
    let want = (s * sc).min(1.0);
    let k = if want < 0.999 { ((want * 4.0).ceil() / 4.0).max(0.125) } else { 1.0 };
    let Some(src) = layer_pixmap(&img.src, img.orient, k, cache) else {
        pm.fill_path(&path, &paint([200, 200, 204, 255]), FillRule::Winding, m, None);
        return;
    };
    let mut mask = Mask::new(pm.width(), pm.height()).expect("mask");
    mask.fill_path(&path, FillRule::Winding, true, m);
    let t = m.pre_translate(rect.x - img.crop.x * sc, rect.y - img.crop.y * sc).pre_scale(sc / k, sc / k);
    pm.draw_pixmap(0, 0, src.as_ref().as_ref(), &PixmapPaint { quality: FilterQuality::Bicubic, ..Default::default() }, t, Some(&mask));
}

/// A soft shadow of `path` (document coordinates through `m`) darkening what's beneath.
fn draw_shadow(pm: &mut Pixmap, path: &tiny_skia::Path, m: Transform, sh: Shadow, s: f32) {
    if sh.opacity <= 0.0 {
        return;
    }
    let Some(b) = path.clone().transform(m.post_translate(0.0, sh.offset * s)).map(|p| p.bounds()) else { return };
    let margin = (sh.blur * s * 1.5).ceil() + 2.0;
    let (pw, ph) = (pm.width() as f32, pm.height() as f32);
    let x0 = (b.left() - margin).floor().clamp(0.0, pw);
    let y0 = (b.top() - margin).floor().clamp(0.0, ph);
    let x1 = (b.right() + margin).ceil().clamp(0.0, pw);
    let y1 = (b.bottom() + margin).ceil().clamp(0.0, ph);
    let (w, h) = ((x1 - x0) as u32, (y1 - y0) as u32);
    if w == 0 || h == 0 {
        return;
    }
    let Some(mut mask) = Mask::new(w, h) else { return };
    mask.fill_path(path, FillRule::Winding, true, m.post_translate(-x0, sh.offset * s - y0));
    let gray = image::GrayImage::from_raw(w, h, mask.data().to_vec()).expect("mask image");
    let sigma = sh.blur * s / 2.0;
    let gray = if sigma > 0.5 { image::imageops::fast_blur(&gray, sigma) } else { gray };
    let pw = pm.width();
    let data = pm.data_mut();
    for y in 0..h {
        for x in 0..w {
            let a = sh.opacity.clamp(0.0, 1.0) * gray.get_pixel(x, y)[0] as f32 / 255.0;
            if a <= 0.0 {
                continue;
            }
            let i = (((y + y0 as u32) * pw + x + x0 as u32) * 4) as usize;
            for c in 0..3 {
                data[i + c] = (data[i + c] as f32 * (1.0 - a)).round() as u8;
            }
            data[i + 3] = (data[i + 3] as f32 + (255.0 - data[i + 3] as f32) * a).round() as u8;
        }
    }
}

fn callout_path(rect: R, shape: CalloutShape, radius: f32) -> Option<tiny_skia::Path> {
    match shape {
        CalloutShape::Rect => round_rect_path(rect, radius),
        CalloutShape::Circle => ellipse_path(rect),
    }
}

fn draw_source(pm: &mut Pixmap, sb: &SourceBox, m: Transform) {
    let Some(path) = callout_path(sb.rect, sb.shape, sb.radius) else { return };
    if let Some(f) = sb.fill {
        pm.fill_path(&path, &paint(f), FillRule::Winding, m, None);
    }
    if sb.width > 0.0 {
        let mut st = Stroke { width: sb.width, line_join: LineJoin::Round, ..Default::default() };
        if sb.dashed {
            st.dash = StrokeDash::new(vec![sb.width * 3.0, sb.width * 2.0], 0.0);
        }
        pm.stroke_path(&path, &paint(sb.color), &st, m, None);
    }
}

/// Detail available in `area` (document pixels): picture pixels per document pixel of the
/// sharpest image layer there (1 for a plain screenshot, where document pixels are its pixels).
pub fn detail_at(doc: &Document, area: R) -> f32 {
    if doc.board.is_none() {
        return 1.0;
    }
    doc.images()
        .iter()
        .filter_map(|o| if let Kind::Image(i) = &o.kind { i.rect.intersect(&area).map(|_| 1.0 / i.scale().max(1e-3)) } else { None })
        .fold(1.0, f32::max)
}

/// The callout's pixels at output size: the source area of everything below the source
/// boxes (redactions included), rendered at full resolution and resampled with Lanczos.
fn callout_patch(doc: &Document, base: &RgbaImage, src: R, c: &Callout, s: f32, cache: &Cache) -> Option<Arc<RgbaImage>> {
    let (ow, oh) = (((c.rect.w * s).round() as u32).max(1), ((c.rect.h * s).round() as u32).max(1));
    if src.w < 1.0 || src.h < 1.0 {
        return None;
    }
    let kc = (c.scale * s).min(detail_at(doc, src).min(2.0)).max(1e-3);
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let below: Vec<&Object> = doc.objects.iter().filter(|o| o.kind.level() <= 1).collect();
    serde_json::to_string(&below).unwrap_or_default().hash(&mut h);
    serde_json::to_string(&(src, doc.orient, doc.size, &doc.board, ow, oh)).unwrap_or_default().hash(&mut h);
    if doc.board.is_some() {
        serde_json::to_string(&doc.frame.background).unwrap_or_default().hash(&mut h);
    }
    kc.to_bits().hash(&mut h);
    let key = h.finish();
    if let Some(img) = cache.callouts.lock().unwrap().get(&key) {
        return Some(img.clone());
    }
    let mut area = doc.clone();
    area.crop = src;
    area.frame.enabled = false;
    let region = pixmap_to_image(&render_lower(&area, base, kc, cache));
    let img = Arc::new(image::imageops::resize(&region, ow, oh, image::imageops::FilterType::Lanczos3));
    let mut map = cache.callouts.lock().unwrap();
    if map.len() > 32 {
        map.clear();
    }
    map.insert(key, img.clone());
    Some(img)
}

fn draw_callout(pm: &mut Pixmap, c: &Callout, patch: Option<&RgbaImage>, m: Transform, s: f32) {
    let Some(path) = callout_path(c.rect, c.shape, c.radius) else { return };
    if let Some(sh) = c.shadow {
        draw_shadow(pm, &path, m, sh, s);
    }
    if let Some(img) = patch {
        let src = image_to_pixmap(img);
        let mut mask = Mask::new(pm.width(), pm.height()).expect("mask");
        mask.fill_path(&path, FillRule::Winding, true, m);
        let t = m.pre_translate(c.rect.x, c.rect.y).pre_scale(c.rect.w / img.width().max(1) as f32, c.rect.h / img.height().max(1) as f32);
        pm.draw_pixmap(0, 0, src.as_ref(), &PixmapPaint { quality: FilterQuality::Bilinear, ..Default::default() }, t, Some(&mask));
    }
    if c.width > 0.0 {
        let st = Stroke { width: c.width, line_join: LineJoin::Round, ..Default::default() };
        pm.stroke_path(&path, &paint(c.border), &st, m, None);
    }
}

fn color(c: Rgba) -> Color {
    Color::from_rgba8(c[0], c[1], c[2], c[3])
}

fn paint(c: Rgba) -> Paint<'static> {
    Paint { shader: Shader::SolidColor(color(c)), anti_alias: true, ..Default::default() }
}

fn round_stroke(width: f32) -> Stroke {
    Stroke { width: width.max(0.5), line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() }
}

fn polyline(points: &[P]) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    let first = points.first()?;
    pb.move_to(first.x, first.y);
    if points.len() == 1 {
        pb.line_to(first.x + 0.01, first.y);
    }
    for q in &points[1..] {
        pb.line_to(q.x, q.y);
    }
    pb.finish()
}

fn rect_path(r: R) -> Option<tiny_skia::Path> {
    tiny_skia::Rect::from_xywh(r.x, r.y, r.w.max(0.01), r.h.max(0.01)).map(PathBuilder::from_rect)
}

pub fn round_rect_path(r: R, radius: f32) -> Option<tiny_skia::Path> {
    let rad = radius.min(r.w / 2.0).min(r.h / 2.0).max(0.0);
    if rad < 0.01 {
        return rect_path(r);
    }
    // Cubic approximation of quarter circles.
    let k = rad * 0.5523;
    let (x0, y0, x1, y1) = (r.x, r.y, r.right(), r.bottom());
    let mut pb = PathBuilder::new();
    pb.move_to(x0 + rad, y0);
    pb.line_to(x1 - rad, y0);
    pb.cubic_to(x1 - rad + k, y0, x1, y0 + rad - k, x1, y0 + rad);
    pb.line_to(x1, y1 - rad);
    pb.cubic_to(x1, y1 - rad + k, x1 - rad + k, y1, x1 - rad, y1);
    pb.line_to(x0 + rad, y1);
    pb.cubic_to(x0 + rad - k, y1, x0, y1 - rad + k, x0, y1 - rad);
    pb.line_to(x0, y0 + rad);
    pb.cubic_to(x0, y0 + rad - k, x0 + rad - k, y0, x0 + rad, y0);
    pb.close();
    pb.finish()
}

fn ellipse_path(r: R) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    pb.push_oval(tiny_skia::Rect::from_xywh(r.x, r.y, r.w.max(0.01), r.h.max(0.01))?);
    pb.finish()
}

fn shape_path(kind: ShapeKind, r: R, radius: f32) -> Option<tiny_skia::Path> {
    match kind {
        ShapeKind::Rect => rect_path(r),
        ShapeKind::RoundRect => round_rect_path(r, radius),
        ShapeKind::Ellipse => ellipse_path(r),
    }
}

/// Draws a straight-alpha image so it covers `r` (object space) under `m`; `s` is the scale
/// the image was rendered at.
fn draw_image_at(pm: &mut Pixmap, img: &RgbaImage, r: R, m: Transform, s: f32) {
    let src = image_to_pixmap(img);
    let (sx, sy) = (r.w * s / img.width().max(1) as f32, r.h * s / img.height().max(1) as f32);
    let t = m.pre_translate(r.x, r.y).pre_scale(sx / s, sy / s);
    pm.draw_pixmap(0, 0, src.as_ref(), &PixmapPaint { quality: FilterQuality::Bilinear, ..Default::default() }, t, None);
}

fn draw_object(pm: &mut Pixmap, kind: &Kind, o: &Object, m: Transform, s: f32, cache: &Cache) {
    match kind {
        Kind::Arrow(a) => draw_arrow(pm, a, m, s),
        // Drawn in their own levels (render_lower / render_content).
        Kind::Image(_) | Kind::Source(_) | Kind::Callout(_) => {}
        Kind::Line(l) => {
            if let Some(path) = polyline(&[l.from, l.to]) {
                pm.stroke_path(&path, &paint(l.color), &round_stroke(l.width), m, None);
            }
        }
        Kind::Shape(sh) => {
            if let Some(path) = shape_path(sh.kind, sh.rect, sh.radius) {
                if let Some(f) = sh.fill {
                    pm.fill_path(&path, &paint(f), FillRule::Winding, m, None);
                }
                if let Some(c) = sh.stroke.filter(|_| sh.width > 0.0) {
                    let st = Stroke { width: sh.width, line_join: LineJoin::Round, ..Default::default() };
                    pm.stroke_path(&path, &paint(c), &st, m, None);
                }
            }
        }
        Kind::Text(t) => {
            // Keyed without the position: moving text reuses its raster.
            let key = key_of(&Text { rect: r(0.0, 0.0, 0.0, 0.0), ..t.clone() }, s);
            let cached = cache.texts.lock().unwrap().get(&key).cloned();
            let img = cached.unwrap_or_else(|| {
                let img = text::render_text(t, s).map(Arc::new);
                let mut map = cache.texts.lock().unwrap();
                if map.len() > 256 {
                    map.clear();
                }
                map.insert(key, img.clone());
                img
            });
            if let Some(img) = img {
                draw_image_at(pm, &img, t.rect, m, s);
            }
        }
        Kind::Highlight(hl) => {
            let mut p = paint(hl.color);
            p.blend_mode = BlendMode::Multiply;
            match &hl.mark {
                Mark::Rect(r) => {
                    if let Some(path) = rect_path(*r) {
                        pm.fill_path(&path, &p, FillRule::Winding, m, None);
                    }
                }
                Mark::Free { points, width } => {
                    if let Some(path) = polyline(points) {
                        let st = Stroke { width: *width, line_cap: LineCap::Square, line_join: LineJoin::Round, ..Default::default() };
                        pm.stroke_path(&path, &p, &st, m, None);
                    }
                }
            }
        }
        Kind::Step(st) => {
            let c = st.center;
            if let Some(path) = PathBuilder::from_circle(c.x, c.y, st.radius) {
                pm.fill_path(&path, &paint(st.color), FillRule::Winding, m, None);
                let ring = Stroke { width: (st.radius * 0.14).max(1.0), ..Default::default() };
                pm.stroke_path(&path, &paint([255, 255, 255, 255]), &ring, m, None);
            }
            let label = st.n.to_string();
            let size = st.radius * if label.len() > 1 { 0.95 } else { 1.15 };
            if let Some(ras) = text::rasterize(&label, "", size * s, 700, [255, 255, 255, 255], Align::Center) {
                if let Some(img) = text::trim(&ras.image) {
                    let (w, h) = (img.width() as f32 / s, img.height() as f32 / s);
                    draw_image_at(pm, &img, r(c.x - w / 2.0, c.y - h / 2.0, w, h), m, s);
                }
            }
        }
        Kind::Pen(pen) => {
            if let Some(path) = polyline(&pen.points) {
                pm.stroke_path(&path, &paint(pen.color), &round_stroke(pen.width), m, None);
            }
        }
        Kind::Emoji(e) => {
            let px = (e.rect.h * s).round().max(4.0) as u32;
            let img = cache
                .emoji
                .lock()
                .unwrap()
                .entry((e.emoji.clone(), px))
                .or_insert_with(|| text::render_emoji(&e.emoji, px as f32 * 0.86).map(Arc::new))
                .clone();
            if let Some(img) = img {
                // Fit inside the rect, centered.
                let k = (e.rect.w / img.width() as f32).min(e.rect.h / img.height() as f32) * s;
                let (w, h) = (img.width() as f32 * k / s, img.height() as f32 * k / s);
                let c = e.rect.center();
                draw_image_at(pm, &img, r(c.x - w / 2.0, c.y - h / 2.0, w, h), m, s);
            }
        }
        Kind::Sticker(st) => {
            let (w, h) = (((st.rect.w * s).round() as u32).max(1), ((st.rect.h * s).round() as u32).max(1));
            let key = (st.src.clone(), w, h);
            let cached = cache.stickers.lock().unwrap().get(&key).cloned();
            let img = cached.or_else(|| {
                let img = Arc::new(stickers::render(&st.src, w, h)?);
                let mut map = cache.stickers.lock().unwrap();
                if map.len() > 64 {
                    map.clear();
                }
                map.insert(key, img.clone());
                Some(img)
            });
            if let Some(img) = img {
                draw_image_at(pm, &img, st.rect, m, s);
            }
        }
        Kind::Redact(_) => {}
    }
    let _ = o;
}

fn draw_arrow(pm: &mut Pixmap, a: &Arrow, m: Transform, s: f32) {
    let mut pts = a.drawn_path();
    if pts.len() < 2 || R::bbox(&pts).w.max(R::bbox(&pts).h) < 1.0 {
        return;
    }
    let head_len = if a.head_size > 0.0 { a.head_size * a.width } else { (a.width * 3.4).max(10.0) };
    let head_w = head_len * 0.95;
    // Each head points along the line's last stretch (robust to wobble and tight curves).
    let dir_at = |pts: &[P], from_end: bool| {
        let tip = if from_end { pts[pts.len() - 1] } else { pts[0] };
        let it: Box<dyn Iterator<Item = &P>> = if from_end { Box::new(pts.iter().rev()) } else { Box::new(pts.iter()) };
        let prev = it.skip(1).find(|q| q.dist(tip) >= head_len * 0.6).copied().unwrap_or(if from_end { pts[0] } else { pts[pts.len() - 1] });
        let (dx, dy) = (tip.x - prev.x, tip.y - prev.y);
        let len = (dx * dx + dy * dy).sqrt().max(1e-3);
        (tip, (dx / len, dy / len))
    };
    let mut ends = vec![dir_at(&pts, true)];
    if a.double {
        ends.push(dir_at(&pts, false));
    }
    // Shorten the shaft under filled heads so its round cap doesn't poke through the tip.
    if a.head == Head::Filled {
        let n = pts.len();
        let (tip, d) = ends[0];
        pts[n - 1] = p(tip.x - d.0 * head_len * 0.7, tip.y - d.1 * head_len * 0.7);
        if a.double {
            let (tip, d) = ends[1];
            pts[0] = p(tip.x - d.0 * head_len * 0.7, tip.y - d.1 * head_len * 0.7);
        }
    }
    let mut stroke = round_stroke(a.width);
    if a.dashed {
        stroke.dash = StrokeDash::new(vec![a.width * 2.6, a.width * 2.2], 0.0);
    }
    let shaft = polyline(&pts);
    let heads: Vec<(tiny_skia::Path, bool)> = ends
        .iter()
        .filter_map(|&(tip, d)| {
            let base = p(tip.x - d.0 * head_len, tip.y - d.1 * head_len);
            let (nx, ny) = (-d.1 * head_w / 2.0, d.0 * head_w / 2.0);
            let left = p(base.x + nx, base.y + ny);
            let right = p(base.x - nx, base.y - ny);
            match a.head {
                Head::Filled => {
                    let mut pb = PathBuilder::new();
                    pb.move_to(tip.x, tip.y);
                    pb.line_to(left.x, left.y);
                    pb.line_to(right.x, right.y);
                    pb.close();
                    pb.finish().map(|p| (p, true))
                }
                Head::Open => polyline(&[left, tip, right]).map(|p| (p, false)),
                Head::None => None,
            }
        })
        .collect();
    if a.shadow {
        let sh = Shadow { blur: (a.width * 1.5).max(6.0), opacity: 0.35, offset: (a.width * 0.8).max(3.0) };
        if let Some(fill) = shaft.as_ref().and_then(|p| p.stroke(&round_stroke(a.width), 1.0)) {
            draw_shadow(pm, &fill, m, sh, s);
        }
        for (h, filled) in &heads {
            let f = if *filled { Some(h.clone()) } else { h.stroke(&round_stroke(a.width), 1.0) };
            if let Some(f) = f {
                draw_shadow(pm, &f, m, sh, s);
            }
        }
    }
    let pnt = paint(a.color);
    if let Some(path) = &shaft {
        pm.stroke_path(path, &pnt, &stroke, m, None);
    }
    for (h, filled) in heads {
        if filled {
            pm.fill_path(&h, &pnt, FillRule::Winding, m, None);
            let edge = Stroke { width: (a.width * 0.35).max(0.5), line_join: LineJoin::Round, ..Default::default() };
            pm.stroke_path(&h, &pnt, &edge, m, None);
        } else {
            pm.stroke_path(&h, &pnt, &round_stroke(a.width), m, None);
        }
    }
}

// ---------- redaction ----------

fn area_path(area: &Area) -> Option<tiny_skia::Path> {
    match area {
        Area::Rect(r) => rect_path(*r),
        Area::RoundRect(r, rad) => round_rect_path(*r, *rad),
        Area::Ellipse(r) => ellipse_path(*r),
        Area::Brush { points, width } => polyline(points)?.stroke(&round_stroke(*width), 1.0),
    }
}

/// Replaces every pixel under the redaction with the effect, computed from the pixels
/// beneath. Pixelation averages whole blocks (many-to-one, not invertible); blur pixelates
/// first and then smooths, so it is just as lossy. The mask has no anti-aliasing: an edge
/// pixel is either fully replaced or untouched.
fn bake_redaction(pm: &Pixmap, rd: &Redact, m: Transform, s: f32, long: u32, crop: R) -> Patch {
    let (pw, ph) = (pm.width(), pm.height());
    let empty = Patch { x: 0, y: 0, w: 0, pixels: Vec::new(), mask: Vec::new() };
    let Some(path) = area_path(&rd.area) else { return empty };
    let Some(mut mask) = Mask::new(pw, ph) else { return empty };
    mask.fill_path(&path, FillRule::Winding, false, m);
    let Some(b) = path.clone().transform(m).map(|p| p.bounds()) else { return empty };
    let x0 = (b.left().floor().max(0.0) as u32).min(pw);
    let y0 = (b.top().floor().max(0.0) as u32).min(ph);
    let x1 = (b.right().ceil().max(0.0) as u32 + 1).min(pw);
    let y1 = (b.bottom().ceil().max(0.0) as u32 + 1).min(ph);
    if x1 <= x0 || y1 <= y0 {
        return empty;
    }
    let (w, h) = (x1 - x0, y1 - y0);
    let src = pm.data();
    let at = |x: u32, y: u32| {
        let i = ((y * pw + x) * 4) as usize;
        [src[i], src[i + 1], src[i + 2], src[i + 3]]
    };
    // Block grid anchored to the image (stable as the crop changes).
    let (gx, gy) = (crop.x * s, crop.y * s);
    let pixelate = |block: f32| -> Vec<u8> {
        let block = block.max(2.0);
        let mut out = vec![0u8; (w * h * 4) as usize];
        let cell = |v: u32, g: f32| ((v as f32 + g) / block).floor() as i64;
        let mut sums: HashMap<(i64, i64), ([u64; 4], u64)> = HashMap::new();
        // Average over whole cells (reaching outside the region where the cell does).
        let cx0 = cell(x0, gx);
        let cy0 = cell(y0, gy);
        let cx1 = cell(x1 - 1, gx);
        let cy1 = cell(y1 - 1, gy);
        let px_range = |c: i64, g: f32, max: u32| {
            let a = ((c as f32 * block - g).floor().max(0.0) as u32).min(max);
            let b = (((c + 1) as f32 * block - g).ceil().max(0.0) as u32).min(max);
            a..b
        };
        for cy in cy0..=cy1 {
            for cx in cx0..=cx1 {
                let mut acc = ([0u64; 4], 0u64);
                for y in px_range(cy, gy, ph) {
                    for x in px_range(cx, gx, pw) {
                        if cell(x, gx) == cx && cell(y, gy) == cy {
                            let v = at(x, y);
                            for k in 0..4 {
                                acc.0[k] += v[k] as u64;
                            }
                            acc.1 += 1;
                        }
                    }
                }
                sums.insert((cx, cy), acc);
            }
        }
        for y in 0..h {
            for x in 0..w {
                let (sum, n) = sums[&(cell(x0 + x, gx), cell(y0 + y, gy))];
                let i = ((y * w + x) * 4) as usize;
                for k in 0..4 {
                    out[i + k] = (sum[k] / n.max(1)) as u8;
                }
            }
        }
        out
    };
    let pixels = match rd.effect {
        Effect::Solid(c) => {
            let c = [c[0], c[1], c[2], 255];
            (0..w * h).flat_map(|_| c).collect()
        }
        Effect::Pixelate(block) => pixelate(block.max(min_block(long)) * s),
        Effect::Blur(strength) => {
            let k = strength.max(min_blur(long)) * s;
            let mut buf = pixelate(k);
            box_blur(&mut buf, w, h, (k * 0.5).round().max(1.0) as u32);
            buf
        }
    };
    let mask_data = mask.data();
    let mask = (0..h).flat_map(|y| (0..w).map(move |x| (y, x))).map(|(y, x)| mask_data[((y0 + y) * pw + x0 + x) as usize] > 0).collect();
    Patch { x: x0, y: y0, w, pixels, mask }
}

fn apply_patch(pm: &mut Pixmap, patch: &Patch) {
    if patch.w == 0 {
        return;
    }
    let pw = pm.width();
    let data = pm.data_mut();
    for (k, on) in patch.mask.iter().enumerate() {
        if !on {
            continue;
        }
        let (x, y) = (patch.x + k as u32 % patch.w, patch.y + k as u32 / patch.w);
        let i = ((y * pw + x) * 4) as usize;
        data[i..i + 4].copy_from_slice(&patch.pixels[k * 4..k * 4 + 4]);
    }
}

/// Three passes of a box blur of `radius` (≈ gaussian), clamped at the edges.
fn box_blur(buf: &mut [u8], w: u32, h: u32, radius: u32) {
    let (w, h, r) = (w as usize, h as usize, radius as usize);
    let mut tmp = vec![0u8; buf.len()];
    for _ in 0..3 {
        for pass in 0..2 {
            let (len, lines) = if pass == 0 { (w, h) } else { (h, w) };
            let idx = |line: usize, i: usize| if pass == 0 { (line * w + i) * 4 } else { (i * w + line) * 4 };
            let (src, dst): (&[u8], &mut [u8]) = if pass == 0 { (&*buf, &mut tmp) } else { (&tmp, &mut *buf) };
            for line in 0..lines {
                let mut acc = [0u32; 4];
                let get = |i: isize| idx(line, i.clamp(0, len as isize - 1) as usize);
                for i in -(r as isize)..=(r as isize) {
                    let j = get(i);
                    for k in 0..4 {
                        acc[k] += src[j + k] as u32;
                    }
                }
                let n = (2 * r + 1) as u32;
                for i in 0..len {
                    let d = idx(line, i);
                    for k in 0..4 {
                        dst[d + k] = (acc[k] / n) as u8;
                    }
                    let add = get(i as isize + r as isize + 1);
                    let sub = get(i as isize - r as isize);
                    for k in 0..4 {
                        acc[k] = acc[k] + src[add + k] as u32 - src[sub + k] as u32;
                    }
                }
            }
        }
    }
}

// ---------- frame ----------

fn scaled(r: R, s: f32) -> R {
    super::doc::r(r.x * s, r.y * s, r.w * s, r.h * s)
}

fn compose_frame(doc: &Document, content: &Pixmap, s: f32, cache: &Cache) -> Pixmap {
    let f = &doc.frame;
    let lay = doc.layout();
    let (w, h) = (((lay.canvas.0 as f32 * s).round() as u32).max(1), ((lay.canvas.1 as f32 * s).round() as u32).max(1));
    let win = scaled(lay.window, s);
    let radius = f.radius as f32 * s * lay.scale;
    let win_path = round_rect_path(win, radius);
    let bkey = key_of(&(f, lay.canvas, (lay.window.x, lay.window.y, lay.window.w, lay.window.h)), s);
    let cached = cache.backdrop.lock().unwrap().iter().find(|(k, _)| *k == bkey).map(|(_, pm)| pm.clone());
    let pm = match cached {
        Some(pm) => (*pm).clone(),
        None => {
            let pm = backdrop(f, w, h, win_path.as_ref(), s, cache);
            let mut slots = cache.backdrop.lock().unwrap();
            if slots.len() >= 3 {
                slots.remove(0);
            }
            slots.push((bkey, Arc::new(pm.clone())));
            pm
        }
    };

    let Some(win_path) = win_path else { return pm };
    compose_window(doc, pm, content, &win_path, win, radius, s, lay)
}

/// Background with the window's drop shadow (blurred window shape, offset down, darkening
/// the background).
fn backdrop(f: &Frame, w: u32, h: u32, win_path: Option<&tiny_skia::Path>, s: f32, cache: &Cache) -> Pixmap {
    let _ = cache;
    let bg = crate::backgrounds::render::render(&f.background, w, h, f.moment as f64, s >= 1.0);
    let mut pm = Pixmap::new(w, h).expect("canvas");
    for (px, src) in pm.data_mut().chunks_exact_mut(4).zip(bg.pixels()) {
        px.copy_from_slice(&[src[0], src[1], src[2], 255]);
    }
    if f.shadow_opacity > 0.0 {
        if let Some(path) = win_path {
            let mut m = Mask::new(w, h).expect("mask");
            m.fill_path(path, FillRule::Winding, true, Transform::from_translate(0.0, f.shadow_offset as f32 * s));
            let gray = image::GrayImage::from_raw(w, h, m.data().to_vec()).expect("mask image");
            let sigma = f.shadow_blur as f32 * s / 2.0;
            let gray = if sigma > 0.5 { image::imageops::fast_blur(&gray, sigma) } else { gray };
            for (px, a) in pm.data_mut().chunks_exact_mut(4).zip(gray.pixels()) {
                let k = 1.0 - f.shadow_opacity.clamp(0.0, 1.0) * a[0] as f32 / 255.0;
                for c in &mut px[..3] {
                    *c = (*c as f32 * k).round() as u8;
                }
            }
        }
    }
    pm
}

#[allow(clippy::too_many_arguments)]
fn compose_window(doc: &Document, mut pm: Pixmap, content: &Pixmap, win_path: &tiny_skia::Path, win: R, radius: f32, s: f32, lay: FrameLayout) -> Pixmap {
    let f = &doc.frame;
    let (w, h) = (pm.width(), pm.height());
    let mut clip = Mask::new(w, h).expect("mask");
    clip.fill_path(win_path, FillRule::Winding, true, Transform::identity());
    pm.fill_path(win_path, &paint([255, 255, 255, 255]), FillRule::Winding, Transform::identity(), None);

    if lay.bar > 0.0 {
        let bar = super::doc::r(win.x, win.y, win.w, lay.bar * s);
        let dark = [0x2c, 0x2c, 0x30, 255];
        if let Some(path) = rect_path(bar) {
            pm.fill_path(&path, &paint(dark), FillRule::Winding, Transform::identity(), Some(&clip));
        }
        let bh = bar.h;
        let dot = bh * if f.window == WindowStyle::Mac { 0.2 } else { 0.14 };
        for (i, c) in [[0xff, 0x5f, 0x57, 255], [0xfe, 0xbc, 0x2e, 255], [0x28, 0xc8, 0x40, 255]].into_iter().enumerate() {
            let cx = bar.x + bh * 0.62 + i as f32 * dot * 3.3;
            if let Some(path) = PathBuilder::from_circle(cx, bar.y + bh / 2.0, dot) {
                pm.fill_path(&path, &paint(c), FillRule::Winding, Transform::identity(), None);
            }
        }
        if f.window == WindowStyle::Browser {
            let x = bar.x + bh * 0.62 + dot * 9.0;
            let pill = super::doc::r(x, bar.y + bh * 0.22, (bar.right() - bh * 0.4 - x).max(bh), bh * 0.56);
            if let Some(path) = round_rect_path(pill, pill.h / 2.0) {
                pm.fill_path(&path, &paint([0x45, 0x45, 0x4a, 255]), FillRule::Winding, Transform::identity(), None);
            }
        }
    }

    let c = scaled(lay.content, s);
    let t = Transform::from_row(lay.scale, 0.0, 0.0, lay.scale, c.x, c.y);
    let quality = if (lay.scale - 1.0).abs() < 1e-4 && c.x.fract() == 0.0 && c.y.fract() == 0.0 {
        FilterQuality::Nearest
    } else {
        FilterQuality::Bilinear
    };
    pm.draw_pixmap(0, 0, content.as_ref(), &PixmapPaint { quality, ..Default::default() }, t, Some(&clip));

    if f.inset {
        let wd = (1.5 * s).max(1.0);
        if let Some(path) = round_rect_path(win.inflate(-wd / 2.0), (radius - wd / 2.0).max(0.0)) {
            let st = Stroke { width: wd, ..Default::default() };
            pm.stroke_path(&path, &paint([255, 255, 255, 70]), &st, Transform::identity(), None);
        }
    }
    pm
}

// ---------- conversions ----------

pub fn image_to_pixmap(img: &RgbaImage) -> Pixmap {
    let mut data = img.as_raw().clone();
    for px in data.chunks_exact_mut(4) {
        let a = px[3] as u16;
        if a < 255 {
            for c in &mut px[..3] {
                *c = ((*c as u16 * a + 127) / 255) as u8;
            }
        }
    }
    Pixmap::from_vec(data, tiny_skia::IntSize::from_wh(img.width().max(1), img.height().max(1)).unwrap())
        .unwrap_or_else(|| Pixmap::new(1, 1).unwrap())
}

pub fn pixmap_to_image(pm: &Pixmap) -> RgbaImage {
    let mut data = pm.data().to_vec();
    for px in data.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a > 0 && a < 255 {
            for c in &mut px[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    RgbaImage::from_raw(pm.width(), pm.height(), data).expect("pixmap size")
}

/// Fills `img` with a rounded rect covering it (straight alpha), used for text pills.
pub fn fill_round_rect_image(img: &mut RgbaImage, radius: f32, c: Rgba) {
    let (w, h) = (img.width(), img.height());
    let Some(mut pm) = Pixmap::new(w, h) else { return };
    if let Some(path) = round_rect_path(super::doc::r(0.0, 0.0, w as f32, h as f32), radius) {
        pm.fill_path(&path, &paint(c), FillRule::Winding, Transform::identity(), None);
    }
    let pill = pixmap_to_image(&pm);
    for (d, s) in img.pixels_mut().zip(pill.pixels()) {
        text::blend(d, s.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Rgba = [255, 0, 0, 255];

    /// A busy, text-like test image: dark glyph strokes on white.
    fn busy(w: u32, h: u32) -> RgbaImage {
        let mut img = RgbaImage::from_pixel(w, h, image::Rgba([255, 255, 255, 255]));
        if let Some(t) = text::rasterize(&"Secret password 12345 hunter2\n".repeat(12), "", 28.0, 400, [0, 0, 0, 255], Align::Left) {
            image::imageops::overlay(&mut img, &t.image, 10, 10);
        }
        img
    }

    fn mean_diff(a: &RgbaImage, b: &RgbaImage, r: R) -> f32 {
        let mut sum = 0.0;
        let mut n = 0.0;
        for y in r.y as u32..r.bottom() as u32 {
            for x in r.x as u32..r.right() as u32 {
                let (p, q) = (a.get_pixel(x, y), b.get_pixel(x, y));
                sum += (0..3).map(|k| (p[k] as f32 - q[k] as f32).abs()).sum::<f32>() / 3.0;
                n += 1.0;
            }
        }
        sum / n
    }

    /// Standard deviation of luma in `r`: text has a lot, a redaction almost none.
    fn detail(img: &RgbaImage, r: R) -> f32 {
        let v: Vec<f32> = (r.y as u32..r.bottom() as u32)
            .flat_map(|y| (r.x as u32..r.right() as u32).map(move |x| (x, y)))
            .map(|(x, y)| {
                let p = img.get_pixel(x, y);
                0.3 * p[0] as f32 + 0.59 * p[1] as f32 + 0.11 * p[2] as f32
            })
            .collect();
        let mean = v.iter().sum::<f32>() / v.len() as f32;
        (v.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / v.len() as f32).sqrt()
    }

    fn callout_pair(d: &mut Document, src: R, scale: f32) -> u64 {
        let s = d.add(Kind::Source(SourceBox { rect: src, shape: CalloutShape::Rect, color: RED, width: 3.0, radius: 0.0, fill: None, dashed: false, visible: true, callout: 0 }));
        let c = Callout { source: 0, rect: r(0.0, 0.0, 0.0, 0.0), scale, shape: CalloutShape::Rect, border: [0, 0, 255, 255], width: 3.0, radius: 0.0, shadow: Some(Shadow { blur: 20.0, opacity: 0.4, offset: 8.0 }), cut: CutMode::Copy, connector: None };
        d.add_callout(s, c, 40.0).unwrap()
    }

    /// The secret (magenta) is under a redaction: the callout of that area shows the
    /// redaction, and no export contains a single magenta pixel, at any scale.
    #[test]
    fn callouts_show_redactions_never_the_secret() {
        let mut base = RgbaImage::from_pixel(1200, 800, image::Rgba([255, 255, 255, 255]));
        for y in 200..300 {
            for x in 200..400 {
                base.put_pixel(x, y, image::Rgba([255, 0, 255, 255]));
            }
        }
        let mut d = Document::new("a.png", (1200, 800));
        // The redaction sits *after* the callout in the list: still applied first.
        let c = callout_pair(&mut d, r(180.0, 180.0, 240.0, 140.0), 2.0);
        d.add(Kind::Redact(Redact { area: Area::Rect(r(190.0, 190.0, 220.0, 120.0)), effect: Effect::Solid([0, 0, 0, 255]) }));
        let magenta = |img: &RgbaImage| img.pixels().filter(|p| p[0] > 200 && p[1] < 60 && p[2] > 200).count();
        for scale in [0.5, 1.0, 2.0] {
            let out = render(&d, &base, scale, &Cache::default());
            assert_eq!(magenta(&out), 0, "magenta leaked at {scale}×");
        }
        // Without the redaction, the callout does show it (the test would catch a leak).
        let mut open = d.clone();
        open.objects.retain(|o| !matches!(o.kind, Kind::Redact(_)));
        let out = render(&open, &base, 1.0, &Cache::default());
        let cr = open.get(c).unwrap().bounds();
        let inside = (cr.y as u32 + 10..cr.bottom() as u32 - 10).flat_map(|y| (cr.x as u32 + 10..cr.right() as u32 - 10).map(move |x| (x, y))).filter(|&(x, y)| {
            let p = out.get_pixel(x, y);
            p[0] > 200 && p[1] < 60 && p[2] > 200
        }).count();
        assert!(inside > 1000, "the callout magnifies the area ({inside} magenta pixels)");
    }

    #[test]
    fn callout_is_a_sharp_magnified_copy() {
        // A checkerboard of 4 px cells: magnified 2×, cells become 8 px in the callout.
        let base = RgbaImage::from_fn(800, 600, |x, y| if (x / 4 + y / 4) % 2 == 0 { image::Rgba([0, 0, 0, 255]) } else { image::Rgba([255, 255, 255, 255]) });
        let mut d = Document::new("a.png", (800, 600));
        let c = callout_pair(&mut d, r(100.0, 100.0, 64.0, 64.0), 2.0);
        let out = render(&d, &base, 1.0, &Cache::default());
        let cr = d.get(c).unwrap().bounds();
        // Count color changes along a row inside the callout: 128 px / 8 px cells ≈ 16.
        let y = (cr.y + cr.h / 2.0) as u32;
        let row: Vec<bool> = (cr.x as u32 + 6..cr.right() as u32 - 6).map(|x| out.get_pixel(x, y)[0] > 127).collect();
        let changes = row.windows(2).filter(|w| w[0] != w[1]).count();
        assert!((12..=17).contains(&changes), "{changes} edges: not a clean 2× copy");
        let mid = row.iter().filter(|b| **b).count() as f32 / row.len() as f32;
        assert!((0.35..0.65).contains(&mid), "contrast kept ({mid})");
    }

    #[test]
    fn board_layers_render_where_laid_out() {
        let dir = std::env::temp_dir().join(format!("clipper-board-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.png"), dir.join("b.png"));
        RgbaImage::from_pixel(300, 200, image::Rgba([255, 0, 0, 255])).save(&a).unwrap();
        RgbaImage::from_pixel(150, 100, image::Rgba([0, 0, 255, 255])).save(&b).unwrap();
        let board = super::super::merge::Board { separator: Some([0, 255, 0, 255]), matching: super::super::merge::Match::Width, ..Default::default() };
        let d = Document::new_board(&[(a, (300, 200)), (b, (150, 100))], board);
        let out = render(&d, &RgbaImage::new(1, 1), 1.0, &Cache::default());
        assert_eq!(out.dimensions(), d.size);
        let rects: Vec<R> = d.images().iter().map(|o| o.bounds()).collect();
        let at = |q: P| *out.get_pixel(q.x as u32, q.y as u32);
        assert_eq!(at(rects[0].center()).0, [255, 0, 0, 255]);
        assert_eq!(at(rects[1].center()).0, [0, 0, 255, 255]);
        // Matched to 225 wide (150 × 1.5), both without distortion.
        assert_eq!((rects[0].w, rects[0].h), (225.0, 150.0));
        assert_eq!((rects[1].w, rects[1].h), (225.0, 150.0));
        // The separator fills the gap.
        assert_eq!(at(p(rects[0].center().x, rects[0].bottom() + 12.0)).0, [0, 255, 0, 255]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The canvas (a reduced scale) and the export (1×, 2×) show the same picture.
    #[test]
    fn export_matches_canvas() {
        let dir = std::env::temp_dir().join(format!("clipper-board-x-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.png"), dir.join("b.png"));
        busy(900, 500).save(&a).unwrap();
        busy(700, 450).save(&b).unwrap();
        let mut d = Document::new_board(&[(a, (900, 500)), (b, (700, 450))], super::super::merge::Board::default());
        let imgs: Vec<R> = d.images().iter().map(|o| o.bounds()).collect();
        let c1 = callout_pair(&mut d, r(imgs[0].x + 100.0, imgs[0].y + 50.0, 160.0, 60.0), 2.0);
        let c2 = callout_pair(&mut d, r(imgs[1].x + 80.0, imgs[1].y + 100.0, 160.0, 60.0), 2.0);
        d.connect(c1, c2, Arrow { style: ArrowStyle::Hand, roughness: 0.6, seed: 9, width: 6.0, ..Default::default() });
        let base = RgbaImage::new(1, 1);
        let full = render(&d, &base, 1.0, &Cache::default());
        assert_eq!(full.dimensions(), d.size);
        let canvas = render(&d, &base, 0.5, &Cache::default());
        let down = image::imageops::resize(&full, canvas.width(), canvas.height(), image::imageops::FilterType::Triangle);
        let diff = mean_diff(&down, &canvas, r(0.0, 0.0, canvas.width() as f32, canvas.height() as f32));
        assert!(diff < 6.0, "canvas differs from the export by {diff}");
        let big = render(&d, &base, 2.0, &Cache::default());
        assert_eq!(big.dimensions(), (d.size.0 * 2, d.size.1 * 2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn redactions_are_baked_and_unrecoverable() {
        let base = busy(1200, 500);
        let area = r(10.0, 10.0, 600.0, 200.0);
        for effect in [Effect::Blur(1.0), Effect::Pixelate(1.0), Effect::Solid([200, 0, 200, 40])] {
            let mut d = Document::new("a.png", (1200, 500));
            d.add(Kind::Redact(Redact { area: Area::Rect(area), effect }));
            let out = render(&d, &base, 1.0, &Cache::default());
            assert_eq!(out.dimensions(), base.dimensions());
            let inner = area.inflate(-4.0);
            let diff = mean_diff(&out, &base, inner);
            let before = detail(&base, inner);
            let after = detail(&out, inner);
            // Text is gone: the area differs from the original and has (almost) no detail left,
            // even though the requested strength (1 px) was far below the minimum.
            assert!(diff > 20.0, "{effect:?}: mean diff {diff}");
            if let Effect::Pixelate(_) = effect {
                // Every block is one flat color: nothing finer than the block survives.
                let b = min_block(1200) as u32;
                for by in (16..176).step_by(b as usize) {
                    for bx in (16..576).step_by(b as usize) {
                        let x0 = (bx / b) * b;
                        let y0 = (by / b) * b;
                        let first = out.get_pixel(x0 + 1, y0 + 1);
                        for (x, y) in [(x0 + b - 2, y0 + 1), (x0 + 1, y0 + b - 2), (x0 + b / 2, y0 + b / 2)] {
                            assert_eq!(out.get_pixel(x, y), first, "block at {x0},{y0} isn't flat");
                        }
                    }
                }
            } else {
                assert!(before > 60.0 && after < before * 0.35, "{effect:?}: detail {before} -> {after}");
            }
            // A solid redaction is opaque whatever alpha was asked for.
            if let Effect::Solid(_) = effect {
                assert_eq!(*out.get_pixel(100, 100), image::Rgba([200, 0, 200, 255]));
            }
            // Pixels outside the redaction are untouched.
            assert_eq!(out.get_pixel(1100, 400), base.get_pixel(1100, 400));
            // No pixel inside equals its original where the original had ink.
            let leaked = (inner.y as u32..inner.bottom() as u32)
                .flat_map(|y| (inner.x as u32..inner.right() as u32).map(move |x| (x, y)))
                .filter(|&(x, y)| base.get_pixel(x, y)[0] < 60 && out.get_pixel(x, y) == base.get_pixel(x, y))
                .count();
            assert_eq!(leaked, 0, "{effect:?}: original ink pixels survive");
        }
    }

    #[test]
    fn redaction_minimums_scale_with_image() {
        assert_eq!(min_block(1000), 16.0);
        assert_eq!(min_block(3840), 32.0);
        assert_eq!(min_blur(1000), 16.0);
        assert!((min_blur(3840) - 38.4).abs() < 1e-3);
    }

    #[test]
    fn redaction_shapes_and_rotation_cover_their_area() {
        let base = busy(800, 400);
        let mut d = Document::new("a.png", (800, 400));
        let id = d.add(Kind::Redact(Redact { area: Area::Ellipse(r(100.0, 50.0, 300.0, 120.0)), effect: Effect::Solid([255, 0, 255, 255]) }));
        let out = render(&d, &base, 1.0, &Cache::default());
        assert_eq!(*out.get_pixel(250, 110), image::Rgba([255, 0, 255, 255]), "center covered");
        assert_ne!(*out.get_pixel(102, 52), image::Rgba([255, 0, 255, 255]), "corner outside the ellipse");
        d.set_rotation(id, 90.0);
        let out = render(&d, &base, 1.0, &Cache::default());
        assert_eq!(*out.get_pixel(250, 230), image::Rgba([255, 0, 255, 255]), "rotated ellipse reaches down");
        // Brush strokes too.
        let mut d = Document::new("a.png", (800, 400));
        d.add(Kind::Redact(Redact { area: Area::Brush { points: vec![p(50.0, 50.0), p(400.0, 50.0)], width: 40.0 }, effect: Effect::Solid([0, 0, 0, 255]) }));
        let out = render(&d, &base, 1.0, &Cache::default());
        assert_eq!(out.get_pixel(200, 60)[0], 0);
        assert_eq!(out.get_pixel(200, 90), base.get_pixel(200, 90));
    }

    #[test]
    fn objects_above_a_redaction_stay_visible_and_below_are_redacted() {
        let base = RgbaImage::from_pixel(400, 300, image::Rgba([255, 255, 255, 255]));
        let mut d = Document::new("a.png", (400, 300));
        let shape = |x| Kind::Shape(Shape { kind: ShapeKind::Rect, rect: r(x, 100.0, 40.0, 40.0), stroke: None, fill: Some(RED), width: 0.0, radius: 0.0 });
        d.add(shape(50.0));
        d.add(Kind::Redact(Redact { area: Area::Rect(r(0.0, 0.0, 400.0, 300.0)), effect: Effect::Solid([0, 0, 0, 255]) }));
        d.add(shape(200.0));
        let out = render(&d, &base, 1.0, &Cache::default());
        assert_eq!(out.get_pixel(70, 120)[0], 0, "under: redacted");
        assert_eq!(*out.get_pixel(220, 120), image::Rgba(RED), "above: visible");
    }

    #[test]
    fn background_padding_and_window_layout() {
        let base = RgbaImage::from_pixel(400, 300, image::Rgba([10, 200, 30, 255]));
        let mut d = Document::new("a.png", (400, 300));
        d.frame.enabled = true;
        d.frame.padding = 50;
        d.frame.radius = 0;
        d.frame.shadow_opacity = 0.0;
        d.frame.background = Background::Solid { color: [0, 0, 255], grain: 0.0 };
        let out = render(&d, &base, 1.0, &Cache::default());
        assert_eq!(out.dimensions(), (500, 400));
        assert_eq!(*out.get_pixel(10, 10), image::Rgba([0, 0, 255, 255]), "background in the padding");
        assert_eq!(*out.get_pixel(250, 200), image::Rgba([10, 200, 30, 255]), "image in the middle");
        assert_eq!(*out.get_pixel(50, 50), image::Rgba([10, 200, 30, 255]), "image starts at the padding");
        assert_eq!(*out.get_pixel(49, 49), image::Rgba([0, 0, 255, 255]));
        // Rounded corners show the background; a shadow darkens below the window.
        d.frame.radius = 40;
        d.frame.shadow_opacity = 0.6;
        let out = render(&d, &base, 1.0, &Cache::default());
        let c = out.get_pixel(52, 52);
        assert!(c[0] < 20 && c[1] < 20 && c[2] > 150, "corner is background: {c:?}");
        assert!(out.get_pixel(250, 365)[2] < 230, "shadow below");
        // Mac bar on top of the content.
        d.frame.window = WindowStyle::Mac;
        d.frame.radius = 0;
        let out = render(&d, &base, 1.0, &Cache::default());
        assert_eq!(out.dimensions(), (500, 428));
        assert_eq!(*out.get_pixel(250, 60), image::Rgba([0x2c, 0x2c, 0x30, 255]), "bar");
        assert_eq!(*out.get_pixel(250, 100), image::Rgba([10, 200, 30, 255]), "content below the bar");
        // Fixed 1:1 canvas.
        d.frame.size = OutputSize::Aspect(1, 1);
        assert_eq!(render(&d, &base, 1.0, &Cache::default()).dimensions(), (500, 500));
        // Gradient and image backgrounds reuse the app's backgrounds.
        d.frame.background = Background::default();
        let g = render(&d, &base, 1.0, &Cache::default());
        d.frame.background = Background::Image(crate::backgrounds::ImageBg::new(crate::backgrounds::ImageSource::Builtin(0)));
        let i = render(&d, &base, 1.0, &Cache::default());
        assert_ne!(g.get_pixel(5, 5), i.get_pixel(5, 5));
    }

    #[test]
    fn proxy_scale_matches_full_layout() {
        let base = RgbaImage::from_pixel(800, 600, image::Rgba([255, 255, 255, 255]));
        let mut d = Document::new("a.png", (800, 600));
        d.add(Kind::Shape(Shape { kind: ShapeKind::Rect, rect: r(200.0, 100.0, 100.0, 60.0), stroke: None, fill: Some(RED), width: 0.0, radius: 0.0 }));
        d.frame.enabled = true;
        d.frame.padding = 40;
        let full = render(&d, &base, 1.0, &Cache::default());
        let half = render(&d, &base, 0.5, &Cache::default());
        assert_eq!((half.width() * 2, half.height() * 2), full.dimensions());
        // The rect's center at 1× is at (40+250, 40+130); at 0.5× half of that.
        assert_eq!(*full.get_pixel(290, 170), image::Rgba(RED));
        assert_eq!(*half.get_pixel(145, 85), image::Rgba(RED));
        let at2 = render(&d, &base, 2.0, &Cache::default());
        assert_eq!(at2.dimensions(), (full.width() * 2, full.height() * 2));
        assert_eq!(*at2.get_pixel(580, 340), image::Rgba(RED));
    }

    #[test]
    fn crop_and_orientation() {
        let mut base = RgbaImage::from_pixel(200, 100, image::Rgba([255, 255, 255, 255]));
        base.put_pixel(0, 0, image::Rgba(RED));
        let mut d = Document::new("a.png", (200, 100));
        d.rotate_image(true);
        let out = render(&d, &base, 1.0, &Cache::default());
        assert_eq!(out.dimensions(), (100, 200));
        assert_eq!(*out.get_pixel(99, 0), image::Rgba(RED), "top-left went to the top-right");
        d.set_crop(r(50.0, 0.0, 50.0, 50.0));
        let out = render(&d, &base, 1.0, &Cache::default());
        assert_eq!(out.dimensions(), (50, 50));
        assert_eq!(*out.get_pixel(49, 0), image::Rgba(RED));
        // A crop reaching past the image is transparent there.
        d.set_crop(r(80.0, -20.0, 40.0, 40.0));
        let out = render(&d, &base, 1.0, &Cache::default());
        assert_eq!(out.get_pixel(30, 5)[3], 0);
    }

    /// Timing on a 4K screenshot (run with --ignored --nocapture).
    #[test]
    #[ignore]
    fn render_speed_4k() {
        let base = busy(3840, 2160);
        let mut d = Document::new("a.png", (3840, 2160));
        d.add(Kind::Redact(Redact { area: Area::Rect(r(100.0, 100.0, 1200.0, 500.0)), effect: Effect::Blur(40.0) }));
        d.add(Kind::Arrow(Arrow { from: p(2000.0, 1500.0), to: p(1400.0, 600.0), bend: 0.2, double: false, head: Head::Filled, color: RED, width: 12.0, ..Default::default() }));
        d.add(Kind::Text(Text { rect: r(2000.0, 1600.0, 600.0, 100.0), text: "Hello".into(), font: String::new(), size: 80.0, weight: 700, color: RED, align: Align::Left, pill: Some([0, 0, 0, 255]), outline: None, shadow: true }));
        d.frame.enabled = true;
        let cache = Cache::default();
        for s in [0.2f32, 0.4, 0.4, 1.0] {
            let t = std::time::Instant::now();
            let img = render(&d, &base, s, &cache);
            println!("scale {s}: {}x{} in {:?}", img.width(), img.height(), t.elapsed());
        }
        // Dragging an object above the redaction: proxy frames reuse the cached pieces.
        render(&d, &base, 0.2, &cache);
        for i in 0..3 {
            d.translate(&[2], 5.0, 5.0);
            let t = std::time::Instant::now();
            render(&d, &base, 0.2, &cache);
            println!("drag frame {i} at 0.2: {:?}", t.elapsed());
        }
    }

    #[test]
    fn every_tool_draws() {
        let base = RgbaImage::from_pixel(600, 400, image::Rgba([255, 255, 255, 255]));
        let kinds = vec![
            Kind::Arrow(Arrow { from: p(20.0, 20.0), to: p(200.0, 120.0), bend: 0.3, double: true, head: Head::Filled, color: RED, width: 6.0, ..Default::default() }),
            Kind::Line(Line { from: p(20.0, 300.0), to: p(200.0, 300.0), color: RED, width: 4.0 }),
            Kind::Shape(Shape { kind: ShapeKind::RoundRect, rect: r(300.0, 20.0, 120.0, 80.0), stroke: Some(RED), fill: Some([0, 0, 255, 100]), width: 4.0, radius: 16.0 }),
            Kind::Text(Text { rect: r(300.0, 150.0, 0.0, 0.0), text: "Hello 👋".into(), font: String::new(), size: 32.0, weight: 700, color: RED, align: Align::Left, pill: None, outline: None, shadow: false }),
            Kind::Highlight(Highlight { mark: Mark::Rect(r(20.0, 200.0, 150.0, 30.0)), color: [255, 230, 0, 255] }),
            Kind::Step(Step { center: p(500.0, 300.0), radius: 20.0, n: 3, color: RED }),
            Kind::Pen(Pen { points: smooth(&[p(300.0, 300.0), p(330.0, 340.0), p(360.0, 300.0)], 2), color: RED, width: 5.0 }),
            Kind::Emoji(Emoji { rect: r(450.0, 20.0, 64.0, 64.0), emoji: "🔥".into() }),
            Kind::Sticker(Sticker { rect: r(450.0, 120.0, 80.0, 80.0), src: StickerSrc::Builtin("badge-check".into()) }),
        ];
        for kind in kinds {
            let mut d = Document::new("a.png", (600, 400));
            let id = d.add(kind.clone());
            if let Some(Object { kind: Kind::Text(t), .. }) = d.get_mut(id) {
                let (w, h) = text::measure(t);
                t.rect.w = w;
                t.rect.h = h;
            }
            let out = render(&d, &base, 1.0, &Cache::default());
            let b = d.get(id).unwrap().outer_bounds().inflate(2.0);
            let changed = (b.y.max(0.0) as u32..(b.bottom() as u32).min(400))
                .flat_map(|y| (b.x.max(0.0) as u32..(b.right() as u32).min(600)).map(move |x| (x, y)))
                .filter(|&(x, y)| out.get_pixel(x, y) != base.get_pixel(x, y))
                .count();
            assert!(changed > 100, "{kind:?} drew only {changed} pixels");
        }
    }
}
