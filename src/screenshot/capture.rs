//! Single-frame grabs through the ScreenCaptureKit helper (`helpers/shot.swift`), at the
//! display's physical pixel resolution. Clipper's own windows (selection overlay, cards,
//! control bar) are left out by process, so they never appear in a screenshot.

use crate::region::{Display, Region};
use anyhow::{Result, anyhow};
use serde::Deserialize;
use std::path::Path;
use std::process::Command;

fn helper() -> Command {
    let mut c = Command::new(crate::recorder::helper("clipper-shot", env!("CLIPPER_SHOT_HELPER")));
    c.args(["--exclude-pid", &std::process::id().to_string()]);
    c
}

fn run(mut cmd: Command) -> Result<(u32, u32)> {
    let out = cmd.output().map_err(|e| anyhow!("Could not start the screenshot helper: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let mut parts = stdout.split_whitespace();
    match (parts.next(), parts.next().and_then(|w| w.parse().ok()), parts.next().and_then(|h| h.parse().ok())) {
        (Some("ok"), Some(w), Some(h)) if out.status.success() => Ok((w, h)),
        _ => Err(anyhow!("Screenshot failed: {}", stderr.trim().trim_start_matches("error: "))),
    }
}

/// The helper's source rect for a region: points relative to the display.
pub fn rect_arg(region: &Region, display: &Display) -> String {
    let (x, y, w, h) = region.to_points(display);
    format!("{x},{y},{w},{h}")
}

/// Captures a whole display, or a region of it, into `out` (PNG). Returns the pixel size.
pub fn display(display: &Display, region: Option<&Region>, cursor: bool, out: &Path) -> Result<(u32, u32)> {
    let mut cmd = helper();
    cmd.arg("--out").arg(out).args(["--display-id", &display.id.to_string()]);
    if let Some(r) = region {
        cmd.args(["--rect", &rect_arg(r, display)]);
    }
    if cursor {
        cmd.arg("--cursor");
    }
    run(cmd)
}

/// Captures one window (without its shadow) into `out`.
pub fn window(id: u32, cursor: bool, out: &Path) -> Result<(u32, u32)> {
    let mut cmd = helper();
    cmd.arg("--out").arg(out).args(["--window-id", &id.to_string()]);
    if cursor {
        cmd.arg("--cursor");
    }
    run(cmd)
}

/// An on-screen window, with its bounds in global points (top-left origin).
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct WindowInfo {
    pub id: u32,
    pub app: String,
    pub title: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl WindowInfo {
    pub fn contains(&self, p: (f64, f64)) -> bool {
        p.0 >= self.x && p.0 < self.x + self.w && p.1 >= self.y && p.1 < self.y + self.h
    }
}

/// Other apps' normal windows, front to back.
pub fn windows() -> Vec<WindowInfo> {
    let mut cmd = helper();
    cmd.arg("--list-windows");
    cmd.output().ok().and_then(|o| serde_json::from_slice(&o.stdout).ok()).unwrap_or_default()
}

/// The frontmost window under a global point.
pub fn window_at(windows: &[WindowInfo], p: (f64, f64)) -> Option<&WindowInfo> {
    windows.iter().find(|w| w.contains(p))
}

/// The display containing a global point (the main display if none does).
pub fn display_at(displays: &[Display], p: (f64, f64)) -> Option<Display> {
    displays
        .iter()
        .find(|d| p.0 >= d.origin.0 && p.0 < d.origin.0 + d.size.0 && p.1 >= d.origin.1 && p.1 < d.origin.1 + d.size.1)
        .or(displays.first())
        .copied()
}

/// The display under the mouse pointer.
pub fn display_under_cursor() -> Option<Display> {
    display_at(&crate::cursor::displays(), crate::cursor::cursor_location())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_crop_math_at_any_scale() {
        // Retina (2×), a scaled 1.5× mode, and a plain 1× monitor to the right.
        let retina = Display { id: 1, origin: (0.0, 0.0), size: (1512.0, 982.0), scale: 2.0 };
        let scaled = Display { id: 2, origin: (0.0, 0.0), size: (1280.0, 800.0), scale: 1.5 };
        let plain = Display { id: 3, origin: (1512.0, -200.0), size: (1920.0, 1080.0), scale: 1.0 };
        for d in [retina, scaled, plain] {
            // A 300×200 pt selection at (100, 50) pt.
            let r = Region::from_points_min(&d, (100.0, 50.0), (400.0, 250.0), 8);
            assert_eq!((r.w, r.h), ((300.0 * d.scale) as u32, (200.0 * d.scale) as u32), "physical pixels at {}x", d.scale);
            assert_eq!((r.x, r.y), ((100.0 * d.scale) as u32, (50.0 * d.scale) as u32));
            // The helper gets points back, which it multiplies by the scale again.
            let v: Vec<f64> = rect_arg(&r, &d).split(',').map(|s| s.parse().unwrap()).collect();
            assert!((v[0] - 100.0).abs() < 1e-9 && (v[2] - 300.0).abs() < 1e-9 && (v[3] - 200.0).abs() < 1e-9);
        }
        // Small screenshot regions stay small (recording regions have a 160 px minimum).
        let tiny = Region::from_points_min(&retina, (10.0, 10.0), (30.0, 20.0), 8);
        assert_eq!((tiny.w, tiny.h), (40, 20));
        // Which display a point is on, including a negative-origin one.
        let all = [retina, plain];
        assert_eq!(display_at(&all, (2000.0, -100.0)).unwrap().id, 3);
        assert_eq!(display_at(&all, (10.0, 10.0)).unwrap().id, 1);
        assert_eq!(display_at(&all, (-5000.0, 0.0)).unwrap().id, 1, "falls back to main");
    }

    #[test]
    fn window_under_point_is_frontmost() {
        let w = |id, x| WindowInfo { id, app: "A".into(), title: String::new(), x, y: 0.0, w: 100.0, h: 100.0 };
        let list = vec![w(1, 50.0), w(2, 0.0)];
        assert_eq!(window_at(&list, (60.0, 10.0)).unwrap().id, 1);
        assert_eq!(window_at(&list, (10.0, 10.0)).unwrap().id, 2);
        assert!(window_at(&list, (500.0, 10.0)).is_none());
    }

    /// Real captures (needs Screen Recording permission): sizes are physical pixels.
    #[test]
    #[ignore]
    fn real_screenshots() {
        let d = crate::cursor::displays()[0];
        let dir = std::env::temp_dir();
        let full = dir.join("clipper-shot-full.png");
        let (w, h) = display(&d, None, false, &full).unwrap();
        assert_eq!((w, h), d.pixels());
        let r = Region::from_points_min(&d, (100.0, 100.0), (400.0, 300.0), 8);
        let part = dir.join("clipper-shot-region.png");
        assert_eq!(display(&d, Some(&r), false, &part).unwrap(), (r.w, r.h));
        assert_eq!(image::image_dimensions(&part).unwrap(), (r.w, r.h));
        if let Some(win) = windows().first() {
            let (w, _) = window(win.id, false, &dir.join("clipper-shot-window.png")).unwrap();
            assert!((w as f64 - win.w * d.scale).abs() <= 2.0);
        }
    }
}
