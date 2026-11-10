//! Zoom regions: spans of the clip where the camera zooms in, either following the cursor
//! (Auto) or holding a fixed point (Manual). They can be suggested from the clicks in the
//! cursor track, then edited on the timeline.

use crate::cursor::Sample;

/// Zoom levels offered in the editor (how much the camera crop shrinks).
pub const DEPTHS: [f64; 5] = [1.25, 1.5, 2.0, 2.5, 3.0];
pub const DEFAULT_DEPTH: f64 = 1.5;
/// Length of a zoom added by hand at the playhead.
pub const DEFAULT_LEN: f64 = 2.5;
/// Shortest region the editor allows.
pub const MIN_LEN: f64 = 0.5;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ZoomRegion {
    /// Seconds on the clip timeline.
    pub start: f64,
    pub end: f64,
    /// Zoom factor (1 = no zoom).
    pub depth: f64,
    /// Manual: hold `focus` instead of following the cursor.
    pub manual: bool,
    /// Focus point as a fraction of the screen (0..1). Suggested zooms start at the click.
    pub focus: (f64, f64),
}

impl ZoomRegion {
    pub fn new(start: f64, end: f64) -> Self {
        Self { start, end, depth: DEFAULT_DEPTH, manual: false, focus: (0.5, 0.5) }
    }

    pub fn len(&self) -> f64 {
        self.end - self.start
    }
}

/// How the camera moves into, out of and between zooms.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ZoomMotion {
    /// Seconds to reach full zoom after a region starts.
    pub zoom_in: f64,
    /// Seconds to return to 1× before a region ends.
    pub zoom_out: f64,
    /// Zooms closer than this stay zoomed and pan across instead of zooming out and in.
    pub connect_gap: f64,
    pub connect: bool,
}

impl Default for ZoomMotion {
    fn default() -> Self {
        Self { zoom_in: 0.6, zoom_out: 0.6, connect_gap: 1.5, connect: true }
    }
}

/// Smooth in-out curve with zero velocity at both ends.
fn ease(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * t * (t * (6.0 * t - 15.0) + 10.0)
}

/// Zoom factor at `t`, and the focus point (screen fraction) when a Manual zoom holds it.
/// `regions` must be sorted by start and not overlap.
pub fn state_at(regions: &[ZoomRegion], t: f64, m: &ZoomMotion) -> (f64, Option<(f64, f64)>) {
    let connected = |a: &ZoomRegion, b: &ZoomRegion| m.connect && b.start - a.end <= m.connect_gap;
    for (i, r) in regions.iter().enumerate() {
        let prev = i.checked_sub(1).map(|j| &regions[j]);
        let next = regions.get(i + 1);
        if t >= r.start && t <= r.end {
            let half = r.len() / 2.0;
            let k_in = if prev.is_some_and(|p| connected(p, r)) {
                1.0
            } else {
                ease((t - r.start) / m.zoom_in.min(half).max(1e-3))
            };
            let k_out = if next.is_some_and(|n| connected(r, n)) {
                1.0
            } else {
                ease((r.end - t) / m.zoom_out.min(half).max(1e-3))
            };
            let scale = 1.0 + (r.depth - 1.0) * k_in.min(k_out);
            return (scale, r.manual.then_some(r.focus));
        }
        // Between two connected zooms: blend the zoom level, stay zoomed.
        if let Some(n) = next {
            if t > r.end && t < n.start && connected(r, n) {
                let k = ease((t - r.end) / (n.start - r.end));
                let scale = r.depth + (n.depth - r.depth) * k;
                let focus = (r.manual && n.manual).then(|| {
                    (r.focus.0 + (n.focus.0 - r.focus.0) * k, r.focus.1 + (n.focus.1 - r.focus.1) * k)
                });
                return (scale, focus);
            }
        }
    }
    (1.0, None)
}

/// Keeps regions sorted, inside the clip, at least `MIN_LEN` long and non-overlapping
/// (a later region that overlaps an earlier one is trimmed, or dropped if nothing is left).
pub fn normalize(regions: &mut Vec<ZoomRegion>, duration: f64) {
    regions.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut out: Vec<ZoomRegion> = Vec::with_capacity(regions.len());
    for mut r in regions.drain(..) {
        r.start = r.start.clamp(0.0, duration);
        r.end = r.end.clamp(0.0, duration);
        if let Some(p) = out.last() {
            r.start = r.start.max(p.end);
        }
        if r.len() >= MIN_LEN - 1e-9 {
            out.push(r);
        }
    }
    *regions = out;
}

/// Free space around `t` where a new region could go, as (start, end).
pub fn free_slot(regions: &[ZoomRegion], t: f64, duration: f64) -> Option<(f64, f64)> {
    if regions.iter().any(|r| t >= r.start && t < r.end) {
        return None;
    }
    let lo = regions.iter().filter(|r| r.end <= t).map(|r| r.end).fold(0.0, f64::max);
    let hi = regions.iter().filter(|r| r.start > t).map(|r| r.start).fold(duration, f64::min);
    (hi - lo >= MIN_LEN).then_some((lo, hi))
}

// ---------- suggestions from the cursor track ----------

/// Clicks closer than this (after the last one) join the same zoom.
const MERGE_GAP: f64 = 2.5;
/// Lead-in before the first click and tail after the last one.
const PAD: f64 = 0.5;
/// A suggested zoom lasts at least this long, so a single click is not a blink.
const MIN_SUGGESTED: f64 = 2.0;

#[derive(Clone, Copy, Debug)]
struct Click {
    t: f64,
    /// Normalized position.
    x: f64,
    y: f64,
    /// How much this click looks like a moment worth zooming on; picks the focus.
    weight: f64,
}

/// Rates a click by what the cursor does next: a double click, a drag that selects text,
/// staying put (typing into a field) or heading down (a menu) beat a plain click.
fn weigh(points: &[(f64, f64, f64, bool)], down: usize, double: bool) -> f64 {
    if double {
        return 1.5;
    }
    let (t0, x0, y0, _) = points[down];
    let up = points[down..].iter().find(|p| !p.3);
    if let Some(&(t1, x1, y1, _)) = up {
        let (dx, dy) = ((x1 - x0).abs(), (y1 - y0).abs());
        if t1 - t0 >= 0.2 && dx > 0.03 && dx > 1.8 * dy {
            return 1.3; // drag-select
        }
    }
    let after: Vec<_> = points.iter().filter(|p| p.0 > t0 + 0.1 && p.0 <= t0 + 2.0).collect();
    let drift = after.iter().map(|p| (p.1 - x0).hypot(p.2 - y0)).fold(0.0, f64::max);
    if after.len() < 3 || drift < 0.02 {
        return 1.1; // stayed put: likely typing
    }
    let (sx, sy) = after.iter().fold((0.0, 0.0), |(sx, sy), p| (sx + (p.1 - x0).abs(), sy + (p.2 - y0).abs()));
    let last = after.last().unwrap();
    if last.2 - y0 > 0.03 && sy > 1.5 * sx {
        return 1.2; // moved down: likely a menu
    }
    0.9
}

/// Zoom regions around the clicks in the cursor track (Auto focus, at the default depth).
pub fn suggest(samples: &[Sample], width: u32, height: u32, duration: f64) -> Vec<ZoomRegion> {
    let (w, h) = (width.max(1) as f64, height.max(1) as f64);
    let points: Vec<(f64, f64, f64, bool)> = samples.iter().map(|s| (s.t, s.x / w, s.y / h, s.down)).collect();

    let mut clicks: Vec<Click> = Vec::new();
    for i in 0..points.len() {
        let (t, x, y, down) = points[i];
        if !down || (i > 0 && points[i - 1].3) {
            continue;
        }
        // A second press close in time and space is a double click: upgrade the first.
        if let Some(prev) = clicks.last_mut() {
            if t - prev.t <= 0.4 && (x - prev.x).hypot(y - prev.y) <= 0.02 {
                prev.weight = prev.weight.max(weigh(&points, i, true));
                continue;
            }
        }
        clicks.push(Click { t, x, y, weight: weigh(&points, i, false) });
    }

    // Group clicks into moments; each group's focus is its strongest click.
    let mut regions: Vec<ZoomRegion> = Vec::new();
    let mut i = 0;
    while i < clicks.len() {
        let mut j = i;
        while j + 1 < clicks.len() && clicks[j + 1].t - clicks[j].t <= MERGE_GAP {
            j += 1;
        }
        let group = &clicks[i..=j];
        let best = group.iter().fold(group[0], |b, c| if c.weight > b.weight { *c } else { b });
        let start = (group[0].t - PAD).max(0.0);
        let end = (group[j - i].t + PAD).max(start + MIN_SUGGESTED).min(duration);
        let mut r = ZoomRegion::new(start, end);
        r.focus = (best.x.clamp(0.0, 1.0), best.y.clamp(0.0, 1.0));
        regions.push(r);
        i = j + 1;
    }
    normalize(&mut regions, duration);
    regions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(clicks: &[(f64, f64, f64)], duration: f64) -> Vec<Sample> {
        // 120 Hz, cursor parked at the last click, button down for 80 ms per click.
        let mut out = Vec::new();
        let mut pos = (500.0, 500.0);
        let n = (duration * 120.0) as usize;
        for k in 0..n {
            let t = k as f64 / 120.0;
            let mut down = false;
            for &(ct, x, y) in clicks {
                if t >= ct {
                    pos = (x, y);
                }
                if t >= ct && t < ct + 0.08 {
                    down = true;
                }
            }
            out.push(Sample { t, x: pos.0, y: pos.1, down });
        }
        out
    }

    #[test]
    fn suggests_one_zoom_per_moment() {
        // Two clicks 1 s apart form one moment; a click 6 s later forms another.
        let s = track(&[(2.0, 100.0, 100.0), (3.0, 800.0, 600.0), (9.0, 300.0, 900.0)], 12.0);
        let z = suggest(&s, 1000, 1000, 12.0);
        assert_eq!(z.len(), 2, "{z:?}");
        assert!((z[0].start - 1.5).abs() < 0.02 && (z[0].end - 3.5).abs() < 0.02, "{:?}", z[0]);
        assert!((z[1].start - 8.5).abs() < 0.02 && (z[1].end - 10.5).abs() < 0.02, "{:?}", z[1]);
        assert!((z[1].focus.0 - 0.3).abs() < 1e-6 && (z[1].focus.1 - 0.9).abs() < 1e-6);
        assert!(z.iter().all(|r| !r.manual && r.depth == DEFAULT_DEPTH));
    }

    #[test]
    fn no_clicks_no_zooms() {
        let s = track(&[], 5.0);
        assert!(suggest(&s, 1000, 1000, 5.0).is_empty());
    }

    #[test]
    fn suggestions_stay_inside_clip() {
        let s = track(&[(0.1, 10.0, 10.0), (4.9, 20.0, 20.0)], 5.0);
        let z = suggest(&s, 1000, 1000, 5.0);
        for r in &z {
            assert!(r.start >= 0.0 && r.end <= 5.0 && r.len() >= MIN_LEN, "{r:?}");
        }
        assert!(z.windows(2).all(|w| w[0].end <= w[1].start));
    }

    #[test]
    fn envelope_ramps_and_holds() {
        let m = ZoomMotion::default();
        let r = ZoomRegion { start: 1.0, end: 4.0, depth: 2.0, manual: false, focus: (0.5, 0.5) };
        let z = [r];
        assert_eq!(state_at(&z, 0.5, &m).0, 1.0);
        assert_eq!(state_at(&z, 1.0, &m).0, 1.0);
        let mid_in = state_at(&z, 1.3, &m).0;
        assert!(mid_in > 1.3 && mid_in < 1.7, "{mid_in}");
        assert_eq!(state_at(&z, 2.5, &m).0, 2.0);
        assert_eq!(state_at(&z, 4.0, &m).0, 1.0);
        assert_eq!(state_at(&z, 5.0, &m).0, 1.0);
        // Monotonic ramp in.
        let ramp: Vec<f64> = (0..=12).map(|i| state_at(&z, 1.0 + i as f64 * 0.05, &m).0).collect();
        assert!(ramp.windows(2).all(|w| w[1] >= w[0]), "{ramp:?}");
    }

    #[test]
    fn connected_zooms_stay_zoomed() {
        let m = ZoomMotion::default();
        let a = ZoomRegion { start: 1.0, end: 3.0, depth: 2.0, manual: false, focus: (0.0, 0.0) };
        let b = ZoomRegion { start: 4.0, end: 6.0, depth: 2.0, manual: false, focus: (1.0, 1.0) };
        let z = [a, b];
        for i in 0..=30 {
            let t = 2.5 + i as f64 * 0.07;
            assert_eq!(state_at(&z, t, &m).0, 2.0, "dipped at {t}");
        }
        let apart = ZoomMotion { connect: false, ..m };
        assert_eq!(state_at(&z, 3.5, &apart).0, 1.0);
    }

    #[test]
    fn manual_reports_focus() {
        let m = ZoomMotion::default();
        let r = ZoomRegion { start: 0.0, end: 2.0, depth: 2.0, manual: true, focus: (0.25, 0.75) };
        assert_eq!(state_at(&[r], 1.0, &m).1, Some((0.25, 0.75)));
        assert_eq!(state_at(&[r], 3.0, &m).1, None);
    }

    #[test]
    fn normalize_and_free_slot() {
        let mut z = vec![ZoomRegion::new(3.0, 5.0), ZoomRegion::new(1.0, 3.5), ZoomRegion::new(4.8, 5.1)];
        normalize(&mut z, 10.0);
        assert_eq!(z.len(), 2, "{z:?}");
        assert_eq!((z[0].start, z[0].end), (1.0, 3.5));
        assert_eq!((z[1].start, z[1].end), (3.5, 5.0));
        assert_eq!(free_slot(&z, 2.0, 10.0), None);
        assert_eq!(free_slot(&z, 7.0, 10.0), Some((5.0, 10.0)));
        assert_eq!(free_slot(&z, 0.5, 10.0), Some((0.0, 1.0)));
    }
}
