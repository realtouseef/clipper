//! Export formats: MP4, GIF and animated WebP from one recording, in one run.
//!
//! Everything goes through the same compositor: the recording is rendered once (background,
//! webcam, captions, click ripples, camera follow). That render is the MP4 when MP4 is
//! picked, or a near-lossless master otherwise; GIF and WebP are made from it, so all three
//! look identical. GIF and WebP have no audio.
//!
//! - GIF: ffmpeg only, two passes (palettegen, then paletteuse), lanczos scaling.
//! - WebP: this ffmpeg has no libwebp encoder, so frames are decoded by ffmpeg (lanczos
//!   scaling) and encoded with libwebp through the `webp-animation` crate.

use crate::export::{self, CANCELLED, Encode, Job};
use anyhow::{Context as _, Result, anyhow};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

pub const FPS_CHOICES: [u32; 5] = [10, 15, 20, 24, 30];
pub const WIDTH_CHOICES: [u32; 4] = [480, 640, 800, 1080];
pub const COLOR_CHOICES: [u32; 3] = [64, 128, 256];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dither {
    None,
    Bayer,
    Sierra,
}

impl Dither {
    pub const ALL: [Dither; 3] = [Dither::None, Dither::Bayer, Dither::Sierra];
    pub fn label(self) -> &'static str {
        match self {
            Dither::None => "None",
            Dither::Bayer => "Bayer",
            Dither::Sierra => "Sierra",
        }
    }
    fn filter(self) -> &'static str {
        match self {
            Dither::None => "dither=none",
            Dither::Bayer => "dither=bayer:bayer_scale=3",
            Dither::Sierra => "dither=sierra2_4a",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct GifOptions {
    pub fps: u32,
    pub width: u32,
    pub colors: u32,
    pub dither: Dither,
    pub looped: bool,
}

impl Default for GifOptions {
    fn default() -> Self {
        Self { fps: 15, width: 640, colors: 256, dither: Dither::Bayer, looped: true }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct WebpOptions {
    pub fps: u32,
    pub width: u32,
    /// Lossy quality 0..=100.
    pub quality: f32,
    pub looped: bool,
}

impl Default for WebpOptions {
    fn default() -> Self {
        Self { fps: 15, width: 640, quality: 75.0, looped: true }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    pub mp4: bool,
    pub gif: bool,
    pub webp: bool,
    pub gif_opts: GifOptions,
    pub webp_opts: WebpOptions,
    /// Warn when a GIF or WebP is estimated above this.
    pub limit_mb: f32,
    /// File name prefix: `<name>_<timestamp>.<ext>`.
    pub name: String,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            mp4: true,
            gif: false,
            webp: false,
            gif_opts: GifOptions::default(),
            webp_opts: WebpOptions::default(),
            limit_mb: 10.0,
            name: "clip".into(),
        }
    }
}

/// Even width for scaling (the height follows the aspect, also even).
fn scale(width: u32) -> String {
    format!("scale={}:-2:flags=lanczos", width / 2 * 2)
}

/// Runs ffmpeg, polling `cancel`, reporting `-progress` against `total` seconds.
fn run(mut cmd: Command, total: f64, progress: &AtomicU32, cancel: &AtomicBool) -> Result<()> {
    let mut child = cmd
        .args(["-progress", "pipe:1", "-nostats"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Failed to launch ffmpeg")?;
    let total_us = (total * 1e6).max(1.0);
    for line in BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok) {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(anyhow!(CANCELLED));
        }
        if let Some(v) = line.strip_prefix("out_time_us=") {
            if let Ok(us) = v.parse::<f64>() {
                progress.store(((us / total_us) * 1000.0).clamp(0.0, 999.0) as u32, Ordering::Relaxed);
            }
        }
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(anyhow!("ffmpeg failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(())
}

/// GIF from a rendered video: pass 1 builds the palette, pass 2 maps the frames onto it.
pub fn gif(src: &Path, out: &Path, o: &GifOptions, duration: f64, progress: &AtomicU32, cancel: &AtomicBool) -> Result<()> {
    let palette = out.with_extension("palette.png");
    let vf = format!("fps={},{}", o.fps, scale(o.width));
    let pass = |args: Vec<String>, p: &AtomicU32| -> Result<()> {
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-hide_banner", "-v", "error", "-nostdin", "-y"]).args(args);
        run(cmd, duration, p, cancel)
    };
    let half = AtomicU32::new(0);
    pass(
        vec![
            "-i".into(),
            src.display().to_string(),
            "-vf".into(),
            format!("{vf},palettegen=max_colors={}:stats_mode=diff", o.colors.clamp(2, 256)),
            "-update".into(),
            "1".into(),
            palette.display().to_string(),
        ],
        &half,
    )?;
    progress.store(300, Ordering::Relaxed);
    let second = AtomicU32::new(0);
    let r = pass(
        vec![
            "-i".into(),
            src.display().to_string(),
            "-i".into(),
            palette.display().to_string(),
            "-lavfi".into(),
            format!("{vf}[x];[x][1:v]paletteuse={}:diff_mode=rectangle", o.dither.filter()),
            "-loop".into(),
            if o.looped { "0" } else { "-1" }.into(),
            out.display().to_string(),
        ],
        &second,
    );
    let _ = std::fs::remove_file(&palette);
    r?;
    progress.store(1000, Ordering::Relaxed);
    Ok(())
}

/// Animated WebP from a rendered video (ffmpeg decodes and scales, libwebp encodes).
pub fn webp(src: &Path, out: &Path, o: &WebpOptions, duration: f64, progress: &AtomicU32, cancel: &AtomicBool) -> Result<()> {
    let (w, h) = probe_size(src)?;
    let ow = (o.width / 2 * 2).max(2);
    let oh = (((h as f64 * ow as f64 / w.max(1) as f64) / 2.0).round() as u32 * 2).max(2);
    let mut child = Command::new("ffmpeg")
        .args(["-hide_banner", "-v", "error", "-nostdin", "-i"])
        .arg(src)
        .args(["-vf", &format!("fps={},scale={ow}:{oh}:flags=lanczos", o.fps)])
        .args(["-f", "rawvideo", "-pix_fmt", "rgba", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("Failed to launch ffmpeg")?;
    use webp_animation::{AnimParams, Encoder, EncoderOptions, EncodingConfig};
    let options = EncoderOptions {
        anim_params: AnimParams { loop_count: if o.looped { 0 } else { 1 } },
        encoding_config: Some(EncodingConfig::new_lossy(o.quality.clamp(0.0, 100.0))),
        ..Default::default()
    };
    let mut enc = Encoder::new_with_options((ow, oh), options).map_err(|e| anyhow!("WebP: {e:?}"))?;
    let mut stdout = child.stdout.take().unwrap();
    let mut buf = vec![0u8; (ow * oh * 4) as usize];
    let frame_ms = 1000.0 / o.fps as f64;
    let total = (duration * o.fps as f64).ceil().max(1.0);
    let mut n = 0u32;
    while stdout.read_exact(&mut buf).is_ok() {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(anyhow!(CANCELLED));
        }
        enc.add_frame(&buf, (n as f64 * frame_ms).round() as i32).map_err(|e| anyhow!("WebP: {e:?}"))?;
        n += 1;
        progress.store(((n as f64 / total) * 990.0).min(990.0) as u32, Ordering::Relaxed);
    }
    let _ = child.wait();
    if n == 0 {
        return Err(anyhow!("WebP: no frames"));
    }
    let data = enc.finalize((n as f64 * frame_ms).round() as i32).map_err(|e| anyhow!("WebP: {e:?}"))?;
    std::fs::write(out, &*data).with_context(|| format!("Could not write {}", out.display()))?;
    progress.store(1000, Ordering::Relaxed);
    Ok(())
}

fn probe_size(path: &Path) -> Result<(u32, u32)> {
    let (w, h, _) = crate::recorder::probe(path)?;
    Ok((w, h))
}

/// Per-format progress (0..=1000) of a running export.
#[derive(Default)]
pub struct Progress {
    /// The compositor render (the MP4, or the master for GIF/WebP).
    pub render: AtomicU32,
    pub gif: AtomicU32,
    pub webp: AtomicU32,
}

/// A multi-format export on a background thread.
pub struct ExportJob {
    pub progress: Arc<Progress>,
    cancel: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<Result<Vec<PathBuf>>>>,
}

/// Inputs for an export, owned so they can move to the job's thread.
pub struct Owned {
    pub rec: Arc<crate::recorder::Recording>,
    pub path: crate::camera::CameraPath,
    pub style: crate::style::Style,
    pub layout: crate::layout::Layout,
    pub mirror: bool,
    pub fps: u32,
    pub captions: Option<(crate::captions::Captions, crate::captions::render::CaptionStyle)>,
    pub srt: bool,
    /// The edit, when anything was cut (`rec`, `path` and `captions` are then re-timed).
    pub cut: Option<crate::editing::Cut>,
}

impl Owned {
    fn job(&self) -> Job<'_> {
        Job {
            rec: &self.rec,
            path: &self.path,
            style: &self.style,
            layout: &self.layout,
            mirror: self.mirror,
            fps: self.fps,
            captions: self.captions.as_ref().map(|(c, s)| (c, s)),
            cut: self.cut.as_ref(),
        }
    }
}

impl ExportJob {
    pub fn start(input: Owned, opts: Options, out_dir: PathBuf) -> Self {
        let progress = Arc::new(Progress::default());
        let cancel = Arc::new(AtomicBool::new(false));
        let (p, c) = (progress.clone(), cancel.clone());
        let handle = std::thread::spawn(move || -> Result<Vec<PathBuf>> {
            std::fs::create_dir_all(&out_dir).with_context(|| format!("Could not create {}", out_dir.display()))?;
            let stamp = crate::recorder::stamp();
            let name = if opts.name.trim().is_empty() { "clip".to_string() } else { opts.name.trim().to_string() };
            let mut saved = Vec::new();
            let duration = input.rec.duration;
            // One compositor render: the MP4 itself, or a master for GIF/WebP.
            let master = if opts.mp4 {
                export::output_path(&out_dir, &name, &stamp, "mp4")
            } else {
                input.rec.dir.join("master.mp4")
            };
            let encode = if opts.mp4 { Encode::Final } else { Encode::Master };
            export::render(input.job(), &master, encode, None, &p.render, &c)?;
            if opts.mp4 {
                if let (true, Some((caps, _))) = (input.srt, &input.captions) {
                    std::fs::write(master.with_extension("srt"), caps.to_srt())?;
                }
                saved.push(master.clone());
            }
            let result = (|| -> Result<()> {
                if opts.gif {
                    let out = export::output_path(&out_dir, &name, &stamp, "gif");
                    gif(&master, &out, &opts.gif_opts, duration, &p.gif, &c).inspect_err(|_| {
                        let _ = std::fs::remove_file(&out);
                    })?;
                    saved.push(out);
                }
                if opts.webp {
                    let out = export::output_path(&out_dir, &name, &stamp, "webp");
                    webp(&master, &out, &opts.webp_opts, duration, &p.webp, &c).inspect_err(|_| {
                        let _ = std::fs::remove_file(&out);
                    })?;
                    saved.push(out);
                }
                Ok(())
            })();
            if !opts.mp4 {
                let _ = std::fs::remove_file(&master);
            }
            result?;
            Ok(saved)
        });
        Self { progress, cancel, handle: Some(handle) }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn poll(&mut self) -> Option<Result<Vec<PathBuf>>> {
        if self.handle.as_ref().is_some_and(|h| h.is_finished()) {
            let h = self.handle.take()?;
            return Some(h.join().unwrap_or_else(|_| Err(anyhow!("export crashed"))));
        }
        None
    }
}

impl Drop for ExportJob {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Estimated sizes in bytes (MP4, GIF, WebP), from a short sample of the recording.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Estimate {
    pub mp4: Option<u64>,
    pub gif: Option<u64>,
    pub webp: Option<u64>,
}

/// Seconds sampled for estimates.
const SAMPLE: f64 = 3.0;

/// Renders the first few seconds through the compositor (once, kept as `sample`), encodes
/// that in each picked format, and scales the sizes to the full length.
pub fn estimate(input: &Owned, opts: &Options, sample: &Path, cancel: &AtomicBool) -> Result<Estimate> {
    let duration = input.rec.duration.max(0.1);
    let len = SAMPLE.min(duration);
    let k = duration / len;
    let none = AtomicU32::new(0);
    if !sample.exists() {
        export::render(input.job(), sample, Encode::Sample, Some(len), &none, cancel)?;
    }
    let size = |p: &Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    let mut est = Estimate::default();
    if opts.mp4 {
        // The sample is encoded close to the final settings.
        est.mp4 = Some((size(sample) as f64 * k) as u64);
    }
    if opts.gif {
        let out = sample.with_extension("est.gif");
        gif(sample, &out, &opts.gif_opts, len, &none, cancel)?;
        est.gif = Some((size(&out) as f64 * k) as u64);
        let _ = std::fs::remove_file(out);
    }
    if opts.webp {
        let out = sample.with_extension("est.webp");
        webp(sample, &out, &opts.webp_opts, len, &none, cancel)?;
        est.webp = Some((size(&out) as f64 * k) as u64);
        let _ = std::fs::remove_file(out);
    }
    Ok(est)
}

pub fn size_label(bytes: u64) -> String {
    let mb = bytes as f64 / 1e6;
    if mb >= 10.0 {
        format!("{mb:.0} MB")
    } else if mb >= 1.0 {
        format!("{mb:.1} MB")
    } else {
        format!("{:.0} KB", (bytes as f64 / 1e3).max(1.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_video(dir: &Path) -> PathBuf {
        let p = dir.join("src.mp4");
        assert!(Command::new("ffmpeg")
            .args(["-hide_banner", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=s=1080x1920:r=30:d=2"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&p)
            .status()
            .unwrap()
            .success());
        p
    }

    #[test]
    fn gif_and_webp_encode() {
        let dir = std::env::temp_dir().join(format!("clipper-formats-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = test_video(&dir);
        let no = AtomicBool::new(false);
        let p = AtomicU32::new(0);
        let g = dir.join("a.gif");
        gif(&src, &g, &GifOptions::default(), 2.0, &p, &no).unwrap();
        let (w, h, d) = crate::recorder::probe(&g).unwrap();
        assert_eq!((w, h), (640, 1138), "gif size");
        assert!((d - 2.0).abs() < 0.2, "gif {d}s");
        // 15 fps × 2 s.
        let frames = String::from_utf8(
            Command::new("ffprobe")
                .args(["-v", "error", "-count_frames", "-select_streams", "v:0", "-show_entries", "stream=nb_read_frames", "-of", "csv=p=0"])
                .arg(&g)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        assert_eq!(frames.trim(), "30");
        let wp = dir.join("a.webp");
        webp(&src, &wp, &WebpOptions { width: 480, ..Default::default() }, 2.0, &p, &no).unwrap();
        let bytes = std::fs::read(&wp).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WEBP");
        let dec = webp_animation::Decoder::new(&bytes).unwrap();
        assert_eq!(dec.dimensions(), (480, 854));
        let n = dec.into_iter().count();
        assert_eq!(n, 30, "webp frames");
        // Cancel stops early.
        let yes = AtomicBool::new(true);
        assert_eq!(gif(&src, &dir.join("b.gif"), &GifOptions::default(), 2.0, &p, &yes).unwrap_err().to_string(), CANCELLED);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_names() {
        let dir = std::env::temp_dir().join(format!("clipper-names-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = export::output_path(&dir, "demo", "20261107-120000", "gif");
        assert_eq!(a.file_name().unwrap(), "demo_20261107-120000.gif");
        std::fs::write(&a, b"x").unwrap();
        let b = export::output_path(&dir, "demo", "20261107-120000", "gif");
        assert_eq!(b.file_name().unwrap(), "demo_20261107-120000-2.gif");
        assert_eq!(size_label(2_500_000), "2.5 MB");
        assert_eq!(size_label(12_400_000), "12 MB");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One recording exported as MP4, GIF and WebP in a single run: named
    /// `<name>_<stamp>.<ext>`, the estimate in the right range, and the same frame (with the
    /// caption and click ripple) in all three.
    #[test]
    fn exports_all_formats_identically() {
        use crate::captions::render::CaptionStyle;
        use crate::captions::{Caption, Captions, Word};
        use crate::cursor::Sample;
        let dir = std::env::temp_dir().join(format!("clipper-multi-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let raw = dir.join("raw.mp4");
        assert!(Command::new("ffmpeg")
            .args(["-hide_banner", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=s=1600x1000:r=30:d=4"])
            .args(["-f", "lavfi", "-i", "sine=f=440:r=48000:d=4", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac"])
            .arg(&raw)
            .status()
            .unwrap()
            .success());
        let samples: Vec<Sample> = (0..480)
            .map(|i| {
                let t = i as f64 / 120.0;
                Sample { t, x: 800.0, y: 500.0, down: (2.0..2.1).contains(&t) }
            })
            .collect();
        let rec = Arc::new(crate::recorder::Recording { dir: dir.clone(), raw, width: 1600, height: 1000, duration: 4.0, samples, webcam: None, px_per_pt: 2.0 });
        let style = crate::style::Style::default();
        let layout = crate::layout::Layout::default();
        let path = crate::camera::compute_with(&rec.samples, 1600, 1000, 4.0, layout.frame(&style, false).screen.aspect(), crate::camera::FollowConfig::default());
        let words = vec![Word { start: 1.5, end: 2.0, text: "Hello".into() }, Word { start: 2.0, end: 3.0, text: "there".into() }];
        let captions = Some((Captions { lines: vec![Caption::from_words(words, 32)], ..Default::default() }, CaptionStyle::default()));
        let input = || Owned { rec: rec.clone(), path: crate::camera::CameraPath { crop_w: path.crop_w, crop_h: path.crop_h, frames: path.frames.clone() }, style: style.clone(), layout, mirror: true, fps: 30, captions: captions.clone(), srt: false, cut: None };
        let opts = Options { gif: true, webp: true, name: "demo".into(), ..Options::default() };
        let est = estimate(&input(), &opts, &dir.join("sample.mp4"), &AtomicBool::new(false)).unwrap();
        let out_dir = dir.join("out");
        let mut job = ExportJob::start(input(), opts, out_dir.clone());
        let saved = loop {
            if let Some(r) = job.poll() {
                break r.unwrap();
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        assert_eq!(saved.len(), 3);
        for (p, ext) in saved.iter().zip(["mp4", "gif", "webp"]) {
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            assert!(name.starts_with("demo_") && name.ends_with(ext), "{name}");
        }
        // Estimates within a factor of 3 of the real size.
        let real = |p: &PathBuf| std::fs::metadata(p).unwrap().len() as f64;
        for (e, p) in [(est.mp4, &saved[0]), (est.gif, &saved[1]), (est.webp, &saved[2])] {
            let ratio = e.unwrap() as f64 / real(p);
            assert!((0.33..3.0).contains(&ratio), "estimate off: {ratio:.2} for {}", p.display());
        }
        assert_eq!(crate::recorder::av_layout(&saved[0]).1, true, "MP4 keeps audio");
        // The same moment (2.1 s: caption + ripple) from each format, at 160 px wide.
        let frame = |src: &Path| -> image::RgbImage {
            let png = dir.join(format!("{}.png", src.extension().unwrap().to_string_lossy()));
            let input = if src.extension().unwrap() == "webp" {
                // ffmpeg can't decode animated WebP here: take the frame with libwebp.
                let bytes = std::fs::read(src).unwrap();
                let dec = webp_animation::Decoder::new(&bytes).unwrap();
                let f = dec.into_iter().find(|f| f.timestamp() as f64 >= 2100.0).unwrap();
                let (w, h) = f.dimensions();
                let img = image::RgbaImage::from_raw(w, h, f.data().to_vec()).unwrap();
                return image::imageops::resize(&image::DynamicImage::ImageRgba8(img).to_rgb8(), 160, 284, image::imageops::FilterType::Triangle);
            } else {
                src
            };
            assert!(Command::new("ffmpeg")
                .args(["-hide_banner", "-v", "error", "-y", "-ss", "2.1", "-i"])
                .arg(input)
                .args(["-frames:v", "1", "-vf", "scale=160:284"])
                .arg(&png)
                .status()
                .unwrap()
                .success());
            image::open(&png).unwrap().to_rgb8()
        };
        let (a, b, c) = (frame(&saved[0]), frame(&saved[1]), frame(&saved[2]));
        let diff = |x: &image::RgbImage, y: &image::RgbImage| {
            x.pixels().zip(y.pixels()).map(|(p, q)| p.0.iter().zip(q.0).map(|(u, v)| (*u as i32 - v as i32).abs()).sum::<i32>()).sum::<i32>() as f64
                / (x.width() * x.height() * 3) as f64
        };
        let (gd, wd) = (diff(&a, &b), diff(&a, &c));
        println!("mean diff vs MP4: gif {gd:.1}, webp {wd:.1}");
        assert!(gd < 18.0 && wd < 18.0, "formats differ: gif {gd:.1}, webp {wd:.1}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
