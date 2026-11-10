//! Click highlight: a ripple drawn at each logged click (left or right), from the cursor
//! track, never captured from the screen. It expands and fades over `duration`.
//!
//! The export draws it onto the screen video right after the cursor, before the camera crop,
//! so it zooms and moves with the content; the preview draws the same ring.

use crate::cursor::Sample;
use crate::style::Color;
use image::{Rgba, RgbaImage};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClickStyle {
    pub enabled: bool,
    pub color: Color,
    /// Final diameter in display points.
    pub size: f32,
    /// Seconds.
    pub duration: f64,
}

impl Default for ClickStyle {
    fn default() -> Self {
        Self { enabled: true, color: [0xa4, 0xf4, 0x7c], size: 56.0, duration: 0.4 }
    }
}

/// One click on the clip timeline, in video pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Click {
    pub t: f64,
    pub x: f64,
    pub y: f64,
}

/// Clicks are the moments a button goes down.
pub fn clicks(samples: &[Sample]) -> Vec<Click> {
    let mut out = Vec::new();
    let mut was = false;
    for s in samples {
        if s.down && !was {
            out.push(Click { t: s.t, x: s.x, y: s.y });
        }
        was = s.down;
    }
    out
}

/// Ripples showing at `t`: each click and its progress 0..1 (just clicked → faded out).
/// (The app reads ripples through the edit: `editing::remap::active_clicks`.)
#[cfg_attr(not(test), allow(dead_code))]
pub fn active(clicks: &[Click], t: f64, duration: f64) -> Vec<(Click, f32)> {
    let d = duration.max(0.01);
    clicks
        .iter()
        .filter(|c| t >= c.t && t < c.t + d)
        .map(|c| (*c, ((t - c.t) / d) as f32))
        .collect()
}

/// The ring's look at `progress`: (radius as a fraction of the final radius, opacity).
pub fn shape(progress: f32) -> (f32, f32) {
    let p = progress.clamp(0.0, 1.0);
    let ease = 1.0 - (1.0 - p).powi(3);
    (0.25 + 0.75 * ease, (1.0 - p).powf(1.4) * 0.9)
}

/// One frame of the ripple, `diameter` px across (the image is a little larger for the
/// ring's width); the click point is the image center.
pub fn sprite(color: Color, diameter: f32, progress: f32) -> RgbaImage {
    let size = (diameter * 1.15).ceil().max(4.0) as u32 | 1;
    let c = size as f32 / 2.0;
    let (r, a) = shape(progress);
    let radius = diameter / 2.0 * r;
    let ring = (diameter * 0.08).max(1.5);
    RgbaImage::from_fn(size, size, |x, y| {
        let d = ((x as f32 + 0.5 - c).powi(2) + (y as f32 + 0.5 - c).powi(2)).sqrt();
        // Ring at `radius`, with a faint fill inside.
        let ring_cov = (ring / 2.0 + 0.5 - (d - radius).abs()).clamp(0.0, 1.0);
        let fill = if d < radius { 0.22 } else { 0.0 };
        let alpha = (ring_cov.max(fill) * a).clamp(0.0, 1.0);
        Rgba([color[0], color[1], color[2], (alpha * 255.0).round() as u8])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(t: f64, down: bool) -> Sample {
        Sample { t, x: t * 100.0, y: 50.0, down }
    }

    #[test]
    fn clicks_are_button_presses() {
        let samples = vec![s(0.0, false), s(0.1, true), s(0.2, true), s(0.3, false), s(0.4, true), s(0.5, false)];
        let c = clicks(&samples);
        assert_eq!(c.len(), 2);
        assert_eq!((c[0].t, c[0].x), (0.1, 10.0));
        assert_eq!(c[1].t, 0.4);
    }

    #[test]
    fn ripple_timing() {
        let c = vec![Click { t: 1.0, x: 0.0, y: 0.0 }, Click { t: 1.2, x: 5.0, y: 5.0 }];
        assert!(active(&c, 0.99, 0.4).is_empty());
        let a = active(&c, 1.0, 0.4);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].1, 0.0);
        // Both ripples overlap at 1.3 s.
        let a = active(&c, 1.3, 0.4);
        assert_eq!(a.len(), 2);
        assert!((a[0].1 - 0.75).abs() < 1e-6 && (a[1].1 - 0.25).abs() < 1e-6);
        // Gone after the duration.
        assert!(active(&c, 1.4, 0.4).iter().all(|(k, _)| k.t == 1.2));
        assert!(active(&c, 1.6, 0.4).is_empty());
        // It grows and fades.
        let (r0, a0) = shape(0.0);
        let (r1, a1) = shape(0.9);
        assert!(r1 > r0 && a1 < a0);
        assert_eq!(shape(1.0).1, 0.0);
    }

    #[test]
    fn sprite_is_centered_ring() {
        let img = sprite([255, 0, 0], 60.0, 0.5);
        let c = img.width() / 2;
        let (r, _) = shape(0.5);
        let ring_x = c + (30.0 * r) as u32;
        assert!(img.get_pixel(ring_x, c)[3] > 60, "ring");
        assert!(img.get_pixel(0, 0)[3] == 0, "corner clear");
    }
}
