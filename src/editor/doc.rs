//! The screenshot editor's document: the original image plus an ordered list of
//! non-destructive objects, the crop/orientation and the background frame. No UI code.
//!
//! Coordinates are pixels of the *oriented* original (after rotate/flip), so objects stay on
//! the content they annotate when the crop changes. Rotating or flipping the image maps every
//! object and the crop along with it.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub type Rgba = [u8; 4];

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug, Default)]
pub struct P {
    pub x: f32,
    pub y: f32,
}

pub const fn p(x: f32, y: f32) -> P {
    P { x, y }
}

impl P {
    pub fn dist(self, o: P) -> f32 {
        ((self.x - o.x).powi(2) + (self.y - o.y).powi(2)).sqrt()
    }
    /// Rotates around `c` by `deg` degrees (clockwise on screen, y down).
    pub fn rotate(self, c: P, deg: f32) -> P {
        if deg == 0.0 {
            return self;
        }
        let (s, k) = deg.to_radians().sin_cos();
        let (dx, dy) = (self.x - c.x, self.y - c.y);
        p(c.x + dx * k - dy * s, c.y + dx * s + dy * k)
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug, Default)]
pub struct R {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

pub const fn r(x: f32, y: f32, w: f32, h: f32) -> R {
    R { x, y, w, h }
}

impl R {
    pub fn from_points(a: P, b: P) -> R {
        r(a.x.min(b.x), a.y.min(b.y), (a.x - b.x).abs(), (a.y - b.y).abs())
    }
    pub fn bbox(points: &[P]) -> R {
        let Some(first) = points.first() else { return R::default() };
        let (mut x0, mut y0, mut x1, mut y1) = (first.x, first.y, first.x, first.y);
        for q in points {
            x0 = x0.min(q.x);
            y0 = y0.min(q.y);
            x1 = x1.max(q.x);
            y1 = y1.max(q.y);
        }
        r(x0, y0, x1 - x0, y1 - y0)
    }
    pub fn center(&self) -> P {
        p(self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn contains(&self, q: P) -> bool {
        q.x >= self.x && q.x <= self.right() && q.y >= self.y && q.y <= self.bottom()
    }
    pub fn inflate(&self, d: f32) -> R {
        r(self.x - d, self.y - d, self.w + 2.0 * d, self.h + 2.0 * d)
    }
    pub fn translate(&self, dx: f32, dy: f32) -> R {
        r(self.x + dx, self.y + dy, self.w, self.h)
    }
    pub fn union(&self, o: &R) -> R {
        let x = self.x.min(o.x);
        let y = self.y.min(o.y);
        r(x, y, self.right().max(o.right()) - x, self.bottom().max(o.bottom()) - y)
    }
    pub fn intersect(&self, o: &R) -> Option<R> {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        let (x1, y1) = (self.right().min(o.right()), self.bottom().min(o.bottom()));
        (x1 > x && y1 > y).then(|| r(x, y, x1 - x, y1 - y))
    }
    /// Maps `q` from this rect onto `to` (relative position kept).
    pub fn map(&self, q: P, to: &R) -> P {
        let fx = if self.w.abs() > 1e-6 { (q.x - self.x) / self.w } else { 0.5 };
        let fy = if self.h.abs() > 1e-6 { (q.y - self.y) / self.h } else { 0.5 };
        p(to.x + fx * to.w, to.y + fy * to.h)
    }
    /// The largest rect of aspect `a` (w/h) centered in this one.
    pub fn fit_aspect(&self, a: f32) -> R {
        let (w, h) = if self.w / self.h.max(1e-6) > a { (self.h * a, self.h) } else { (self.w, self.w / a) };
        let c = self.center();
        r(c.x - w / 2.0, c.y - h / 2.0, w, h)
    }
}

// ---------- objects ----------

pub use super::spline::{Anchor, ArrowStyle};
use super::spline;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Head {
    #[default]
    Filled,
    Open,
    None,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ShapeKind {
    #[default]
    Rect,
    RoundRect,
    Ellipse,
}

/// An arrow end attached to another object: the point at `(u, v)` of its bounds (0..1),
/// on the edge it was dropped on. The end follows when the object moves or resizes.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
pub struct Glue {
    pub id: u64,
    pub u: f32,
    pub v: f32,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Arrow {
    /// The ends (kept equal to the first and last anchor).
    pub from: P,
    pub to: P,
    /// Older projects: the curve's control point offset from the midpoint, perpendicular,
    /// as a fraction of the length (0 = straight). Used only until `anchors` is filled.
    pub bend: f32,
    pub double: bool,
    pub head: Head,
    pub color: Rgba,
    pub width: f32,
    #[serde(default)]
    pub style: ArrowStyle,
    /// The spline (anchors with tangent handles). Empty in older projects: derived from
    /// `from`, `to` and `bend`.
    #[serde(default)]
    pub anchors: Vec<Anchor>,
    /// Marker wobble, 0..1 (deterministic from `seed`).
    #[serde(default)]
    pub roughness: f32,
    #[serde(default)]
    pub seed: u32,
    #[serde(default)]
    pub dashed: bool,
    #[serde(default)]
    pub shadow: bool,
    /// Arrowhead length as a multiple of the line width (0 = the default).
    #[serde(default)]
    pub head_size: f32,
    /// Start and end attached to objects.
    #[serde(default)]
    pub glue: [Option<Glue>; 2],
}

impl Default for Arrow {
    fn default() -> Self {
        Self {
            from: P::default(),
            to: P::default(),
            bend: 0.0,
            double: false,
            head: Head::Filled,
            color: [255, 59, 48, 255],
            width: 6.0,
            style: ArrowStyle::Curved,
            anchors: Vec::new(),
            roughness: 0.0,
            seed: 1,
            dashed: false,
            shadow: false,
            head_size: 0.0,
            glue: [None, None],
        }
    }
}

impl Arrow {
    /// The spline: the anchors, or the older curve converted.
    pub fn spline(&self) -> Vec<Anchor> {
        if self.anchors.len() >= 2 {
            return self.anchors.clone();
        }
        if self.bend == 0.0 {
            return spline::fit(&[self.from, self.to]).into_iter().map(|mut a| {
                a.c_in = a.p;
                a.c_out = a.p;
                a
            }).collect();
        }
        spline::from_quadratic(self.from, self.to, self.control())
    }

    /// Makes the anchors explicit (before reshaping).
    pub fn materialize(&mut self) {
        if self.anchors.len() < 2 {
            self.anchors = self.spline();
        }
        self.sync_ends();
    }

    /// `from`/`to` follow the first and last anchor.
    pub fn sync_ends(&mut self) {
        if let (Some(a), Some(b)) = (self.anchors.first(), self.anchors.last()) {
            self.from = a.p;
            self.to = b.p;
        }
    }

    /// The centerline as a polyline (`n` samples per curved segment), before any wobble.
    pub fn path(&self, n: usize) -> Vec<P> {
        spline::sample(&self.spline(), self.style, n)
    }

    /// The line as drawn: with the marker wobble (hand-drawn style or roughness).
    pub fn drawn_path(&self) -> Vec<P> {
        let pts = self.path(32);
        let rough = if self.style == ArrowStyle::Hand { self.roughness.max(0.35) } else { self.roughness };
        spline::wobble(&pts, rough, self.width, self.seed)
    }

    pub fn control(&self) -> P {
        let (dx, dy) = (self.to.x - self.from.x, self.to.y - self.from.y);
        let mid = p((self.from.x + self.to.x) / 2.0, (self.from.y + self.to.y) / 2.0);
        // Normal (rotated -90°) scaled by bend × length.
        p(mid.x + dy * self.bend, mid.y - dx * self.bend)
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Line {
    pub from: P,
    pub to: P,
    pub color: Rgba,
    pub width: f32,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Shape {
    pub kind: ShapeKind,
    pub rect: R,
    pub stroke: Option<Rgba>,
    pub fill: Option<Rgba>,
    pub width: f32,
    pub radius: f32,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Text {
    /// Top-left and laid-out size (the editor measures the text and keeps w/h current).
    pub rect: R,
    pub text: String,
    /// Font family; empty = the bundled default.
    pub font: String,
    pub size: f32,
    pub weight: u16,
    pub color: Rgba,
    pub align: Align,
    pub pill: Option<Rgba>,
    pub outline: Option<Rgba>,
    pub shadow: bool,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub enum Mark {
    Rect(R),
    Free { points: Vec<P>, width: f32 },
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Highlight {
    pub mark: Mark,
    pub color: Rgba,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Step {
    pub center: P,
    pub radius: f32,
    pub n: u32,
    pub color: Rgba,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Pen {
    pub points: Vec<P>,
    pub color: Rgba,
    pub width: f32,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Emoji {
    pub rect: R,
    pub emoji: String,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Hash, Debug)]
pub enum StickerSrc {
    /// Name in the built-in pack.
    Builtin(String),
    /// An imported PNG, SVG or WebP.
    File(PathBuf),
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Sticker {
    pub rect: R,
    pub src: StickerSrc,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub enum Area {
    Rect(R),
    RoundRect(R, f32),
    Ellipse(R),
    Brush { points: Vec<P>, width: f32 },
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
pub enum Effect {
    /// Strength in pixels (raised to a minimum for the image size at render).
    Blur(f32),
    /// Block size in pixels (raised to a minimum for the image size at render).
    Pixelate(f32),
    Solid(Rgba),
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Redact {
    pub area: Area,
    pub effect: Effect,
}

/// One image on a merged canvas: a crop of a picture file, placed (and scaled uniformly)
/// at `rect`.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct ImageLayer {
    /// The picture (a copy kept next to the project; never modified).
    pub src: PathBuf,
    /// Its size in pixels, before orientation.
    pub natural: (u32, u32),
    #[serde(default)]
    pub orient: Orient,
    /// The visible part, in oriented pixels.
    pub crop: R,
    /// Where it sits on the canvas (same aspect as `crop`).
    pub rect: R,
    #[serde(default)]
    pub radius: f32,
    #[serde(default)]
    pub shadow: bool,
}

impl ImageLayer {
    /// Canvas pixels per picture pixel.
    pub fn scale(&self) -> f32 {
        self.rect.w / self.crop.w.max(1e-3)
    }

    /// Shown larger than its pixels.
    pub fn low_res(&self) -> bool {
        self.scale() > 1.001
    }

    pub fn oriented_size(&self) -> (u32, u32) {
        if self.orient.quarter % 2 == 1 { (self.natural.1, self.natural.0) } else { self.natural }
    }

    /// Canvas point → picture pixel (oriented).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn to_source(&self, q: P) -> P {
        self.rect.map(q, &self.crop)
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum CalloutShape {
    #[default]
    Rect,
    Circle,
}

/// The box marking the area a callout magnifies.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct SourceBox {
    pub rect: R,
    pub shape: CalloutShape,
    pub color: Rgba,
    pub width: f32,
    pub radius: f32,
    /// A light tint inside the box.
    pub fill: Option<Rgba>,
    pub dashed: bool,
    /// "Show source highlight": drawn or not (the area is still what the callout shows).
    pub visible: bool,
    /// The callout showing this area.
    pub callout: u64,
}

/// What a callout leaves behind in the source area.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum CutMode {
    /// The source stays as it is.
    #[default]
    Copy,
    /// The source area is blanked.
    Blank,
    /// The source area is blurred.
    Blur,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
pub struct Shadow {
    pub blur: f32,
    /// 0..1
    pub opacity: f32,
    pub offset: f32,
}

/// A magnified, live copy of a source box's area, placed anywhere. It references the area
/// (never a baked copy) and samples the full-resolution, already-redacted pixels.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Callout {
    /// The `SourceBox` object.
    pub source: u64,
    /// Placement: always the source size × `scale` (kept by `Document::sync_links`).
    pub rect: R,
    /// 1.5 to 8.
    pub scale: f32,
    pub shape: CalloutShape,
    pub border: Rgba,
    pub width: f32,
    pub radius: f32,
    pub shadow: Option<Shadow>,
    pub cut: CutMode,
    /// The arrow from the source box to this callout, when "Auto connector" is on.
    pub connector: Option<u64>,
}

impl Callout {
    pub const MIN_SCALE: f32 = 1.5;
    pub const MAX_SCALE: f32 = 8.0;

    /// A point of the source area → where it shows in the callout.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn map_from_source(&self, source: R, q: P) -> P {
        source.map(q, &self.rect)
    }

    /// A point of the callout → the source point it shows.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn map_to_source(&self, source: R, q: P) -> P {
        self.rect.map(q, &source)
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub enum Kind {
    Image(ImageLayer),
    Source(SourceBox),
    Callout(Callout),
    Arrow(Arrow),
    Line(Line),
    Shape(Shape),
    Text(Text),
    Highlight(Highlight),
    Step(Step),
    Pen(Pen),
    Emoji(Emoji),
    Sticker(Sticker),
    Redact(Redact),
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Object {
    pub id: u64,
    pub kind: Kind,
    /// Degrees, clockwise, around the bounds' center.
    #[serde(default)]
    pub rotation: f32,
    #[serde(default)]
    pub locked: bool,
}

/// Width around a point list, for bounds and hit tests.
fn pad_points(points: &[P], width: f32) -> R {
    R::bbox(points).inflate(width / 2.0)
}

impl Kind {
    /// Drawing level, bottom to top: images, annotations and redactions (in list order),
    /// source boxes, callouts, arrows. Reordering works within a level.
    pub fn level(&self) -> u8 {
        match self {
            Kind::Image(_) => 0,
            Kind::Source(_) => 2,
            Kind::Callout(_) => 3,
            Kind::Arrow(_) => 4,
            _ => 1,
        }
    }

    /// Geometric points of point-based objects (endpoints, path), if it is one.
    fn points_mut(&mut self) -> Option<Vec<&mut P>> {
        Some(match self {
            Kind::Arrow(a) => {
                let mut v: Vec<&mut P> = vec![&mut a.from, &mut a.to];
                for an in &mut a.anchors {
                    v.extend([&mut an.p, &mut an.c_in, &mut an.c_out]);
                }
                v
            }
            Kind::Line(l) => vec![&mut l.from, &mut l.to],
            Kind::Pen(pen) => pen.points.iter_mut().collect(),
            Kind::Highlight(Highlight { mark: Mark::Free { points, .. }, .. }) => points.iter_mut().collect(),
            Kind::Redact(Redact { area: Area::Brush { points, .. }, .. }) => points.iter_mut().collect(),
            _ => return None,
        })
    }

    fn rect_mut(&mut self) -> Option<&mut R> {
        match self {
            Kind::Image(i) => Some(&mut i.rect),
            Kind::Source(sb) => Some(&mut sb.rect),
            Kind::Callout(c) => Some(&mut c.rect),
            Kind::Shape(s) => Some(&mut s.rect),
            Kind::Text(t) => Some(&mut t.rect),
            Kind::Highlight(Highlight { mark: Mark::Rect(rect), .. }) => Some(rect),
            Kind::Emoji(e) => Some(&mut e.rect),
            Kind::Sticker(s) => Some(&mut s.rect),
            Kind::Redact(Redact { area: Area::Rect(rect) | Area::RoundRect(rect, _) | Area::Ellipse(rect), .. }) => Some(rect),
            _ => None,
        }
    }

}

impl Object {
    /// Unrotated bounds of the geometry (including stroke width for lines and paths).
    pub fn bounds(&self) -> R {
        match &self.kind {
            Kind::Arrow(a) => pad_points(&a.path(16), a.width * 3.0 + a.roughness * (3.0 + a.width)),
            Kind::Image(i) => i.rect,
            Kind::Source(sb) => sb.rect,
            Kind::Callout(c) => c.rect,
            Kind::Line(l) => pad_points(&[l.from, l.to], l.width),
            Kind::Shape(s) => s.rect,
            Kind::Text(t) => t.rect,
            Kind::Highlight(h) => match &h.mark {
                Mark::Rect(rect) => *rect,
                Mark::Free { points, width } => pad_points(points, *width),
            },
            Kind::Step(s) => r(s.center.x - s.radius, s.center.y - s.radius, s.radius * 2.0, s.radius * 2.0),
            Kind::Pen(pen) => pad_points(&pen.points, pen.width),
            Kind::Emoji(e) => e.rect,
            Kind::Sticker(s) => s.rect,
            Kind::Redact(rd) => match &rd.area {
                Area::Rect(rect) | Area::RoundRect(rect, _) | Area::Ellipse(rect) => *rect,
                Area::Brush { points, width } => pad_points(points, *width),
            },
        }
    }

    /// The four corners of the rotated bounds.
    pub fn corners(&self) -> [P; 4] {
        let b = self.bounds();
        let c = b.center();
        [p(b.x, b.y), p(b.right(), b.y), p(b.right(), b.bottom()), p(b.x, b.bottom())].map(|q| q.rotate(c, self.rotation))
    }

    /// Bounds of the rotated object (axis aligned).
    pub fn outer_bounds(&self) -> R {
        R::bbox(&self.corners())
    }

    pub fn translate(&mut self, dx: f32, dy: f32) {
        if let Some(points) = self.kind.points_mut() {
            for q in points {
                q.x += dx;
                q.y += dy;
            }
        } else if let Some(rect) = self.kind.rect_mut() {
            *rect = rect.translate(dx, dy);
        } else if let Kind::Step(s) = &mut self.kind {
            s.center.x += dx;
            s.center.y += dy;
        }
    }

    /// Fits the object into `to` (unrotated bounds). Text scales its font with the height,
    /// a step marker keeps its circle.
    pub fn set_bounds(&mut self, to: R) {
        let from = self.bounds();
        // Images and callouts scale uniformly (never stretched).
        if let Kind::Image(img) = &mut self.kind {
            let a = img.crop.w / img.crop.h.max(1e-3);
            img.rect = r(to.x, to.y, to.w.max(8.0), to.w.max(8.0) / a);
            return;
        }
        if let Kind::Callout(c) = &mut self.kind {
            let k = to.w / c.rect.w.max(1e-3);
            let scale = (c.scale * k).clamp(Callout::MIN_SCALE, Callout::MAX_SCALE);
            let k = scale / c.scale;
            let (w, h) = (c.rect.w * k, c.rect.h * k);
            let cc = to.center();
            c.scale = scale;
            c.rect = r(cc.x - w / 2.0, cc.y - h / 2.0, w, h);
            return;
        }
        if let Kind::Text(t) = &mut self.kind {
            if t.rect.h > 0.0 {
                t.size = (t.size * to.h / t.rect.h).max(4.0);
            }
            t.rect = to;
            return;
        }
        if let Kind::Step(s) = &mut self.kind {
            s.radius = (to.w.min(to.h) / 2.0).max(4.0);
            s.center = to.center();
            return;
        }
        if let Some(points) = self.kind.points_mut() {
            // Bounds include half the stroke; map the bare point hull.
            let pad = (from.w - R::bbox(&points.iter().map(|q| **q).collect::<Vec<_>>()).w) / 2.0;
            let inner_from = from.inflate(-pad);
            let inner_to = to.inflate(-pad);
            for q in points {
                *q = inner_from.map(*q, &inner_to);
            }
        } else if let Some(rect) = self.kind.rect_mut() {
            *rect = to;
        }
    }

    /// Hit test in document coordinates, `tol` pixels of slack.
    pub fn hit(&self, q: P, tol: f32) -> bool {
        let b = self.bounds();
        let local = q.rotate(b.center(), -self.rotation);
        let near_polyline = |pts: &[P], width: f32| {
            pts.windows(2).any(|w| seg_dist(local, w[0], w[1]) <= width / 2.0 + tol)
                || (pts.len() == 1 && pts[0].dist(local) <= width / 2.0 + tol)
        };
        match &self.kind {
            Kind::Arrow(a) => near_polyline(&a.path(24), a.width * 2.0),
            // A source box is picked by its outline (clicks inside reach the image under it).
            Kind::Source(sb) => {
                let inner = sb.rect.inflate(-(sb.width / 2.0 + tol));
                b.inflate(tol).contains(local) && !(inner.w > 0.0 && inner.h > 0.0 && inner.contains(local))
            }
            Kind::Line(l) => near_polyline(&[l.from, l.to], l.width),
            Kind::Pen(pen) => near_polyline(&pen.points, pen.width),
            Kind::Highlight(Highlight { mark: Mark::Free { points, width }, .. }) => near_polyline(points, *width),
            Kind::Redact(Redact { area: Area::Brush { points, width }, .. }) => near_polyline(points, *width),
            Kind::Step(s) => s.center.dist(local) <= s.radius + tol,
            _ => b.inflate(tol).contains(local),
        }
    }
}

/// The point of `b`'s outline nearest `q` (an ellipse for round objects), as a glue:
/// relative position in `b` and the point itself.
pub fn edge_point(b: R, q: P, round: bool) -> (f32, f32, P) {
    let at = if round {
        let c = b.center();
        let (rx, ry) = (b.w / 2.0, b.h / 2.0);
        let (dx, dy) = (q.x - c.x, q.y - c.y);
        let len = ((dx / rx.max(1e-3)).powi(2) + (dy / ry.max(1e-3)).powi(2)).sqrt().max(1e-6);
        p(c.x + dx / len, c.y + dy / len)
    } else {
        let x = q.x.clamp(b.x, b.right());
        let y = q.y.clamp(b.y, b.bottom());
        // Inside: push to the nearest side.
        if b.contains(q) {
            let d = [(q.x - b.x, 0), (b.right() - q.x, 1), (q.y - b.y, 2), (b.bottom() - q.y, 3)];
            let side = d.iter().min_by(|a, c| a.0.total_cmp(&c.0)).unwrap().1;
            match side {
                0 => p(b.x, y),
                1 => p(b.right(), y),
                2 => p(x, b.y),
                _ => p(x, b.bottom()),
            }
        } else {
            p(x, y)
        }
    };
    ((at.x - b.x) / b.w.max(1e-3), (at.y - b.y) / b.h.max(1e-3), at)
}

/// Distance from `q` to the segment `a`–`b`.
pub fn seg_dist(q: P, a: P, b: P) -> f32 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 { (((q.x - a.x) * dx + (q.y - a.y) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    q.dist(p(a.x + t * dx, a.y + t * dy))
}

/// The arrow's shaft sampled as a polyline (`n` segments; straight arrows have 1).
#[cfg_attr(not(test), allow(dead_code))]
pub fn arrow_path(a: &Arrow, n: usize) -> Vec<P> {
    a.path(n)
}

/// Chaikin smoothing for freehand strokes (keeps the end points).
pub fn smooth(points: &[P], passes: usize) -> Vec<P> {
    let mut pts = points.to_vec();
    for _ in 0..passes {
        if pts.len() < 3 {
            break;
        }
        let mut out = vec![pts[0]];
        for w in pts.windows(2) {
            let (a, b) = (w[0], w[1]);
            out.push(p(0.75 * a.x + 0.25 * b.x, 0.75 * a.y + 0.25 * b.y));
            out.push(p(0.25 * a.x + 0.75 * b.x, 0.25 * a.y + 0.75 * b.y));
        }
        out.push(*pts.last().unwrap());
        pts = out;
    }
    pts
}

/// Drops points closer than `min` to the previous kept one.
pub fn simplify(points: &[P], min: f32) -> Vec<P> {
    let mut out: Vec<P> = Vec::new();
    for (i, q) in points.iter().enumerate() {
        if out.last().is_none_or(|l| l.dist(*q) >= min) || i == points.len() - 1 {
            out.push(*q);
        }
    }
    out
}

// ---------- background frame ----------

pub use crate::backgrounds::Background;

/// Reads a background, falling back to the default for project files saved before the
/// shared background system (a different shape).
fn lenient_background<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Background, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(v).unwrap_or_default())
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum WindowStyle {
    #[default]
    None,
    Mac,
    Browser,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug, Default)]
pub enum OutputSize {
    /// Image plus padding.
    #[default]
    Auto,
    /// Smallest canvas of this aspect (w:h) around image plus padding.
    Aspect(u32, u32),
    /// Exactly this canvas; the window is fitted inside the padding.
    Custom(u32, u32),
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Frame {
    /// Off: the export is the cropped image alone.
    pub enabled: bool,
    #[serde(deserialize_with = "lenient_background", default)]
    pub background: Background,
    /// Which moment of an animated background a still shows (seconds into its loop).
    #[serde(default)]
    pub moment: f32,
    pub padding: u32,
    pub radius: u32,
    pub shadow_blur: u32,
    /// 0..=1
    pub shadow_opacity: f32,
    pub shadow_offset: i32,
    /// A thin light line inside the window edge.
    pub inset: bool,
    pub window: WindowStyle,
    pub size: OutputSize,
}

impl Default for Frame {
    fn default() -> Self {
        Self {
            enabled: false,
            background: Background::default(),
            moment: 0.0,
            padding: 96,
            radius: 24,
            shadow_blur: 60,
            shadow_opacity: 0.4,
            shadow_offset: 16,
            inset: false,
            window: WindowStyle::None,
            size: OutputSize::Auto,
        }
    }
}

/// Where everything goes in the output, at 1× (content pixels).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct FrameLayout {
    pub canvas: (u32, u32),
    /// The window: title bar (if any) plus content.
    pub window: R,
    /// The cropped image inside the window.
    pub content: R,
    pub bar: f32,
    /// Content scale inside a fixed canvas (1 unless it had to shrink to fit).
    pub scale: f32,
}

impl Frame {
    /// Title bar height for a `content_w` wide image.
    pub fn bar_height(&self, content_w: f32) -> f32 {
        match self.window {
            WindowStyle::None => 0.0,
            WindowStyle::Mac => (content_w * 0.022).clamp(28.0, 72.0).round(),
            WindowStyle::Browser => (content_w * 0.032).clamp(40.0, 104.0).round(),
        }
    }

    pub fn layout(&self, content: (u32, u32)) -> FrameLayout {
        let (cw, ch) = (content.0 as f32, content.1 as f32);
        if !self.enabled {
            return FrameLayout { canvas: content, window: r(0.0, 0.0, cw, ch), content: r(0.0, 0.0, cw, ch), bar: 0.0, scale: 1.0 };
        }
        let bar = self.bar_height(cw);
        let (ww, wh) = (cw, ch + bar);
        let pad = self.padding as f32;
        let (canvas, scale) = match self.size {
            OutputSize::Auto => (((ww + 2.0 * pad).round(), (wh + 2.0 * pad).round()), 1.0),
            OutputSize::Aspect(aw, ah) => {
                let a = aw as f32 / ah.max(1) as f32;
                let (mw, mh) = (ww + 2.0 * pad, wh + 2.0 * pad);
                let c = if mw / mh > a { (mw, mw / a) } else { (mh * a, mh) };
                ((c.0.round(), c.1.round()), 1.0)
            }
            OutputSize::Custom(w, h) => {
                let (w, h) = (w.max(16) as f32, h.max(16) as f32);
                let s = ((w - 2.0 * pad).max(8.0) / ww).min((h - 2.0 * pad).max(8.0) / wh);
                ((w, h), s)
            }
        };
        let (sw, sh) = (ww * scale, wh * scale);
        let window = r(((canvas.0 - sw) / 2.0).round(), ((canvas.1 - sh) / 2.0).round(), sw, sh);
        let content = r(window.x, window.y + bar * scale, cw * scale, ch * scale);
        FrameLayout { canvas: (canvas.0 as u32, canvas.1 as u32), window, content, bar: bar * scale, scale }
    }
}

// ---------- document ----------

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Orient {
    /// Clockwise quarter turns, applied first.
    pub quarter: u8,
    pub flip_h: bool,
    pub flip_v: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Order {
    Front,
    Back,
    Forward,
    Backward,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Document {
    pub version: u32,
    /// The original screenshot's file name (next to the project file). Never modified.
    pub original: String,
    /// Original image size in pixels.
    pub size: (u32, u32),
    pub orient: Orient,
    /// The visible part, in oriented pixels. May reach outside the image (canvas resize):
    /// the area outside is transparent.
    pub crop: R,
    pub objects: Vec<Object>,
    pub frame: Frame,
    pub next_id: u64,
    /// A merged canvas of several images (`Kind::Image` objects): no single original.
    #[serde(default)]
    pub board: Option<super::merge::Board>,
}

impl Document {
    pub fn new(original: &str, size: (u32, u32)) -> Self {
        Self {
            version: 1,
            original: original.to_string(),
            size,
            orient: Orient::default(),
            crop: r(0.0, 0.0, size.0 as f32, size.1 as f32),
            objects: Vec::new(),
            frame: Frame::default(),
            next_id: 1,
            board: None,
        }
    }

    /// Size of the oriented image.
    pub fn oriented_size(&self) -> (u32, u32) {
        if self.orient.quarter % 2 == 1 { (self.size.1, self.size.0) } else { self.size }
    }

    /// Cropped content size in pixels.
    pub fn content_size(&self) -> (u32, u32) {
        (self.crop.w.round().max(1.0) as u32, self.crop.h.round().max(1.0) as u32)
    }

    /// Where the content goes in the output. A board is its own frame (its background and
    /// padding), so the frame never wraps it a second time.
    pub fn layout(&self) -> FrameLayout {
        if self.board.is_some() {
            return Frame { enabled: false, ..self.frame.clone() }.layout(self.content_size());
        }
        self.frame.layout(self.content_size())
    }

    pub fn add(&mut self, kind: Kind) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.objects.push(Object { id, kind, rotation: 0.0, locked: false });
        id
    }

    pub fn get(&self, id: u64) -> Option<&Object> {
        self.objects.iter().find(|o| o.id == id)
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut Object> {
        self.objects.iter_mut().find(|o| o.id == id)
    }

    /// The next step marker number.
    pub fn next_step(&self) -> u32 {
        self.objects.iter().filter_map(|o| if let Kind::Step(s) = &o.kind { Some(s.n) } else { None }).max().unwrap_or(0) + 1
    }

    /// Removes the unlocked objects among `ids`; returns how many. A source box and its
    /// callout go together (with the callout's auto connector).
    pub fn remove(&mut self, ids: &[u64]) -> usize {
        let before = self.objects.len();
        let mut gone: Vec<u64> = self.objects.iter().filter(|o| ids.contains(&o.id) && !o.locked).map(|o| o.id).collect();
        let linked: Vec<u64> = self
            .objects
            .iter()
            .filter(|o| gone.contains(&o.id))
            .flat_map(|o| match &o.kind {
                Kind::Source(sb) => vec![Some(sb.callout), self.get(sb.callout).and_then(|c| if let Kind::Callout(c) = &c.kind { c.connector } else { None })],
                Kind::Callout(c) => vec![Some(c.source), c.connector],
                _ => vec![],
            })
            .flatten()
            .collect();
        gone.extend(linked);
        self.objects.retain(|o| !gone.contains(&o.id));
        self.sync_links();
        before - self.objects.len()
    }

    /// Copies of `ids`, offset a little, placed above the originals. Returns the new ids.
    pub fn duplicate(&mut self, ids: &[u64], offset: f32) -> Vec<u64> {
        // Callouts and their source boxes are linked pairs: make another with the tool.
        let copies: Vec<Object> = self.objects.iter().filter(|o| ids.contains(&o.id) && !matches!(o.kind, Kind::Source(_) | Kind::Callout(_))).cloned().collect();
        copies
            .into_iter()
            .map(|mut o| {
                o.id = self.next_id;
                self.next_id += 1;
                o.locked = false;
                o.translate(offset, offset);
                if let Kind::Step(s) = &mut o.kind {
                    s.n = self.next_step();
                }
                let id = o.id;
                self.objects.push(o);
                id
            })
            .collect()
    }

    /// Moves the unlocked objects among `ids`.
    pub fn translate(&mut self, ids: &[u64], dx: f32, dy: f32) {
        for o in self.objects.iter_mut().filter(|o| ids.contains(&o.id) && !o.locked) {
            o.translate(dx, dy);
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn set_bounds(&mut self, id: u64, to: R) {
        if let Some(o) = self.get_mut(id).filter(|o| !o.locked) {
            o.set_bounds(to);
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn set_rotation(&mut self, id: u64, deg: f32) {
        if let Some(o) = self.get_mut(id).filter(|o| !o.locked) {
            o.rotation = deg.rem_euclid(360.0);
        }
    }

    pub fn set_locked(&mut self, id: u64, locked: bool) {
        if let Some(o) = self.get_mut(id) {
            o.locked = locked;
        }
    }

    pub fn reorder(&mut self, id: u64, order: Order) {
        let Some(i) = self.objects.iter().position(|o| o.id == id) else { return };
        let o = self.objects.remove(i);
        let last = self.objects.len();
        let j = match order {
            Order::Front => last,
            Order::Back => 0,
            Order::Forward => (i + 1).min(last),
            Order::Backward => i.saturating_sub(1),
        };
        self.objects.insert(j, o);
    }

    /// The topmost object under `q`, in drawing order (arrows over callouts over source
    /// boxes over everything else).
    pub fn hit(&self, q: P, tol: f32) -> Option<u64> {
        let mut order: Vec<&Object> = self.objects.iter().collect();
        order.sort_by_key(|o| o.kind.level());
        order.into_iter().rev().find(|o| {
            let hidden = matches!(&o.kind, Kind::Source(sb) if !sb.visible);
            !hidden && o.hit(q, tol)
        }).map(|o| o.id)
    }

    /// Keeps linked objects consistent after any change: each callout is its source area ×
    /// its scale (around its own center); glued arrow ends sit on their objects. Links to
    /// objects that are gone are dropped.
    pub fn sync_links(&mut self) {
        let sources: Vec<(u64, R)> = self.objects.iter().filter_map(|o| if let Kind::Source(sb) = &o.kind { Some((o.id, sb.rect)) } else { None }).collect();
        for o in &mut self.objects {
            if let Kind::Callout(c) = &mut o.kind {
                if let Some((_, src)) = sources.iter().find(|(id, _)| *id == c.source) {
                    let cc = c.rect.center();
                    let (w, h) = (src.w * c.scale, src.h * c.scale);
                    c.rect = r(cc.x - w / 2.0, cc.y - h / 2.0, w, h);
                }
            }
        }
        let ids: Vec<u64> = self.objects.iter().map(|o| o.id).collect();
        let bounds: Vec<(u64, R)> = self.objects.iter().map(|o| (o.id, o.outer_bounds())).collect();
        for o in &mut self.objects {
            if let Kind::Arrow(a) = &mut o.kind {
                for end in 0..2 {
                    let Some(g) = a.glue[end] else { continue };
                    let Some((_, b)) = bounds.iter().find(|(id, _)| *id == g.id) else {
                        a.glue[end] = None;
                        continue;
                    };
                    a.materialize();
                    let target = p(b.x + g.u * b.w, b.y + g.v * b.h);
                    let i = if end == 0 { 0 } else { a.anchors.len() - 1 };
                    let cur = a.anchors[i].p;
                    a.anchors[i].shift(target.x - cur.x, target.y - cur.y);
                    a.sync_ends();
                }
            }
        }
        self.objects.retain(|o| match &o.kind {
            Kind::Callout(c) => ids.contains(&c.source),
            // 0: still being drawn (its callout comes on release).
            Kind::Source(sb) => sb.callout == 0 || ids.contains(&sb.callout),
            _ => true,
        });
    }

    /// Adds `c` as the callout of source box `source`, placed outside the source on the
    /// side with the most room (on a board: beside the image it's on), clear of other
    /// callouts. A board's canvas grows when the callout doesn't fit; otherwise it's kept
    /// inside the canvas. Returns the callout's id.
    pub fn add_callout(&mut self, source: u64, mut c: Callout, gap: f32) -> Option<u64> {
        let src = match self.get(source).map(|o| &o.kind) {
            Some(Kind::Source(sb)) => sb.rect,
            _ => return None,
        };
        let (w, h) = (src.w * c.scale, src.h * c.scale);
        let canvas = self.crop;
        let anchor = if self.board.is_some() {
            self.images().iter().map(|o| o.bounds()).find(|b| b.contains(src.center())).unwrap_or(src)
        } else {
            src
        };
        let taken: Vec<R> = self.objects.iter().filter_map(|o| if let Kind::Callout(c) = &o.kind { Some(c.rect) } else { None }).collect();
        let (cx, cy) = (src.center().x, src.center().y);
        let at = |side: u8| match side {
            0 => r(anchor.right() + gap, cy - h / 2.0, w, h),
            1 => r(anchor.x - gap - w, cy - h / 2.0, w, h),
            2 => r(cx - w / 2.0, anchor.bottom() + gap, w, h),
            _ => r(cx - w / 2.0, anchor.y - gap - h, w, h),
        };
        let clear = |mut rect: R, side: u8| {
            for _ in 0..8 {
                if !taken.iter().any(|t| t.inflate(gap / 2.0).intersect(&rect).is_some()) {
                    break;
                }
                rect = if side < 2 { rect.translate(0.0, h + gap) } else { rect.translate(w + gap, 0.0) };
            }
            rect
        };
        let room = [canvas.right() - anchor.right(), anchor.x - canvas.x, canvas.bottom() - anchor.bottom(), anchor.y - canvas.y];
        let mut sides = [0u8, 1, 2, 3];
        sides.sort_by(|a, b| room[*b as usize].total_cmp(&room[*a as usize]));
        // Inside the canvas and, on a board, off every image (a callout goes beside them).
        let images: Vec<R> = self.images().iter().map(|o| o.outer_bounds()).collect();
        let fits = |rect: &R| {
            rect.x >= canvas.x && rect.y >= canvas.y && rect.right() <= canvas.right() && rect.bottom() <= canvas.bottom() && !images.iter().any(|i| i.intersect(rect).is_some())
        };
        let placed = sides.iter().map(|&sd| clear(at(sd), sd)).find(fits);
        c.rect = match placed {
            Some(rect) => rect,
            // A board grows to the right; a single screenshot keeps it inside.
            None if self.board.is_some() => clear(at(0), 0),
            None => {
                let rect = clear(at(sides[0]), sides[0]);
                r(rect.x.clamp(canvas.x, (canvas.right() - w).max(canvas.x)), rect.y.clamp(canvas.y, (canvas.bottom() - h).max(canvas.y)), w, h)
            }
        };
        c.source = source;
        let rect = c.rect;
        let id = self.add(Kind::Callout(c));
        if let Some(Object { kind: Kind::Source(sb), .. }) = self.get_mut(source) {
            sb.callout = id;
        }
        if self.board.is_some() && !fits(&rect) {
            self.fit_canvas_to(rect);
        }
        self.sync_links();
        Some(id)
    }

    /// Grows a board's canvas (its expansion on each side) so `area` fits with some margin.
    pub fn fit_canvas_to(&mut self, area: R) {
        let Some(board) = self.board.as_mut() else { return };
        let margin = (board.padding / 2.0).max(16.0);
        let c = self.crop;
        let need = [c.x - (area.x - margin), c.y - (area.y - margin), area.right() + margin - c.right(), area.bottom() + margin - c.bottom()];
        for (e, n) in board.expand.iter_mut().zip(need) {
            if n > 0.0 {
                *e += n.ceil();
            }
        }
        self.arrange(None);
    }

    /// Adds an arrow from object `from` to object `to`, both ends glued to their edges
    /// (nearest each other), shaped like `template`. Returns its id.
    pub fn connect(&mut self, from: u64, to: u64, template: Arrow) -> Option<u64> {
        let (a, b) = (self.get(from)?.outer_bounds(), self.get(to)?.outer_bounds());
        let round = |id: u64| matches!(self.get(id).map(|o| &o.kind), Some(Kind::Callout(Callout { shape: CalloutShape::Circle, .. })) | Some(Kind::Source(SourceBox { shape: CalloutShape::Circle, .. })));
        let (ua, va, pa) = edge_point(a, b.center(), round(from));
        let (ub, vb, pb) = edge_point(b, a.center(), round(to));
        let bend = if template.style == ArrowStyle::Curved || template.style == ArrowStyle::Hand { 0.2 } else { 0.0 };
        let mut anchors = spline::default_curve(pa, pb, bend);
        if bend == 0.0 {
            for an in &mut anchors {
                an.c_in = an.p;
                an.c_out = an.p;
            }
        }
        let arrow = Arrow { from: pa, to: pb, anchors, glue: [Some(Glue { id: from, u: ua, v: va }), Some(Glue { id: to, u: ub, v: vb })], ..template };
        let id = self.add(Kind::Arrow(arrow));
        self.sync_links();
        Some(id)
    }

    /// The image layers, in order.
    pub fn images(&self) -> Vec<&Object> {
        self.objects.iter().filter(|o| matches!(o.kind, Kind::Image(_))).collect()
    }

    /// A merged canvas of `images` (path, size in pixels), arranged with `board`.
    pub fn new_board(images: &[(PathBuf, (u32, u32))], board: super::merge::Board) -> Self {
        let mut d = Document::new("", (1, 1));
        d.board = Some(board);
        for (src, size) in images.iter().take(super::merge::MAX_IMAGES) {
            d.add_image(src.clone(), *size);
        }
        d.arrange(None);
        d
    }

    /// Adds an image layer (placed after the others; `arrange` lays it out).
    pub fn add_image(&mut self, src: PathBuf, natural: (u32, u32)) -> u64 {
        let (w, h) = (natural.0.max(1) as f32, natural.1.max(1) as f32);
        // After the others along both axes, so a stack keeps the order images were added in.
        let x = self.images().iter().map(|o| o.bounds().right()).fold(0.0, f32::max);
        let y = self.images().iter().map(|o| o.bounds().bottom()).fold(0.0, f32::max);
        self.add(Kind::Image(ImageLayer { src, natural, orient: Orient::default(), crop: r(0.0, 0.0, w, h), rect: r(x, y, w, h), radius: 0.0, shadow: false }))
    }

    /// Lays out a board: stacks the images (in `order`, else by where they are along the
    /// stack) and fits the canvas around them plus padding and any expansion. Objects on an
    /// image move with it; everything shifts together when the canvas grows on the left/top.
    pub fn arrange(&mut self, order: Option<&[u64]>) {
        let Some(board) = self.board.clone() else { return };
        let mut imgs: Vec<(u64, R, (f32, f32))> = self
            .objects
            .iter()
            // Stacked at their current size: an image the user resized keeps that size
            // (stacking from the natural size snapped it back on release).
            .filter_map(|o| if let Kind::Image(i) = &o.kind { Some((o.id, i.rect, (i.rect.w, i.rect.h))) } else { None })
            .collect();
        if board.layout != super::merge::BoardLayout::Free {
            match order {
                Some(ids) => imgs.sort_by_key(|(id, ..)| ids.iter().position(|x| x == id).unwrap_or(usize::MAX)),
                None => imgs.sort_by(|a, b| {
                    let key = |r: &R| if board.layout == super::merge::BoardLayout::Vertical { r.center().y } else { r.center().x };
                    key(&a.1).total_cmp(&key(&b.1))
                }),
            }
            let sizes: Vec<(f32, f32)> = imgs.iter().map(|i| i.2).collect();
            // Stack from where the images are now, so only a real change of the margins
            // (below) moves everything else.
            let x0 = imgs.iter().map(|i| i.1.x).fold(f32::MAX, f32::min);
            let y0 = imgs.iter().map(|i| i.1.y).fold(f32::MAX, f32::min);
            let placed = super::merge::arrange(&sizes, board.layout, board.gap, board.align, board.matching);
            for ((id, old, _), new) in imgs.iter().zip(&placed) {
                self.move_image(*id, *old, new.translate(x0, y0));
            }
        }
        let rects: Vec<R> = self.images().iter().map(|o| o.outer_bounds()).collect();
        let Some(bbox) = rects.iter().skip(1).fold(rects.first().copied(), |a, b| a.map(|a| a.union(b))) else { return };
        let [el, et, er, eb] = board.expand.map(|v| v.max(0.0));
        let (pad, dx, dy) = (board.padding.max(0.0), board.padding.max(0.0) + el - bbox.x, board.padding.max(0.0) + et - bbox.y);
        if dx.abs() > 1e-3 || dy.abs() > 1e-3 {
            for o in &mut self.objects {
                o.translate(dx, dy);
            }
        }
        let (w, h) = ((bbox.w + 2.0 * pad + el + er).round().max(16.0), (bbox.h + 2.0 * pad + et + eb).round().max(16.0));
        self.size = (w as u32, h as u32);
        self.orient = Orient::default();
        self.crop = r(0.0, 0.0, w, h);
        self.sync_links();
    }

    /// Moves (and scales) image `id` from `old` to `new`, taking the objects on it along.
    fn move_image(&mut self, id: u64, old: R, new: R) {
        if old == new {
            return;
        }
        let on_it: Vec<u64> = self
            .objects
            .iter()
            .filter(|o| !matches!(o.kind, Kind::Image(_) | Kind::Callout(_) | Kind::Arrow(_)) && old.contains(o.bounds().center()))
            .map(|o| o.id)
            .collect();
        for o in &mut self.objects {
            if o.id == id {
                if let Kind::Image(i) = &mut o.kind {
                    i.rect = new;
                }
            } else if on_it.contains(&o.id) {
                let b = o.bounds();
                let nb = r(new.x + (b.x - old.x) * new.w / old.w.max(1e-3), new.y + (b.y - old.y) * new.h / old.h.max(1e-3), b.w * new.w / old.w.max(1e-3), b.h * new.h / old.h.max(1e-3));
                if let Kind::Source(sb) = &mut o.kind {
                    sb.rect = nb;
                } else {
                    o.set_bounds(nb);
                }
            }
        }
    }

    /// Turns a single-screenshot document into a board whose first image is that
    /// screenshot (`original` is its file). Objects keep their place on it.
    pub fn into_board(&mut self, original: PathBuf) {
        if self.board.is_some() {
            return;
        }
        let crop = self.crop;
        let (ow, oh) = self.oriented_size();
        let visible = crop.intersect(&r(0.0, 0.0, ow as f32, oh as f32)).unwrap_or(crop);
        for o in &mut self.objects {
            o.translate(-visible.x, -visible.y);
        }
        let layer = ImageLayer { src: original, natural: self.size, orient: self.orient, crop: visible, rect: r(0.0, 0.0, visible.w, visible.h), radius: 0.0, shadow: false };
        let id = self.next_id;
        self.next_id += 1;
        self.objects.insert(0, Object { id, kind: Kind::Image(layer), rotation: 0.0, locked: false });
        self.original = String::new();
        self.board = Some(super::merge::Board::default());
        self.arrange(None);
    }

    /// Rotates the image (and every object and the crop) a quarter turn.
    pub fn rotate_image(&mut self, clockwise: bool) {
        let (w, h) = self.oriented_size();
        let (w, h) = (w as f32, h as f32);
        let map = |q: P| if clockwise { p(h - q.y, q.x) } else { p(q.y, w - q.x) };
        let turn = if clockwise { 90.0 } else { -90.0 };
        self.transform(map, |rot| rot + turn);
        // The oriented image is flip(turn^q(original)); turning it is flip'(turn^(q±1)) with
        // the horizontal and vertical flips swapped.
        self.orient.quarter = (self.orient.quarter + if clockwise { 1 } else { 3 }) % 4;
        std::mem::swap(&mut self.orient.flip_h, &mut self.orient.flip_v);
    }

    pub fn flip(&mut self, horizontal: bool) {
        let (w, h) = self.oriented_size();
        let (w, h) = (w as f32, h as f32);
        if horizontal {
            self.transform(|q| p(w - q.x, q.y), |rot| -rot);
            self.orient.flip_h = !self.orient.flip_h;
        } else {
            self.transform(|q| p(q.x, h - q.y), |rot| -rot);
            self.orient.flip_v = !self.orient.flip_v;
        }
        if self.orient.flip_h && self.orient.flip_v {
            self.orient.flip_h = false;
            self.orient.flip_v = false;
            self.orient.quarter = (self.orient.quarter + 2) % 4;
        }
    }

    /// Maps object geometry with `map`. Point objects map their points; boxed objects move
    /// their center and keep their size, turning by `rot` instead (text stays readable).
    fn transform(&mut self, map: impl Fn(P) -> P, rot: impl Fn(f32) -> f32) {
        let crop = self.crop;
        self.crop = R::from_points(map(p(crop.x, crop.y)), map(p(crop.right(), crop.bottom())));
        for o in &mut self.objects {
            if let Some(points) = o.kind.points_mut() {
                for q in points {
                    *q = map(*q);
                }
                continue;
            }
            if let Kind::Step(s) = &mut o.kind {
                s.center = map(s.center);
                continue;
            }
            let b = o.bounds();
            let c = map(b.center());
            o.translate(c.x - b.center().x, c.y - b.center().y);
            o.rotation = rot(o.rotation).rem_euclid(360.0);
        }
    }

    /// Sets the crop (at least 8×8 pixels).
    pub fn set_crop(&mut self, crop: R) {
        self.crop = r(crop.x.round(), crop.y.round(), crop.w.round().max(8.0), crop.h.round().max(8.0));
    }

    /// Crops to the largest centered rect of `aspect` inside the current crop.
    pub fn crop_to_aspect(&mut self, aspect: f32) {
        let c = self.crop.fit_aspect(aspect);
        self.set_crop(c);
    }

    pub fn reset_crop(&mut self) {
        let (w, h) = self.oriented_size();
        self.crop = r(0.0, 0.0, w as f32, h as f32);
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    pub fn from_json(s: &str) -> anyhow::Result<Self> {
        Ok(serde_json::from_str(s)?)
    }

    /// The project (sidecar) file for an original: `shot.png` → `shot.clipper.json`.
    pub fn sidecar_path(original: &Path) -> PathBuf {
        original.with_extension("clipper.json")
    }
}

/// Unlimited undo/redo by whole-document snapshots (documents are small: the image is not
/// part of them).
#[derive(Default)]
pub struct History {
    undo: Vec<Document>,
    redo: Vec<Document>,
}

impl History {
    /// Call before changing `doc`.
    pub fn checkpoint(&mut self, doc: &Document) {
        if self.undo.last() != Some(doc) {
            self.undo.push(doc.clone());
        }
        self.redo.clear();
    }

    /// Drops the last checkpoint if nothing changed since (e.g. a click without a drag).
    pub fn discard_if_unchanged(&mut self, doc: &Document) {
        if self.undo.last() == Some(doc) {
            self.undo.pop();
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self, doc: &mut Document) -> bool {
        let Some(prev) = self.undo.pop() else { return false };
        self.redo.push(std::mem::replace(doc, prev));
        true
    }

    pub fn redo(&mut self, doc: &mut Document) -> bool {
        let Some(next) = self.redo.pop() else { return false };
        self.undo.push(std::mem::replace(doc, next));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Rgba = [255, 0, 0, 255];

    fn source(rect: R) -> Kind {
        Kind::Source(SourceBox { rect, shape: CalloutShape::Rect, color: RED, width: 4.0, radius: 0.0, fill: None, dashed: false, visible: true, callout: 0 })
    }

    fn callout(scale: f32) -> Callout {
        Callout { source: 0, rect: r(0.0, 0.0, 0.0, 0.0), scale, shape: CalloutShape::Rect, border: RED, width: 4.0, radius: 8.0, shadow: None, cut: CutMode::Copy, connector: None }
    }

    #[test]
    fn callout_maps_its_source_and_follows_it() {
        let mut d = Document::new("a.png", (1600, 1000));
        let s = d.add(source(r(100.0, 100.0, 200.0, 100.0)));
        let c = d.add_callout(s, callout(2.0), 40.0).unwrap();
        let Kind::Callout(co) = &d.get(c).unwrap().kind else { panic!() };
        // Twice the source, outside it, inside the screenshot.
        assert_eq!((co.rect.w, co.rect.h), (400.0, 200.0));
        assert!(co.rect.intersect(&r(100.0, 100.0, 200.0, 100.0)).is_none(), "placed outside the source: {:?}", co.rect);
        assert!(co.rect.x >= 0.0 && co.rect.right() <= 1600.0 && co.rect.bottom() <= 1000.0);
        // Source ↔ callout coordinates: corners and center map onto each other.
        let src = r(100.0, 100.0, 200.0, 100.0);
        assert_eq!(co.map_from_source(src, p(100.0, 100.0)), p(co.rect.x, co.rect.y));
        assert_eq!(co.map_from_source(src, src.center()), co.rect.center());
        let q = p(250.0, 130.0);
        assert!(co.map_to_source(src, co.map_from_source(src, q)).dist(q) < 1e-3);
        // Resizing the source resizes the callout (same scale, same center); moving the
        // callout leaves the source alone.
        let center = co.rect.center();
        if let Some(Object { kind: Kind::Source(sb), .. }) = d.get_mut(s) {
            sb.rect = r(120.0, 90.0, 100.0, 80.0);
        }
        d.sync_links();
        let Kind::Callout(co) = &d.get(c).unwrap().kind else { panic!() };
        assert_eq!((co.rect.w, co.rect.h, co.rect.center()), (200.0, 160.0, center));
        d.translate(&[c], 300.0, 0.0);
        d.sync_links();
        assert_eq!(d.get(s).unwrap().bounds(), r(120.0, 90.0, 100.0, 80.0));
        // Deleting either one removes the pair.
        d.remove(&[s]);
        assert!(d.get(c).is_none());
    }

    #[test]
    fn glued_arrows_follow_and_reopen_the_same() {
        let mut d = Document::new("a.png", (2000, 2000));
        let s1 = d.add(source(r(100.0, 100.0, 100.0, 100.0)));
        let c1 = d.add_callout(s1, callout(2.0), 40.0).unwrap();
        let s2 = d.add(source(r(100.0, 1200.0, 100.0, 100.0)));
        let c2 = d.add_callout(s2, callout(2.0), 40.0).unwrap();
        let a = d.connect(c1, c2, Arrow { style: ArrowStyle::Hand, roughness: 0.7, seed: 42, ..Default::default() }).unwrap();
        let end = |d: &Document| match &d.get(a).unwrap().kind {
            Kind::Arrow(ar) => ar.to,
            _ => panic!(),
        };
        let before = end(&d);
        let b2 = d.get(c2).unwrap().bounds();
        assert!(b2.inflate(0.5).contains(before), "the end sits on the callout's edge");
        d.translate(&[c2], 250.0, 60.0);
        d.sync_links();
        let after = end(&d);
        assert!((after.x - before.x - 250.0).abs() < 1e-3 && (after.y - before.y - 60.0).abs() < 1e-3, "{before:?} → {after:?}");
        // The wobble is part of the arrow: the same after saving and opening again.
        let path = match &d.get(a).unwrap().kind {
            Kind::Arrow(ar) => ar.drawn_path(),
            _ => panic!(),
        };
        let reopened = Document::from_json(&d.to_json()).unwrap();
        let again = match &reopened.get(a).unwrap().kind {
            Kind::Arrow(ar) => ar.drawn_path(),
            _ => panic!(),
        };
        assert_eq!(path, again);
        // Old projects (no anchors) still load and draw.
        let old = r#"{"from":{"x":0.0,"y":0.0},"to":{"x":100.0,"y":0.0},"bend":0.2,"double":false,"head":"Filled","color":[255,0,0,255],"width":4.0}"#;
        let ar: Arrow = serde_json::from_str(old).unwrap();
        assert_eq!(ar.spline().len(), 2);
        assert!(ar.path(8).iter().any(|q| q.y < -5.0), "still curved");
    }

    #[test]
    fn callouts_beside_board_images_fit_the_canvas() {
        use super::super::merge::{Board, Match};
        let imgs = vec![(PathBuf::from("/a.png"), (280, 498)), (PathBuf::from("/b.png"), (1716, 984))];
        let mut d = Document::new_board(&imgs, Board { matching: Match::Width, ..Board::default() });
        let rects: Vec<R> = d.images().iter().map(|o| o.bounds()).collect();
        let mut ids = Vec::new();
        for img in rects {
            let s = d.add(source(r(img.center().x - 40.0, img.center().y - 20.0, 80.0, 40.0)));
            ids.push(d.add_callout(s, callout(2.0), 40.0).unwrap());
        }
        for id in ids {
            let b = d.get(id).unwrap().bounds();
            assert!(d.crop.contains(p(b.x, b.y)) && d.crop.contains(p(b.right(), b.bottom())), "callout {b:?} outside canvas {:?}", d.crop);
            assert!(d.images().iter().all(|i| i.bounds().intersect(&b).is_none()), "callout over an image");
        }
    }

    #[test]
    fn resized_board_images_keep_their_size() {
        use super::super::merge::Board;
        let imgs = vec![(PathBuf::from("/a.png"), (800, 400)), (PathBuf::from("/b.png"), (400, 300))];
        let mut d = Document::new_board(&imgs, Board::default());
        let id = d.images()[1].id;
        let b = d.get(id).unwrap().bounds();
        d.get_mut(id).unwrap().set_bounds(r(b.x, b.y, 1200.0, 900.0));
        d.arrange(None);
        let after = d.get(id).unwrap().bounds();
        assert_eq!((after.w, after.h), (1200.0, 900.0), "snapped back to its natural size");
        assert_eq!(d.size, (1200 + 96, 400 + 24 + 900 + 96), "the canvas grows to fit");
        // One background: a board's layout ignores the frame.
        d.frame.enabled = true;
        assert_eq!(d.layout().canvas, d.size);
    }

    #[test]
    fn boards_stack_and_screenshots_convert() {
        use super::super::merge::{Board, BoardLayout, Match};
        let imgs = vec![(PathBuf::from("/a.png"), (800, 400)), (PathBuf::from("/b.png"), (400, 300))];
        let mut d = Document::new_board(&imgs, Board { matching: Match::Width, ..Board::default() });
        let rects: Vec<R> = d.images().iter().map(|o| o.bounds()).collect();
        // 48 px padding, 24 px gap, matched to 600 wide.
        assert_eq!(rects, vec![r(48.0, 48.0, 600.0, 300.0), r(48.0, 372.0, 600.0, 450.0)]);
        assert_eq!(d.size, (696, 870));
        // A highlight on the second image moves with it when the layout changes.
        let h = d.add(source(r(100.0, 600.0, 50.0, 50.0)));
        d.board.as_mut().unwrap().layout = BoardLayout::Horizontal;
        d.board.as_mut().unwrap().matching = Match::None;
        d.arrange(None);
        let second = d.images()[1].bounds();
        assert!(second.contains(d.get(h).unwrap().bounds().center()), "highlight stays on its image");
        // Expanding the canvas on the left shifts everything right.
        let x0 = d.images()[0].bounds().x;
        d.board.as_mut().unwrap().expand[0] = 100.0;
        d.arrange(None);
        assert_eq!(d.images()[0].bounds().x, x0 + 100.0);
        // A plain screenshot becomes a board without its annotations moving off the content.
        let mut s = Document::new("shot.png", (1000, 600));
        s.crop = r(100.0, 50.0, 800.0, 500.0);
        let mark = s.add(source(r(500.0, 300.0, 40.0, 40.0)));
        s.into_board(PathBuf::from("/shot.png"));
        let img = s.images()[0].bounds();
        let m = s.get(mark).unwrap().bounds();
        assert_eq!((m.x - img.x, m.y - img.y), (400.0, 250.0), "same spot on the picture");
        assert_eq!(s.size, (896, 596));
    }

    fn rect_obj(x: f32, y: f32, w: f32, h: f32) -> Kind {
        Kind::Shape(Shape { kind: ShapeKind::Rect, rect: r(x, y, w, h), stroke: Some(RED), fill: None, width: 4.0, radius: 0.0 })
    }

    #[test]
    fn add_move_resize_rotate_duplicate_delete() {
        let mut d = Document::new("a.png", (800, 600));
        let a = d.add(rect_obj(10.0, 10.0, 100.0, 50.0));
        let b = d.add(Kind::Line(Line { from: p(0.0, 0.0), to: p(100.0, 100.0), color: RED, width: 4.0 }));
        d.translate(&[a], 5.0, 7.0);
        assert_eq!(d.get(a).unwrap().bounds(), r(15.0, 17.0, 100.0, 50.0));
        d.set_bounds(a, r(0.0, 0.0, 200.0, 100.0));
        assert_eq!(d.get(a).unwrap().bounds(), r(0.0, 0.0, 200.0, 100.0));
        // A line maps its end points (the bounds include half the stroke).
        d.set_bounds(b, r(-2.0, -2.0, 54.0, 54.0));
        let Kind::Line(l) = &d.get(b).unwrap().kind else { panic!() };
        assert!((l.to.x - 50.0).abs() < 1e-3 && (l.to.y - 50.0).abs() < 1e-3);
        d.set_rotation(a, 450.0);
        assert_eq!(d.get(a).unwrap().rotation, 90.0);
        // Rotated 90°: the 200×100 box covers 100×200 around the same center.
        let ob = d.get(a).unwrap().outer_bounds();
        assert!((ob.w - 100.0).abs() < 1e-3 && (ob.h - 200.0).abs() < 1e-3);
        let copies = d.duplicate(&[a], 20.0);
        assert_eq!(d.objects.len(), 3);
        assert_eq!(d.get(copies[0]).unwrap().bounds().x, 20.0);
        assert_eq!(d.remove(&[a, b]), 2);
        assert_eq!(d.objects.len(), 1);
    }

    #[test]
    fn lock_reorder_and_hit() {
        let mut d = Document::new("a.png", (800, 600));
        let a = d.add(rect_obj(0.0, 0.0, 100.0, 100.0));
        let b = d.add(rect_obj(50.0, 50.0, 100.0, 100.0));
        assert_eq!(d.hit(p(75.0, 75.0), 0.0), Some(b), "topmost wins");
        d.reorder(b, Order::Back);
        assert_eq!(d.hit(p(75.0, 75.0), 0.0), Some(a));
        d.reorder(b, Order::Forward);
        assert_eq!(d.objects[1].id, b);
        d.set_locked(a, true);
        d.translate(&[a], 10.0, 0.0);
        assert_eq!(d.get(a).unwrap().bounds().x, 0.0, "locked objects don't move");
        assert_eq!(d.remove(&[a]), 0, "or delete");
        d.set_locked(a, false);
        assert_eq!(d.remove(&[a]), 1);
        assert_eq!(d.hit(p(500.0, 500.0), 2.0), None);
    }

    #[test]
    fn steps_auto_increment() {
        let mut d = Document::new("a.png", (800, 600));
        for _ in 0..3 {
            let n = d.next_step();
            d.add(Kind::Step(Step { center: p(10.0, 10.0), radius: 16.0, n, color: RED }));
        }
        assert_eq!(d.next_step(), 4);
        let id = d.objects[0].id;
        let copies = d.duplicate(&[id], 10.0);
        let Kind::Step(s) = &d.get(copies[0]).unwrap().kind else { panic!() };
        assert_eq!(s.n, 4);
    }

    #[test]
    fn undo_redo_every_action() {
        let mut d = Document::new("a.png", (800, 600));
        let mut h = History::default();
        h.checkpoint(&d);
        let a = d.add(rect_obj(0.0, 0.0, 10.0, 10.0));
        h.checkpoint(&d);
        d.translate(&[a], 5.0, 0.0);
        h.checkpoint(&d);
        d.crop_to_aspect(1.0);
        h.checkpoint(&d);
        d.frame.enabled = true;
        let end = d.clone();
        assert!(h.undo(&mut d));
        assert!(!d.frame.enabled);
        assert!(h.undo(&mut d));
        assert_eq!(d.crop, r(0.0, 0.0, 800.0, 600.0));
        assert!(h.undo(&mut d));
        assert_eq!(d.get(a).unwrap().bounds().x, 0.0);
        assert!(h.undo(&mut d));
        assert!(d.objects.is_empty());
        assert!(!h.undo(&mut d));
        while h.redo(&mut d) {}
        assert_eq!(d, end);
        // A new change clears redo.
        h.undo(&mut d);
        h.checkpoint(&d);
        d.add(rect_obj(1.0, 1.0, 2.0, 2.0));
        assert!(!h.can_redo());
        // A click without a change leaves no step.
        let steps = h.undo.len();
        h.checkpoint(&d);
        h.discard_if_unchanged(&d);
        assert_eq!(h.undo.len(), steps);
    }

    #[test]
    fn rotate_and_flip_carry_objects_and_crop() {
        let mut d = Document::new("a.png", (800, 600));
        let a = d.add(rect_obj(0.0, 0.0, 100.0, 50.0));
        let l = d.add(Kind::Line(Line { from: p(10.0, 20.0), to: p(30.0, 40.0), color: RED, width: 2.0 }));
        d.set_crop(r(0.0, 0.0, 400.0, 300.0));
        d.rotate_image(true);
        assert_eq!(d.oriented_size(), (600, 800));
        // Top-left corner of an 800×600 image goes to the top-right of the 600×800 one.
        assert_eq!(d.crop, r(300.0, 0.0, 300.0, 400.0));
        let Kind::Line(line) = &d.get(l).unwrap().kind else { panic!() };
        assert_eq!(line.from, p(580.0, 10.0));
        let o = d.get(a).unwrap();
        assert_eq!(o.rotation, 90.0);
        assert_eq!(o.bounds().center(), p(575.0, 50.0));
        // Four turns are the identity.
        for _ in 0..3 {
            d.rotate_image(true);
        }
        assert_eq!(d.orient, Orient::default());
        assert_eq!(d.crop, r(0.0, 0.0, 400.0, 300.0));
        let Kind::Line(line) = &d.get(l).unwrap().kind else { panic!() };
        assert!(line.from.dist(p(10.0, 20.0)) < 1e-3);
        // Flip twice is the identity; flipping both ways is a half turn.
        d.flip(true);
        assert!(d.orient.flip_h);
        d.flip(true);
        assert_eq!(d.orient, Orient::default());
        d.flip(true);
        d.flip(false);
        assert_eq!(d.orient, Orient { quarter: 2, flip_h: false, flip_v: false });
    }

    #[test]
    fn crop_aspect_presets() {
        let mut d = Document::new("a.png", (1920, 1080));
        d.crop_to_aspect(1.0);
        assert_eq!(d.crop, r(420.0, 0.0, 1080.0, 1080.0));
        d.reset_crop();
        d.crop_to_aspect(9.0 / 16.0);
        assert_eq!((d.crop.w, d.crop.h), (608.0, 1080.0));
        d.reset_crop();
        d.crop_to_aspect(16.0 / 9.0);
        assert_eq!(d.crop, r(0.0, 0.0, 1920.0, 1080.0));
        d.set_crop(r(10.0, 10.0, 2.0, 2.0));
        assert_eq!((d.crop.w, d.crop.h), (8.0, 8.0), "minimum crop");
    }

    #[test]
    fn frame_layout_padding_and_sizes() {
        let mut f = Frame { enabled: true, padding: 100, ..Frame::default() };
        let l = f.layout((800, 600));
        assert_eq!(l.canvas, (1000, 800));
        assert_eq!(l.content, r(100.0, 100.0, 800.0, 600.0));
        f.window = WindowStyle::Mac;
        let l = f.layout((800, 600));
        assert_eq!(l.bar, 28.0);
        assert_eq!(l.canvas, (1000, 828));
        assert_eq!(l.content.y, 128.0);
        f.window = WindowStyle::None;
        // 1:1 canvas around 1000×800: 1000×1000, window centered vertically.
        f.size = OutputSize::Aspect(1, 1);
        let l = f.layout((800, 600));
        assert_eq!(l.canvas, (1000, 1000));
        assert_eq!(l.content, r(100.0, 200.0, 800.0, 600.0));
        // 9:16 is taller than wide.
        f.size = OutputSize::Aspect(9, 16);
        let l = f.layout((800, 600));
        assert_eq!(l.canvas.0, 1000);
        assert!((l.canvas.1 as f32 - 1000.0 * 16.0 / 9.0).abs() <= 1.0);
        // A fixed canvas shrinks the window to fit inside the padding, centered.
        f.size = OutputSize::Custom(500, 500);
        let l = f.layout((800, 600));
        assert_eq!(l.canvas, (500, 500));
        assert!((l.scale - 300.0 / 800.0).abs() < 1e-6);
        assert!((l.content.w - 300.0).abs() < 1e-3 && (l.content.x - 100.0).abs() < 1e-3);
        assert!((l.content.y + l.content.h / 2.0 - 250.0).abs() < 1.0);
        // Disabled: just the image.
        f.enabled = false;
        assert_eq!(f.layout((800, 600)).canvas, (800, 600));
    }

    #[test]
    fn json_roundtrip_and_sidecar() {
        let mut d = Document::new("shot.png", (100, 100));
        d.add(Kind::Text(Text {
            rect: r(1.0, 2.0, 30.0, 10.0),
            text: "Hi".into(),
            font: String::new(),
            size: 24.0,
            weight: 700,
            color: RED,
            align: Align::Center,
            pill: Some([0, 0, 0, 200]),
            outline: None,
            shadow: true,
        }));
        d.add(Kind::Redact(Redact { area: Area::Brush { points: vec![p(1.0, 1.0), p(5.0, 5.0)], width: 10.0 }, effect: Effect::Pixelate(12.0) }));
        let back = Document::from_json(&d.to_json()).unwrap();
        assert_eq!(back, d);
        assert_eq!(Document::sidecar_path(Path::new("/x/shot.png")), PathBuf::from("/x/shot.clipper.json"));
    }

    #[test]
    fn arrow_curve_and_smoothing() {
        let mut a = Arrow { from: p(0.0, 0.0), to: p(100.0, 0.0), bend: 0.0, double: false, head: Head::Filled, color: RED, width: 4.0, ..Default::default() };
        a.bend = 0.4;
        assert!((a.control().y + 40.0).abs() < 1e-3);
        let path = arrow_path(&a, 10);
        assert_eq!(path.len(), 11);
        assert!((path[5].y + 20.0).abs() < 1e-3, "peak halfway to the control point");
        let s = smooth(&[p(0.0, 0.0), p(10.0, 10.0), p(20.0, 0.0)], 2);
        assert_eq!(s.first(), Some(&p(0.0, 0.0)));
        assert_eq!(s.last(), Some(&p(20.0, 0.0)));
        assert!(s.len() > 3);
        assert_eq!(simplify(&[p(0.0, 0.0), p(0.5, 0.0), p(3.0, 0.0)], 2.0).len(), 2);
    }
}
