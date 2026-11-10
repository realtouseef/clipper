//! Arrow geometry: a spline of anchors with Bezier tangent handles, drawn as Curved,
//! Straight, Elbow or Hand-drawn. Sampling, hit testing, inserting anchors, bending a
//! segment by dragging its body, simplifying and fitting freehand strokes, and the seeded
//! marker wobble. Pure functions; no UI.

use super::doc::{P, p};
use serde::{Deserialize, Serialize};

/// A point on the spline with its two tangent handles (absolute positions; a handle on the
/// anchor itself means no tangent on that side). `smooth` keeps the handles in line.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
pub struct Anchor {
    pub p: P,
    pub c_in: P,
    pub c_out: P,
    #[serde(default = "yes")]
    pub smooth: bool,
}

fn yes() -> bool {
    true
}

impl Anchor {
    pub fn sharp(q: P) -> Self {
        Anchor { p: q, c_in: q, c_out: q, smooth: false }
    }

    /// Moves the anchor and both its handles.
    pub fn shift(&mut self, dx: f32, dy: f32) {
        for q in [&mut self.p, &mut self.c_in, &mut self.c_out] {
            q.x += dx;
            q.y += dy;
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum ArrowStyle {
    #[default]
    Curved,
    Straight,
    Elbow,
    Hand,
}

impl ArrowStyle {
    pub const ALL: [ArrowStyle; 4] = [ArrowStyle::Curved, ArrowStyle::Straight, ArrowStyle::Elbow, ArrowStyle::Hand];

    pub fn label(self) -> &'static str {
        match self {
            ArrowStyle::Curved => "Curved",
            ArrowStyle::Straight => "Straight",
            ArrowStyle::Elbow => "Elbow",
            ArrowStyle::Hand => "Hand-drawn",
        }
    }

    /// Styles whose segments are Bezier curves (with tangent handles to edit).
    pub fn curved(self) -> bool {
        matches!(self, ArrowStyle::Curved | ArrowStyle::Hand)
    }
}

fn lerp(a: P, b: P, t: f32) -> P {
    p(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

/// Point on the cubic Bezier `a b c d` at `t`.
pub fn cubic(a: P, b: P, c: P, d: P, t: f32) -> P {
    let u = 1.0 - t;
    let (w0, w1, w2, w3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    p(w0 * a.x + w1 * b.x + w2 * c.x + w3 * d.x, w0 * a.y + w1 * b.y + w2 * c.y + w3 * d.y)
}

/// The control points of segment `i` (anchor `i` to `i + 1`).
fn seg(anchors: &[Anchor], i: usize) -> [P; 4] {
    [anchors[i].p, anchors[i].c_out, anchors[i + 1].c_in, anchors[i + 1].p]
}

/// A two-anchor curve equivalent to the old arrow model: a quadratic with control `c`.
pub fn from_quadratic(from: P, to: P, c: P) -> Vec<Anchor> {
    let k = 2.0 / 3.0;
    vec![
        Anchor { p: from, c_in: from, c_out: lerp(from, c, k), smooth: true },
        Anchor { p: to, c_in: lerp(to, c, k), c_out: to, smooth: true },
    ]
}

/// The default gentle curve from `from` to `to` (bend as a fraction of the length, to the
/// left of the direction of travel; 0 = straight).
pub fn default_curve(from: P, to: P, bend: f32) -> Vec<Anchor> {
    let (dx, dy) = (to.x - from.x, to.y - from.y);
    let mid = lerp(from, to, 0.5);
    from_quadratic(from, to, p(mid.x + dy * bend, mid.y - dx * bend))
}

/// Orthogonal route between two points: horizontal, vertical, horizontal.
fn elbow(a: P, b: P) -> [P; 4] {
    let mx = (a.x + b.x) / 2.0;
    [a, p(mx, a.y), p(mx, b.y), b]
}

/// The arrow's centerline as a polyline (`n` samples per curved segment).
pub fn sample(anchors: &[Anchor], style: ArrowStyle, n: usize) -> Vec<P> {
    if anchors.len() < 2 {
        return anchors.iter().map(|a| a.p).collect();
    }
    let mut out = vec![anchors[0].p];
    for i in 0..anchors.len() - 1 {
        match style {
            ArrowStyle::Straight => out.push(anchors[i + 1].p),
            ArrowStyle::Elbow => out.extend_from_slice(&elbow(anchors[i].p, anchors[i + 1].p)[1..]),
            ArrowStyle::Curved | ArrowStyle::Hand => {
                let [a, b, c, d] = seg(anchors, i);
                out.extend((1..=n).map(|k| cubic(a, b, c, d, k as f32 / n as f32)));
            }
        }
    }
    out
}

/// The point of the spline nearest `q`: (segment, parameter along it, distance, point).
pub fn nearest(anchors: &[Anchor], style: ArrowStyle, q: P) -> Option<(usize, f32, f32, P)> {
    const N: usize = 48;
    let mut best: Option<(usize, f32, f32, P)> = None;
    for i in 0..anchors.len().saturating_sub(1) {
        let pts: Vec<P> = match style {
            ArrowStyle::Straight => vec![anchors[i].p, anchors[i + 1].p],
            ArrowStyle::Elbow => elbow(anchors[i].p, anchors[i + 1].p).to_vec(),
            _ => {
                let [a, b, c, d] = seg(anchors, i);
                (0..=N).map(|k| cubic(a, b, c, d, k as f32 / N as f32)).collect()
            }
        };
        let m = pts.len() - 1;
        for (k, w) in pts.windows(2).enumerate() {
            let (dx, dy) = (w[1].x - w[0].x, w[1].y - w[0].y);
            let len2 = dx * dx + dy * dy;
            let f = if len2 > 0.0 { (((q.x - w[0].x) * dx + (q.y - w[0].y) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
            let at = p(w[0].x + f * dx, w[0].y + f * dy);
            let dist = at.dist(q);
            if best.is_none_or(|b| dist < b.2) {
                best = Some((i, (k as f32 + f) / m as f32, dist, at));
            }
        }
    }
    best
}

/// Is `q` within `tol` of the spline?
#[cfg_attr(not(test), allow(dead_code))]
pub fn hit(anchors: &[Anchor], style: ArrowStyle, q: P, tol: f32) -> bool {
    nearest(anchors, style, q).is_some_and(|(_, _, d, _)| d <= tol)
}

/// Inserts an anchor on segment `i` at `t` without changing the curve's shape (de Casteljau
/// split). Returns the new anchor's index.
pub fn insert(anchors: &mut Vec<Anchor>, i: usize, t: f32) -> usize {
    let t = t.clamp(0.01, 0.99);
    let [a, b, c, d] = seg(anchors, i);
    let (ab, bc, cd) = (lerp(a, b, t), lerp(b, c, t), lerp(c, d, t));
    let (abc, bcd) = (lerp(ab, bc, t), lerp(bc, cd, t));
    let mid = lerp(abc, bcd, t);
    anchors[i].c_out = ab;
    anchors[i + 1].c_in = cd;
    anchors.insert(i + 1, Anchor { p: mid, c_in: abc, c_out: bcd, smooth: true });
    i + 1
}

/// Removes anchor `i` (never the two ends' last pair: an arrow keeps at least 2 anchors).
pub fn remove(anchors: &mut Vec<Anchor>, i: usize) -> bool {
    if anchors.len() <= 2 || i >= anchors.len() {
        return false;
    }
    anchors.remove(i);
    true
}

/// Bends segment `i` so its point at `t` moves by `delta`: both inner handles move by the
/// same offset, which moves the point by 3t(1−t) of it. Sharp straight segments get handles.
pub fn bend(anchors: &mut [Anchor], i: usize, t: f32, delta: P) {
    let t = t.clamp(0.1, 0.9);
    let k = 1.0 / (3.0 * t * (1.0 - t));
    let off = p(delta.x * k, delta.y * k);
    let (a, b) = (i, i + 1);
    anchors[a].c_out = p(anchors[a].c_out.x + off.x, anchors[a].c_out.y + off.y);
    anchors[b].c_in = p(anchors[b].c_in.x + off.x, anchors[b].c_in.y + off.y);
    for j in [a, b] {
        if anchors[j].smooth {
            mirror(anchors, j, j == a);
        }
    }
}

/// Keeps a smooth anchor's handles in line: the other handle points opposite the moved one
/// (keeping its own length).
pub fn mirror(anchors: &mut [Anchor], i: usize, moved_out: bool) {
    let a = anchors[i];
    let (moved, other) = if moved_out { (a.c_out, a.c_in) } else { (a.c_in, a.c_out) };
    let (mx, my) = (moved.x - a.p.x, moved.y - a.p.y);
    let ml = (mx * mx + my * my).sqrt();
    let ol = other.dist(a.p);
    if ml < 1e-3 || ol < 1e-3 {
        return;
    }
    let o = p(a.p.x - mx / ml * ol, a.p.y - my / ml * ol);
    if moved_out {
        anchors[i].c_in = o;
    } else {
        anchors[i].c_out = o;
    }
}

/// Smooth: handles along the line between the neighbors (Catmull-Rom). Sharp: no handles.
pub fn set_smooth(anchors: &mut [Anchor], i: usize, smooth: bool) {
    let n = anchors.len();
    anchors[i].smooth = smooth;
    let q = anchors[i].p;
    if !smooth || n < 2 {
        anchors[i].c_in = q;
        anchors[i].c_out = q;
        return;
    }
    let prev = anchors[i.saturating_sub(1)].p;
    let next = anchors[(i + 1).min(n - 1)].p;
    let (tx, ty) = ((next.x - prev.x) / 6.0, (next.y - prev.y) / 6.0);
    anchors[i].c_in = if i > 0 { p(q.x - tx, q.y - ty) } else { q };
    anchors[i].c_out = if i + 1 < n { p(q.x + tx, q.y + ty) } else { q };
}

/// Ramer-Douglas-Peucker: drops points within `eps` of the simplified line.
pub fn rdp(points: &[P], eps: f32) -> Vec<P> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let (a, b) = (points[0], points[points.len() - 1]);
    let (mut far, mut idx) = (0.0, 0);
    for (i, q) in points.iter().enumerate().take(points.len() - 1).skip(1) {
        let d = super::doc::seg_dist(*q, a, b);
        if d > far {
            far = d;
            idx = i;
        }
    }
    if far <= eps {
        return vec![a, b];
    }
    let mut left = rdp(&points[..=idx], eps);
    let right = rdp(&points[idx..], eps);
    left.pop();
    left.extend(right);
    left
}

/// A smooth spline through `points` (Catmull-Rom tangents as Bezier handles).
pub fn fit(points: &[P]) -> Vec<Anchor> {
    let mut out: Vec<Anchor> = points.iter().map(|q| Anchor::sharp(*q)).collect();
    if out.len() >= 3 {
        for i in 0..out.len() {
            set_smooth(&mut out, i, true);
        }
    } else if out.len() == 2 {
        let (a, b) = (out[0].p, out[1].p);
        out[0].c_out = lerp(a, b, 1.0 / 3.0);
        out[1].c_in = lerp(a, b, 2.0 / 3.0);
        out[0].smooth = true;
        out[1].smooth = true;
    }
    out
}

/// A tiny deterministic hash (splitmix) for the wobble's phases.
fn hash(seed: u32, k: u32) -> f32 {
    let mut z = (seed as u64) << 32 | k as u64;
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^= z >> 31;
    (z >> 40) as f32 / (1u64 << 24) as f32
}

/// Marker wobble: the polyline resampled every few pixels and pushed sideways by a sum of
/// waves along its length. Fully determined by the geometry, `roughness` (0..1), `width`
/// and `seed`, so preview, export and reopening all match. The ends stay put (heads and
/// connectors land exactly).
pub fn wobble(points: &[P], roughness: f32, width: f32, seed: u32) -> Vec<P> {
    if roughness <= 0.0 || points.len() < 2 {
        return points.to_vec();
    }
    // Resample at a fixed step.
    let step = 3.0;
    let mut pts = vec![points[0]];
    let mut carry = 0.0;
    for w in points.windows(2) {
        let len = w[0].dist(w[1]);
        let mut at = step - carry;
        while at < len {
            pts.push(lerp(w[0], w[1], at / len));
            at += step;
        }
        carry = (carry + len) % step;
    }
    pts.push(points[points.len() - 1]);
    let total = (pts.len() - 1) as f32 * step;
    let amp = roughness * (1.5 + width * 0.45);
    let waves: Vec<(f32, f32, f32)> = (0..3)
        .map(|k| {
            let lambda = [90.0, 37.0, 15.0][k] * (0.8 + 0.4 * hash(seed, k as u32 * 2));
            let phase = hash(seed, k as u32 * 2 + 1) * std::f32::consts::TAU;
            (lambda, phase, 1.0 / (k as f32 + 1.0))
        })
        .collect();
    let n = pts.len();
    (0..n)
        .map(|i| {
            let s = i as f32 * step;
            let taper = (s / 18.0).min((total - s) / 18.0).clamp(0.0, 1.0);
            let wave: f32 = waves.iter().map(|(l, ph, w)| (s / l * std::f32::consts::TAU + ph).sin() * w).sum();
            let (a, b) = (pts[i.saturating_sub(1)], pts[(i + 1).min(n - 1)]);
            let (dx, dy) = (b.x - a.x, b.y - a.y);
            let len = (dx * dx + dy * dy).sqrt().max(1e-3);
            let off = wave * amp * taper;
            p(pts[i].x - dy / len * off, pts[i].y + dx / len * off)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_testing_and_inserting_keep_the_shape() {
        let mut a = default_curve(p(0.0, 0.0), p(300.0, 0.0), 0.25);
        // The curve bulges up (negative y): its middle is near y = -37.5.
        let mid = sample(&a, ArrowStyle::Curved, 2)[1];
        assert!((mid.x - 150.0).abs() < 0.5 && (mid.y + 37.5).abs() < 0.5, "{mid:?}");
        assert!(hit(&a, ArrowStyle::Curved, p(150.0, -33.0), 8.0));
        assert!(!hit(&a, ArrowStyle::Curved, p(150.0, 0.0), 8.0), "the chord is not the curve");
        assert!(hit(&a, ArrowStyle::Straight, p(150.0, 4.0), 8.0));
        let (seg, t, _, _) = nearest(&a, ArrowStyle::Curved, p(75.0, -30.0)).unwrap();
        assert_eq!(seg, 0);
        let before = sample(&a, ArrowStyle::Curved, 64);
        let i = insert(&mut a, seg, t);
        assert_eq!((i, a.len()), (1, 3));
        // Same curve after the split.
        for q in before.iter().step_by(4) {
            let (_, _, d, _) = nearest(&a, ArrowStyle::Curved, *q).unwrap();
            assert!(d < 0.6, "moved by {d} at {q:?}");
        }
        assert!(remove(&mut a, 1));
        assert!(!remove(&mut a, 0), "keeps two anchors");
    }

    #[test]
    fn bending_moves_the_grabbed_point() {
        let mut a = fit(&[p(0.0, 0.0), p(300.0, 0.0)]);
        let (seg, t, _, at) = nearest(&a, ArrowStyle::Curved, p(100.0, 0.0)).unwrap();
        bend(&mut a, seg, t, p(0.0, 60.0));
        let moved = cubic(a[0].p, a[0].c_out, a[1].c_in, a[1].p, t);
        assert!(moved.dist(p(at.x, at.y + 60.0)) < 0.5, "{moved:?}");
        // The ends stay.
        assert_eq!((a[0].p, a[1].p), (p(0.0, 0.0), p(300.0, 0.0)));
    }

    #[test]
    fn rdp_stays_within_tolerance() {
        let pts: Vec<P> = (0..=200).map(|i| {
            let x = i as f32 * 2.0;
            p(x, (x / 40.0).sin() * 30.0 + if i % 7 == 0 { 0.8 } else { 0.0 })
        }).collect();
        for eps in [0.5, 2.0, 6.0] {
            let s = rdp(&pts, eps);
            assert!(s.len() < pts.len() && s.len() >= 2);
            assert_eq!((s[0], *s.last().unwrap()), (pts[0], *pts.last().unwrap()));
            // Every original point is within eps of the simplified polyline.
            for q in &pts {
                let d = s.windows(2).map(|w| super::super::doc::seg_dist(*q, w[0], w[1])).fold(f32::MAX, f32::min);
                assert!(d <= eps + 1e-3, "{q:?} is {d} away at eps {eps}");
            }
        }
        // The fitted spline passes through the kept points.
        let s = rdp(&pts, 2.0);
        let f = fit(&s);
        assert_eq!(f.len(), s.len());
        assert!(f.iter().zip(&s).all(|(a, q)| a.p == *q));
    }

    #[test]
    fn wobble_is_seeded_and_keeps_the_ends() {
        let line = [p(0.0, 0.0), p(400.0, 0.0)];
        let a = wobble(&line, 0.8, 6.0, 7);
        assert_eq!(a, wobble(&line, 0.8, 6.0, 7), "same seed, same wobble");
        assert_ne!(a, wobble(&line, 0.8, 6.0, 8), "a reroll changes it");
        assert_eq!((a[0], *a.last().unwrap()), (line[0], line[1]));
        let max = a.iter().map(|q| q.y.abs()).fold(0.0, f32::max);
        assert!(max > 1.0 && max < 12.0, "wobble amplitude {max}");
        assert_eq!(wobble(&line, 0.0, 6.0, 7), line.to_vec(), "no roughness, no change");
    }

    #[test]
    fn styles_route_differently() {
        let a = fit(&[p(0.0, 0.0), p(100.0, 60.0)]);
        assert_eq!(sample(&a, ArrowStyle::Straight, 8), vec![p(0.0, 0.0), p(100.0, 60.0)]);
        assert_eq!(sample(&a, ArrowStyle::Elbow, 8), vec![p(0.0, 0.0), p(50.0, 0.0), p(50.0, 60.0), p(100.0, 60.0)]);
        assert_eq!(sample(&a, ArrowStyle::Curved, 8).len(), 9);
        let mut s = fit(&[p(0.0, 0.0), p(50.0, 50.0), p(100.0, 0.0)]);
        set_smooth(&mut s, 1, false);
        assert_eq!((s[1].c_in, s[1].c_out), (s[1].p, s[1].p), "sharp corner");
        set_smooth(&mut s, 1, true);
        assert!(s[1].c_in != s[1].p && s[1].c_out != s[1].p);
    }
}
