//! Preview frame extraction and the final ffmpeg crop/export pass.

use crate::camera::CameraPath;
use crate::composite::{self, Composite};
use crate::layout::Layout;
use crate::style::Style;
use crate::recorder::{self, Recording, sessions_root, stamp};
use anyhow::{Context as _, Result, anyhow};
use gpui::RenderImage;
use std::fmt::Write as _;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

pub const PREVIEW_WIDTH: u32 = 640;
const MAX_PREVIEW_FRAMES: f64 = 200.0;

pub struct Preview {
    pub frames: Vec<Arc<RenderImage>>,
    pub fps: f64,
}

/// Extracts small full-frame JPEGs and decodes them into GPU-ready images.
pub fn build_preview(rec: &Recording) -> Result<Preview> {
    let fps = (MAX_PREVIEW_FRAMES / rec.duration.max(0.1)).clamp(1.0, 12.0);
    let dir = rec.dir.join("preview");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-v", "error", "-y", "-i"])
        .arg(&rec.raw)
        .args(["-vf", &format!("fps={fps:.4},scale={PREVIEW_WIDTH}:-2"), "-q:v", "4"])
        .arg(dir.join("%05d.jpg"))
        .status()?;
    if !status.success() {
        return Err(anyhow!("Could not build preview"));
    }

    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "jpg"))
        .collect();
    files.sort();

    let mut frames = Vec::with_capacity(files.len());
    for f in files {
        let mut rgba = image::open(&f)?.to_rgba8();
        for px in rgba.chunks_exact_mut(4) {
            px.swap(0, 2); // GPUI expects BGRA
        }
        frames.push(Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(rgba)])));
    }
    if frames.is_empty() {
        return Err(anyhow!("Recording has no frames"));
    }
    Ok(Preview { frames, fps })
}

/// Where the export goes. Remembered between launches.
pub fn load_export_dir() -> PathBuf {
    std::fs::read_to_string(sessions_root().join(".export_dir"))
        .ok()
        .map(|s| PathBuf::from(s.trim()))
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(sessions_root)
}

pub fn save_export_dir(dir: &Path) {
    let root = sessions_root();
    let _ = std::fs::create_dir_all(&root);
    let _ = std::fs::write(root.join(".export_dir"), dir.to_string_lossy().as_bytes());
}

/// `<name>_<YYYYMMDD-HHMMSS>.<ext>` in `dir`, never overwriting an existing file.
pub fn output_path(dir: &Path, name: &str, stamp: &str, ext: &str) -> PathBuf {
    let base = format!("{name}_{stamp}");
    let mut out = dir.join(format!("{base}.{ext}"));
    let mut n = 2;
    while out.exists() {
        out = dir.join(format!("{base}-{n}.{ext}"));
        n += 1;
    }
    out
}

/// Everything a render needs.
#[derive(Clone, Copy)]
pub struct Job<'a> {
    pub rec: &'a Recording,
    pub path: &'a CameraPath,
    pub style: &'a Style,
    pub layout: &'a Layout,
    pub mirror: bool,
    pub fps: u32,
    pub captions: Option<(&'a crate::captions::Captions, &'a crate::captions::render::CaptionStyle)>,
    /// The edit, when anything was cut: `rec`, `path` and `captions` are then already on the
    /// output timeline (see `editing::apply`), and the raw tracks are cut to match.
    pub cut: Option<&'a crate::editing::Cut>,
}

/// How a render is encoded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Encode {
    /// The MP4 you get (H.264 CRF 18, AAC).
    Final,
    /// A near-lossless intermediate that GIF and WebP are made from.
    Master,
    /// A quick short render for size estimates.
    Sample,
}

pub const CANCELLED: &str = "Export cancelled";

/// Renders `job` through the compositor into `out` (MP4). `limit` renders only the first
/// that many seconds. Blocking; `progress` receives 0..=1000; `cancel` stops it.
pub fn render(job: Job, out: &Path, encode: Encode, limit: Option<f64>, progress: &AtomicU32, cancel: &AtomicBool) -> Result<()> {
    let Job { rec, path, style, layout, mirror, fps, captions, cut } = job;
    let Composite { stills, bg_video, videos, captions: caption_list, ripples, filter } =
        composite::build(rec, path, style, layout, mirror, fps, captions, cut)?;
    // Edited: the mic track cut with crossfades (what the preview plays), as a WAV input.
    let cut_audio = match cut {
        Some(c) => edited_audio(rec, c)?,
        None => None,
    };

    // Run inside the session dir so the sendcmd path needs no escaping.
    let mut cmd = Command::new("ffmpeg");
    cmd.current_dir(&rec.dir)
        .args(["-hide_banner", "-v", "error", "-nostdin", "-y", "-i"])
        .arg(&rec.raw);
    let dur = format!("{:.3}", rec.duration.max(0.1));
    for (i, still) in stills.iter().enumerate() {
        if i == 0 && bg_video {
            cmd.arg("-i").arg(still);
        } else {
            cmd.args(["-loop", "1", "-framerate", &fps.to_string(), "-t", &dur, "-i"]).arg(still);
        }
    }
    for video in &videos {
        cmd.arg("-i").arg(video);
    }
    if let Some(list) = &caption_list {
        cmd.args(["-f", "concat", "-safe", "0", "-i"]).arg(list);
    }
    if let Some(list) = &ripples {
        cmd.args(["-f", "concat", "-safe", "0", "-i"]).arg(list);
    }
    let audio_in = 1 + stills.len() + videos.len() + caption_list.is_some() as usize + ripples.is_some() as usize;
    if let Some(wav) = &cut_audio {
        cmd.arg("-i").arg(wav);
    }
    // Mic audio: the video is moved to 0 above (setpts=PTS-STARTPTS), so shift the audio by
    // the same video start to keep the shared-clock alignment (the raw's video starts a few
    // ms after its audio because of AAC priming).
    let (v0, has_audio) = recorder::av_layout(&rec.raw);
    let mut filter = filter;
    if cut.is_some() {
        let _ = write!(filter, ";[{audio_in}:a]atrim=end={dur}[a]");
    }
    let has_audio = if cut.is_some() { cut_audio.is_some() } else { has_audio };
    if has_audio && cut.is_none() {
        let _ = write!(
            filter,
            ";[0:a]asetpts=PTS-{v0:.6}/TB,aresample=async=1:min_hard_comp=0.01:first_pts=0,atrim=end={dur}[a]"
        );
    }
    cmd.args(["-filter_complex", &filter, "-map", "[v]"]);
    if has_audio {
        cmd.args(["-map", "[a]", "-c:a", "aac", "-b:a", "160k"]);
    }
    let (preset, crf) = match encode {
        Encode::Final => ("medium", "18"),
        Encode::Master => ("veryfast", "12"),
        Encode::Sample => ("veryfast", "18"),
    };
    if let Some(t) = limit {
        cmd.args(["-t", &format!("{t:.3}")]);
    }
    let mut child = cmd
        .args(["-c:v", "libx264", "-preset", preset, "-crf", crf, "-pix_fmt", "yuv420p"])
        .args(["-movflags", "+faststart", "-progress", "pipe:1", "-nostats"])
        .arg(out)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Failed to launch ffmpeg")?;

    let total_us = (limit.unwrap_or(rec.duration).min(rec.duration) * 1_000_000.0).max(1.0);
    for line in BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok) {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(out);
            return Err(anyhow!(CANCELLED));
        }
        if let Some(v) = line.strip_prefix("out_time_us=") {
            if let Ok(us) = v.parse::<f64>() {
                progress.store(((us / total_us) * 1000.0).clamp(0.0, 999.0) as u32, Ordering::Relaxed);
            }
        }
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        let _ = std::fs::remove_file(out);
        return Err(anyhow!("Export failed: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    progress.store(1000, Ordering::Relaxed);
    Ok(())
}

/// The raw mic track (aligned like the unedited export) cut to the edit, written next to
/// the recording. `None` when the recording has no audio.
fn edited_audio(rec: &Recording, cut: &crate::editing::Cut) -> Result<Option<PathBuf>> {
    let source = Recording {
        dir: rec.dir.clone(),
        raw: rec.raw.clone(),
        width: rec.width,
        height: rec.height,
        duration: cut.source_duration,
        samples: Vec::new(),
        webcam: None,
        px_per_pt: rec.px_per_pt,
    };
    let Some(pcm) = crate::audio::decode_at(&source, crate::audio::RATE)? else { return Ok(None) };
    let out = rec.dir.join("edit-audio.wav");
    crate::editing::audio::write_wav(&out, &crate::editing::audio::cut(&pcm, crate::audio::RATE, &cut.edit), crate::audio::RATE)?;
    Ok(Some(out))
}

/// Renders the styled canvas (background, rounded screen rect with shadow, cursor, webcam,
/// captions, click ripples) into an MP4 of the layout's canvas size at `fps` inside
/// `out_dir`, with the mic audio. Blocking; `progress` receives 0..=1000.
#[allow(clippy::too_many_arguments)]
#[cfg_attr(not(test), allow(dead_code))] // tests; the app exports through `formats::ExportJob`
pub fn export(
    rec: &Recording,
    path: &CameraPath,
    style: &Style,
    layout: &Layout,
    mirror: bool,
    fps: u32,
    out_dir: &Path,
    progress: Arc<AtomicU32>,
    captions: Option<(&crate::captions::Captions, &crate::captions::render::CaptionStyle)>,
    srt: bool,
) -> Result<PathBuf> {
    std::fs::create_dir_all(out_dir).with_context(|| format!("Could not create {}", out_dir.display()))?;
    let out = output_path(out_dir, "clip", &stamp(), "mp4");
    let job = Job { rec, path, style, layout, mirror, fps, captions, cut: None };
    render(job, &out, Encode::Final, None, &progress, &AtomicBool::new(false))?;
    if let (true, Some((caps, _))) = (srt, captions) {
        std::fs::write(out.with_extension("srt"), caps.to_srt())
            .with_context(|| format!("Could not write {}", out.with_extension("srt").display()))?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// First time (s) a measurement crosses `threshold`, from ffmpeg `metadata=print` output.
    fn first_onset(file: &Path, stream: &str, filter: &str, key: &str, threshold: f64) -> f64 {
        let flag = if stream == "v" { "-vf" } else { "-af" };
        let out = Command::new("ffmpeg")
            .args(["-hide_banner", "-nostats", "-i"])
            .arg(file)
            .args(["-map", &format!("0:{stream}"), flag, filter, "-f", "null", "-"])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stderr);
        let mut t = 0.0;
        for line in text.lines() {
            if let Some(v) = line.split("pts_time:").nth(1) {
                t = v.trim().parse().unwrap_or(t);
            } else if let Some(v) = line.split(&format!("{key}=")).nth(1) {
                if v.trim().parse::<f64>().is_ok_and(|x| x > threshold) {
                    return t;
                }
            }
        }
        panic!("no onset of {key} in {}", file.display());
    }

    /// Rising edges of a per-frame/per-chunk measurement over `threshold`.
    fn onsets(file: &Path, stream: &str, filter: &str, key: &str, threshold: f64) -> Vec<f64> {
        let flag = if stream == "v" { "-vf" } else { "-af" };
        let out = Command::new("ffmpeg")
            .args(["-hide_banner", "-nostats", "-i"])
            .arg(file)
            .args(["-map", &format!("0:{stream}"), flag, filter, "-f", "null", "-"])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stderr);
        let (mut t, mut on, mut edges) = (0.0, false, Vec::new());
        for line in text.lines() {
            if let Some(v) = line.split("pts_time:").nth(1) {
                t = v.trim().parse().unwrap_or(t);
            } else if let Some(v) = line.split(&format!("{key}=")).nth(1) {
                let high = v.trim().parse::<f64>().is_ok_and(|x| x > threshold);
                if high && !on {
                    edges.push(t);
                }
                on = high;
            }
        }
        edges
    }

    fn duration_of(file: &Path) -> f64 {
        let out = Command::new("ffprobe")
            .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
            .arg(file)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
    }

    /// An edited export has only the kept parts: the right length, the removed flash gone,
    /// the flash after the cut at its new time with its beep still in sync, and the raw
    /// recording byte-identical afterwards.
    #[test]
    fn edited_export_cuts_video_and_audio_together() {
        use crate::editing::{EditList, apply};
        let dir = std::env::temp_dir().join(format!("clipper-edit-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let raw = dir.join("raw.mp4");
        // Flash + beep at 0.5 (kept), 1.5 (removed) and 3.0 s (kept), streams 0.1 s late in the file.
        let flashes = "between(t,0.5,0.7)+between(t,1.5,1.7)+between(t,3.0,3.2)";
        assert!(Command::new("ffmpeg")
            .args(["-hide_banner", "-v", "error", "-y"])
            .args(["-f", "lavfi", "-i", "color=black:s=320x240:r=30:d=4"])
            .args(["-f", "lavfi", "-i", "sine=f=1000:r=48000:d=4"])
            .args([
                "-filter_complex",
                &format!(
                    "[0:v]drawbox=c=white:t=fill:enable='{flashes}',setpts=PTS+0.1/TB[v];\
                     [1:a]volume=enable='not({flashes})':volume=0,adelay=100:all=1[a]"
                ),
            ])
            .args(["-map", "[v]", "-map", "[a]", "-c:v", "libx264", "-g", "30", "-pix_fmt", "yuv420p", "-c:a", "aac"])
            .arg(&raw)
            .status()
            .unwrap()
            .success());
        let before = std::fs::read(&raw).unwrap();
        let rec = Recording { dir: dir.clone(), raw: raw.clone(), width: 320, height: 240, duration: 4.0, samples: Vec::new(), webcam: None, px_per_pt: 1.0 };
        let path = CameraPath { crop_w: 320, crop_h: 240, frames: vec![(0, 0, 320, 240); 120] };
        let edit = EditList { segments: vec![(0.0, 1.0), (2.0, 4.0)] };
        let rules = crate::captions::segment::Rules::for_ratio(crate::layout::Ratio::Landscape);
        let e = apply(&rec, &path, None, rules, &edit);
        let out = dir.join("edited.mp4");
        let (style, layout) = (Style::default(), Layout::default());
        let job = Job { rec: &e.rec, path: &e.path, style: &style, layout: &layout, mirror: false, fps: 30, captions: None, cut: Some(&e.cut) };
        render(job, &out, Encode::Sample, None, &AtomicU32::new(0), &AtomicBool::new(false)).unwrap();

        let len = duration_of(&out);
        assert!((len - 3.0).abs() < 0.07, "edited length {len}");
        let flash = onsets(&out, "v", "signalstats,metadata=print:key=lavfi.signalstats.YAVG", "lavfi.signalstats.YAVG", 100.0);
        let beep = onsets(
            &out,
            "a",
            "asetnsamples=n=48:p=0,astats=metadata=1:reset=1,ametadata=print:key=lavfi.astats.Overall.Peak_level",
            "lavfi.astats.Overall.Peak_level",
            -20.0,
        );
        println!("flashes {flash:?}, beeps {beep:?}");
        assert_eq!(flash.len(), 2, "the removed flash must be gone: {flash:?}");
        assert!((flash[0] - 0.5).abs() < 0.04 && (flash[1] - 2.0).abs() < 0.04, "flashes at {flash:?}");
        assert_eq!(beep.len(), 2, "beeps {beep:?}");
        for (f, b) in flash.iter().zip(&beep) {
            assert!((f - b).abs() < 1.0 / 30.0, "audio off by {:.3}s after the cut", b - f);
        }
        assert!(std::fs::read(&raw).unwrap() == before, "raw recording changed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The webcam is cut with the screen: in every webcam layout an edited export (with click
    /// ripples and an animated-free background) renders at the edited length.
    #[test]
    fn edited_export_with_webcam() {
        use crate::editing::{EditList, apply};
        use crate::layout::{Placement, Ratio};
        let dir = std::env::temp_dir().join(format!("clipper-edit-cam-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ff = |args: &[&str], out: &Path| {
            assert!(Command::new("ffmpeg").args(["-hide_banner", "-v", "error", "-y"]).args(args).arg(out).status().unwrap().success());
        };
        let raw = dir.join("raw.mp4");
        ff(&["-f", "lavfi", "-i", "color=gray:s=640x400:r=30:d=3", "-f", "lavfi", "-i", "sine=d=3", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac"], &raw);
        let cam = dir.join("webcam.mp4");
        ff(&["-f", "lavfi", "-i", "color=red:s=320x180:r=30:d=3", "-c:v", "libx264", "-pix_fmt", "yuv420p"], &cam);
        let samples: Vec<crate::cursor::Sample> = (0..90).map(|i| crate::cursor::Sample { t: i as f64 / 30.0, x: 300.0, y: 200.0, down: i % 30 == 5 }).collect();
        let rec = Recording {
            dir: dir.clone(),
            raw,
            width: 640,
            height: 400,
            duration: 3.0,
            samples,
            webcam: Some(crate::webcam::Track { path: cam, width: 320, height: 180, spans: vec![(0.0, 3.0)] }),
            px_per_pt: 1.0,
        };
        let edit = EditList { segments: vec![(0.0, 0.8), (1.5, 2.2), (2.6, 3.0)] };
        for placement in [Placement::FALLBACK, Placement::Split(crate::layout::Side::Top)] {
            let layout = Layout { ratio: Ratio::Portrait, placement, ..Layout::default() };
            let style = Style::default();
            let frame = layout.frame(&style, true);
            let path = crate::camera::compute_with(&rec.samples, 640, 400, 3.0, frame.screen.aspect(), crate::camera::FollowConfig::default());
            let e = apply(&rec, &path, None, crate::captions::segment::Rules::for_ratio(Ratio::Portrait), &edit);
            let out = dir.join(format!("cam-{placement:?}.mp4").replace([' ', '(', ')'], ""));
            let job = Job { rec: &e.rec, path: &e.path, style: &style, layout: &layout, mirror: true, fps: 30, captions: None, cut: Some(&e.cut) };
            render(job, &out, Encode::Sample, None, &AtomicU32::new(0), &AtomicBool::new(false)).unwrap();
            let len = duration_of(&out);
            assert!((len - 1.9).abs() < 0.07, "{placement:?}: length {len}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A flash and a beep at the same instant must still coincide after export, even when
    /// the raw's streams don't start at 0 (as with the shared-clock recordings).
    #[test]
    fn export_keeps_audio_in_sync() {
        let dir = std::env::temp_dir().join(format!("clipper-sync-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let raw = dir.join("raw.mp4");
        // Flash and beep at 1.0s; both streams then pushed 0.1s later in the file.
        let ok = Command::new("ffmpeg")
            .args(["-hide_banner", "-v", "error", "-y"])
            .args(["-f", "lavfi", "-i", "color=black:s=320x240:r=30:d=2"])
            .args(["-f", "lavfi", "-i", "sine=f=1000:r=48000:d=2"])
            .args([
                "-filter_complex",
                "[0:v]drawbox=c=white:t=fill:enable='between(t,1,1.2)',setpts=PTS+0.1/TB[v];\
                 [1:a]volume=enable='not(between(t,1,1.2))':volume=0,adelay=100:all=1[a]",
            ])
            .args(["-map", "[v]", "-map", "[a]", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac"])
            .arg(&raw)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let rec = Recording { dir: dir.clone(), raw, width: 320, height: 240, duration: 2.0, samples: Vec::new(), webcam: None, px_per_pt: 1.0 };
        let path = CameraPath { crop_w: 320, crop_h: 240, frames: vec![(0, 0, 320, 240); 60] };
        let out = export(&rec, &path, &Style::default(), &Layout::default(), true, recorder::FPS, &dir, Arc::new(AtomicU32::new(0)), None, false).unwrap();

        let (v0, has_audio) = crate::recorder::av_layout(&out);
        assert!(has_audio, "export has no audio stream");
        assert!(v0.abs() < 0.01, "video starts at {v0}");
        let flash = first_onset(&out, "v", "signalstats,metadata=print:key=lavfi.signalstats.YAVG", "lavfi.signalstats.YAVG", 100.0);
        let beep = first_onset(
            &out,
            "a",
            "asetnsamples=n=48:p=0,astats=metadata=1:reset=1,ametadata=print:key=lavfi.astats.Overall.Peak_level",
            "lavfi.astats.Overall.Peak_level",
            -20.0,
        );
        println!("flash {flash:.3}s, beep {beep:.3}s");
        assert!((flash - 1.0).abs() < 0.04, "flash at {flash}");
        assert!((beep - flash).abs() < 1.0 / recorder::FPS as f64, "audio off by {:.3}s", beep - flash);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
