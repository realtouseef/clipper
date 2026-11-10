//! Live preview of the composited canvas. Uses the same `style` layers as the export, so what
//! the preview shows is what gets rendered: backdrop+shadow, cropped screen, rounded corners,
//! cursor sprite on top, then the webcam (split region, or a floating bubble above it all).

use crate::layout::{CamFrame, Frame, Rect};
use crate::recorder::Recording;
use crate::style::{self, Color, Style};
use gpui::prelude::*;
use gpui::{Div, ObjectFit, Pixels, RenderImage, Size, Window, div, img, px};
use image::RgbaImage;
use std::sync::Arc;

/// Cached GPU images for one style + display size. Rebuild with [`CanvasArt::new`] whenever
/// the `Style`, the canvas, the display size or the crop width changes, and hand the old one
/// to [`CanvasArt::release`] so its atlas textures are freed.
pub struct CanvasArt {
    bg: Arc<RenderImage>,
    corners: Arc<RenderImage>,
    cursor: Arc<RenderImage>,
    /// Logical size of the cursor sprite and its hotspot offset.
    cursor_size: (f32, f32),
    cursor_hot: f32,
    /// Logical canvas size.
    size: (f32, f32),
    /// Logical inner rect (x, y, w, h).
    inner: (f32, f32, f32, f32),
    /// Screen content rect in output pixels, for cursor placement.
    screen: Rect,
    /// Logical px per output px.
    scale: f32,
    /// Unzoomed crop width the cursor sprite was sized for.
    crop_w: u32,
    cam: Option<CamArt>,
    anim: Option<AnimArt>,
}

/// Layers for an animated background: drawn over each frame's background image.
struct AnimArt {
    /// Shadow and empty-webcam fill (what the backdrop adds over the plain background).
    overlay: Arc<RenderImage>,
    grain: Arc<RenderImage>,
    patches: Vec<Patch>,
    /// Physical size the patches were cut at.
    phys: (u32, u32),
}

/// A rounded corner of the screen (or split webcam) region: where the background shows
/// over the video. Re-tinted from each animated frame.
struct Patch {
    /// x, y, w, h in physical pixels, and the alpha there.
    rect: (u32, u32, u32, u32),
    alpha: Vec<u8>,
    /// Logical rect.
    logical: (f32, f32, f32, f32),
}

/// The webcam's place on the preview canvas.
struct CamArt {
    frame: CamFrame,
    /// Floating webcam's shadow + border sprite and its logical rect.
    sprite: Option<(Arc<RenderImage>, (f32, f32, f32, f32))>,
}

/// The CPU-side layers for a `CanvasArt`. Rendering them is the slow part (the shadow
/// blur), so it can run on a background thread; `CanvasArt::upload` is cheap.
pub struct ArtPixels {
    bg: RgbaImage,
    corners: RgbaImage,
    cursor: RgbaImage,
    cursor_hot: f32,
    scale_factor: f32,
    size: (f32, f32),
    inner: (f32, f32, f32, f32),
    screen: Rect,
    scale: f32,
    crop_w: u32,
    cam: Option<(CamFrame, Option<(RgbaImage, (f32, f32, f32, f32))>)>,
    anim: Option<(RgbaImage, RgbaImage, Vec<Patch>, (u32, u32))>,
}

impl ArtPixels {
    /// `canvas` is the output size (e.g. 1080×1920); `display` is the logical size the canvas
    /// is drawn at (same aspect). `px_per_pt` is video pixels per display point and `crop_w`
    /// the unzoomed camera crop width in video pixels.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        style: &Style,
        frame: &Frame,
        cam_border_color: Color,
        cam_shadow: bool,
        display: Size<Pixels>,
        scale_factor: f32,
        px_per_pt: f32,
        crop_w: u32,
    ) -> Self {
        let canvas = frame.canvas;
        let scale = f32::from(display.width) / canvas.0 as f32;
        let physical = scale * scale_factor;
        let backdrop = style::render_backdrop(style, frame, physical);

        let Rect { x: ix, y: iy, w: iw, h: ih } = frame.screen;
        let cam = frame.cam.map(|cf| {
            let sprite = cf.floating.then(|| {
                let sp = style::render_cam_frame(&cf, cam_border_color, cam_shadow, physical);
                let (w, h) = sp.image.dimensions();
                let rect = (
                    cf.rect.x as f32 * scale + sp.offset.0 as f32 / scale_factor,
                    cf.rect.y as f32 * scale + sp.offset.1 as f32 / scale_factor,
                    w as f32 / scale_factor,
                    h as f32 / scale_factor,
                );
                (sp.image, rect)
            });
            (cf, sprite)
        });
        // Animated background: the overlay, grain and corner patches to draw per frame.
        let anim = style.bg.is_animated().then(|| {
            let (pw, ph) = backdrop.corners.dimensions();
            let mut rects = vec![(frame.screen, style.radius)];
            rects.extend(frame.cam.filter(|c| !c.floating).map(|c| (c.rect, c.radius)));
            let mut patches = Vec::new();
            for (r, radius) in rects {
                let rr = (radius as f32 * physical).ceil() as u32 + 2;
                let x0 = (r.x as f32 * physical).floor() as u32;
                let y0 = (r.y as f32 * physical).floor() as u32;
                let x1 = (((r.x + r.w) as f32 * physical).ceil() as u32).min(pw);
                let y1 = (((r.y + r.h) as f32 * physical).ceil() as u32).min(ph);
                for (bx, by) in [(x0, y0), (x1.saturating_sub(rr), y0), (x0, y1.saturating_sub(rr)), (x1.saturating_sub(rr), y1.saturating_sub(rr))] {
                    let (w, h) = (rr.min(pw - bx.min(pw)), rr.min(ph - by.min(ph)));
                    if w == 0 || h == 0 {
                        continue;
                    }
                    let alpha = (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).map(|(x, y)| backdrop.corners.get_pixel(bx + x, by + y)[3]).collect();
                    let sf = scale_factor;
                    patches.push(Patch { rect: (bx, by, w, h), alpha, logical: (bx as f32 / sf, by as f32 / sf, w as f32 / sf, h as f32 / sf) });
                }
            }
            let grain = crate::backgrounds::render::grain_layer(pw, ph, crate::backgrounds::render::grain_of(&style.bg));
            (backdrop.overlay.clone(), grain, patches, (pw, ph))
        });
        // Cursor: display points → video px → output px → physical px.
        let k = iw as f32 / crop_w.max(1) as f32;
        let sprite = style::render_cursor(px_per_pt * k * style.cursor_scale * physical);
        Self {
            bg: backdrop.bg,
            corners: backdrop.corners,
            cursor_hot: sprite.hotspot / scale_factor,
            cursor: sprite.image,
            scale_factor,
            size: (canvas.0 as f32 * scale, canvas.1 as f32 * scale),
            inner: (ix as f32 * scale, iy as f32 * scale, iw as f32 * scale, ih as f32 * scale),
            screen: frame.screen,
            scale,
            crop_w: crop_w.max(1),
            cam,
            anim,
        }
    }
}

impl CanvasArt {
    /// Renders and uploads in one go (blocking).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        style: &Style,
        frame: &Frame,
        cam_border_color: Color,
        cam_shadow: bool,
        display: Size<Pixels>,
        rec: &Recording,
        crop_w: u32,
        window: &mut Window,
    ) -> Self {
        Self::upload(ArtPixels::render(
            style,
            frame,
            cam_border_color,
            cam_shadow,
            display,
            window.scale_factor(),
            px_per_pt(rec),
            crop_w,
        ))
    }

    /// Turns rendered layers into GPU images.
    pub fn upload(p: ArtPixels) -> Self {
        let (cw, ch) = p.cursor.dimensions();
        Self {
            bg: to_render_image(p.bg),
            corners: to_render_image(p.corners),
            cursor_size: (cw as f32 / p.scale_factor, ch as f32 / p.scale_factor),
            cursor_hot: p.cursor_hot,
            cursor: to_render_image(p.cursor),
            size: p.size,
            inner: p.inner,
            screen: p.screen,
            scale: p.scale,
            crop_w: p.crop_w,
            cam: p.cam.map(|(frame, sprite)| CamArt {
                frame,
                sprite: sprite.map(|(img, rect)| (to_render_image(img), rect)),
            }),
            anim: p.anim.map(|(overlay, grain, patches, phys)| AnimArt { overlay: to_render_image(overlay), grain: to_render_image(grain), patches, phys }),
        }
    }

    /// The rounded-corner patches tinted from an animated background frame (`low`, which
    /// covers the whole canvas at low resolution).
    pub fn anim_corners(&self, low: &image::RgbImage) -> Vec<(Arc<RenderImage>, (f32, f32, f32, f32))> {
        let Some(a) = &self.anim else { return Vec::new() };
        let (lw, lh) = low.dimensions();
        let (kx, ky) = (lw as f32 / a.phys.0.max(1) as f32, ly(lh, a.phys.1));
        fn ly(lh: u32, ph: u32) -> f32 {
            lh as f32 / ph.max(1) as f32
        }
        a.patches
            .iter()
            .map(|p| {
                let (bx, by, w, h) = p.rect;
                let img = RgbaImage::from_fn(w, h, |x, y| {
                    let sx = (((bx + x) as f32 + 0.5) * kx).min(lw as f32 - 1.0) as u32;
                    let sy = (((by + y) as f32 + 0.5) * ky).min(lh as f32 - 1.0) as u32;
                    let c = low.get_pixel(sx, sy);
                    image::Rgba([c[0], c[1], c[2], p.alpha[(y * w + x) as usize]])
                });
                (to_render_image(img), p.logical)
            })
            .collect()
    }

    pub fn animated(&self) -> bool {
        self.anim.is_some()
    }

    /// Frees the atlas textures. Call on the art being replaced.
    pub fn release(self, window: &mut Window) {
        let sprite = self.cam.and_then(|c| c.sprite).map(|(img, _)| img);
        let anim = self.anim.map(|a| [a.overlay, a.grain]).into_iter().flatten();
        for image in [self.bg, self.corners, self.cursor].into_iter().chain(sprite).chain(anim) {
            let _ = window.drop_image(image);
        }
    }
}

/// Video pixels per display point (2.0 on Retina). Capture is always the full main display
/// width, so width is the reliable axis (the height is cropped for the control bar).
pub fn px_per_pt(rec: &Recording) -> f32 {
    rec.px_per_pt as f32
}

/// The webcam spot's content.
pub enum CamShow {
    /// No webcam at this moment (none recorded, or it dropped out): floating layouts draw
    /// nothing, split layouts keep their empty region.
    Hidden,
    /// The webcam's frame without video (live feed not up yet).
    Placeholder,
    Frame(crate::webcam::CamImage),
}

/// The composited canvas for one moment.
/// `frame` is a full-screen preview frame (any resolution, scaled to `rec` size); `crop` is the
/// camera window (x, y, w, h) in video pixels; `cursor` is the cursor position in video pixels;
/// `webcam` is what to show in the layout's webcam spot at this moment.
pub fn canvas_preview(
    art: &CanvasArt,
    frame: Arc<RenderImage>,
    rec: &Recording,
    crop: (u32, u32, u32, u32),
    cursor: Option<(f64, f64)>,
    webcam: CamShow,
    caption: Option<(Arc<RenderImage>, (f32, f32, f32, f32))>,
    ripples: &[(crate::clicks::Click, f32)],
    click: crate::clicks::ClickStyle,
    anim: Option<(Arc<RenderImage>, &[(Arc<RenderImage>, (f32, f32, f32, f32))])>,
) -> Div {
    let (show_cam, webcam) = match webcam {
        CamShow::Hidden => (false, None),
        CamShow::Placeholder => (true, None),
        CamShow::Frame(img) => (true, Some(img)),
    };
    let s = art.scale;
    // A split webcam, framed by `CamFrame::fit` (zoomed out over a blurred fill when its
    // share is small). Drawn under the corner overlay, which rounds it like the export.
    let split_video = |cam: &CamArt, image: &crate::webcam::CamImage| {
        let r = cam.frame.inner();
        let src = image.frame.size(0);
        let (sw, sh) = (i32::from(src.width).max(1) as u32, i32::from(src.height).max(1) as u32);
        let fit = cam.frame.fit((sw, sh));
        let (cw, _, cx, cy) = fit.crop;
        let d = fit.dst;
        let k = d.w as f32 / cw.max(1) as f32 * s;
        let video = div()
            .absolute()
            .left(px(d.x as f32 * s))
            .top(px(d.y as f32 * s))
            .w(px(d.w as f32 * s))
            .h(px(d.h as f32 * s))
            .overflow_hidden()
            .child(layer(image.frame.clone(), -(cx as f32) * k, -(cy as f32) * k, sw as f32 * k, sh as f32 * k));
        div()
            .absolute()
            .inset_0()
            .when(fit.fills, |el| {
                el.child(
                    img(image.blur.clone())
                        .absolute()
                        .left(px(r.x as f32 * s))
                        .top(px(r.y as f32 * s))
                        .w(px(r.w as f32 * s))
                        .h(px(r.h as f32 * s))
                        .object_fit(ObjectFit::Cover),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(r.x as f32 * s))
                        .top(px(r.y as f32 * s))
                        .w(px(r.w as f32 * s))
                        .h(px(r.h as f32 * s))
                        .bg(gpui::hsla(0., 0., 0., 1.0 - crate::composite::DIM)),
                )
            })
            .child(video)
    };
    let (x, y, cw, ch) = crop;
    let (ix, iy, iw, ih) = art.inner;
    let kx = iw / cw.max(1) as f32;
    let ky = ih / ch.max(1) as f32;
    // Zoomed in, the cursor grows with the content (the export draws it before the crop).
    let zoom = art.crop_w as f32 / cw.max(1) as f32;
    let hot = art.cursor_hot * zoom;
    let cursor_at = cursor
        .and_then(|c| style::cursor_position(c, crop, art.screen))
        .map(|(cx, cy)| (cx as f32 * art.scale - hot, cy as f32 * art.scale - hot));

    // The webcam video, cover-fit and rounded to its inner rect.
    let cam_video = |cam: &CamArt, image: &crate::webcam::CamImage| {
        let image = image.frame.clone();
        let r = cam.frame.inner();
        img(image)
            .absolute()
            .left(px(r.x as f32 * s))
            .top(px(r.y as f32 * s))
            .w(px(r.w as f32 * s))
            .h(px(r.h as f32 * s))
            .object_fit(ObjectFit::Cover)
            .rounded(px(cam.frame.inner_radius() as f32 * s))
    };
    let split = art.cam.as_ref().filter(|c| !c.frame.floating);
    let floating = art.cam.as_ref().filter(|c| c.frame.floating);

    div()
        .relative()
        .w(px(art.size.0))
        .h(px(art.size.1))
        .overflow_hidden()
        .when(anim.is_none(), |d| d.child(stage_layer(art.bg.clone(), art.size)))
        // Animated: this moment's background (upscaled on the GPU), grain, then the shadow.
        .when_some(anim.as_ref().zip(art.anim.as_ref()), |d, ((frame, _), a)| {
            d.child(stage_layer(frame.clone(), art.size))
                .child(stage_layer(a.grain.clone(), art.size))
                .child(stage_layer(a.overlay.clone(), art.size))
        })
        .child(
            div()
                .absolute()
                .left(px(ix))
                .top(px(iy))
                .w(px(iw))
                .h(px(ih))
                .overflow_hidden()
                .child(layer(
                    frame,
                    -(x as f32) * kx,
                    -(y as f32) * ky,
                    rec.width as f32 * kx,
                    rec.height as f32 * ky,
                )),
        )
        .when_some(split.zip(webcam.as_ref()), |d, (cam, image)| d.child(split_video(cam, image)))
        .when(anim.is_none(), |d| d.child(stage_layer(art.corners.clone(), art.size)))
        .when_some(anim.as_ref(), |d, (_, patches)| d.children(patches.iter().map(|(img, (x, y, w, h))| layer(img.clone(), *x, *y, *w, *h))))
        .when_some(cursor_at, |d, (cx, cy)| {
            d.child(layer(art.cursor.clone(), cx, cy, art.cursor_size.0 * zoom, art.cursor_size.1 * zoom))
        })
        // Click ripples, above the cursor (zooming with the content, like the export).
        .children(ripples.iter().filter_map(|(c, p)| {
            let (ox, oy) = style::cursor_position((c.x, c.y), crop, art.screen)?;
            let k = art.screen.w as f32 / crop.2.max(1) as f32;
            let d = click.size * rec.px_per_pt as f32 * k * art.scale;
            let (r, a) = crate::clicks::shape(*p);
            let dd = d * r;
            let (cx, cy) = (ox as f32 * art.scale, oy as f32 * art.scale);
            let col = gpui::Rgba { r: click.color[0] as f32 / 255.0, g: click.color[1] as f32 / 255.0, b: click.color[2] as f32 / 255.0, a };
            let fill = gpui::Rgba { a: a * 0.22, ..col };
            Some(
                div()
                    .absolute()
                    .left(px(cx - dd / 2.0))
                    .top(px(cy - dd / 2.0))
                    .size(px(dd))
                    .rounded_full()
                    .border(px((d * 0.08).max(1.0)))
                    .border_color(col)
                    .bg(fill),
            )
        }))
        // Captions: above the screen and cursor, below a floating webcam.
        .when_some(caption, |d, (img, (x, y, w, h))| d.child(layer(img, x, y, w, h)))
        .when_some(floating.filter(|_| show_cam), |d, cam| {
            d.when_some(cam.sprite.clone(), |d, (sprite, (x, y, w, h))| d.child(layer(sprite, x, y, w, h)))
                .when_some(webcam.as_ref(), |d, image| d.child(cam_video(cam, image)))
        })
}

/// The preview's corner radius (the stage it sits in is rounded the same).
pub const STAGE_RADIUS: f32 = 16.0;

/// A full-size layer, rounded to the stage: GPUI clips children to a square box, so a
/// rounded parent alone would leave square image corners.
fn stage_layer(image: Arc<RenderImage>, size: (f32, f32)) -> impl IntoElement {
    img(image).absolute().left_0().top_0().w(px(size.0)).h(px(size.1)).rounded(px(STAGE_RADIUS)).object_fit(ObjectFit::Fill)
}

fn layer(image: Arc<RenderImage>, x: f32, y: f32, w: f32, h: f32) -> impl IntoElement {
    img(image)
        .absolute()
        .left(px(x))
        .top(px(y))
        .w(px(w))
        .h(px(h))
        .object_fit(ObjectFit::Fill)
}

/// RGBA buffer → GPUI image (which expects BGRA).
pub fn to_render_image(mut rgba: RgbaImage) -> Arc<RenderImage> {
    for p in rgba.chunks_exact_mut(4) {
        p.swap(0, 2);
    }
    Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(rgba)]))
}
