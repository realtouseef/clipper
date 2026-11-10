//! Caption style and rasterization, shared by the preview and the export (the ffmpeg here
//! has no text filter, so captions are drawn into RGBA images).
//!
//! Text is shaped with rustybuzz (so Urdu, Arabic and other joined or right-to-left scripts
//! render correctly) and drawn with ab_glyph. When the chosen font lacks a script's glyphs,
//! a macOS system font that has them is used for that caption.
//!
//! Placement: captions live in a safe area, either the visible screen region or the canvas
//! (minus a split webcam's region, so the webcam never covers them), at the top, middle or
//! bottom. Text wraps to the area's width; if it's still too tall it shrinks, never below
//! a minimum size, and lines that still don't fit are dropped rather than overflowing.

use super::Caption;
use crate::layout::{Frame, Rect};
use crate::style::Color;
use ab_glyph::{Font, FontRef, GlyphId, PxScale, point};
use image::{Rgba, RgbaImage};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum Position {
    Top,
    Middle,
    Bottom,
}

impl Position {
    pub const ALL: [Position; 3] = [Position::Top, Position::Middle, Position::Bottom];
    pub fn label(self) -> &'static str {
        match self {
            Position::Top => "Top",
            Position::Middle => "Middle",
            Position::Bottom => "Bottom",
        }
    }
}

/// Where captions may go.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum Area {
    /// Inside the screen recording's rect.
    Screen,
    /// The canvas safe area (minus a split webcam's region).
    Canvas,
}

impl Area {
    pub const ALL: [Area; 2] = [Area::Screen, Area::Canvas];
    pub fn label(self) -> &'static str {
        match self {
            Area::Screen => "On screen",
            Area::Canvas => "Canvas",
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct CaptionStyle {
    /// Index into `fonts()`.
    pub font: usize,
    /// Text size in output px on a 1080 px-wide (short side) canvas.
    pub size: f32,
    pub color: Color,
    /// Outline width in output px at 1080 (0 = none).
    pub outline: f32,
    pub outline_color: Color,
    pub pill: bool,
    pub pill_color: Color,
    pub pill_opacity: f32,
    pub position: Position,
    pub area: Area,
    pub highlight: bool,
    pub highlight_color: Color,
    pub uppercase: bool,
}

impl Default for CaptionStyle {
    fn default() -> Self {
        Self {
            font: 0,
            size: 64.0,
            color: [255, 255, 255],
            outline: 5.0,
            outline_color: [0, 0, 0],
            pill: false,
            pill_color: [0, 0, 0],
            pill_opacity: 0.6,
            position: Position::Bottom,
            area: Area::Canvas,
            highlight: true,
            highlight_color: [0xa4, 0xf4, 0x7c],
            uppercase: false,
        }
    }
}

pub const MIN_SIZE: f32 = 30.0;
pub const SIZES: (f32, f32) = (32.0, 110.0);

impl CaptionStyle {
    /// A key that changes whenever the rendered pixels could.
    pub fn key(&self) -> String {
        format!("{self:?}")
    }
}

// ---------- fonts ----------

pub struct FontChoice {
    pub name: String,
    /// `None` = the bundled default.
    pub path: Option<PathBuf>,
}

static BUNDLED: &[u8] = include_bytes!("../../assets/fonts/Poppins-Bold.ttf");

/// The bundled default, then the system's fonts (scanned once).
pub fn fonts() -> &'static [FontChoice] {
    static LIST: OnceLock<Vec<FontChoice>> = OnceLock::new();
    LIST.get_or_init(|| {
        let mut list = vec![FontChoice { name: "Poppins Bold".into(), path: None }];
        let home = std::env::var("HOME").unwrap_or_default();
        let dirs = [
            PathBuf::from("/System/Library/Fonts"),
            PathBuf::from("/System/Library/Fonts/Supplemental"),
            PathBuf::from("/Library/Fonts"),
            PathBuf::from(home).join("Library/Fonts"),
        ];
        let mut found: Vec<FontChoice> = Vec::new();
        for dir in dirs {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for e in entries.flatten() {
                let p = e.path();
                let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
                if !matches!(ext.as_str(), "ttf" | "otf" | "ttc") {
                    continue;
                }
                let name = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                // Symbol, emoji and braille fonts aren't for captions.
                if name.is_empty() || ["Emoji", "Braille", "Symbol", "LastResort", "Keyboard", "Wingding", "Webdings"].iter().any(|s| name.contains(s)) {
                    continue;
                }
                found.push(FontChoice { name, path: Some(p) });
            }
        }
        found.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        found.dedup_by(|a, b| a.name == b.name);
        list.extend(found);
        list
    })
}

/// A loaded font: the file bytes (kept for the app's lifetime) and face index.
struct Loaded {
    data: &'static [u8],
    index: u32,
}

impl Loaded {
    fn ab(&self) -> Option<FontRef<'static>> {
        FontRef::try_from_slice_and_index(self.data, self.index).ok()
    }
    fn rb(&self) -> Option<rustybuzz::Face<'static>> {
        rustybuzz::Face::from_slice(self.data, self.index)
    }
    fn covers(&self, text: &str) -> bool {
        let Some(f) = self.ab() else { return false };
        text.chars().filter(|c| !c.is_whitespace()).all(|c| f.glyph_id(c).0 != 0)
    }
}

fn load(path: Option<&PathBuf>) -> Option<&'static Loaded> {
    static CACHE: OnceLock<Mutex<HashMap<Option<PathBuf>, Option<&'static Loaded>>>> = OnceLock::new();
    let mut cache = CACHE.get_or_init(Default::default).lock().unwrap();
    *cache.entry(path.cloned()).or_insert_with(|| {
        let data: &'static [u8] = match path {
            None => BUNDLED,
            Some(p) => Box::leak(std::fs::read(p).ok()?.into_boxed_slice()),
        };
        let l: &'static Loaded = Box::leak(Box::new(Loaded { data, index: 0 }));
        l.ab().map(|_| l)
    })
}

/// The chosen font if it has every glyph of `text`, else the first system fallback that does.
fn font_for(choice: usize, text: &str) -> Option<&'static Loaded> {
    let list = fonts();
    let chosen = load(list.get(choice).and_then(|c| c.path.as_ref()));
    if let Some(f) = chosen.filter(|f| f.covers(text)) {
        return Some(f);
    }
    const FALLBACKS: [&str; 6] = [
        "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        "/System/Library/Fonts/GeezaPro.ttc",
        "/System/Library/Fonts/Kohinoor.ttc",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        "/System/Library/Fonts/AppleSDGothicNeo.ttc",
        "/System/Library/Fonts/SFNS.ttf",
    ];
    FALLBACKS
        .iter()
        .filter_map(|p| load(Some(&PathBuf::from(p))))
        .find(|f| f.covers(text))
        .or(chosen)
}

// ---------- layout ----------

/// The rect captions must stay inside, in output px.
pub fn area(frame: &Frame, style: &CaptionStyle) -> Rect {
    let (cw, ch) = frame.canvas;
    let short = cw.min(ch) as f32;
    let inset = |r: Rect, mx: u32, my: u32| Rect {
        x: r.x + mx,
        y: r.y + my,
        w: r.w.saturating_sub(2 * mx).max(2),
        h: r.h.saturating_sub(2 * my).max(2),
    };
    match style.area {
        Area::Screen => {
            let m = (frame.screen.w.min(frame.screen.h) as f32 * 0.05) as u32;
            inset(frame.screen, m, m)
        }
        Area::Canvas => {
            // Platform safe margins: generous at top and bottom of vertical video (app UI).
            let portrait = ch > cw;
            let mx = (short * 0.06) as u32;
            let my = (ch as f32 * if portrait { 0.10 } else { 0.07 }) as u32;
            let mut r = inset(Rect { x: 0, y: 0, w: cw, h: ch }, mx, my);
            // A split webcam's region is off limits: keep to the screen's side of the split.
            if let Some(cam) = frame.cam.filter(|c| !c.floating) {
                let (c, sc) = (cam.rect, frame.screen);
                let gap = (short * 0.03) as u32;
                let (mut x0, mut y0, mut x1, mut y1) = (r.x, r.y, r.x + r.w, r.y + r.h);
                if c.y + c.h <= sc.y {
                    y0 = y0.max(c.y + c.h + gap); // webcam above
                } else if c.y >= sc.y + sc.h {
                    y1 = y1.min(c.y.saturating_sub(gap)); // webcam below
                } else if c.x + c.w <= sc.x {
                    x0 = x0.max(c.x + c.w + gap); // webcam left
                } else {
                    x1 = x1.min(c.x.saturating_sub(gap)); // webcam right
                }
                r = Rect { x: x0, y: y0, w: x1.saturating_sub(x0).max(2), h: y1.saturating_sub(y0).max(2) };
            }
            r
        }
    }
}

/// A rendered caption block and where it goes (top-left), in canvas px × scale.
pub struct Placed {
    pub image: RgbaImage,
    pub x: i32,
    pub y: i32,
}

struct Glyph {
    id: GlyphId,
    x: f32,
    y: f32,
}

/// A shaped word: glyphs relative to its origin, and its advance.
struct ShapedWord {
    glyphs: Vec<Glyph>,
    width: f32,
    highlight: bool,
}

fn is_rtl(text: &str) -> bool {
    text.chars().find(|c| c.is_alphabetic()).is_some_and(|c| {
        let u = c as u32;
        (0x0590..=0x08FF).contains(&u) || (0xFB1D..=0xFDFF).contains(&u) || (0xFE70..=0xFEFF).contains(&u)
    })
}

/// Shapes `text` at `px` (em size in pixels).
fn shape(font: &Loaded, text: &str, px: f32, rtl: bool) -> (Vec<Glyph>, f32) {
    let Some(face) = font.rb() else { return (Vec::new(), 0.0) };
    let k = px / face.units_per_em() as f32;
    let mut buf = rustybuzz::UnicodeBuffer::new();
    buf.push_str(text);
    buf.set_direction(if rtl { rustybuzz::Direction::RightToLeft } else { rustybuzz::Direction::LeftToRight });
    buf.guess_segment_properties();
    let out = rustybuzz::shape(&face, &[], buf);
    let mut x = 0.0;
    let glyphs = out
        .glyph_infos()
        .iter()
        .zip(out.glyph_positions())
        .map(|(info, pos)| {
            let g = Glyph { id: GlyphId(info.glyph_id as u16), x: x + pos.x_offset as f32 * k, y: -pos.y_offset as f32 * k };
            x += pos.x_advance as f32 * k;
            g
        })
        .collect();
    (glyphs, x)
}

/// Renders `caption` (with word `word` highlighted) in `style` for `frame`, at `scale` px per
/// output px. `None` for an empty caption.
pub fn render(caption: &Caption, word: Option<usize>, style: &CaptionStyle, frame: &Frame, scale: f32) -> Option<Placed> {
    let text = if style.uppercase { caption.text.to_uppercase() } else { caption.text.clone() };
    let lines: Vec<Vec<&str>> = text.lines().map(|l| l.split_whitespace().collect()).filter(|l: &Vec<&str>| !l.is_empty()).collect();
    if lines.is_empty() {
        return None;
    }
    let all: String = lines.iter().flatten().copied().collect::<Vec<_>>().join(" ");
    let font = font_for(style.font, &all)?;
    let ab = font.ab()?;
    // Highlight needs one timed word per displayed word.
    let n_tokens: usize = lines.iter().map(|l| l.len()).sum();
    let hl = if style.highlight && caption.words.len() == n_tokens { word } else { None };
    let rtl = is_rtl(&all);

    let r = area(frame, style);
    let short = frame.canvas.0.min(frame.canvas.1) as f32 / 1080.0;
    let (aw, ah) = (r.w as f32 * scale, r.h as f32 * scale);
    let want = style.size.max(MIN_SIZE) * short * scale;
    let min = MIN_SIZE * short * scale;
    let outline = style.outline * short * scale;

    // Fit: shrink until the wrapped text fits the area (never below `min`).
    let mut px = want;
    let (rows, line_h, pad) = loop {
        let pad = if style.pill { px * 0.45 } else { outline };
        let max_w = (aw - 2.0 * pad).max(px);
        let (space, _) = {
            let (_, w) = shape(font, " ", px, false);
            (w, ())
        };
        let mut rows: Vec<Vec<ShapedWord>> = Vec::new();
        let mut k = 0;
        for line in &lines {
            let mut row: Vec<ShapedWord> = Vec::new();
            let mut row_w = 0.0;
            for tok in line {
                let (glyphs, width) = shape(font, tok, px, rtl);
                let w = ShapedWord { glyphs, width, highlight: hl == Some(k) };
                k += 1;
                if !row.is_empty() && row_w + space + w.width > max_w {
                    rows.push(std::mem::take(&mut row));
                    row_w = 0.0;
                }
                row_w += if row.is_empty() { w.width } else { space + w.width };
                row.push(w);
            }
            rows.push(row);
        }
        let line_h = px * 1.22;
        let total = rows.len() as f32 * line_h + 2.0 * pad;
        if total <= ah || px <= min {
            // Lines that still don't fit are dropped (never overflow the safe area).
            let fit = (((ah - 2.0 * pad) / line_h).floor() as usize).max(1);
            rows.truncate(fit);
            break (rows, line_h, pad);
        }
        px = (px * 0.9).max(min);
    };
    let (space, _) = {
        let (_, w) = shape(font, " ", px, false);
        (w, ())
    };
    let row_w = |row: &Vec<ShapedWord>| row.iter().map(|w| w.width).sum::<f32>() + space * row.len().saturating_sub(1) as f32;
    let block_w = rows.iter().map(row_w).fold(0.0, f32::max).min(aw - 2.0 * pad) + 2.0 * pad;
    let block_h = rows.len() as f32 * line_h + 2.0 * pad;
    let (bw, bh) = (block_w.ceil().max(1.0) as u32, block_h.ceil().max(1.0) as u32);

    // Coverage of the text (and which pixels belong to the highlighted word).
    let mut cov = vec![0f32; (bw * bh) as usize];
    let mut hil = vec![false; (bw * bh) as usize];
    let scaled = PxScale::from(px * ab.height_unscaled() / ab.units_per_em().unwrap_or(1000.0));
    let ascent = ab.ascent_unscaled() * px / ab.units_per_em().unwrap_or(1000.0);
    for (i, row) in rows.iter().enumerate() {
        let w = row_w(row);
        let mut x = (block_w - w) / 2.0;
        let baseline = pad + i as f32 * line_h + (line_h - px * 1.0) / 2.0 + ascent * 0.92;
        let order: Vec<&ShapedWord> = if rtl { row.iter().rev().collect() } else { row.iter().collect() };
        for word in order {
            for g in &word.glyphs {
                let glyph = g.id.with_scale_and_position(scaled, point(x + g.x, baseline + g.y));
                if let Some(o) = ab.outline_glyph(glyph) {
                    let bb = o.px_bounds();
                    o.draw(|gx, gy, c| {
                        let (px_, py_) = (bb.min.x as i32 + gx as i32, bb.min.y as i32 + gy as i32);
                        if px_ >= 0 && py_ >= 0 && (px_ as u32) < bw && (py_ as u32) < bh {
                            let i = (py_ as u32 * bw + px_ as u32) as usize;
                            cov[i] = (cov[i] + c).min(1.0);
                            if word.highlight && c > 0.0 {
                                hil[i] = true;
                            }
                        }
                    });
                }
            }
            x += word.width + space;
        }
    }

    // Outline: the coverage dilated by a disc.
    let rad = outline.round() as i32;
    let ring = if rad > 0 {
        let mut out = vec![0f32; cov.len()];
        let offs: Vec<(i32, i32, f32)> = (-rad..=rad)
            .flat_map(|dy| (-rad..=rad).map(move |dx| (dx, dy)))
            .filter_map(|(dx, dy)| {
                let d = ((dx * dx + dy * dy) as f32).sqrt();
                (d <= rad as f32 + 0.5).then_some((dx, dy, (rad as f32 + 0.5 - d).clamp(0.0, 1.0)))
            })
            .collect();
        for y in 0..bh as i32 {
            for x in 0..bw as i32 {
                let c = cov[(y as u32 * bw + x as u32) as usize];
                if c <= 0.0 {
                    continue;
                }
                for &(dx, dy, f) in &offs {
                    let (nx, ny) = (x + dx, y + dy);
                    if nx >= 0 && ny >= 0 && (nx as u32) < bw && (ny as u32) < bh {
                        let o = &mut out[(ny as u32 * bw + nx as u32) as usize];
                        *o = o.max(c * f);
                    }
                }
            }
        }
        Some(out)
    } else {
        None
    };

    // Composite: pill, outline, text.
    let radius = (block_h.min(px * 1.6)) * 0.35;
    let pill = style.pill.then(|| (style.pill_color, style.pill_opacity.clamp(0.0, 1.0)));
    let mut img = RgbaImage::new(bw, bh);
    for y in 0..bh {
        for x in 0..bw {
            let i = (y * bw + x) as usize;
            let mut p = [0f32; 4];
            if let Some((c, a)) = pill {
                let cov = rounded(x as f32 + 0.5, y as f32 + 0.5, block_w, block_h, radius);
                blend(&mut p, c, a * cov);
            }
            if let Some(ring) = &ring {
                blend(&mut p, style.outline_color, ring[i]);
            }
            let fill = if hil[i] { style.highlight_color } else { style.color };
            blend(&mut p, fill, cov[i]);
            img.put_pixel(x, y, Rgba([(p[0] * 255.0) as u8, (p[1] * 255.0) as u8, (p[2] * 255.0) as u8, (p[3] * 255.0) as u8]));
        }
    }

    // Place in the area: centered, at the top, middle or bottom.
    let x = r.x as f32 * scale + (aw - block_w) / 2.0;
    let y = r.y as f32 * scale
        + match style.position {
            Position::Top => 0.0,
            Position::Middle => (ah - block_h) / 2.0,
            Position::Bottom => ah - block_h,
        };
    Some(Placed { image: img, x: x.round() as i32, y: y.round() as i32 })
}

/// Writes the captions as a timed image sequence for ffmpeg's concat demuxer: one RGBA PNG
/// (the size of the caption area) per caption state (each highlighted word gets its own),
/// transparent between captions. Returns the list file and the area it's overlaid at.
pub fn export_frames(
    caps: &super::Captions,
    style: &CaptionStyle,
    frame: &Frame,
    duration: f64,
    dir: &std::path::Path,
) -> anyhow::Result<Option<(std::path::PathBuf, Rect)>> {
    use std::fmt::Write as _;
    if caps.lines.is_empty() {
        return Ok(None);
    }
    let r = area(frame, style);
    let out = dir.join("captions");
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out)?;
    let blank = out.join("blank.png");
    RgbaImage::new(r.w, r.h).save(&blank)?;
    let mut list = String::from("ffconcat version 1.0\n");
    let mut push = |file: &std::path::Path, d: f64| {
        if d > 0.0005 {
            let _ = write!(list, "file '{}'\nduration {d:.4}\n", file.display());
        }
    };
    let mut t = 0.0;
    let mut n = 0;
    let mut sorted: Vec<&Caption> = caps.lines.iter().collect();
    sorted.sort_by(|a, b| a.start.total_cmp(&b.start));
    for c in sorted {
        let (start, end) = (c.start.max(t), c.end.min(duration));
        if end <= start {
            continue;
        }
        push(&blank, start - t);
        // One state per highlighted word, or a single state.
        let mut cuts: Vec<(f64, Option<usize>)> = vec![(start, if style.highlight { c.word_at(start) } else { None })];
        if style.highlight {
            for (k, w) in c.words.iter().enumerate().skip(1) {
                if w.start > start && w.start < end {
                    cuts.push((w.start, Some(k)));
                }
            }
        }
        for (i, &(at, word)) in cuts.iter().enumerate() {
            let until = cuts.get(i + 1).map_or(end, |c| c.0);
            let file = out.join(format!("c{n:05}.png"));
            n += 1;
            let mut img = RgbaImage::new(r.w, r.h);
            if let Some(p) = render(c, word, style, frame, 1.0) {
                image::imageops::overlay(&mut img, &p.image, p.x as i64 - r.x as i64, p.y as i64 - r.y as i64);
            }
            img.save(&file)?;
            push(&file, until - at);
        }
        t = end;
    }
    push(&blank, (duration - t).max(0.04));
    // The concat demuxer needs the last file listed again to honour its duration.
    let _ = write!(list, "file '{}'\n", blank.display());
    let path = dir.join("captions.txt");
    std::fs::write(&path, list)?;
    Ok(Some((path, r)))
}

fn rounded(x: f32, y: f32, w: f32, h: f32, r: f32) -> f32 {
    let qx = (x - w / 2.0).abs() - (w / 2.0 - r);
    let qy = (y - h / 2.0).abs() - (h / 2.0 - r);
    let d = qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r;
    (0.5 - d).clamp(0.0, 1.0)
}

/// Straight-alpha "over" of a flat color.
fn blend(dst: &mut [f32; 4], c: Color, a: f32) {
    if a <= 0.0 {
        return;
    }
    let out = a + dst[3] * (1.0 - a);
    for i in 0..3 {
        dst[i] = (c[i] as f32 / 255.0 * a + dst[i] * dst[3] * (1.0 - a)) / out;
    }
    dst[3] = out;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::captions::Word;
    use crate::layout::{Corner, Layout, Placement, Ratio, Shape, Side};
    use crate::style::Style;

    fn caption(text: &str) -> Caption {
        let words: Vec<Word> = text
            .split_whitespace()
            .enumerate()
            .map(|(i, t)| Word { start: i as f64 * 0.3, end: i as f64 * 0.3 + 0.25, text: t.into() })
            .collect();
        Caption::from_words(words, 32)
    }

    fn layouts() -> Vec<Layout> {
        let mut v = Vec::new();
        for ratio in Ratio::ALL {
            for placement in Placement::options(ratio) {
                for split in [0.25, 0.5, 0.75] {
                    v.push(Layout { ratio, placement, split, ..Layout::default() });
                }
            }
        }
        v
    }

    fn overlaps(a: (i32, i32, i32, i32), b: Rect) -> bool {
        let (x, y, w, h) = a;
        x < (b.x + b.w) as i32 && (b.x as i32) < x + w && y < (b.y + b.h) as i32 && (b.y as i32) < y + h
    }

    /// In every layout, ratio, position and area: the caption stays inside the canvas and its
    /// safe area, and never touches a split webcam's region. Long text wraps instead of
    /// overflowing, and the size never drops below the minimum.
    #[test]
    fn captions_fit_and_avoid_the_webcam() {
        let long = caption("This is a very long caption that keeps going well past what fits on a single line of text");
        let short = caption("Hello there");
        for l in layouts() {
            let frame = l.frame(&Style::default(), true);
            for area in Area::ALL {
                for position in Position::ALL {
                    for size in [SIZES.0, 64.0, SIZES.1] {
                        let style = CaptionStyle { area, position, size, pill: true, ..CaptionStyle::default() };
                        let safe = super::area(&frame, &style);
                        for c in [&long, &short] {
                            let p = render(c, Some(1), &style, &frame, 0.5).unwrap();
                            let (x, y, w, h) = (p.x, p.y, p.image.width() as i32, p.image.height() as i32);
                            let s = |v: u32| (v as f32 * 0.5).round() as i32;
                            assert!(x >= s(safe.x) - 1 && y >= s(safe.y) - 1, "{l:?} {style:?}: block at {x},{y} outside {safe:?}");
                            assert!(x + w <= s(safe.x + safe.w) + 1 && y + h <= s(safe.y + safe.h) + 1, "{l:?} {style:?}: block {x},{y} {w}x{h} overflows {safe:?}");
                            if let Some(cam) = frame.cam.filter(|c| !c.floating) {
                                let r = cam.rect;
                                let cam_s = Rect { x: s(r.x) as u32, y: s(r.y) as u32, w: s(r.w) as u32, h: s(r.h) as u32 };
                                assert!(!overlaps((x, y, w, h), cam_s), "{l:?} {style:?}: caption over the webcam");
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn highlight_and_style() {
        let frame = Layout::default().frame(&Style::default(), false);
        let style = CaptionStyle { outline: 0.0, ..CaptionStyle::default() };
        let c = caption("one two");
        let p = render(&c, Some(0), &style, &frame, 1.0).unwrap();
        // Left word green (highlighted), right word white.
        let (w, h) = p.image.dimensions();
        let count = |x0: u32, x1: u32, pred: &dyn Fn(&Rgba<u8>) -> bool| {
            (x0..x1).flat_map(|x| (0..h).map(move |y| (x, y))).filter(|&(x, y)| pred(p.image.get_pixel(x, y))).count()
        };
        let green = |q: &Rgba<u8>| q[3] > 200 && q[1] > 200 && q[0] < 200;
        let white = |q: &Rgba<u8>| q[3] > 200 && q[0] > 230 && q[1] > 230 && q[2] > 230;
        assert!(count(0, w / 2, &green) > 50 && count(0, w / 2, &white) < 10);
        assert!(count(w / 2, w, &white) > 50 && count(w / 2, w, &green) < 10);
        // Uppercase renders wider text than lowercase.
        let up = render(&c, None, &CaptionStyle { uppercase: true, ..style.clone() }, &frame, 1.0).unwrap();
        assert!(up.image.width() > w);
    }

    #[test]
    fn urdu_shapes_with_a_fallback_font() {
        let frame = Layout::default().frame(&Style::default(), false);
        let c = caption("یہ ایک ٹیسٹ ہے");
        let p = render(&c, None, &CaptionStyle::default(), &frame, 0.5).expect("urdu caption");
        let ink = p.image.pixels().filter(|q| q[3] > 128).count();
        assert!(ink > 200, "no glyphs drawn ({ink})");
        let _ = (Corner::TopLeft, Shape::Circle, Side::Top);
    }

    /// `CLIPPER_KEEP=<dir> cargo test caption_samples` writes sample renders for a look.
    #[test]
    fn caption_samples() {
        let Some(dir) = std::env::var_os("CLIPPER_KEEP") else { return };
        let frame = Layout::default().frame(&Style::default(), false);
        let samples = [
            ("plain", caption("So today I want to\nshow you how this works"), CaptionStyle::default(), Some(2)),
            ("pill", caption("First we open the settings"), CaptionStyle { pill: true, outline: 0.0, uppercase: true, ..CaptionStyle::default() }, Some(1)),
            ("urdu", caption("یہ ایک ٹیسٹ ہے"), CaptionStyle::default(), None),
        ];
        for (name, c, style, w) in samples {
            let p = render(&c, w, &style, &frame, 0.5).unwrap();
            let mut bg = RgbaImage::from_pixel(p.image.width() + 40, p.image.height() + 40, Rgba([90, 110, 140, 255]));
            image::imageops::overlay(&mut bg, &p.image, 20, 20);
            bg.save(std::path::Path::new(&dir).join(format!("cap-{name}.png"))).unwrap();
        }
    }
}
