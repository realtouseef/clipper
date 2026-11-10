//! Recording a region of one display instead of the whole screen.
//!
//! A region is stored in the display's physical pixels plus the display id, so it means the
//! same thing at any scale factor. The selector works in the display's points (what the
//! mouse reports); `from_points` converts, normalizes, clamps to the display, enforces the
//! 160 px minimum and rounds the size to even numbers (H.264 needs even dimensions).

use serde::{Deserialize, Serialize};

/// Smallest region, in physical pixels per side.
pub const MIN: u32 = 160;

/// One connected display.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Display {
    /// CoreGraphics display id.
    pub id: u32,
    /// Top-left in global points (the main display's top-left is 0,0).
    pub origin: (f64, f64),
    /// Size in points.
    pub size: (f64, f64),
    /// Physical pixels per point (2.0 on Retina).
    pub scale: f64,
}

impl Display {
    /// Size in physical pixels.
    pub fn pixels(&self) -> (u32, u32) {
        ((self.size.0 * self.scale).round() as u32, (self.size.1 * self.scale).round() as u32)
    }
}

/// A rectangle on one display, in that display's physical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Region {
    pub display: u32,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Rounds down to an even number (at least 2).
pub fn even(v: u32) -> u32 {
    (v / 2 * 2).max(2)
}

impl Region {
    /// The region spanned by two corners `a` and `b`, in points relative to the display's
    /// top-left (any order). Clamped to the display, at least `MIN` px per side, even sized.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn from_points(display: &Display, a: (f64, f64), b: (f64, f64)) -> Region {
        Self::from_points_min(display, a, b, MIN)
    }

    /// Like `from_points` with a different minimum side (screenshots allow small regions).
    pub fn from_points_min(display: &Display, a: (f64, f64), b: (f64, f64), min: u32) -> Region {
        let s = display.scale;
        let (x0, x1) = (a.0.min(b.0), a.0.max(b.0));
        let (y0, y1) = (a.1.min(b.1), a.1.max(b.1));
        let px = |v: f64| (v * s).round().max(0.0) as u32;
        Region { display: display.id, x: px(x0), y: px(y0), w: px(x1) - px(x0).min(px(x1)), h: px(y1) - px(y0).min(px(y1)) }
            .clamped_min(display, min)
    }

    /// Fits the region inside the display: at least `MIN` (or the display size, if smaller)
    /// per side, even sized, moved (not shrunk) back inside if it hangs over an edge.
    pub fn clamped(self, display: &Display) -> Region {
        self.clamped_min(display, MIN)
    }

    pub fn clamped_min(self, display: &Display, min: u32) -> Region {
        let (dw, dh) = display.pixels();
        let (dw, dh) = (even(dw), even(dh));
        let w = even(self.w.max(min).min(dw));
        let h = even(self.h.max(min).min(dh));
        let x = self.x.min(dw - w);
        let y = self.y.min(dh - h);
        Region { display: display.id, x, y, w, h }
    }

    /// The region in points relative to the display (what ScreenCaptureKit's source rect and
    /// the selector use).
    pub fn to_points(&self, display: &Display) -> (f64, f64, f64, f64) {
        let s = display.scale;
        (self.x as f64 / s, self.y as f64 / s, self.w as f64 / s, self.h as f64 / s)
    }

    /// Global top-left in points.
    pub fn global_origin(&self, display: &Display) -> (f64, f64) {
        let (x, y, _, _) = self.to_points(display);
        (display.origin.0 + x, display.origin.1 + y)
    }

    /// A cursor position in global points → video pixels of the region's recording (0,0 at
    /// the region's top-left). May fall outside the region; the cursor is drawn only inside.
    pub fn cursor_to_video(&self, display: &Display, global: (f64, f64)) -> (f64, f64) {
        let (gx, gy) = self.global_origin(display);
        ((global.0 - gx) * display.scale, (global.1 - gy) * display.scale)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn contains_video(&self, p: (f64, f64)) -> bool {
        p.0 >= 0.0 && p.1 >= 0.0 && p.0 < self.w as f64 && p.1 < self.h as f64
    }

    pub fn label(&self) -> String {
        format!("{} × {} px", self.w, self.h)
    }
}

/// What gets recorded.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Source {
    /// A whole display (by id).
    Display(u32),
    /// A region of one display.
    Region(Region),
}

impl Source {
    pub fn display_id(&self) -> u32 {
        match self {
            Source::Display(id) => *id,
            Source::Region(r) => r.display,
        }
    }
}

fn last_path() -> std::path::PathBuf {
    crate::recorder::sessions_root().join(".region.json")
}

/// The last confirmed region (remembered between launches).
pub fn load_last() -> Option<Region> {
    serde_json::from_slice(&std::fs::read(last_path()).ok()?).ok()
}

pub fn save_last(r: &Region) {
    let p = last_path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(p, serde_json::to_vec(r).unwrap_or_default());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn retina() -> Display {
        Display { id: 7, origin: (0.0, 0.0), size: (1680.0, 1050.0), scale: 2.0 }
    }

    fn second() -> Display {
        // A 100% display to the right of the main one.
        Display { id: 9, origin: (1680.0, -200.0), size: (1920.0, 1080.0), scale: 1.0 }
    }

    #[test]
    fn points_map_to_physical_pixels_with_dpi() {
        let r = Region::from_points(&retina(), (100.0, 50.0), (500.25, 350.5));
        // ×2, rounded, then even.
        // 400.25 pt × 2 = 800.5 px → 800 (even); 300.5 pt × 2 = 601 px → 600.
        assert_eq!(r, Region { display: 7, x: 200, y: 100, w: 800, h: 600 });
        assert_eq!(r.to_points(&retina()), (100.0, 50.0, 400.0, 300.0));
        let r = Region::from_points(&second(), (10.0, 20.0), (410.0, 320.0));
        assert_eq!(r, Region { display: 9, x: 10, y: 20, w: 400, h: 300 });
        // Corners in any order.
        assert_eq!(Region::from_points(&second(), (410.0, 320.0), (10.0, 20.0)), r);
    }

    #[test]
    fn sizes_are_even() {
        for (w, h) in [(301.0, 199.0), (160.5, 777.7), (999.0, 1001.0)] {
            for d in [retina(), second()] {
                let r = Region::from_points(&d, (3.0, 3.0), (3.0 + w, 3.0 + h));
                assert!(r.w % 2 == 0 && r.h % 2 == 0, "{r:?}");
            }
        }
        assert_eq!(even(7), 6);
        assert_eq!(even(1), 2);
    }

    #[test]
    fn clamps_to_display_and_minimum() {
        let d = second();
        // Tiny drag → minimum size.
        let r = Region::from_points(&d, (50.0, 50.0), (60.0, 55.0));
        assert_eq!((r.w, r.h), (MIN, MIN));
        // Dragged past the bottom-right corner → stays inside.
        let r = Region::from_points(&d, (1800.0, 1000.0), (2500.0, 1500.0));
        assert!(r.x + r.w <= 1920 && r.y + r.h <= 1080, "{r:?}");
        assert!(r.w >= MIN && r.h >= MIN);
        // Negative (left of / above the display) → clamped at 0.
        let r = Region::from_points(&d, (-50.0, -50.0), (300.0, 300.0));
        assert_eq!((r.x, r.y), (0, 0));
        // Larger than the display → the whole display.
        let r = Region::from_points(&retina(), (-10.0, -10.0), (5000.0, 5000.0));
        assert_eq!(r, Region { display: 7, x: 0, y: 0, w: 3360, h: 2100 });
        // A minimum-size region at the very edge is moved inside, not shrunk.
        let r = Region { display: 9, x: 1900, y: 1070, w: 100, h: 100 }.clamped(&d);
        assert_eq!(r, Region { display: 9, x: 1920 - MIN, y: 1080 - MIN, w: MIN, h: MIN });
    }

    #[test]
    fn cursor_maps_into_region_video() {
        let d = second();
        let r = Region { display: 9, x: 100, y: 50, w: 800, h: 600 };
        // Display origin (1680, -200) + region (100, 50) = global (1780, -150).
        assert_eq!(r.cursor_to_video(&d, (1780.0, -150.0)), (0.0, 0.0));
        assert_eq!(r.cursor_to_video(&d, (1880.0, -100.0)), (100.0, 50.0));
        assert!(r.contains_video((100.0, 50.0)));
        assert!(!r.contains_video(r.cursor_to_video(&d, (1700.0, 0.0))));
        // Retina: points × 2.
        let d = retina();
        let r = Region { display: 7, x: 200, y: 100, w: 600, h: 600 };
        assert_eq!(r.cursor_to_video(&d, (150.0, 75.0)), (100.0, 50.0));
    }
}
