//! Merged documents ("boards"): several images on one canvas, stacked vertically,
//! side by side, or placed freely. Layout math only; no UI.

use super::doc::{R, r};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum BoardLayout {
    #[default]
    Vertical,
    Horizontal,
    Free,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum BoardAlign {
    Start,
    #[default]
    Center,
    End,
}

/// Scale images so their edges line up: the same width (stacked) or height (side by side).
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum Match {
    #[default]
    None,
    Width,
    Height,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Board {
    pub layout: BoardLayout,
    /// Space between images (canvas pixels).
    pub gap: f32,
    /// Fill of the gaps between images; `None` shows the background.
    pub separator: Option<super::doc::Rgba>,
    pub align: BoardAlign,
    pub matching: Match,
    /// Space around the images.
    pub padding: f32,
    /// Extra canvas on each side (left, top, right, bottom), for callouts outside the images.
    pub expand: [f32; 4],
    /// A canvas size the user typed in (pixels). None: the canvas fits the images (plus
    /// padding and expansion) whenever the layout changes. Either way, moving or resizing an
    /// image never changes it.
    #[serde(default)]
    pub canvas: Option<(f32, f32)>,
}

impl Default for Board {
    fn default() -> Self {
        Self { layout: BoardLayout::Vertical, gap: 24.0, separator: None, align: BoardAlign::Center, matching: Match::None, padding: 48.0, expand: [0.0; 4], canvas: None }
    }
}

/// Most images on one board.
pub const MAX_IMAGES: usize = 6;

/// Matching edges never enlarges an image more than this.
pub const MAX_UPSCALE: f32 = 1.5;

/// Places images of the given (cropped) sizes for a stacked layout, from (0, 0), in order.
/// Images are scaled uniformly only when matching, to a common width (vertical) or height
/// (horizontal): the largest, but no image grows past 1.5× (wider ones shrink instead), so
/// a phone screenshot next to a desktop one doesn't turn to mush. Never stretched.
/// (Callouts sample the original pixels, so shrinking keeps them sharp.)
pub fn arrange(sizes: &[(f32, f32)], layout: BoardLayout, gap: f32, align: BoardAlign, matching: Match) -> Vec<R> {
    let vertical = match layout {
        BoardLayout::Vertical => true,
        BoardLayout::Horizontal => false,
        BoardLayout::Free => return Vec::new(),
    };
    let target = |v: Vec<f32>| {
        let (lo, hi) = (v.iter().copied().fold(f32::MAX, f32::min), v.iter().copied().fold(0.0, f32::max));
        hi.min(lo * MAX_UPSCALE)
    };
    let target_w = target(sizes.iter().map(|s| s.0).collect());
    let target_h = target(sizes.iter().map(|s| s.1).collect());
    let scaled: Vec<(f32, f32)> = sizes
        .iter()
        .map(|&(w, h)| {
            let k = match matching {
                Match::Width if w > 0.0 => target_w / w,
                Match::Height if h > 0.0 => target_h / h,
                _ => 1.0,
            };
            (w * k, h * k)
        })
        .collect();
    let cross = scaled.iter().map(|s| if vertical { s.0 } else { s.1 }).fold(0.0, f32::max);
    let mut at = 0.0;
    scaled
        .iter()
        .map(|&(w, h)| {
            let size_cross = if vertical { w } else { h };
            let off = match align {
                BoardAlign::Start => 0.0,
                BoardAlign::Center => ((cross - size_cross) / 2.0).round(),
                BoardAlign::End => cross - size_cross,
            };
            let rect = if vertical { r(off, at, w, h) } else { r(at, off, w, h) };
            at += (if vertical { h } else { w }) + gap;
            rect
        })
        .collect()
}

/// The gaps between consecutive placed images (for the separator fill), spanning the
/// whole cross axis of the stack.
pub fn gaps(rects: &[R], layout: BoardLayout) -> Vec<R> {
    if rects.len() < 2 || layout == BoardLayout::Free {
        return Vec::new();
    }
    let all = rects.iter().skip(1).fold(rects[0], |a, b| a.union(b));
    rects
        .windows(2)
        .filter_map(|w| match layout {
            BoardLayout::Vertical if w[1].y > w[0].bottom() => Some(r(all.x, w[0].bottom(), all.w, w[1].y - w[0].bottom())),
            BoardLayout::Horizontal if w[1].x > w[0].right() => Some(r(w[0].right(), all.y, w[1].x - w[0].right(), all.h)),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vertical_stack_with_gaps_alignment_and_match_width() {
        let sizes = [(800.0, 400.0), (400.0, 300.0)];
        let v = arrange(&sizes, BoardLayout::Vertical, 24.0, BoardAlign::Center, Match::None);
        assert_eq!(v, vec![r(0.0, 0.0, 800.0, 400.0), r(200.0, 424.0, 400.0, 300.0)]);
        let start = arrange(&sizes, BoardLayout::Vertical, 24.0, BoardAlign::Start, Match::None);
        assert_eq!(start[1].x, 0.0);
        let end = arrange(&sizes, BoardLayout::Vertical, 24.0, BoardAlign::End, Match::None);
        assert_eq!(end[1].x, 400.0);
        // Match width: 600 wide (the narrow one grows at most 1.5×, the wide one shrinks).
        let m = arrange(&sizes, BoardLayout::Vertical, 10.0, BoardAlign::Center, Match::Width);
        assert_eq!(m, vec![r(0.0, 0.0, 600.0, 300.0), r(0.0, 310.0, 600.0, 450.0)]);
        // Similar sizes match to the widest.
        let close = arrange(&[(800.0, 400.0), (700.0, 350.0)], BoardLayout::Vertical, 0.0, BoardAlign::Center, Match::Width);
        assert!(close.iter().all(|r| (r.w - 800.0).abs() < 1e-3));
        // Never distorted: aspect ratios kept.
        for (rect, s) in m.iter().zip(&sizes) {
            assert!((rect.w / rect.h - s.0 / s.1).abs() < 1e-4);
        }
        assert_eq!(gaps(&m, BoardLayout::Vertical), vec![r(0.0, 300.0, 600.0, 10.0)]);
    }

    #[test]
    fn horizontal_side_by_side_with_match_height() {
        let sizes = [(600.0, 400.0), (300.0, 200.0), (500.0, 500.0)];
        let h = arrange(&sizes, BoardLayout::Horizontal, 20.0, BoardAlign::Start, Match::None);
        assert_eq!(h, vec![r(0.0, 0.0, 600.0, 400.0), r(620.0, 0.0, 300.0, 200.0), r(940.0, 0.0, 500.0, 500.0)]);
        let m = arrange(&sizes, BoardLayout::Horizontal, 20.0, BoardAlign::Center, Match::Height);
        // 200 × 1.5 = 300 tall.
        assert!(m.iter().all(|r| (r.h - 300.0).abs() < 1e-3 && r.y == 0.0), "{m:?}");
        assert_eq!(m[0].w, 450.0);
        assert_eq!(m[1].x, 470.0);
        assert_eq!(gaps(&m, BoardLayout::Horizontal).len(), 2);
        assert!(arrange(&sizes, BoardLayout::Free, 20.0, BoardAlign::Start, Match::None).is_empty());
    }
}
