//! Canvas geometry, separate from the window: mapping between screen and document points,
//! where an object's handles are, snapping a move to guides, and resizing a (possibly
//! rotated) object from a handle. Pure functions, tested here; the view only feeds in
//! pointer positions and applies the results.

use super::doc::*;

/// Document point ↔ window point for the current zoom, pan and frame layout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Map {
    /// Window position of output pixel (0, 0).
    pub ox: f32,
    pub oy: f32,
    /// Screen points per output pixel.
    pub k: f32,
    pub crop: R,
    /// The cropped image inside the output (frame layout).
    pub content: R,
    pub lscale: f32,
}

impl Map {
    pub fn to_screen(&self, q: P) -> P {
        p(self.ox + (self.content.x + (q.x - self.crop.x) * self.lscale) * self.k, self.oy + (self.content.y + (q.y - self.crop.y) * self.lscale) * self.k)
    }

    pub fn to_doc(&self, s: P) -> P {
        p(((s.x - self.ox) / self.k - self.content.x) / self.lscale + self.crop.x, ((s.y - self.oy) / self.k - self.content.y) / self.lscale + self.crop.y)
    }

    /// Screen points per document pixel.
    pub fn unit(&self) -> f32 {
        self.k * self.lscale
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PointHandle {
    From,
    To,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Handle {
    Point(PointHandle),
    /// An arrow's anchor.
    Anchor(usize),
    /// An arrow anchor's tangent handle (`true`: the outgoing one).
    Tangent(usize, bool),
    /// Edge directions: -1 left/top, 1 right/bottom, 0 middle.
    Resize(i8, i8),
    Rotate,
}

/// Distance of the rotation handle above the top edge, in screen points.
const ROTATE_GAP: f32 = 26.0;

/// An object's handles in document coordinates. Lines and arrows get their end points (and
/// the arrow's bend point); everything else gets 8 resize handles and a rotation handle.
pub fn handles(o: &Object, unit: f32) -> Vec<(Handle, P)> {
    let b = o.bounds();
    let c = b.center();
    let rot = |q: P| q.rotate(c, o.rotation);
    match &o.kind {
        Kind::Arrow(a) => {
            // Anchors first, so they win over a tangent handle on top of them.
            let anchors = a.spline();
            let mut out: Vec<(Handle, P)> = anchors.iter().enumerate().map(|(i, an)| (Handle::Anchor(i), rot(an.p))).collect();
            if a.style.curved() {
                for (i, an) in anchors.iter().enumerate() {
                    if an.c_in.dist(an.p) > 0.5 {
                        out.push((Handle::Tangent(i, false), rot(an.c_in)));
                    }
                    if an.c_out.dist(an.p) > 0.5 {
                        out.push((Handle::Tangent(i, true), rot(an.c_out)));
                    }
                }
            }
            out
        }
        Kind::Line(l) => vec![(Handle::Point(PointHandle::From), rot(l.from)), (Handle::Point(PointHandle::To), rot(l.to))],
        _ => {
            let mut out: Vec<(Handle, P)> = [(-1, -1), (0, -1), (1, -1), (1, 0), (1, 1), (0, 1), (-1, 1), (-1, 0)]
                .into_iter()
                .map(|(ex, ey)| (Handle::Resize(ex, ey), rot(p(c.x + ex as f32 * b.w / 2.0, c.y + ey as f32 * b.h / 2.0))))
                .collect();
            out.push((Handle::Rotate, rot(p(c.x, b.y - ROTATE_GAP / unit))));
            out
        }
    }
}

/// The handle within `radius` document pixels of `q` (the nearest).
pub fn handle_at(o: &Object, q: P, unit: f32, radius: f32) -> Option<Handle> {
    let hs = handles(o, unit);
    let near = |h: &(Handle, P)| h.1.dist(q) <= radius;
    // An anchor in reach beats a tangent handle that's a little nearer.
    hs.iter()
        .filter(|h| near(h) && matches!(h.0, Handle::Anchor(_)))
        .min_by(|a, b| a.1.dist(q).total_cmp(&b.1.dist(q)))
        .or_else(|| hs.iter().filter(|h| near(h)).min_by(|a, b| a.1.dist(q).total_cmp(&b.1.dist(q))))
        .map(|(h, _)| *h)
}

/// Lines a move can snap to: the crop's edges and center, and other objects' edges and
/// centers (x values, y values).
pub fn snap_targets(doc: &Document, moving: &[u64]) -> (Vec<f32>, Vec<f32>) {
    let c = doc.crop;
    let mut xs = vec![c.x, c.center().x, c.right()];
    let mut ys = vec![c.y, c.center().y, c.bottom()];
    for o in doc.objects.iter().filter(|o| !moving.contains(&o.id)) {
        let b = o.outer_bounds();
        xs.extend([b.x, b.center().x, b.right()]);
        ys.extend([b.y, b.center().y, b.bottom()]);
    }
    (xs, ys)
}

/// Snaps a move of `bounds` by `(dx, dy)`: if an edge or the center comes within `threshold`
/// of a target, the move is adjusted to land on it. Returns the move and the guide lines hit.
pub fn snap_move(bounds: R, dx: f32, dy: f32, targets: &(Vec<f32>, Vec<f32>), threshold: f32) -> (f32, f32, Option<f32>, Option<f32>) {
    let nearest = |vals: [f32; 3], ts: &[f32]| -> Option<(f32, f32)> {
        let mut best: Option<(f32, f32)> = None;
        for v in vals {
            for &t in ts {
                let d = t - v;
                if d.abs() <= threshold && best.is_none_or(|(bd, _)| d.abs() < bd.abs()) {
                    best = Some((d, t));
                }
            }
        }
        best
    };
    let sx = nearest([bounds.x + dx, bounds.center().x + dx, bounds.right() + dx], &targets.0);
    let sy = nearest([bounds.y + dy, bounds.center().y + dy, bounds.bottom() + dy], &targets.1);
    (dx + sx.map_or(0.0, |s| s.0), dy + sy.map_or(0.0, |s| s.0), sx.map(|s| s.1), sy.map(|s| s.1))
}

/// New unrotated bounds when the handle `(ex, ey)` of `orig` is dragged to `q` (document
/// point). The opposite handle stays where it is on screen, also for rotated objects;
/// `keep_aspect` keeps the proportions on corner handles.
pub fn resize(orig: &Object, ex: i8, ey: i8, q: P, keep_aspect: bool) -> R {
    let b = orig.bounds();
    let c = b.center();
    let local = q.rotate(c, -orig.rotation);
    let min = 4.0;
    let (mut x0, mut y0, mut x1, mut y1) = (b.x, b.y, b.right(), b.bottom());
    match ex {
        -1 => x0 = local.x.min(x1 - min),
        1 => x1 = local.x.max(x0 + min),
        _ => {}
    }
    match ey {
        -1 => y0 = local.y.min(y1 - min),
        1 => y1 = local.y.max(y0 + min),
        _ => {}
    }
    if keep_aspect && ex != 0 && ey != 0 && b.w > 0.0 && b.h > 0.0 {
        let a = b.w / b.h;
        let (w, h) = (x1 - x0, y1 - y0);
        let (w, h) = if w / h > a { (h * a, h) } else { (w, w / a) };
        if ex < 0 { x0 = x1 - w } else { x1 = x0 + w }
        if ey < 0 { y0 = y1 - h } else { y1 = y0 + h }
    }
    let nb = r(x0, y0, x1 - x0, y1 - y0);
    // Rotation is about the center, which moved: shift so the fixed corner stays put.
    let fixed_local = p(c.x - ex as f32 * b.w / 2.0, c.y - ey as f32 * b.h / 2.0);
    let world_fixed = fixed_local.rotate(c, orig.rotation);
    let rel = p(fixed_local.x - nb.center().x, fixed_local.y - nb.center().y).rotate(p(0.0, 0.0), orig.rotation);
    let nc = p(world_fixed.x - rel.x, world_fixed.y - rel.y);
    r(nc.x - nb.w / 2.0, nc.y - nb.h / 2.0, nb.w, nb.h)
}

/// Points snapped to 45° steps from `a` (Shift while drawing lines and arrows).
pub fn snap_angle(a: P, b: P) -> P {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len = (dx * dx + dy * dy).sqrt();
    let ang = (dy.atan2(dx) / std::f32::consts::FRAC_PI_4).round() * std::f32::consts::FRAC_PI_4;
    p(a.x + len * ang.cos(), a.y + len * ang.sin())
}

/// The square corner from `a` toward `b` (Shift while drawing boxes).
pub fn square(a: P, b: P) -> P {
    let s = (b.x - a.x).abs().max((b.y - a.y).abs());
    p(a.x + s * (b.x - a.x).signum(), a.y + s * (b.y - a.y).signum())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Object {
        Object { id: 1, kind: Kind::Shape(Shape { kind: ShapeKind::Rect, rect: r(x, y, w, h), stroke: None, fill: Some([0, 0, 0, 255]), width: 1.0, radius: 0.0 }), rotation: 0.0, locked: false }
    }

    #[test]
    fn mapping_round_trips_with_frame_and_zoom() {
        let m = Map { ox: 100.0, oy: 50.0, k: 0.5, crop: r(200.0, 100.0, 800.0, 600.0), content: r(40.0, 60.0, 400.0, 300.0), lscale: 0.5 };
        let q = p(500.0, 400.0);
        let s = m.to_screen(q);
        assert!(m.to_doc(s).dist(q) < 1e-3);
        // The crop's top-left lands at the content's top-left.
        assert_eq!(m.to_screen(p(200.0, 100.0)), p(100.0 + 40.0 * 0.5, 50.0 + 60.0 * 0.5));
        assert_eq!(m.unit(), 0.25);
    }

    #[test]
    fn handles_follow_rotation() {
        let mut o = rect(0.0, 0.0, 100.0, 50.0);
        let hs = handles(&o, 1.0);
        assert_eq!(hs.len(), 9);
        assert_eq!(hs[0], (Handle::Resize(-1, -1), p(0.0, 0.0)));
        assert_eq!(hs[8], (Handle::Rotate, p(50.0, -26.0)));
        o.rotation = 90.0;
        let tl = handles(&o, 1.0)[0].1;
        // Top-left of a 100×50 box turned 90° about (50, 25) sits at (75, -25).
        assert!(tl.dist(p(75.0, -25.0)) < 1e-3, "{tl:?}");
        assert_eq!(handle_at(&o, p(76.0, -24.0), 1.0, 5.0), Some(Handle::Resize(-1, -1)));
        assert_eq!(handle_at(&o, p(300.0, 300.0), 1.0, 5.0), None);
        // Lines: just their ends.
        let line = Object { kind: Kind::Line(Line { from: p(0.0, 0.0), to: p(10.0, 0.0), color: [0; 4], width: 1.0 }), ..o };
        assert_eq!(handles(&line, 1.0).len(), 2);
    }

    #[test]
    fn moves_snap_to_center_and_edges() {
        let mut d = Document::new("a.png", (1000, 600));
        d.add(rect(700.0, 100.0, 100.0, 100.0).kind);
        let t = snap_targets(&d, &[]);
        // A 100-wide box at x=440 moved by 7: its center (497) snaps to the crop center (500).
        let (dx, dy, gx, gy) = snap_move(r(440.0, 320.0, 100.0, 50.0), 7.0, 0.0, &t, 5.0);
        assert_eq!((dx, gx), (10.0, Some(500.0)));
        assert_eq!((dy, gy), (0.0, None));
        // Its top edge snaps to the other object's top.
        let (_, dy, _, gy) = snap_move(r(100.0, 104.0, 50.0, 50.0), 0.0, 0.0, &t, 5.0);
        assert_eq!((dy, gy), (-4.0, Some(100.0)));
        // Out of range: unchanged.
        assert_eq!(snap_move(r(100.0, 150.0, 50.0, 50.0), 3.0, 3.0, &t, 2.0), (3.0, 3.0, None, None));
    }

    #[test]
    fn resize_keeps_the_opposite_corner_fixed() {
        let o = rect(0.0, 0.0, 100.0, 50.0);
        assert_eq!(resize(&o, 1, 1, p(200.0, 80.0), false), r(0.0, 0.0, 200.0, 80.0));
        assert_eq!(resize(&o, 1, 1, p(200.0, 80.0), true), r(0.0, 0.0, 160.0, 80.0), "keeps 2:1");
        assert_eq!(resize(&o, -1, 0, p(-50.0, 999.0), false), r(-50.0, 0.0, 150.0, 50.0), "edge handle moves one side");
        // Rotated: dragging the bottom-right corner leaves the top-left corner where it was.
        let mut o = rect(0.0, 0.0, 100.0, 50.0);
        o.rotation = 30.0;
        let before = o.corners()[0];
        let target = o.corners()[2];
        let nb = resize(&o, 1, 1, p(target.x + 20.0, target.y + 20.0), false);
        let mut moved = o.clone();
        moved.set_bounds(nb);
        assert!(moved.corners()[0].dist(before) < 1e-2, "{:?} vs {:?}", moved.corners()[0], before);
        assert!(nb.w > 100.0);
    }

    #[test]
    fn shift_constraints() {
        let q = snap_angle(p(0.0, 0.0), p(10.0, 9.0));
        assert!((q.x - q.y).abs() < 1e-3, "45°");
        assert_eq!(square(p(0.0, 0.0), p(10.0, -4.0)), p(10.0, -10.0));
    }
}
