//! Builds a virtual camera path (a moving crop window) from the cursor track.
//!
//! The camera is purely reactive: it never moves while the cursor is inside the
//! deadzone, and when the cursor reaches the deadzone edge it shifts by the minimum
//! amount, eased with a critically damped spring that never overshoots.

use crate::cursor::Sample;
use crate::recorder::FPS;
use crate::zoom::{self, ZoomMotion, ZoomRegion};

#[derive(Clone, Copy, Debug)]
pub struct FollowConfig {
    /// Edge margin as a fraction of crop width. The deadzone is the crop window
    /// inset by this margin on every side.
    pub margin: f64,
    /// Critically damped settling time in seconds (to within 2% of the target).
    pub settle: f64,
}

impl Default for FollowConfig {
    fn default() -> Self {
        Self { margin: 0.04, settle: 0.12 }
    }
}

/// One camera crop: top-left and size in video pixels.
pub type Crop = (u32, u32, u32, u32);

pub struct CameraPath {
    /// Unzoomed crop size.
    pub crop_w: u32,
    pub crop_h: u32,
    /// Crop for each output frame (at `FPS`). Smaller than `crop_w × crop_h` while zoomed.
    pub frames: Vec<Crop>,
}

impl CameraPath {
    pub fn at_time(&self, t: f64) -> Crop {
        let i = ((t.max(0.0) * FPS as f64) as usize).min(self.frames.len().saturating_sub(1));
        self.frames.get(i).copied().unwrap_or((0, 0, self.crop_w, self.crop_h))
    }
}

fn even(v: f64) -> u32 {
    ((v / 2.0).round() as u32 * 2).max(2)
}

pub fn cursor_at(samples: &[Sample], t: f64) -> Option<(f64, f64)> {
    let first = samples.first()?;
    let i = samples.partition_point(|s| s.t < t);
    if i == 0 {
        return Some((first.x, first.y));
    }
    if i >= samples.len() {
        let l = samples.last()?;
        return Some((l.x, l.y));
    }
    let (a, b) = (samples[i - 1], samples[i]);
    let k = if b.t > a.t { (t - a.t) / (b.t - a.t) } else { 0.0 };
    Some((a.x + (b.x - a.x) * k, a.y + (b.y - a.y) * k))
}

/// The camera path for a crop aspect (w / h) and follow tuning, without zooms.
#[allow(dead_code)] // used by tests
pub fn compute_with(
    samples: &[Sample],
    width: u32,
    height: u32,
    duration: f64,
    aspect: f64,
    cfg: FollowConfig,
) -> CameraPath {
    compute_zoomed(samples, width, height, duration, aspect, 1.0, cfg, &[], &ZoomMotion::default())
}

/// The unzoomed crop size for a crop aspect (w / h): the largest rect of that aspect on the
/// screen, shrunk by `base_zoom` (≥ 1). Even dimensions, never larger than the screen.
pub fn crop_size(width: u32, height: u32, aspect: f64, base_zoom: f64) -> (u32, u32) {
    let (w, h) = (width as f64, height as f64);
    let z = base_zoom.max(1.0);
    let (cw, ch) = if w / h > aspect { (h * aspect, h) } else { (w, w / aspect) };
    (even(cw / z).min(width), even(ch / z).min(height))
}

/// The camera path with zoom regions applied. `aspect` is the screen content rect's aspect
/// on the canvas (so nothing is stretched) and `base_zoom` the layout's extra zoom.
#[allow(clippy::too_many_arguments)]
pub fn compute_zoomed(
    samples: &[Sample],
    width: u32,
    height: u32,
    duration: f64,
    aspect: f64,
    base_zoom: f64,
    cfg: FollowConfig,
    zooms: &[ZoomRegion],
    motion: &ZoomMotion,
) -> CameraPath {
    let (w, h) = (width as f64, height as f64);
    let (crop_w, crop_h) = crop_size(width, height, aspect, base_zoom);
    let n = ((duration * FPS as f64).ceil() as usize).max(1);
    let center = (w / 2.0, h / 2.0);
    let cursor: Vec<(f64, f64)> = (0..n)
        .map(|i| cursor_at(samples, i as f64 / FPS as f64).unwrap_or(center))
        .collect();
    let zoom: Vec<(f64, Option<(f64, f64)>)> = (0..n)
        .map(|i| {
            let (s, f) = zoom::state_at(zooms, i as f64 / FPS as f64, motion);
            (s, f.map(|(fx, fy)| (fx * w, fy * h)))
        })
        .collect();
    let frames = follow_zoomed(&cursor, &zoom, width, height, crop_w, crop_h, cfg);
    CameraPath { crop_w, crop_h, frames }
}

/// One axis of the camera.
struct Axis {
    screen: f64,
    /// Current crop size along this axis.
    size: f64,
    /// Where the camera is heading (integer pixel offset).
    target: f64,
    pos: f64,
    vel: f64,
    /// The cursor is kept inside the crop. Off while a Manual zoom holds the camera, and
    /// until the camera has caught the cursor again afterwards.
    tracking: bool,
}

impl Axis {
    fn new(cursor: f64, size: u32, screen: u32) -> Self {
        let limit = (screen - size) as f64;
        let start = (cursor - size as f64 / 2.0).round().clamp(0.0, limit);
        Self { screen: screen as f64, size: size as f64, target: start, pos: start, vel: 0.0, tracking: true }
    }

    fn limit(&self) -> f64 {
        self.screen - self.size
    }

    /// Changes the crop size, scaling about `anchor` (the cursor) so the point under it
    /// stays put on screen.
    fn resize(&mut self, size: u32, anchor: f64) {
        let size = size as f64;
        if size == self.size {
            return;
        }
        let k = size / self.size;
        let a = anchor.clamp(self.pos, self.pos + self.size);
        self.pos = a - (a - self.pos) * k;
        let a = anchor.clamp(self.target, self.target + self.size);
        self.target = a - (a - self.target) * k;
        self.vel *= k;
        self.size = size;
        let limit = self.limit();
        self.pos = self.pos.clamp(0.0, limit);
        self.target = self.target.round().clamp(0.0, limit);
    }

    /// Advances one frame and returns the integer crop offset. `focus` (a Manual zoom)
    /// replaces the cursor follow with a fixed point to center on.
    fn step(&mut self, c: f64, margin: f64, focus: Option<f64>, omega: f64, dt: f64) -> u32 {
        let limit = self.limit();
        match focus {
            Some(f) => {
                self.tracking = false;
                self.target = (f - self.size / 2.0).round();
            }
            None => {
                // 1. Deadzone. The target changes only once the cursor reaches the deadzone
                //    edge, and then by the minimum shift that puts it back on that edge.
                let (lo, hi) = (self.target + margin, self.target + self.size - margin);
                if c > hi {
                    self.target = (c - (self.size - margin)).ceil();
                } else if c < lo {
                    self.target = (c - margin).floor();
                }
            }
        }
        self.target = self.target.clamp(0.0, limit);

        // 2. Critically damped spring toward the target (exact step, no overshoot).
        let prev = self.pos;
        let d = self.pos - self.target;
        if d != 0.0 || self.vel != 0.0 {
            let e = (-omega * dt).exp();
            let k = (self.vel + omega * d) * dt;
            let mut nd = (d + k) * e;
            let mut nv = (self.vel - omega * k) * e;
            if nd * d <= 0.0 || nd.abs() < 0.01 {
                (nd, nv) = (0.0, 0.0); // reached or would pass the target: settle on it
            }
            self.pos = self.target + nd;
            self.vel = nv;
        }
        self.pos = self.pos.clamp(0.0, limit);

        // After a Manual zoom, follow resumes once the eased camera has the cursor again.
        if focus.is_none() && !self.tracking && c >= self.pos && c <= self.pos + self.size {
            self.tracking = true;
        }

        // 3. Hard invariant while tracking: cursor inside the crop, crop inside the screen.
        let (min, max) = if self.tracking { ((c - self.size).max(0.0), c.min(limit)) } else { (0.0, limit) };
        if self.pos < min || self.pos > max {
            self.pos = self.pos.clamp(min, max);
            self.vel = (self.pos - prev) / dt;
        }

        // 4. Integer offset: round toward the target so motion shows on the first
        //    frame, then re-apply the invariant in integer space.
        let out = if self.target > self.pos {
            self.pos.ceil()
        } else if self.target < self.pos {
            self.pos.floor()
        } else {
            self.pos.round()
        };
        out.clamp(min.ceil(), max.floor()) as u32
    }
}

/// Reactive camera follow: one cursor position per frame in, one crop offset per frame out.
#[allow(dead_code)] // used by tests
pub fn follow_path(
    cursor: &[(f64, f64)],
    width: u32,
    height: u32,
    crop_w: u32,
    crop_h: u32,
    cfg: FollowConfig,
) -> Vec<(u32, u32)> {
    let zoom = vec![(1.0, None); cursor.len()];
    follow_zoomed(cursor, &zoom, width, height, crop_w, crop_h, cfg)
        .into_iter()
        .map(|(x, y, _, _)| (x, y))
        .collect()
}

/// Reactive follow with a per-frame zoom factor (crop = base / zoom) and an optional
/// Manual focus point in video pixels.
pub fn follow_zoomed(
    cursor: &[(f64, f64)],
    zoom: &[(f64, Option<(f64, f64)>)],
    width: u32,
    height: u32,
    crop_w: u32,
    crop_h: u32,
    cfg: FollowConfig,
) -> Vec<Crop> {
    let Some(&first) = cursor.first() else { return Vec::new() };
    let clamp = |(x, y): (f64, f64)| (x.clamp(0.0, width as f64), y.clamp(0.0, height as f64));
    // Crop size for a zoom factor, keeping the base aspect and even dimensions.
    let size_for = |s: f64| -> (u32, u32) {
        if s <= 1.0 {
            return (crop_w, crop_h);
        }
        let w = even(crop_w as f64 / s).min(crop_w);
        let h = even(w as f64 * crop_h as f64 / crop_w as f64).min(crop_h);
        (w, h)
    };
    let first = clamp(first);
    let mut ax = Axis::new(first.0, crop_w, width);
    let mut ay = Axis::new(first.1, crop_h, height);

    // (1 + x)e^-x = 0.02 at x ≈ 5.834 → settles to 2% in `settle` seconds.
    let omega = 5.834 / cfg.settle.max(1e-3);
    let dt = 1.0 / FPS as f64;

    let mut out = Vec::with_capacity(cursor.len());
    for (i, &c) in cursor.iter().enumerate() {
        let (cx, cy) = clamp(c);
        let (s, focus) = zoom.get(i).copied().unwrap_or((1.0, None));
        let (w, h) = size_for(s);
        ax.resize(w, cx);
        ay.resize(h, cy);
        if i == 0 {
            out.push((ax.pos as u32, ay.pos as u32, w, h));
            continue;
        }
        // The deadzone margin is a fraction of the (current) crop width on both axes.
        let margin = (cfg.margin * w as f64).max(0.0);
        let x = ax.step(cx, margin.min(w as f64 / 2.0), focus.map(|f| f.0), omega, dt);
        let y = ay.step(cy, margin.min(h as f64 / 2.0), focus.map(|f| f.1), omega, dt);
        out.push((x, y, w, h));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 1000;
    const H: u32 = 1000;
    const CW: u32 = 400;
    const CH: u32 = 400;

    fn cfg() -> FollowConfig {
        FollowConfig::default()
    }

    fn margin() -> f64 {
        cfg().margin * CW as f64 // 16 px
    }

    fn run(cursor: &[(f64, f64)]) -> Vec<(u32, u32)> {
        follow_path(cursor, W, H, CW, CH, cfg())
    }

    /// Cursor inside the crop and crop inside the screen, every frame.
    fn assert_invariant(cursor: &[(f64, f64)], frames: &[(u32, u32)]) {
        assert_eq!(cursor.len(), frames.len());
        for (i, (&(cx, cy), &(x, y))) in cursor.iter().zip(frames).enumerate() {
            let (cx, cy) = (cx.clamp(0.0, W as f64), cy.clamp(0.0, H as f64));
            assert!(x + CW <= W && y + CH <= H, "frame {i}: crop ({x},{y}) outside screen");
            assert!(
                cx >= x as f64 && cx <= (x + CW) as f64 && cy >= y as f64 && cy <= (y + CH) as f64,
                "frame {i}: cursor ({cx},{cy}) outside crop ({x},{y})"
            );
        }
    }

    fn in_deadzone(cam: (u32, u32), c: (f64, f64)) -> bool {
        let m = margin();
        let (x, y) = (cam.0 as f64, cam.1 as f64);
        c.0 >= x + m && c.0 <= x + CW as f64 - m && c.1 >= y + m && c.1 <= y + CH as f64 - m
    }

    #[test]
    fn camera_static_inside_deadzone() {
        // Wander in a circle well inside the initial deadzone [316, 684].
        let cursor: Vec<_> = std::iter::once((500.0, 500.0))
            .chain((0..120).map(|i| {
                let a = i as f64 * 0.2;
                (500.0 + 150.0 * a.cos(), 500.0 + 150.0 * a.sin())
            }))
            .collect();
        let frames = run(&cursor);
        assert!(frames.iter().all(|&f| f == (300, 300)), "camera moved: {frames:?}");
        assert_invariant(&cursor, &frames);
    }

    fn exit_case(dir: (f64, f64)) {
        // Move steadily from the center, then hold still.
        let mut cursor: Vec<_> =
            (0..60).map(|i| (500.0 + dir.0 * 6.5 * i as f64, 500.0 + dir.1 * 6.5 * i as f64)).collect();
        let last = *cursor.last().unwrap();
        cursor.extend(std::iter::repeat_n(last, 30));
        let frames = run(&cursor);
        assert_invariant(&cursor, &frames);

        let start = frames[0];
        // First frame where the cursor reaches/crosses the deadzone edge of the resting camera.
        let exit = cursor.iter().position(|&c| !in_deadzone(start, c)).expect("never exits");
        assert!(frames[..exit].iter().all(|&f| f == start), "{dir:?}: moved before exit at {exit}");
        assert_ne!(frames[exit], start, "{dir:?}: did not move on the exit frame {exit}");

        // Only the pushed axis moves.
        for f in &frames {
            if dir.0 == 0.0 {
                assert_eq!(f.0, start.0);
            }
            if dir.1 == 0.0 {
                assert_eq!(f.1, start.1);
            }
        }

        // Settled: cursor sits on the deadzone edge (minimum shift), not re-centered.
        let (cx, cy) = last;
        let (x, y) = (frames.last().unwrap().0 as f64, frames.last().unwrap().1 as f64);
        let edge = (CW as f64 - margin(), margin());
        let off = match dir {
            (d, _) if d > 0.0 => cx - x - edge.0,
            (d, _) if d < 0.0 => cx - x - edge.1,
            (_, d) if d > 0.0 => cy - y - edge.0,
            _ => cy - y - edge.1,
        };
        assert!(off.abs() <= 1.0, "{dir:?}: cursor {off}px from deadzone edge after settling");
    }

    #[test]
    fn exits_right() {
        exit_case((1.0, 0.0));
    }

    #[test]
    fn exits_left() {
        exit_case((-1.0, 0.0));
    }

    #[test]
    fn exits_bottom() {
        exit_case((0.0, 1.0));
    }

    #[test]
    fn exits_top() {
        exit_case((0.0, -1.0));
    }

    #[test]
    fn fast_flick_keeps_cursor_visible() {
        let mut cursor = vec![(20.0, 20.0); 10];
        cursor.extend(vec![(990.0, 980.0); 10]); // full-screen jump in one frame
        cursor.extend(vec![(5.0, 600.0); 10]);
        cursor.extend((0..20).map(|i| if i % 2 == 0 { (0.0, 0.0) } else { (1000.0, 1000.0) }));
        for settle in [0.06, 0.12, 0.5, 2.0] {
            let frames = follow_path(&cursor, W, H, CW, CH, FollowConfig { settle, ..cfg() });
            assert_invariant(&cursor, &frames);
        }
    }

    #[test]
    fn screen_corners_clamp() {
        let corners = [(0.0, 0.0), (1000.0, 0.0), (0.0, 1000.0), (1000.0, 1000.0), (-50.0, 1200.0)];
        for &corner in &corners {
            let mut cursor = vec![(500.0, 500.0); 5];
            cursor.extend(vec![corner; 30]);
            let frames = run(&cursor);
            assert_invariant(&cursor, &frames);
            let expect = |v: f64| if v <= 0.0 { 0 } else { W - CW };
            assert_eq!(*frames.last().unwrap(), (expect(corner.0), expect(corner.1)), "{corner:?}");
        }
    }

    fn zoomed(cursor: &[(f64, f64)], zoom: &[(f64, Option<(f64, f64)>)]) -> Vec<Crop> {
        follow_zoomed(cursor, zoom, W, H, CW, CH, cfg())
    }

    #[test]
    fn auto_zoom_shrinks_crop_and_keeps_cursor() {
        // Cursor sweeps the screen while the zoom ramps 1× → 2× → 1×.
        let n = 150;
        let cursor: Vec<_> = (0..n).map(|i| (50.0 + 6.0 * i as f64, 900.0 - 5.0 * i as f64)).collect();
        let zoom: Vec<_> = (0..n)
            .map(|i| {
                let k = (i as f64 / n as f64 * std::f64::consts::PI).sin();
                (1.0 + k, None)
            })
            .collect();
        let frames = zoomed(&cursor, &zoom);
        let mid = frames[n / 2];
        assert_eq!((mid.2, mid.3), (200, 200), "crop at 2× {mid:?}");
        assert_eq!((frames[0].2, frames[0].3), (CW, CH));
        for (i, (&(cx, cy), &(x, y, w, h))) in cursor.iter().zip(&frames).enumerate() {
            assert!(x + w <= W && y + h <= H, "frame {i}: crop outside screen");
            assert!(w % 2 == 0 && h % 2 == 0, "frame {i}: odd crop {w}x{h}");
            let (cx, cy) = (cx.clamp(0.0, W as f64), cy.clamp(0.0, H as f64));
            assert!(
                cx >= x as f64 && cx <= (x + w) as f64 && cy >= y as f64 && cy <= (y + h) as f64,
                "frame {i}: cursor ({cx},{cy}) outside crop ({x},{y},{w},{h})"
            );
        }
    }

    #[test]
    fn zoom_in_is_anchored_on_cursor() {
        // Cursor parked off-center; zooming in keeps the point under it fixed.
        let cursor = vec![(400.0, 450.0); 40];
        let zoom: Vec<_> = (0..40).map(|i| (if i < 10 { 1.0 } else { 2.0 }, None)).collect();
        let frames = zoomed(&cursor, &zoom);
        let (x0, y0, w0, h0) = frames[9];
        let (x1, y1, w1, h1) = frames[39];
        let rel = |c: f64, o: u32, s: u32| (c - o as f64) / s as f64;
        assert!((rel(400.0, x0, w0) - rel(400.0, x1, w1)).abs() < 0.02);
        assert!((rel(450.0, y0, h0) - rel(450.0, y1, h1)).abs() < 0.02);
    }

    #[test]
    fn manual_zoom_centers_on_focus_then_follows_again() {
        // Cursor parked at the top-left; a Manual zoom holds the bottom-right, then ends.
        let cursor = vec![(150.0, 150.0); 120];
        let zoom: Vec<_> = (0..120).map(|i| if (10..60).contains(&i) { (2.0, Some((800.0, 800.0))) } else { (1.0, None) }).collect();
        let frames = zoomed(&cursor, &zoom);
        let (x, y, w, h) = frames[55];
        assert_eq!((w, h), (200, 200));
        assert_eq!((x + w / 2, y + h / 2), (800, 800), "not centered on focus: {:?}", frames[55]);
        // Afterwards the camera eases back (no jump) and has the cursor again.
        let steps: Vec<i64> = frames[59..].windows(2).map(|p| (p[1].0 as i64 - p[0].0 as i64).abs()).collect();
        let total: i64 = steps.iter().sum();
        assert!(steps.iter().filter(|&&s| s > 0).count() >= 3, "snapped back in one go: {steps:?}");
        assert!(*steps.iter().max().unwrap() < total * 2 / 3, "camera jumped: {steps:?}");
        let (x, y, w, h) = *frames.last().unwrap();
        assert!(150 >= x && 150 <= x + w && 150 >= y && 150 <= y + h, "lost the cursor: {:?}", frames.last());
    }

    #[test]
    fn zoom_regions_drive_compute() {
        let samples: Vec<_> = (0..90).map(|i| Sample { t: i as f64 / 30.0, x: 500.0, y: 500.0, down: false }).collect();
        let r = ZoomRegion { start: 0.5, end: 2.5, depth: 2.0, manual: false, focus: (0.5, 0.5) };
        let path = compute_zoomed(&samples, W, H, 3.0, 1.0, 1.0, cfg(), &[r], &ZoomMotion::default());
        assert_eq!((path.crop_w, path.crop_h), (1000, 1000));
        assert_eq!(path.at_time(0.1).2, 1000);
        assert_eq!(path.at_time(1.5).2, 500);
        assert_eq!(path.at_time(2.9).2, 1000);
    }

    #[test]
    fn vertical_crop_on_real_screen() {
        // Largest 9:16 crop of a 3360×2100 screen.
        let (cw, ch) = (1182, 2100);
        let cursor: Vec<_> = (0..90).map(|i| (100.0 + 36.0 * i as f64, 1000.0)).collect();
        let frames = follow_path(&cursor, 3360, 2100, cw, ch, cfg());
        assert!(frames.iter().all(|f| f.1 == 0));
        for (&(cx, _), &(x, _)) in cursor.iter().zip(&frames) {
            assert!(cx >= x as f64 && cx <= (x + cw) as f64);
        }
    }

    // ---------- layouts ----------

    use crate::layout::{Layout, Placement, Ratio, Res};
    use crate::style::Style;

    fn all_layouts() -> Vec<Layout> {
        let mut v = Vec::new();
        for ratio in Ratio::ALL {
            for res in Res::ALL {
                for placement in Placement::options(ratio) {
                    for split in [0.25, 0.5, 2.0 / 3.0, 0.75] {
                        for zoom in [1.0, 1.6, 2.5] {
                            v.push(Layout { ratio, res, placement, split, zoom, ..Layout::default() });
                        }
                    }
                }
            }
        }
        v
    }

    /// A cursor that sweeps across the whole screen and back, with pauses.
    fn sweep(w: u32, h: u32, n: usize) -> Vec<Sample> {
        (0..n)
            .map(|i| {
                let t = i as f64 / FPS as f64;
                let a = (t * 0.9).sin() * 0.5 + 0.5;
                let b = (t * 0.55).cos() * 0.5 + 0.5;
                Sample { t, x: a * w as f64, y: b * h as f64, down: false }
            })
            .collect()
    }

    /// The crop has the screen content rect's aspect in every layout, so nothing stretches.
    #[test]
    fn crop_aspect_matches_content_rect_in_every_layout() {
        let style = Style::default();
        for (sw, sh) in [(3360, 2100), (1920, 1080), (2560, 1440)] {
            for l in all_layouts() {
                for webcam in [true, false] {
                    let rect = l.frame(&style, webcam).screen;
                    let (cw, ch) = crop_size(sw, sh, rect.aspect(), l.zoom as f64);
                    assert!(cw <= sw && ch <= sh && cw % 2 == 0 && ch % 2 == 0);
                    // Even rounding of both sides may move the aspect by up to ~1 px per side.
                    let tol = 2.5 / ch.min(cw) as f64 * rect.aspect().max(1.0);
                    let err = ((cw as f64 / ch as f64) / rect.aspect() - 1.0).abs();
                    assert!(err < tol.max(0.004), "{l:?} on {sw}x{sh}: crop {cw}x{ch} vs rect {rect:?} ({err})");
                }
            }
        }
    }

    /// Split layouts keep the follow rules: no motion until the cursor reaches the deadzone
    /// edge, the cursor always inside the crop, the crop always inside the screen.
    #[test]
    fn follow_invariant_holds_in_split_layouts() {
        let style = Style::default();
        let (sw, sh) = (3360, 2100);
        for l in all_layouts().into_iter().filter(|l| matches!(l.placement, Placement::Split(_))) {
            let rect = l.frame(&style, true).screen;
            let (cw, ch) = crop_size(sw, sh, rect.aspect(), l.zoom as f64);
            // Start at the center and creep right: the camera must hold still while the
            // cursor is inside the deadzone.
            let mut cursor: Vec<(f64, f64)> = (0..240).map(|i| (sw as f64 / 2.0 + i as f64 * 4.0, sh as f64 / 2.0)).collect();
            cursor.extend(sweep(sw, sh, 300).iter().map(|s| (s.x, s.y)));
            let frames = follow_path(&cursor, sw, sh, cw, ch, cfg());
            let m = cfg().margin * cw as f64;
            let (x0, y0) = frames[0];
            let mut left = false;
            for (i, (&(cx, cy), &(x, y))) in cursor.iter().zip(&frames).enumerate() {
                assert!(x + cw <= sw && y + ch <= sh, "{l:?} frame {i}: crop outside screen");
                assert!(
                    cx >= x as f64 && cx <= (x + cw) as f64 && cy >= y as f64 && cy <= (y + ch) as f64,
                    "{l:?} frame {i}: cursor ({cx},{cy}) outside crop ({x},{y},{cw},{ch})"
                );
                let inside = cx >= x0 as f64 + m
                    && cx <= (x0 + cw) as f64 - m
                    && cy >= y0 as f64 + m
                    && cy <= (y0 + ch) as f64 - m;
                left |= !inside;
                if !left {
                    assert_eq!((x, y), (x0, y0), "{l:?} frame {i}: moved before the deadzone edge");
                }
            }
        }
    }

    /// Switching layout mid-preview recomputes the path with the new crop; at every moment of
    /// either path the cursor is inside the crop.
    #[test]
    fn layout_change_mid_preview_keeps_cursor_in_crop() {
        let style = Style::default();
        let (sw, sh) = (2560, 1600);
        let samples = sweep(sw, sh, 360);
        let layouts = all_layouts();
        for pair in layouts.windows(2).step_by(7) {
            let paths: Vec<CameraPath> = pair
                .iter()
                .map(|l| {
                    let aspect = l.frame(&style, true).screen.aspect();
                    compute_zoomed(&samples, sw, sh, 12.0, aspect, l.zoom as f64, cfg(), &[], &ZoomMotion::default())
                })
                .collect();
            // The preview shows layout A until 6 s, then layout B.
            for i in 0..360 {
                let t = i as f64 / FPS as f64;
                let path = if t < 6.0 { &paths[0] } else { &paths[1] };
                let (x, y, w, h) = path.frames[i];
                let (cx, cy) = cursor_at(&samples, t).unwrap();
                assert!(
                    cx >= x as f64 && cx <= (x + w) as f64 && cy >= y as f64 && cy <= (y + h) as f64,
                    "{:?} at {t:.2}s: cursor outside crop",
                    pair
                );
            }
        }
    }

    /// 16:9 output of a 16:9 screen with no split: the crop is the whole screen and the
    /// camera never moves; with Zoom above 1× the camera follows.
    #[test]
    fn zoom_setting_enables_follow_on_matching_aspect() {
        let style = Style { padding: 0, ..Style::default() };
        let (sw, sh) = (1920, 1080);
        let samples = sweep(sw, sh, 300);
        let flat = Layout { ratio: Ratio::Landscape, ..Layout::default() };
        let aspect = flat.frame(&style, false).screen.aspect();
        let still = compute_zoomed(&samples, sw, sh, 10.0, aspect, 1.0, cfg(), &[], &ZoomMotion::default());
        assert_eq!((still.crop_w, still.crop_h), (1920, 1080));
        assert!(still.frames.iter().all(|&(x, y, _, _)| (x, y) == (0, 0)));
        let zoomed = compute_zoomed(&samples, sw, sh, 10.0, aspect, 2.0, cfg(), &[], &ZoomMotion::default());
        assert_eq!((zoomed.crop_w, zoomed.crop_h), (960, 540));
        assert!(zoomed.frames.iter().any(|&(x, ..)| x > 0), "camera never followed");
    }
}
