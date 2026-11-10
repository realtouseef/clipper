//! Output layout: canvas ratio and size, where the webcam goes, and the resulting rects.
//!
//! Every layer (preview, export, camera follow) asks [`Layout::frame`] for the rects, so the
//! screen content rect the camera crop is sized for is exactly the rect it is drawn into.

use crate::style::{Color, Style};

/// Output aspect ratio.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ratio {
    /// 9:16, for Shorts / Reels / TikTok.
    Portrait,
    /// 16:9, for YouTube and other landscape players.
    Landscape,
}

impl Ratio {
    pub const ALL: [Ratio; 2] = [Ratio::Portrait, Ratio::Landscape];

    pub fn label(self) -> &'static str {
        match self {
            Ratio::Portrait => "9:16",
            Ratio::Landscape => "16:9",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Ratio::Portrait => "Vertical",
            Ratio::Landscape => "Horizontal",
        }
    }

    /// Where a video of this shape fits.
    pub fn platforms(self) -> &'static str {
        match self {
            Ratio::Portrait => "TikTok · YouTube Shorts · Instagram Reels · Facebook Reels",
            Ratio::Landscape => "YouTube · Facebook · LinkedIn · X",
        }
    }
}

/// Output resolution along the short side.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Res {
    P1080,
    P720,
}

impl Res {
    pub const ALL: [Res; 2] = [Res::P1080, Res::P720];

    pub fn label(self) -> &'static str {
        match self {
            Res::P1080 => "1080p",
            Res::P720 => "720p",
        }
    }
}

/// Canvas size in output pixels.
pub fn canvas(ratio: Ratio, res: Res) -> (u32, u32) {
    let (short, long) = match res {
        Res::P1080 => (1080, 1920),
        Res::P720 => (720, 1280),
    };
    match ratio {
        Ratio::Portrait => (short, long),
        Ratio::Landscape => (long, short),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    pub const ALL: [Corner; 4] = [Corner::TopLeft, Corner::TopRight, Corner::BottomLeft, Corner::BottomRight];

    pub fn label(self) -> &'static str {
        match self {
            Corner::TopLeft => "top left",
            Corner::TopRight => "top right",
            Corner::BottomLeft => "bottom left",
            Corner::BottomRight => "bottom right",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shape {
    Circle,
    /// Rounded square.
    Square,
}

/// The side of the canvas the webcam takes in a split layout.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Top,
    Bottom,
    Left,
    Right,
}

impl Side {
    fn swapped(self) -> Self {
        match self {
            Side::Top => Side::Bottom,
            Side::Bottom => Side::Top,
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        }
    }

    fn fits(self, ratio: Ratio) -> bool {
        matches!(
            (self, ratio),
            (Side::Top | Side::Bottom, Ratio::Portrait) | (Side::Left | Side::Right, Ratio::Landscape)
        )
    }

    /// The equivalent side for the other ratio: top ↔ left, bottom ↔ right.
    fn rotated(self) -> Self {
        match self {
            Side::Top => Side::Left,
            Side::Left => Side::Top,
            Side::Bottom => Side::Right,
            Side::Right => Side::Bottom,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Side::Top => "Webcam top",
            Side::Bottom => "Webcam bottom",
            Side::Left => "Webcam left",
            Side::Right => "Webcam right",
        }
    }
}

/// Where the webcam goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Placement {
    None,
    /// Floats above the screen content in a corner.
    Float(Shape, Corner),
    /// Takes one side of the canvas; the screen content gets the rest.
    Split(Side),
}

impl Placement {
    pub const FALLBACK: Placement = Placement::Float(Shape::Circle, Corner::BottomRight);

    /// The placements offered for `ratio`, in picker order.
    pub fn options(ratio: Ratio) -> Vec<Placement> {
        let mut v = vec![Placement::None];
        for shape in [Shape::Circle, Shape::Square] {
            v.extend(Corner::ALL.map(|c| Placement::Float(shape, c)));
        }
        v.extend(
            [Side::Top, Side::Bottom, Side::Left, Side::Right]
                .into_iter()
                .filter(|s| s.fits(ratio))
                .map(Placement::Split),
        );
        v
    }

    pub fn valid_for(self, ratio: Ratio) -> bool {
        match self {
            Placement::Split(s) => s.fits(ratio),
            _ => true,
        }
    }

    /// The closest placement that is valid for `ratio`.
    pub fn for_ratio(self, ratio: Ratio) -> Placement {
        match self {
            p if p.valid_for(ratio) => p,
            Placement::Split(s) if s.rotated().fits(ratio) => Placement::Split(s.rotated()),
            _ => Placement::FALLBACK,
        }
    }

    pub fn swapped(self) -> Placement {
        match self {
            Placement::Split(s) => Placement::Split(s.swapped()),
            p => p,
        }
    }

    pub fn label(self) -> String {
        match self {
            Placement::None => "No webcam".into(),
            Placement::Float(Shape::Circle, c) => format!("Circle, {}", c.label()),
            Placement::Float(Shape::Square, c) => format!("Square, {}", c.label()),
            Placement::Split(s) => s.label().into(),
        }
    }
}

/// Split webcam share limits.
pub const MIN_SPLIT: f32 = 0.25;
pub const MAX_SPLIT: f32 = 0.75;
/// The default share; at or above it the webcam crop-fills its region.
pub const DEFAULT_SPLIT: f32 = 2.0 / 3.0;

/// Layout settings (Output → Layout, Camera Overlay).
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub ratio: Ratio,
    pub res: Res,
    pub placement: Placement,
    /// Floating webcam diameter as a fraction of the shorter canvas side, 0.10..=0.40.
    pub cam_size: f32,
    /// Floating webcam distance from the canvas edges, in output px.
    pub cam_margin: u32,
    pub cam_border: u32,
    pub cam_border_color: Color,
    pub cam_shadow: bool,
    /// Share of the canvas the webcam takes in a split layout, 0.25..=0.75. Low values keep
    /// the focus on the screen recording.
    pub split: f32,
    /// Extra screen zoom (1.0..=2.5). The camera crop shrinks by this factor, so follow has
    /// room to move even when the content rect matches the screen's shape.
    pub zoom: f32,
    /// A floating webcam dragged to a spot of its own: its center as a fraction of the
    /// canvas (0..=1 each way). `None` uses the placement's corner.
    pub cam_pos: Option<(f32, f32)>,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            ratio: Ratio::Portrait,
            res: Res::P1080,
            placement: Placement::None,
            cam_size: 0.22,
            cam_margin: 32,
            cam_border: 4,
            cam_border_color: [255, 255, 255],
            cam_shadow: true,
            split: DEFAULT_SPLIT,
            zoom: 1.0,
            cam_pos: None,
        }
    }
}

/// Integer rect in output pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub fn aspect(&self) -> f64 {
        self.w as f64 / self.h.max(1) as f64
    }
}

/// The webcam's rect and how it is drawn.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct CamFrame {
    /// Outer rect, including the border.
    pub rect: Rect,
    /// Corner radius of the outer rect (half the size for a circle).
    pub radius: u32,
    /// Floating above the screen (with border and optional shadow) vs a split region.
    pub floating: bool,
    pub border: u32,
    /// Framing: 1.0 crop-fills the rect; lower values zoom the camera out (down to showing
    /// the whole camera frame), with a blurred copy filling the rest of the rect.
    pub zoom_out: f32,
}

/// How the webcam video sits in its rect.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct CamFit {
    /// Part of the camera frame shown: (w, h, x, y) in camera pixels, even sizes.
    pub crop: (u32, u32, u32, u32),
    /// Where that part is drawn on the canvas (inside the webcam rect).
    pub dst: Rect,
    /// The video leaves part of the rect uncovered (filled with a blurred copy).
    pub fills: bool,
}

impl CamFrame {
    /// Places a `src`-sized camera frame in the rect. At `zoom_out` 1.0 it's a cover fit; below
    /// that the camera shrinks in proportion, never below a contain fit, and never stretched.
    pub fn fit(&self, src: (u32, u32)) -> CamFit {
        let r = self.inner();
        let (sw, sh) = (src.0.max(2) as f64, src.1.max(2) as f64);
        let cover = (r.w as f64 / sw).max(r.h as f64 / sh);
        let contain = (r.w as f64 / sw).min(r.h as f64 / sh);
        let s = (cover * self.zoom_out.clamp(0.0, 1.0) as f64).clamp(contain, cover);
        // Drawn size: the scaled frame, cut to the rect.
        let dw = even((sw * s).min(r.w as f64)).min(r.w);
        let dh = even((sh * s).min(r.h as f64)).min(r.h);
        // Matching crop of the camera frame (same aspect as the drawn size).
        let cw = even(dw as f64 / s).min(src.0);
        let ch = even(dh as f64 / s).min(src.1);
        CamFit {
            crop: (cw, ch, (src.0 - cw) / 4 * 2, (src.1 - ch) / 4 * 2),
            dst: Rect { x: r.x + (r.w - dw) / 4 * 2, y: r.y + (r.h - dh) / 4 * 2, w: dw, h: dh },
            fills: dw < r.w || dh < r.h,
        }
    }

    /// The video area inside the border.
    pub fn inner(&self) -> Rect {
        let b = self.border.min(self.rect.w / 4);
        Rect { x: self.rect.x + b, y: self.rect.y + b, w: self.rect.w - 2 * b, h: self.rect.h - 2 * b }
    }

    pub fn inner_radius(&self) -> u32 {
        self.radius.saturating_sub(self.border.min(self.rect.w / 4))
    }
}

/// Resolved rects for one canvas.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Frame {
    pub canvas: (u32, u32),
    /// The screen content rect (inside the padding). The camera crop has its aspect.
    pub screen: Rect,
    pub cam: Option<CamFrame>,
}

fn even(v: f64) -> u32 {
    ((v / 2.0).round().max(1.0) as u32) * 2
}

/// Padding clamped so a region of `w × h` keeps a usable inner rect.
fn pad_for(style: &Style, w: u32, h: u32) -> u32 {
    let max = (w.min(h) / 2).saturating_sub(48);
    (style.padding.min(max) / 2) * 2
}

impl Layout {
    pub fn canvas(&self) -> (u32, u32) {
        canvas(self.ratio, self.res)
    }

    /// Changes the ratio, keeping the closest equivalent webcam placement.
    pub fn set_ratio(&mut self, ratio: Ratio) {
        self.ratio = ratio;
        self.placement = self.placement.for_ratio(ratio);
    }

    /// Rects for this layout. `webcam` is whether there is a webcam to show; without one the
    /// screen gets the whole canvas.
    pub fn frame(&self, style: &Style, webcam: bool) -> Frame {
        let canvas = self.canvas();
        let (cw, ch) = canvas;
        let placement = if webcam { self.placement.for_ratio(self.ratio) } else { Placement::None };
        let full = |pad: u32| Rect { x: pad, y: pad, w: cw - 2 * pad, h: ch - 2 * pad };
        match placement {
            Placement::None => Frame { canvas, screen: full(pad_for(style, cw, ch)), cam: None },
            Placement::Float(shape, corner) => {
                let d = even(cw.min(ch) as f64 * self.cam_size.clamp(0.10, 0.40) as f64);
                let m = self.cam_margin.min((cw.min(ch) - d) / 2);
                let (x, y) = match (self.cam_pos, corner) {
                    // Dragged: centered on the spot, kept fully on the canvas.
                    (Some((fx, fy)), _) => {
                        let at = |f: f32, len: u32| ((f.clamp(0.0, 1.0) * len as f32 - d as f32 / 2.0).round().max(0.0) as u32).min(len - d);
                        (at(fx, cw), at(fy, ch))
                    }
                    (None, Corner::TopLeft) => (m, m),
                    (None, Corner::TopRight) => (cw - m - d, m),
                    (None, Corner::BottomLeft) => (m, ch - m - d),
                    (None, Corner::BottomRight) => (cw - m - d, ch - m - d),
                };
                let radius = match shape {
                    Shape::Circle => d / 2,
                    Shape::Square => (d as f32 * 0.2).round() as u32,
                };
                Frame {
                    canvas,
                    screen: full(pad_for(style, cw, ch)),
                    cam: Some(CamFrame {
                        rect: Rect { x, y, w: d, h: d },
                        radius,
                        floating: true,
                        border: self.cam_border,
                        zoom_out: 1.0,
                    }),
                }
            }
            Placement::Split(side) => {
                let vertical = matches!(side, Side::Top | Side::Bottom);
                let along = if vertical { ch } else { cw };
                let cam_len = even(along as f64 * self.split.clamp(MIN_SPLIT, MAX_SPLIT) as f64);
                let screen_len = along - cam_len;
                // Regions along the split axis: (start, length) for the webcam and the screen.
                let (cam_at, screen_at) = match side {
                    Side::Top | Side::Left => (0, cam_len),
                    Side::Bottom | Side::Right => (screen_len, 0),
                };
                // Each region is inset by the padding, except the shared edge, which gets
                // half on each side so the gap between them equals the padding.
                let region = |at: u32, len: u32| -> Rect {
                    let (w, h) = if vertical { (cw, len) } else { (len, ch) };
                    let p = pad_for(style, w, h);
                    let half = (p / 4) * 2;
                    let lead = if at == 0 { p } else { half };
                    let trail = if at + len == along { p } else { half };
                    if vertical {
                        Rect { x: p, y: at + lead, w: cw - 2 * p, h: len - lead - trail }
                    } else {
                        Rect { x: at + lead, y: p, w: len - lead - trail, h: ch - 2 * p }
                    }
                };
                Frame {
                    canvas,
                    screen: region(screen_at, screen_len),
                    cam: Some(CamFrame {
                        rect: region(cam_at, cam_len),
                        radius: style.radius,
                        floating: false,
                        border: 0,
                        // Shrinking the webcam below the default share zooms the camera out
                        // with it, so the person gets smaller instead of more cropped.
                        zoom_out: (self.split / DEFAULT_SPLIT).min(1.0),
                    }),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layouts() -> Vec<Layout> {
        let mut v = Vec::new();
        for ratio in Ratio::ALL {
            for res in Res::ALL {
                for placement in Placement::options(ratio) {
                    for split in [MIN_SPLIT, 1.0 / 3.0, 0.5, 2.0 / 3.0, MAX_SPLIT] {
                        v.push(Layout { ratio, res, placement, split, ..Layout::default() });
                    }
                }
            }
        }
        v
    }

    #[test]
    fn options_depend_on_ratio() {
        let p = Placement::options(Ratio::Portrait);
        let l = Placement::options(Ratio::Landscape);
        // No webcam + 8 floating + 2 splits.
        assert_eq!(p.len(), 11);
        assert_eq!(l.len(), 11);
        assert!(p.contains(&Placement::Split(Side::Top)) && !p.contains(&Placement::Split(Side::Left)));
        assert!(l.contains(&Placement::Split(Side::Right)) && !l.contains(&Placement::Split(Side::Bottom)));
    }

    #[test]
    fn ratio_switch_keeps_closest_placement() {
        let mut l = Layout { placement: Placement::Split(Side::Top), ..Layout::default() };
        l.set_ratio(Ratio::Landscape);
        assert_eq!(l.placement, Placement::Split(Side::Left));
        l.placement = Placement::Split(Side::Right);
        l.set_ratio(Ratio::Portrait);
        assert_eq!(l.placement, Placement::Split(Side::Bottom));
        l.placement = Placement::Float(Shape::Square, Corner::TopLeft);
        l.set_ratio(Ratio::Landscape);
        assert_eq!(l.placement, Placement::Float(Shape::Square, Corner::TopLeft));
    }

    /// Every rect is even-sized, inside the canvas, and split regions don't overlap.
    #[test]
    fn rects_fit_and_split_regions_dont_overlap() {
        let style = Style::default();
        for l in layouts() {
            let f = l.frame(&style, true);
            let (cw, ch) = f.canvas;
            let inside = |r: Rect| r.x + r.w <= cw && r.y + r.h <= ch && r.w > 0 && r.h > 0;
            assert!(inside(f.screen), "{l:?}");
            assert!(f.screen.w % 2 == 0 && f.screen.h % 2 == 0 && f.screen.x % 2 == 0 && f.screen.y % 2 == 0, "{l:?} {:?}", f.screen);
            match (l.placement, f.cam) {
                (Placement::None, cam) => assert!(cam.is_none()),
                (Placement::Float(..), Some(cam)) => {
                    assert!(cam.floating && inside(cam.rect));
                    assert_eq!(cam.rect.w, cam.rect.h);
                }
                (Placement::Split(_), Some(cam)) => {
                    assert!(!cam.floating && inside(cam.rect));
                    let (a, b) = (f.screen, cam.rect);
                    let overlap = a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h;
                    assert!(!overlap, "{l:?}: {a:?} {b:?}");
                    // Along the split axis the webcam takes its share of the canvas.
                    let vertical = l.ratio == Ratio::Portrait;
                    let (cl, sl) = if vertical { (b.h, a.h) } else { (b.w, a.w) };
                    let share = cl as f32 / (cl + sl) as f32;
                    assert!((share - l.split).abs() < 0.05, "{l:?}: webcam share {share}");
                }
                _ => panic!("{l:?}: missing webcam rect"),
            }
            // Without a webcam the screen gets the full padded canvas.
            assert!(l.frame(&style, false).cam.is_none());
        }
    }

    #[test]
    fn split_two_thirds() {
        let style = Style { padding: 0, ..Style::default() };
        let l = Layout { placement: Placement::Split(Side::Top), ..Layout::default() };
        let f = l.frame(&style, true);
        assert_eq!(f.cam.unwrap().rect, Rect { x: 0, y: 0, w: 1080, h: 1280 });
        assert_eq!(f.screen, Rect { x: 0, y: 1280, w: 1080, h: 640 });
        let l = Layout { ratio: Ratio::Landscape, placement: Placement::Split(Side::Right), ..Layout::default() };
        let f = l.frame(&style, true);
        assert_eq!(f.cam.unwrap().rect, Rect { x: 640, y: 0, w: 1280, h: 1080 });
        assert_eq!(f.screen, Rect { x: 0, y: 0, w: 640, h: 1080 });
    }

    #[test]
    fn floating_size_and_corner() {
        let l = Layout { placement: Placement::Float(Shape::Circle, Corner::BottomRight), ..Layout::default() };
        let cam = l.frame(&Style::default(), true).cam.unwrap();
        // 22% of 1080 ≈ 238, 32 px from the right and bottom edges.
        assert_eq!(cam.rect.w, 238);
        assert_eq!((cam.rect.x + cam.rect.w, cam.rect.y + cam.rect.h), (1080 - 32, 1920 - 32));
        assert_eq!(cam.radius, 119);
    }

    /// The webcam can shrink to a quarter of the canvas so the screen gets the focus.
    #[test]
    fn split_quarter() {
        let style = Style { padding: 0, ..Style::default() };
        let l = Layout { placement: Placement::Split(Side::Bottom), split: MIN_SPLIT, ..Layout::default() };
        let f = l.frame(&style, true);
        assert_eq!(f.cam.unwrap().rect, Rect { x: 0, y: 1440, w: 1080, h: 480 });
        assert_eq!(f.screen, Rect { x: 0, y: 0, w: 1080, h: 1440 });
        let l = Layout { ratio: Ratio::Landscape, placement: Placement::Split(Side::Left), split: MIN_SPLIT, ..Layout::default() };
        let f = l.frame(&style, true);
        assert_eq!(f.cam.unwrap().rect, Rect { x: 0, y: 0, w: 480, h: 1080 });
        assert_eq!(f.screen, Rect { x: 480, y: 0, w: 1440, h: 1080 });
        // Below the minimum is clamped.
        let tiny = Layout { split: 0.1, ..l }.frame(&style, true);
        assert_eq!(tiny.cam.unwrap().rect.w, 480);
    }

    /// At the default share the webcam fills its region. Shrunk to 25% the camera zooms out:
    /// the person is drawn smaller (not just cropped harder), the aspect is kept, and the
    /// rest of the region is left for the blurred fill.
    #[test]
    fn small_split_zooms_the_camera_out() {
        let style = Style::default();
        let cam = (1280, 720);
        let scale = |f: &CamFit| f.dst.w as f64 / f.crop.0 as f64;
        for (ratio, side) in [(Ratio::Portrait, Side::Top), (Ratio::Landscape, Side::Left)] {
            let at = |split| {
                let l = Layout { ratio, placement: Placement::Split(side), split, ..Layout::default() };
                let cf = l.frame(&style, true).cam.unwrap();
                (cf, cf.fit(cam))
            };
            let (cf, full) = at(DEFAULT_SPLIT);
            assert!(!full.fills, "{ratio:?}: default should fill");
            assert_eq!((full.dst.w, full.dst.h), (cf.inner().w, cf.inner().h));
            let (small_cf, small) = at(MIN_SPLIT);
            let r = small_cf.inner();
            // Inside the region, centered, aspect kept.
            assert!(small.dst.x >= r.x && small.dst.x + small.dst.w <= r.x + r.w);
            assert!(small.dst.y >= r.y && small.dst.y + small.dst.h <= r.y + r.h);
            let a = small.dst.w as f64 / small.dst.h as f64;
            let b = small.crop.0 as f64 / small.crop.1 as f64;
            assert!((a / b - 1.0).abs() < 0.02, "{ratio:?}: stretched {a} vs {b}");
            assert!(small.crop.2 + small.crop.0 <= cam.0 && small.crop.3 + small.crop.1 <= cam.1);
            // The person is smaller on screen than a plain cover fit of the small region.
            let cover = (r.w as f64 / cam.0 as f64).max(r.h as f64 / cam.1 as f64);
            assert!(scale(&small) < cover * 0.8, "{ratio:?}: {} vs cover {cover}", scale(&small));
            assert!(small.fills);
            // More of the camera frame is visible.
            assert!(small.crop.0 * small.crop.1 > full.crop.0 * full.crop.1 / 2, "{ratio:?}");
        }
        // Portrait at 25%: the whole camera frame fits (contain).
        let l = Layout { placement: Placement::Split(Side::Bottom), split: MIN_SPLIT, ..Layout::default() };
        let fit = l.frame(&style, true).cam.unwrap().fit(cam);
        assert_eq!((fit.crop.0, fit.crop.1), cam);
    }

    #[test]
    fn dragged_webcam_goes_anywhere_on_the_canvas() {
        let style = Style::default();
        let mut l = Layout { placement: Placement::Float(Shape::Circle, Corner::BottomRight), ..Layout::default() };
        let corner = l.frame(&style, true).cam.unwrap().rect;
        l.cam_pos = Some((0.5, 0.3));
        let (cw, ch) = l.canvas();
        let r = l.frame(&style, true).cam.unwrap().rect;
        assert_eq!((r.w, r.h), (corner.w, corner.h), "same size");
        assert!(((r.x + r.w / 2) as i64 - cw as i64 / 2).abs() <= 1);
        assert!(((r.y + r.h / 2) as f32 - ch as f32 * 0.3).abs() <= 1.0);
        // Past the edge: kept fully on the canvas.
        l.cam_pos = Some((1.2, -0.5));
        let r = l.frame(&style, true).cam.unwrap().rect;
        assert_eq!((r.x + r.w, r.y), (cw, 0));
        // Only floating webcams move; a split ignores it.
        l.placement = Placement::Split(Side::Top);
        assert!(!l.frame(&style, true).cam.unwrap().floating);
    }
}