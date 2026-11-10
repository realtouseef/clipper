//! Fitting an image into the output without distortion: cover (crop around a focal point,
//! with zoom and pan) and contain (whole image, centered). Sizes are (width, height).

/// Orientation tag from pixel size.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Orientation {
    Landscape,
    Portrait,
    Square,
}

impl Orientation {
    pub fn of(w: u32, h: u32) -> Self {
        let r = w as f32 / h.max(1) as f32;
        if r > 1.05 {
            Orientation::Landscape
        } else if r < 0.95 {
            Orientation::Portrait
        } else {
            Orientation::Square
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Orientation::Landscape => "Landscape",
            Orientation::Portrait => "Portrait",
            Orientation::Square => "Square",
        }
    }
}

/// The part of the source (x, y, w, h in source pixels) that fills the output in Cover mode.
/// It has the output's aspect, is `zoom` times smaller than the largest such crop, is
/// centered on `focal` (fractions of the source) as far as the edges allow, then shifted by
/// `pan` (-1..=1 of the remaining room).
pub fn cover_crop(src: (u32, u32), out: (u32, u32), focal: (f32, f32), zoom: f32, pan: (f32, f32)) -> (f32, f32, f32, f32) {
    let (sw, sh) = (src.0.max(1) as f32, src.1.max(1) as f32);
    let a = out.0.max(1) as f32 / out.1.max(1) as f32;
    let (mut w, mut h) = if sw / sh > a { (sh * a, sh) } else { (sw, sw / a) };
    let z = zoom.clamp(1.0, 3.0);
    w /= z;
    h /= z;
    let place = |center: f32, len: f32, total: f32, pan: f32| {
        let max = total - len;
        let x = (center - len / 2.0).clamp(0.0, max);
        // Pan moves toward an edge by its share of the room on that side.
        let p = pan.clamp(-1.0, 1.0);
        if p >= 0.0 { x + (max - x) * p } else { x + x * p }
    };
    let x = place(focal.0.clamp(0.0, 1.0) * sw, w, sw, pan.0);
    let y = place(focal.1.clamp(0.0, 1.0) * sh, h, sh, pan.1);
    (x, y, w, h)
}

/// Where the whole image goes in Contain mode (x, y, w, h in output pixels), centered.
pub fn contain_rect(src: (u32, u32), out: (u32, u32)) -> (f32, f32, f32, f32) {
    let (sw, sh) = (src.0.max(1) as f32, src.1.max(1) as f32);
    let (ow, oh) = (out.0 as f32, out.1 as f32);
    let k = (ow / sw).min(oh / sh);
    let (w, h) = (sw * k, sh * k);
    ((ow - w) / 2.0, (oh - h) / 2.0, w, h)
}

/// Whether the image has fewer pixels than the output needs (it would be upscaled).
pub fn low_resolution(src: (u32, u32), out: (u32, u32), cover: bool, zoom: f32) -> bool {
    if cover {
        let (_, _, w, _) = cover_crop(src, out, (0.5, 0.5), zoom, (0.0, 0.0));
        w + 0.5 < out.0 as f32
    } else {
        let (_, _, w, _) = contain_rect(src, out);
        w > src.0 as f32 + 0.5
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orientation_detection() {
        assert_eq!(Orientation::of(1920, 1080), Orientation::Landscape);
        assert_eq!(Orientation::of(1080, 1920), Orientation::Portrait);
        assert_eq!(Orientation::of(1000, 1000), Orientation::Square);
        assert_eq!(Orientation::of(1020, 1000), Orientation::Square, "within 5% is square");
    }

    #[test]
    fn cover_keeps_aspect_and_follows_focal_point() {
        // A 4000×2000 image into a 9:16 output: full height, 9:16 wide.
        let (x, y, w, h) = cover_crop((4000, 2000), (1080, 1920), (0.5, 0.5), 1.0, (0.0, 0.0));
        assert!((w / h - 1080.0 / 1920.0).abs() < 1e-4, "no distortion");
        assert_eq!((y, h), (0.0, 2000.0));
        assert!((x + w / 2.0 - 2000.0).abs() < 0.01, "centered");
        // Focal point on the right third: the crop moves there.
        let (x, _, w, _) = cover_crop((4000, 2000), (1080, 1920), (0.8, 0.5), 1.0, (0.0, 0.0));
        assert!((x + w / 2.0 - 3200.0).abs() < 0.01);
        // At the edge it stops inside the image.
        let (x, _, w, _) = cover_crop((4000, 2000), (1080, 1920), (1.0, 0.5), 1.0, (0.0, 0.0));
        assert!((x + w - 4000.0).abs() < 0.01);
        // Zoom 2×: half the size, still on the focal point.
        let (x, y, w2, h2) = cover_crop((4000, 2000), (1080, 1920), (0.5, 0.25), 2.0, (0.0, 0.0));
        assert!((w2 - w / 2.0).abs() < 0.01 && (h2 - 1000.0).abs() < 0.01);
        assert!((x + w2 / 2.0 - 2000.0).abs() < 0.01 && (y - 0.0).abs() < 0.01, "clamped to the top");
        // Pan all the way right reaches the right edge.
        let (x, _, w, _) = cover_crop((4000, 2000), (1080, 1920), (0.5, 0.5), 1.0, (1.0, 0.0));
        assert!((x + w - 4000.0).abs() < 0.01);
    }

    #[test]
    fn contain_fits_inside_centered() {
        let (x, y, w, h) = contain_rect((4000, 2000), (1080, 1920));
        assert_eq!((x, w), (0.0, 1080.0));
        assert!((h - 540.0).abs() < 0.01 && (y - 690.0).abs() < 0.01);
        assert!((w / h - 2.0).abs() < 1e-4, "no distortion");
    }

    #[test]
    fn low_resolution_badge() {
        assert!(low_resolution((800, 600), (1080, 1920), true, 1.0));
        assert!(!low_resolution((4000, 4000), (1080, 1920), true, 1.0));
        assert!(low_resolution((4000, 4000), (1080, 1920), true, 3.0), "zooming in needs more pixels");
        assert!(low_resolution((500, 500), (1920, 1080), false, 1.0));
    }
}
