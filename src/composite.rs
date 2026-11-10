//! Builds the ffmpeg video graph for the styled canvas, matching `canvas_preview` exactly.
//! Bottom to top: backdrop+shadow → cropped screen (with the cursor) at the screen rect →
//! split webcam region → rounded-corner overlay → floating webcam (frame sprite + masked video).

use crate::camera::CameraPath;
use crate::canvas_preview::px_per_pt;
use crate::layout::{Layout, Rect};
use crate::recorder::{FPS, Recording};
use crate::style::{self, Style};
use anyhow::{Context as _, Result};
use std::fmt::Write as _;
use std::path::PathBuf;

/// Inputs and graph for one export. Stills are looped inputs 1.. (in order), then `videos`
/// as plain inputs; the `-filter_complex` graph reads `[0:v]` (the raw recording) and those,
/// and ends in `[v]`.
pub struct Composite {
    pub stills: Vec<PathBuf>,
    /// The first still is a video (an animated background), not a looped image.
    pub bg_video: bool,
    pub videos: Vec<PathBuf>,
    /// Caption image sequence (an ffconcat list), added as the last input.
    pub captions: Option<PathBuf>,
    /// Click ripple image sequence (an ffconcat list), added after the captions.
    pub ripples: Option<PathBuf>,
    pub filter: String,
}

/// Writes the click ripples as a timed image sequence (ffconcat list) plus a sendcmd file
/// that moves the overlay to each click. Returns the list path and the sprite size.
fn ripple_sequence(rec: &Recording, style: &Style, fps: u32, cut: Option<&crate::editing::Cut>) -> Result<Option<(PathBuf, u32)>> {
    let cs = style.click;
    // Each click with the time its ripple must be gone by (the next cut, when edited).
    let all: Vec<(crate::clicks::Click, f64)> = match cut {
        Some(c) => c.clicks.clone(),
        None => crate::clicks::clicks(&rec.samples).into_iter().map(|c| (c, f64::INFINITY)).collect(),
    };
    let (clicks, ends): (Vec<_>, Vec<f64>) = all
        .into_iter()
        .filter(|(c, _)| c.x >= 0.0 && c.y >= 0.0 && c.x < rec.width as f64 && c.y < rec.height as f64)
        .unzip();
    if !cs.enabled || clicks.is_empty() {
        return Ok(None);
    }
    let dir = rec.dir.join("ripples");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let diameter = cs.size * rec.px_per_pt as f32;
    let frames = ((cs.duration * fps as f64).ceil() as usize).max(1);
    let mut files = Vec::with_capacity(frames);
    let mut size = 0;
    for k in 0..frames {
        let img = crate::clicks::sprite(cs.color, diameter, k as f32 / frames as f32);
        size = img.width();
        let p = dir.join(format!("r{k:03}.png"));
        img.save(&p)?;
        files.push(p);
    }
    let blank = dir.join("blank.png");
    image::RgbaImage::new(size, size).save(&blank)?;
    let mut list = String::from("ffconcat version 1.0\n");
    let mut cmd = String::new();
    let mut push = |f: &PathBuf, d: f64| {
        if d > 0.0005 {
            let _ = write!(list, "file '{}'\nduration {d:.4}\n", f.display());
        }
    };
    let mut t = 0.0;
    let half = size as f64 / 2.0;
    for (i, c) in clicks.iter().enumerate() {
        let start = c.t.max(t);
        let end = (c.t + cs.duration).min(ends[i]).min(clicks.get(i + 1).map_or(f64::INFINITY, |n| n.t)).min(rec.duration);
        if end <= start {
            continue;
        }
        push(&blank, start - t);
        let _ = writeln!(cmd, "{start:.4} overlay@rip x {}, overlay@rip y {};", (c.x - half).round(), (c.y - half).round());
        let mut at = start;
        for f in &files {
            if at >= end {
                break;
            }
            let d = (1.0 / fps as f64).min(end - at);
            push(f, d);
            at += d;
        }
        t = end;
    }
    push(&blank, (rec.duration - t).max(0.04));
    let _ = writeln!(list, "file '{}'", blank.display());
    let path = rec.dir.join("ripples.txt");
    std::fs::write(&path, list)?;
    std::fs::write(rec.dir.join("ripple.cmd"), if cmd.is_empty() { "0 overlay@rip x -10000;\n".to_string() } else { cmd })?;
    Ok(Some((path, size)))
}

/// Brightness of the blurred fill behind a zoomed-out split webcam (the preview matches it).
pub const DIM: f32 = 0.7;

/// Cover-fit crop of a `src` frame to the aspect of `dst`: (w, h, x, y), even sizes.
pub fn cover_crop(src: (u32, u32), dst: (u32, u32)) -> (u32, u32, u32, u32) {
    let (sw, sh) = (src.0 as f64, src.1 as f64);
    let a = dst.0 as f64 / dst.1.max(1) as f64;
    let (w, h) = if sw / sh > a { (sh * a, sh) } else { (sw, sw / a) };
    let even = |v: f64, max: u32| (((v / 2.0).floor() as u32) * 2).clamp(2, max);
    let (w, h) = (even(w, src.0), even(h, src.1));
    (w, h, (src.0 - w) / 2, (src.1 - h) / 2)
}

/// Renders an animated background for every output frame (timeline time `i / fps`) at low
/// resolution into a lossless-ish video; the compositor upscales it.
fn write_background_video(a: &crate::backgrounds::Animated, canvas: (u32, u32), fps: u32, duration: f64, out: &std::path::Path) -> Result<()> {
    use std::io::Write as _;
    let (w, h) = crate::backgrounds::render::low_size(canvas.0, canvas.1);
    let n = ((duration * fps as f64).ceil() as usize).max(1) + 1;
    let mut child = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-v", "error", "-nostdin", "-y", "-f", "rawvideo", "-pix_fmt", "rgb24"])
        .args(["-s", &format!("{w}x{h}"), "-r", &fps.to_string(), "-i", "-"])
        .args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "6", "-pix_fmt", "yuv444p"])
        .arg(out)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .context("Could not start ffmpeg for the background")?;
    let mut stdin = child.stdin.take().unwrap();
    for i in 0..n {
        let frame = crate::backgrounds::render::animated_frame(a, w, h, i as f64 / fps as f64);
        stdin.write_all(frame.as_raw())?;
    }
    drop(stdin);
    let status = child.wait()?;
    anyhow::ensure!(status.success(), "Could not render the animated background");
    Ok(())
}

/// Writes the stills and sendcmd files into `rec.dir` (ffmpeg must run with `rec.dir` as
/// its working directory) and returns the graph. `layout` gives the canvas and webcam
/// placement; `mirror` flips the webcam; `fps` is the output frame rate.
pub fn build(
    rec: &Recording,
    path: &CameraPath,
    style: &Style,
    layout: &Layout,
    mirror: bool,
    fps: u32,
    captions: Option<(&crate::captions::Captions, &crate::captions::render::CaptionStyle)>,
    cut: Option<&crate::editing::Cut>,
) -> Result<Composite> {
    let edit = cut.map(|c| &c.edit);
    let webcam = rec.webcam.as_ref();
    let frame = layout.frame(style, webcam.is_some());
    let Rect { x: px0, y: py0, w: iw, h: ih } = frame.screen;

    let backdrop = style::render_backdrop(style, &frame, 1.0);
    // The cursor is drawn onto the screen recording before the camera crop, so it zooms
    // with the content. Sized in video pixels.
    let cursor = style::render_cursor(px_per_pt(rec) * style.cursor_scale);

    let save = |name: &str, img: image::DynamicImage| -> Result<PathBuf> {
        let p = rec.dir.join(name);
        img.save(&p).with_context(|| format!("Could not write {}", p.display()))?;
        Ok(p)
    };
    // An animated background is a video of its own (inputs 1, with the grain and shadow
    // overlay as inputs 4 and 5, and the corner mask as input 2); a static one an image.
    let animated = match &style.bg {
        crate::backgrounds::Background::Animated(a) if !a.frozen => Some(a.clone()),
        _ => None,
    };
    let mut stills = match &animated {
        Some(a) => {
            let video = rec.dir.join("canvas-bg.mp4");
            write_background_video(a, frame.canvas, fps, rec.duration, &video)?;
            let mask = image::GrayImage::from_fn(backdrop.corners.width(), backdrop.corners.height(), |x, y| image::Luma([backdrop.corners.get_pixel(x, y)[3]]));
            let (cw, ch) = frame.canvas;
            vec![
                video,
                save("canvas-corner-mask.png", mask.into())?,
                save("canvas-cursor.png", cursor.image.into())?,
                save("canvas-grain.png", crate::backgrounds::render::grain_layer(cw, ch, a.grain).into())?,
                save("canvas-overlay.png", backdrop.overlay.into())?,
            ]
        }
        None => vec![
            save("canvas-bg.png", image::DynamicImage::ImageRgba8(backdrop.bg).to_rgb8().into())?,
            save("canvas-corners.png", backdrop.corners.into())?,
            save("canvas-cursor.png", cursor.image.into())?,
        ],
    };
    let mut videos = Vec::new();

    // Camera crop per source frame (the camera path runs at the recording's FPS). The crop
    // size changes while zoomed; the scale after it keeps the output size fixed.
    let mut cam = String::new();
    let mut last = None;
    for (i, &(x, y, w, h)) in path.frames.iter().enumerate() {
        if last == Some((x, y, w, h)) {
            continue;
        }
        last = Some((x, y, w, h));
        let _ = writeln!(cam, "{:.4} crop@c w {w}, crop@c h {h}, crop@c x {x}, crop@c y {y};", i as f64 / FPS as f64);
    }
    std::fs::write(rec.dir.join("camera.cmd"), cam)?;

    // Cursor per output frame, from the 120 Hz cursor track (smoother than the video).
    let hot = cursor.hotspot as f64;
    let n = ((rec.duration * fps as f64).ceil() as usize).max(1);
    let mut cur = String::new();
    let mut first = None;
    for i in 0..n {
        let t = i as f64 / fps as f64;
        // Hidden while the cursor is outside the recorded area (e.g. outside a region).
        let pos = crate::camera::cursor_at(&rec.samples, t)
            .filter(|&(x, y)| x >= 0.0 && y >= 0.0 && x < rec.width as f64 && y < rec.height as f64)
            .map(|(x, y)| ((x - hot).round() as i64, (y - hot).round() as i64))
            .unwrap_or((-10_000, -10_000));
        first.get_or_insert(pos);
        let _ = writeln!(cur, "{t:.4} overlay@cur x {}, overlay@cur y {};", pos.0, pos.1);
    }
    std::fs::write(rec.dir.join("cursor.cmd"), cur)?;
    let (fx, fy) = first.unwrap_or((-10_000, -10_000));

    let (x0, y0, w0, h0) = path.frames.first().copied().unwrap_or((0, 0, path.crop_w, path.crop_h));
    // Click ripples go on the screen video right after the cursor (above it), before the crop.
    let ripples = ripple_sequence(rec, style, fps, cut)?;
    let ripple_chain = match &ripples {
        Some(_) => ",sendcmd=f=ripple.cmd[curd];[@RIP@:v]fps={fps},format=rgba[ripv];\
                    [curd][ripv]overlay@rip=x=-10000:y=-10000:eof_action=pass:format=auto"
            .replace("{fps}", &fps.to_string()),
        None => String::new(),
    };
    // The raw screen, cut to the edit's kept runs (just rebased to 0 when unedited).
    let screen_in = crate::editing::filter::trimmed("0:v", edit, "s");
    let mut filter = format!(
        "{screen_in}fps={fps},sendcmd=f=cursor.cmd[src];\
         [src][3:v]overlay@cur=x={fx}:y={fy}:shortest=1{ripple_chain},\
         sendcmd=f=camera.cmd,crop@c=w={w0}:h={h0}:x={x0}:y={y0},\
         scale={iw}:{ih}:flags=lanczos,setsar=1[cam];\
         @BG@\
         [bg][cam]overlay=x={px0}:y={py0}:shortest=1[a]",
    );
    let (canvas_w, canvas_h) = frame.canvas;
    let bg_chain = if animated.is_some() {
        // Upscale the low-res frames bilinearly, grain after, then the shadow; the corners
        // are the same background through the corner mask.
        format!(
            "[1:v]setpts=PTS-STARTPTS,fps={fps},scale={canvas_w}:{canvas_h}:flags=bilinear,format=rgba[bga];\
             [bga][4:v]overlay=format=auto:shortest=1[bgg];[bgg]split[bgc][bgs];\
             [bgs][5:v]overlay=format=auto:shortest=1,format=yuv444p[bg];\
             [2:v]format=gray[cmask];[bgc][cmask]alphamerge[corn];"
        )
    } else {
        "[1:v]format=yuv444p[bg];".to_string()
    };
    filter = filter.replace("@BG@", &bg_chain);

    // Webcam: matched to output frames by timestamp (the fps filter duplicates or drops
    // frames), cover-fit to its rect, shown only while it has real frames.
    let cam_layer = webcam.zip(frame.cam);
    let mut top = "a".to_string();
    let mut cam_in = None;
    if let Some((track, cf)) = cam_layer {
        let r = cf.inner();
        let flip = if mirror { ",hflip" } else { "" };
        let enable = track.enable_expr(rec.duration).map(|e| format!(":enable='{e}'")).unwrap_or_default();
        let idx = 1 + stills.len() + if cf.floating { 2 } else { 0 };
        let src = (track.width, track.height);
        let cam_src = crate::editing::filter::trimmed(&format!("{idx}:v"), edit, "w");
        let _ = write!(filter, ";{cam_src}fps={fps}{flip}");
        if cf.floating {
            let (cw, ch, cx, cy) = cover_crop(src, (r.w, r.h));
            let _ = write!(
                filter,
                ",crop={cw}:{ch}:{cx}:{cy},scale={}:{}:flags=lanczos,setsar=1,format=yuva444p[wc]",
                r.w, r.h
            );
        } else {
            let fit = cf.fit(src);
            let (cw, ch, cx, cy) = fit.crop;
            let d = fit.dst;
            if fit.fills {
                // Zoomed out: the framed webcam over a blurred, dimmed cover-fill of itself.
                // The blur is a downscale to a few pixels and back (what the preview does).
                let (bw, bh, bx, by) = cover_crop(src, (r.w, r.h));
                let tiny = (24.0 * r.w as f64 / r.w.max(r.h) as f64, 24.0 * r.h as f64 / r.w.max(r.h) as f64);
                let (tw, th) = ((tiny.0.round() as u32).max(2), (tiny.1.round() as u32).max(2));
                let _ = write!(
                    filter,
                    ",split=2[wsrc][wbg];\
                     [wbg]crop={bw}:{bh}:{bx}:{by},scale={tw}:{th}:flags=area,\
                     scale={}:{}:flags=bilinear,colorchannelmixer=rr={DIM}:gg={DIM}:bb={DIM},setsar=1[wblur];\
                     [wsrc]crop={cw}:{ch}:{cx}:{cy},scale={}:{}:flags=lanczos,setsar=1[wfit];\
                     [wblur][wfit]overlay=x={}:y={}[wc];\
                     [a][wc]overlay=x={}:y={}{enable}[aw]",
                    r.w, r.h, d.w, d.h, d.x - r.x, d.y - r.y, r.x, r.y
                );
            } else {
                let _ = write!(
                    filter,
                    ",crop={cw}:{ch}:{cx}:{cy},scale={}:{}:flags=lanczos,setsar=1[wc];\
                     [a][wc]overlay=x={}:y={}{enable}[aw]",
                    d.w, d.h, d.x, d.y
                );
            }
            top = "aw".into();
        }
        cam_in = Some((track.path.clone(), cf, enable, idx));
    }
    let corners_in = if animated.is_some() { "[corn]" } else { "[2:v]" };
    let _ = write!(filter, ";[{top}]{corners_in}overlay=0:0:shortest=1[b]");
    let mut top = "b".to_string();
    // Captions: above the screen and cursor, below a floating webcam. The input index is
    // known once every still and video is in (filled in at the end).
    let caption_seq = match captions {
        Some((caps, style)) => crate::captions::render::export_frames(caps, style, &frame, rec.duration, &rec.dir)?,
        None => None,
    };
    if let Some((_, area)) = &caption_seq {
        let _ = write!(
            filter,
            ";[@CAP@:v]fps={fps},format=rgba[capv];[b][capv]overlay=x={}:y={}:eof_action=pass:format=auto[bk]",
            area.x, area.y
        );
        top = "bk".into();
    }
    if let Some((video, cf, enable, idx)) = cam_in {
        if cf.floating {
            let sprite = style::render_cam_frame(&cf, layout.cam_border_color, layout.cam_shadow, 1.0);
            stills.push(save("canvas-cam-frame.png", sprite.image.into())?);
            stills.push(save("canvas-cam-mask.png", style::render_cam_mask(&cf).into())?);
            let (sprite_in, mask_in) = (idx - 2, idx - 1);
            let r = cf.inner();
            let sx = cf.rect.x as i32 + sprite.offset.0;
            let sy = cf.rect.y as i32 + sprite.offset.1;
            let _ = write!(
                filter,
                ";[{mask_in}:v]format=gray[wm];[wc][wm]alphamerge[wa];\
                 [{top}][{sprite_in}:v]overlay=x={sx}:y={sy}:shortest=1{enable}[bf];\
                 [bf][wa]overlay=x={}:y={}{enable}[bc]",
                r.x, r.y
            );
            top = "bc".into();
        }
        videos.push(video);
    }
    let _ = write!(filter, ";[{top}]format=yuv420p[v]");
    let cap_idx = 1 + stills.len() + videos.len();
    let rip_idx = cap_idx + caption_seq.is_some() as usize;
    let filter = filter.replace("@CAP@", &cap_idx.to_string()).replace("@RIP@", &rip_idx.to_string());
    Ok(Composite { stills, bg_video: animated.is_some(), videos, captions: caption_seq.map(|(p, _)| p), ripples: ripples.map(|(p, _)| p), filter })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor::Sample;
    use std::process::Command;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU32;

    /// Exports a synthetic gray screen with a cursor track and checks each layer in a frame:
    /// background at the canvas edge, screen content in the middle, backdrop in the rounded
    /// corner, and the dark cursor sprite at the expected spot.
    #[test]
    fn styled_export_layers() {
        let dir = std::env::temp_dir().join(format!("clipper-composite-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let raw = dir.join("raw.mp4");
        assert!(Command::new("ffmpeg")
            .args(["-hide_banner", "-v", "error", "-y", "-f", "lavfi", "-i", "color=0x808080:s=1600x1000:r=30:d=1"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&raw)
            .status()
            .unwrap()
            .success());
        // Cursor parked at the screen center.
        let samples = (0..30).map(|i| Sample { t: i as f64 / 30.0, x: 800.0, y: 500.0, down: false }).collect();
        let rec = Recording { dir: dir.clone(), raw, width: 1600, height: 1000, duration: 1.0, samples, webcam: None, px_per_pt: 1.0 };
        let style = Style { bg: crate::backgrounds::Background::Solid { color: [20, 200, 60], grain: 0.0 }, shadow_opacity: 0.0, ..Style::default() };
        let output = (1080, 1920);
        let layout = Layout::default();
        let cfg = crate::camera::FollowConfig::default();
        let aspect = layout.frame(&style, false).screen.aspect();
        let path = crate::camera::compute_with(&rec.samples, 1600, 1000, 1.0, aspect, cfg);
        // Cursor parked at the center: the camera centers on it.
        assert!(path.frames.iter().all(|&(x, ..)| x == (1600 - path.crop_w) / 2));
        let out = crate::export::export(&rec, &path, &style, &layout, true, 30, &dir, Arc::new(AtomicU32::new(0)), None, false).unwrap();

        let png = dir.join("frame.png");
        assert!(Command::new("ffmpeg")
            .args(["-hide_banner", "-v", "error", "-y", "-ss", "0.5", "-i"])
            .arg(&out)
            .args(["-frames:v", "1"])
            .arg(&png)
            .status()
            .unwrap()
            .success());
        let f = image::open(&png).unwrap().to_rgb8();
        assert_eq!(f.dimensions(), output);
        let near = |p: &image::Rgb<u8>, c: [u8; 3]| p.0.iter().zip(c).all(|(a, b)| (*a as i32 - b as i32).abs() < 14);
        assert!(near(f.get_pixel(10, 10), [20, 200, 60]), "background {:?}", f.get_pixel(10, 10));
        assert!(near(f.get_pixel(48 + 3, 48 + 3), [20, 200, 60]), "rounded corner {:?}", f.get_pixel(51, 51));
        assert!(near(f.get_pixel(48 + 40, 48 + 200), [128, 128, 128]), "content {:?}", f.get_pixel(88, 248));
        // Hotspot maps to the center of the inner rect; the arrow body sits just below-right.
        let (cx, cy) = (540, 960);
        let tip = f.get_pixel(cx + 2, cy + 8);
        assert!(tip.0.iter().all(|&v| v < 60), "cursor {tip:?}");
        println!("frame: {}", png.display());
    }

    /// A 2× zoom region doubles the size of on-screen content in the export, and the
    /// output keeps its size while the crop shrinks.
    #[test]
    fn zoom_region_magnifies_export() {
        let dir = std::env::temp_dir().join(format!("clipper-zoom-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let raw = dir.join("raw.mp4");
        // Gray screen with a 100 px red square at the center.
        assert!(Command::new("ffmpeg")
            .args(["-hide_banner", "-v", "error", "-y", "-f", "lavfi", "-i", "color=0x808080:s=1600x1000:r=30:d=3"])
            .args(["-vf", "drawbox=x=750:y=450:w=100:h=100:c=red:t=fill"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&raw)
            .status()
            .unwrap()
            .success());
        // Cursor parked off the square so the sprite doesn't cover the measured row.
        let samples = (0..90).map(|i| Sample { t: i as f64 / 30.0, x: 800.0, y: 700.0, down: false }).collect();
        let rec = Recording { dir: dir.clone(), raw, width: 1600, height: 1000, duration: 3.0, samples, webcam: None, px_per_pt: 1.0 };
        let style = Style { bg: crate::backgrounds::Background::Solid { color: [0xf2, 0xf2, 0xf4], grain: 0.0 }, shadow_opacity: 0.0, ..Style::default() };
        let layout = Layout::default();
        let z = crate::zoom::ZoomRegion { start: 1.0, end: 3.0, depth: 2.0, manual: true, focus: (0.5, 0.5) };
        let path = crate::camera::compute_zoomed(
            &rec.samples,
            1600,
            1000,
            3.0,
            layout.frame(&style, false).screen.aspect(),
            1.0,
            crate::camera::FollowConfig::default(),
            &[z],
            &crate::zoom::ZoomMotion::default(),
        );
        let out = crate::export::export(&rec, &path, &style, &layout, true, 30, &dir, Arc::new(AtomicU32::new(0)), None, false).unwrap();
        assert_eq!(crate::recorder::probe(&out).unwrap().0, 1080);

        // Width of the red run across the canvas center at time `t`.
        let red_width = |t: &str| {
            let png = dir.join(format!("z{t}.png"));
            assert!(Command::new("ffmpeg")
                .args(["-hide_banner", "-v", "error", "-y", "-ss", t, "-i"])
                .arg(&out)
                .args(["-frames:v", "1"])
                .arg(&png)
                .status()
                .unwrap()
                .success());
            let f = image::open(&png).unwrap().to_rgb8();
            (0..f.width()).filter(|&x| {
                let p = f.get_pixel(x, 960).0;
                p[0] > 170 && p[1] < 90 && p[2] < 90
            }).count() as f64
        };
        let (before, zoomed) = (red_width("0.5"), red_width("2.0"));
        println!("red run: {before} px → {zoomed} px");
        assert!(before > 100.0, "square not visible before the zoom ({before} px)");
        assert!((zoomed / before - 2.0).abs() < 0.12, "zoom ratio {:.2}", zoomed / before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every floating placement and every split layout, for both ratios: the export is the
    /// canvas size, the webcam (left half red, right half blue, mirrored → blue on the left)
    /// fills its rect without stretching, the screen content fills its rect, and a webcam
    /// span that ends mid-clip removes the overlay for the rest.
    #[test]
    fn webcam_layouts_export() {
        use crate::layout::{Placement, Ratio, Res};
        use crate::webcam::Track;
        let dir = std::env::temp_dir().join(format!("clipper-layouts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ff = |args: &[&str], out: &std::path::Path| {
            assert!(Command::new("ffmpeg").args(["-hide_banner", "-v", "error", "-y"]).args(args).arg(out).status().unwrap().success());
        };
        let raw = dir.join("raw.mp4");
        ff(&["-f", "lavfi", "-i", "color=0x808080:s=1600x1000:r=30:d=1", "-c:v", "libx264", "-pix_fmt", "yuv420p"], &raw);
        let cam = dir.join("webcam.mp4");
        ff(
            &[
                "-f", "lavfi", "-i", "color=red:s=640x360:r=30:d=1",
                "-vf", "drawbox=x=320:y=0:w=320:h=360:c=blue:t=fill",
                "-c:v", "libx264", "-pix_fmt", "yuv420p",
            ],
            &cam,
        );
        // Cursor far in a corner so the sprite never covers a probed pixel.
        let samples: Vec<Sample> = (0..30).map(|i| Sample { t: i as f64 / 30.0, x: 5.0, y: 5.0, down: false }).collect();
        let full = Track { path: cam.clone(), width: 640, height: 360, spans: vec![(0.0, 1.0)] };
        let style = Style { bg: crate::backgrounds::Background::Solid { color: [20, 200, 60], grain: 0.0 }, shadow_opacity: 0.0, ..Style::default() };
        let near = |p: [u8; 3], c: [u8; 3]| p.iter().zip(c).all(|(a, b)| (*a as i32 - b as i32).abs() < 40);
        let mut n = 0;
        for ratio in Ratio::ALL {
            for placement in Placement::options(ratio).into_iter().filter(|p| *p != Placement::None) {
                let layout = Layout { ratio, res: Res::P720, placement, ..Layout::default() };
                let spans = if n % 5 == 4 { vec![(0.0, 0.5)] } else { vec![(0.0, 1.0)] };
                let lost = spans[0].1 < 1.0;
                let rec = Recording {
                    dir: dir.clone(),
                    raw: raw.clone(),
                    width: 1600,
                    height: 1000,
                    duration: 1.0,
                    samples: samples.clone(),
                    webcam: Some(Track { spans, ..full.clone() }),
                    px_per_pt: 1.0,
                };
                let frame = layout.frame(&style, true);
                let path = crate::camera::compute_with(&rec.samples, 1600, 1000, 1.0, frame.screen.aspect(), crate::camera::FollowConfig::default());
                assert!(((path.crop_w as f64 / path.crop_h as f64) / frame.screen.aspect() - 1.0).abs() < 0.01);
                let out = crate::export::export(&rec, &path, &style, &layout, true, 30, &dir, Arc::new(AtomicU32::new(0)), None, false).unwrap();
                let png = dir.join(format!("layout-{n}.png"));
                ff(&["-ss", "0.8", "-i", out.to_str().unwrap(), "-frames:v", "1"], &png);
                let f = image::open(&png).unwrap().to_rgb8();
                assert_eq!(f.dimensions(), layout.canvas(), "{placement:?}");
                let px = |x: u32, y: u32| f.get_pixel(x, y).0;
                let cf = frame.cam.unwrap();
                let r = cf.inner();
                // Mirrored: blue on the left, red on the right of the webcam rect.
                let (lx, rx, cy) = (r.x + r.w / 4, r.x + r.w * 3 / 4, r.y + r.h / 2);
                if lost && cf.floating {
                    assert!(near(px(lx, cy), [128, 128, 128]) || near(px(lx, cy), [20, 200, 60]), "{placement:?} lost: {:?}", px(lx, cy));
                } else if lost {
                    assert!(near(px(lx, cy), style::CAM_EMPTY), "{placement:?} lost split: {:?}", px(lx, cy));
                } else {
                    assert!(near(px(lx, cy), [0, 0, 255]), "{placement:?} webcam left {:?}", px(lx, cy));
                    assert!(near(px(rx, cy), [255, 0, 0]), "{placement:?} webcam right {:?}", px(rx, cy));
                }
                // Screen content in its rect, away from the webcam.
                let s = frame.screen;
                let probe = [(s.x + s.w / 2, s.y + s.h / 2), (s.x + s.w / 5, s.y + s.h / 5), (s.x + s.w * 4 / 5, s.y + s.h * 4 / 5)]
                    .into_iter()
                    .find(|&(x, y)| {
                        let c = cf.rect;
                        !(x + 20 >= c.x && x <= c.x + c.w + 20 && y + 20 >= c.y && y <= c.y + c.h + 20)
                    })
                    .unwrap();
                assert!(near(px(probe.0, probe.1), [128, 128, 128]), "{placement:?} screen {:?} at {probe:?}", px(probe.0, probe.1));
                let _ = std::fs::remove_file(&out);
                n += 1;
            }
        }
        assert_eq!(n, 20);
        // CLIPPER_KEEP=1 keeps the frames for a look.
        if std::env::var_os("CLIPPER_KEEP").is_none() {
            let _ = std::fs::remove_dir_all(&dir);
        } else {
            println!("frames in {}", dir.display());
        }
    }

    /// A split webcam shrunk to 25%: the camera is zoomed out (the drawn video is smaller
    /// than the region and keeps its aspect) and a dimmed blur of it fills the rest.
    #[test]
    fn small_split_webcam_zooms_out_in_export() {
        use crate::layout::{MIN_SPLIT, Placement, Ratio, Res, Side};
        use crate::webcam::Track;
        let dir = std::env::temp_dir().join(format!("clipper-zoomout-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ff = |args: &[&str], out: &std::path::Path| {
            assert!(Command::new("ffmpeg").args(["-hide_banner", "-v", "error", "-y"]).args(args).arg(out).status().unwrap().success());
        };
        let raw = dir.join("raw.mp4");
        ff(&["-f", "lavfi", "-i", "color=0x808080:s=1600x1000:r=30:d=1", "-c:v", "libx264", "-pix_fmt", "yuv420p"], &raw);
        let cam = dir.join("webcam.mp4");
        ff(
            &[
                "-f", "lavfi", "-i", "color=red:s=640x360:r=30:d=1",
                "-vf", "drawbox=x=320:y=0:w=320:h=360:c=blue:t=fill",
                "-c:v", "libx264", "-pix_fmt", "yuv420p",
            ],
            &cam,
        );
        let samples: Vec<Sample> = (0..30).map(|i| Sample { t: i as f64 / 30.0, x: 5.0, y: 5.0, down: false }).collect();
        let style = Style { bg: crate::backgrounds::Background::Solid { color: [20, 200, 60], grain: 0.0 }, shadow_opacity: 0.0, ..Style::default() };
        let near = |p: [u8; 3], c: [u8; 3]| p.iter().zip(c).all(|(a, b)| (*a as i32 - b as i32).abs() < 40);
        for (ratio, side) in [(Ratio::Portrait, Side::Bottom), (Ratio::Landscape, Side::Left)] {
            let layout = Layout { ratio, res: Res::P720, placement: Placement::Split(side), split: MIN_SPLIT, ..Layout::default() };
            let rec = Recording {
                dir: dir.clone(),
                raw: raw.clone(),
                width: 1600,
                height: 1000,
                duration: 1.0,
                samples: samples.clone(),
                webcam: Some(Track { path: cam.clone(), width: 640, height: 360, spans: vec![(0.0, 1.0)] }),
                px_per_pt: 1.0,
            };
            let frame = layout.frame(&style, true);
            let cf = frame.cam.unwrap();
            let fit = cf.fit((640, 360));
            assert!(fit.fills, "{ratio:?}");
            let path = crate::camera::compute_with(&rec.samples, 1600, 1000, 1.0, frame.screen.aspect(), crate::camera::FollowConfig::default());
            let out = crate::export::export(&rec, &path, &style, &layout, true, 30, &dir, Arc::new(AtomicU32::new(0)), None, false).unwrap();
            let png = dir.join(format!("zoomout-{ratio:?}.png"));
            ff(&["-ss", "0.5", "-i", out.to_str().unwrap(), "-frames:v", "1"], &png);
            let f = image::open(&png).unwrap().to_rgb8();
            let px = |x: u32, y: u32| f.get_pixel(x, y).0;
            let (r, d) = (cf.inner(), fit.dst);
            // Mirrored webcam inside its drawn rect.
            let cy = d.y + d.h / 2;
            assert!(near(px(d.x + d.w / 4, cy), [0, 0, 255]), "{ratio:?} left {:?}", px(d.x + d.w / 4, cy));
            assert!(near(px(d.x + d.w * 3 / 4, cy), [255, 0, 0]), "{ratio:?} right {:?}", px(d.x + d.w * 3 / 4, cy));
            // Outside it but inside the region: the dimmed blur (never full brightness).
            let (bx, by) = if d.w < r.w { (r.x + (d.x - r.x) / 2, cy) } else { (d.x + d.w / 2, r.y + (d.y - r.y) / 2) };
            let b = px(bx, by);
            assert!(b.iter().all(|&v| v < 215), "{ratio:?} fill {b:?} at ({bx},{by})");
            assert!(!near(b, [20, 200, 60]), "{ratio:?} fill shows the background {b:?}");
            println!("{ratio:?}: region {r:?}, video {d:?}, crop {:?}", fit.crop);
        }
        if std::env::var_os("CLIPPER_KEEP").is_none() {
            let _ = std::fs::remove_dir_all(&dir);
        } else {
            println!("frames in {}", dir.display());
        }
    }

    /// Burned-in captions: shown only during their time, inside the caption area, above the
    /// screen; in a split layout never over the webcam. The .srt lands next to the MP4.
    #[test]
    fn captions_burn_in_and_srt() {
        use crate::captions::render::{Area, CaptionStyle, Position};
        use crate::captions::{Caption, Captions, Word};
        use crate::layout::{Placement, Ratio, Res, Side};
        let dir = std::env::temp_dir().join(format!("clipper-captions-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let raw = dir.join("raw.mp4");
        assert!(Command::new("ffmpeg")
            .args(["-hide_banner", "-v", "error", "-y", "-f", "lavfi", "-i", "color=0x808080:s=1600x1000:r=30:d=3"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&raw)
            .status()
            .unwrap()
            .success());
        let words = vec![
            Word { start: 1.0, end: 1.4, text: "HELLO".into() },
            Word { start: 1.5, end: 2.0, text: "WORLD".into() },
        ];
        let caps = Captions { lines: vec![Caption::from_words(words, 32)], ..Default::default() };
        // Green text, no outline: easy to find against the gray screen and green-free backdrop.
        let cstyle = CaptionStyle {
            color: [0, 255, 0],
            highlight: false,
            outline: 0.0,
            area: Area::Canvas,
            position: Position::Bottom,
            size: 90.0,
            ..CaptionStyle::default()
        };
        let style = Style { bg: crate::backgrounds::Background::Solid { color: [40, 40, 120], grain: 0.0 }, shadow_opacity: 0.0, ..Style::default() };
        for (ratio, placement) in [(Ratio::Portrait, Placement::None), (Ratio::Portrait, Placement::Split(Side::Bottom))] {
            let layout = Layout { ratio, res: Res::P720, placement, ..Layout::default() };
            let cam = dir.join("webcam.mp4");
            assert!(Command::new("ffmpeg")
                .args(["-hide_banner", "-v", "error", "-y", "-f", "lavfi", "-i", "color=red:s=640x360:r=30:d=3"])
                .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
                .arg(&cam)
                .status()
                .unwrap()
                .success());
            let webcam = (placement != Placement::None)
                .then(|| crate::webcam::Track { path: cam.clone(), width: 640, height: 360, spans: vec![(0.0, 3.0)] });
            let rec = Recording { dir: dir.clone(), raw: raw.clone(), width: 1600, height: 1000, duration: 3.0, samples: Vec::new(), webcam, px_per_pt: 1.0 };
            let frame = layout.frame(&style, rec.webcam.is_some());
            let path = crate::camera::compute_with(&rec.samples, 1600, 1000, 3.0, frame.screen.aspect(), crate::camera::FollowConfig::default());
            let out = crate::export::export(&rec, &path, &style, &layout, true, 30, &dir, Arc::new(AtomicU32::new(0)), Some((&caps, &cstyle)), true).unwrap();
            let srt = std::fs::read_to_string(out.with_extension("srt")).unwrap();
            assert!(srt.starts_with("1\n00:00:01,000 --> 00:00:02,000\nHELLO WORLD"), "{srt}");
            let green_in = |t: &str| {
                let png = dir.join(format!("cap-{t}.png"));
                assert!(Command::new("ffmpeg")
                    .args(["-hide_banner", "-v", "error", "-y", "-ss", t, "-i"])
                    .arg(&out)
                    .args(["-frames:v", "1"])
                    .arg(&png)
                    .status()
                    .unwrap()
                    .success());
                let f = image::open(&png).unwrap().to_rgb8();
                let mut pts = Vec::new();
                for (x, y, p) in f.enumerate_pixels() {
                    if p[1] > 200 && p[0] < 90 && p[2] < 90 {
                        pts.push((x, y));
                    }
                }
                pts
            };
            assert!(green_in("0.5").is_empty(), "{placement:?}: caption before it starts");
            let during = green_in("1.6");
            assert!(during.len() > 200, "{placement:?}: caption missing ({} px)", during.len());
            let area = crate::captions::render::area(&frame, &cstyle);
            for &(x, y) in &during {
                assert!(x >= area.x && x < area.x + area.w && y >= area.y && y < area.y + area.h, "{placement:?}: caption pixel {x},{y} outside {area:?}");
                if let Some(c) = frame.cam {
                    let r = c.rect;
                    assert!(!(x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h), "{placement:?}: caption over the webcam");
                }
            }
            assert!(green_in("2.6").is_empty(), "{placement:?}: caption after it ends");
            let _ = std::fs::remove_file(&out);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A click at the screen center at 1.0 s shows a ripple around the canvas center during
    /// its 0.4 s, and nothing before or after (rendered from the click log).
    #[test]
    fn click_ripple_in_export() {
        let dir = std::env::temp_dir().join(format!("clipper-ripple-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let raw = dir.join("raw.mp4");
        assert!(Command::new("ffmpeg")
            .args(["-hide_banner", "-v", "error", "-y", "-f", "lavfi", "-i", "color=0x808080:s=1600x1000:r=30:d=2"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&raw)
            .status()
            .unwrap()
            .success());
        // Cursor parked at the center, button down 1.0–1.1 s.
        let samples: Vec<Sample> = (0..240)
            .map(|i| {
                let t = i as f64 / 120.0;
                Sample { t, x: 800.0, y: 500.0, down: (1.0..1.1).contains(&t) }
            })
            .collect();
        let rec = Recording { dir: dir.clone(), raw, width: 1600, height: 1000, duration: 2.0, samples, webcam: None, px_per_pt: 2.0 };
        let style = Style {
            bg: crate::backgrounds::Background::Solid { color: [40, 40, 120], grain: 0.0 },
            shadow_opacity: 0.0,
            click: crate::clicks::ClickStyle { color: [0, 255, 0], size: 80.0, ..Default::default() },
            ..Style::default()
        };
        let layout = Layout::default();
        let frame = layout.frame(&style, false);
        let path = crate::camera::compute_with(&rec.samples, 1600, 1000, 2.0, frame.screen.aspect(), crate::camera::FollowConfig::default());
        let out = crate::export::export(&rec, &path, &style, &layout, true, 30, &dir, Arc::new(AtomicU32::new(0)), None, false).unwrap();
        let green = |t: &str| {
            let png = dir.join(format!("rip-{t}.png"));
            assert!(Command::new("ffmpeg")
                .args(["-hide_banner", "-v", "error", "-y", "-ss", t, "-i"])
                .arg(&out)
                .args(["-frames:v", "1"])
                .arg(&png)
                .status()
                .unwrap()
                .success());
            let f = image::open(&png).unwrap().to_rgb8();
            f.enumerate_pixels().filter(|(_, _, p)| p[1] > 150 && p[0] < 110 && p[2] < 110).map(|(x, y, _)| (x, y)).collect::<Vec<_>>()
        };
        assert!(green("0.8").is_empty(), "ripple before the click");
        let during = green("1.15");
        assert!(during.len() > 100, "no ripple ({} px)", during.len());
        // Centered on the canvas center (where the camera put the clicked point).
        let (sx, sy) = during.iter().fold((0u64, 0u64), |a, &(x, y)| (a.0 + x as u64, a.1 + y as u64));
        let (cx, cy) = (sx as f64 / during.len() as f64, sy as f64 / during.len() as f64);
        assert!((cx - 540.0).abs() < 25.0 && (cy - 960.0).abs() < 25.0, "ripple centered at {cx:.0},{cy:.0}");
        assert!(green("1.6").is_empty(), "ripple after its duration");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An animated background in the export is the renderer's frame at the same timeline
    /// time, at any frame rate, and it moves.
    #[test]
    fn animated_background_in_export() {
        let dir = std::env::temp_dir().join(format!("clipper-animbg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let raw = dir.join("raw.mp4");
        assert!(Command::new("ffmpeg")
            .args(["-hide_banner", "-v", "error", "-y", "-f", "lavfi", "-i", "color=0x808080:s=1600x1000:r=30:d=2"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&raw)
            .status()
            .unwrap()
            .success());
        let samples: Vec<Sample> = (0..240).map(|i| Sample { t: i as f64 / 120.0, x: 800.0, y: 500.0, down: false }).collect();
        let rec = Recording { dir: dir.clone(), raw, width: 1600, height: 1000, duration: 2.0, samples, webcam: None, px_per_pt: 2.0 };
        let mut anim = crate::backgrounds::presets::animated()[0].anim.clone();
        anim.period = 4.0;
        anim.grain = 0.0;
        let style = Style { bg: crate::backgrounds::Background::Animated(anim.clone()), shadow_opacity: 0.0, ..Style::default() };
        let layout = Layout::default();
        let frame = layout.frame(&style, false);
        let path = crate::camera::compute_with(&rec.samples, 1600, 1000, 2.0, frame.screen.aspect(), crate::camera::FollowConfig::default());
        let (cw, ch) = frame.canvas;
        // A spot in the padding (background only).
        let (px0, py0) = (12u32, ch / 2);
        let expected = |t: f64| {
            let (lw, lh) = crate::backgrounds::render::low_size(cw, ch);
            let low = crate::backgrounds::render::animated_frame(&anim, lw, lh, t);
            let up = image::imageops::resize(&low, cw, ch, image::imageops::FilterType::Triangle);
            *up.get_pixel(px0, py0)
        };
        for fps in [15u32, 30] {
            let out_dir = dir.join(format!("out{fps}"));
            std::fs::create_dir_all(&out_dir).unwrap();
            let out = crate::export::export(&rec, &path, &style, &layout, false, fps, &out_dir, Arc::new(AtomicU32::new(0)), None, false).unwrap();
            let got = |t: f64| {
                let png = out_dir.join(format!("f-{t}.png"));
                assert!(Command::new("ffmpeg")
                    .args(["-hide_banner", "-v", "error", "-y", "-ss", &format!("{t}"), "-i"])
                    .arg(&out)
                    .args(["-frames:v", "1"])
                    .arg(&png)
                    .status()
                    .unwrap()
                    .success());
                *image::open(&png).unwrap().to_rgb8().get_pixel(px0, py0)
            };
            for t in [0.0, 1.0] {
                let (g, e) = (got(t), expected(t));
                let d = (0..3).map(|k| (g[k] as i32 - e[k] as i32).abs()).max().unwrap();
                assert!(d <= 14, "fps {fps}, t {t}: export {g:?} vs renderer {e:?}");
            }
        }
        let (a, b) = (expected(0.0), expected(1.0));
        assert!((0..3).any(|k| (a[k] as i32 - b[k] as i32).abs() > 4), "the background moves");
        let _ = std::fs::remove_dir_all(&dir);
    }
}