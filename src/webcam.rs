//! Webcam capture as its own raw track, next to screen, mic and cursor.
//!
//! The camera is never baked into the screen capture. Each recording segment (between a
//! start/resume and a pause/stop) runs a separate capture process. AVFoundation stamps camera
//! frames on the same macOS host clock as the screen and mic (see `clock`), and ffmpeg
//! reports each segment's input start on that clock, so `align` can place webcam frames on
//! the clip timeline exactly, duplicating or dropping frames to match the screen.
//!
//! Platform code sits behind [`Backend`]; [`AvFoundation`] is the macOS implementation.

use crate::recorder::FPS;
use anyhow::{Context as _, Result, anyhow};
use gpui::RenderImage;
use image::RgbaImage;
use std::fmt::Write as _;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Requested capture size; cameras that can't do it fall back to their default mode.
const CAPTURE: (u32, u32) = (1280, 720);
/// Live preview frame size (16:9) and rate.
pub const LIVE_SIZE: (u32, u32) = (320, 180);
const LIVE_FPS: u32 = 15;
/// A capture that hasn't delivered a frame by then counts as failed (no camera permission,
/// device busy, or wedged).
const FIRST_FRAME_DEADLINE: Duration = Duration::from_secs(8);

/// Webcam settings (Output → Camera Overlay).
#[derive(Clone, Debug)]
pub struct Settings {
    pub enabled: bool,
    /// Device name from [`Backend::devices`]; `None` = the first camera.
    pub device: Option<String>,
    pub mirror: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { enabled: false, device: None, mirror: true }
    }
}

/// Camera access for one platform.
pub trait Backend: Send + Sync {
    /// Names of the connected cameras.
    fn devices(&self) -> Vec<String>;
    /// Starts recording `device` into `path` until [`Capture::quit`].
    fn capture(&self, device: &str, path: PathBuf) -> Result<Capture>;
    /// Starts a small live feed of `device` for the settings preview.
    fn live(&self, device: &str, mirror: Arc<AtomicBool>) -> Result<Live>;
}

/// The backend for this platform.
pub fn backend() -> &'static dyn Backend {
    &AvFoundation
}

/// The device to use for `settings`: the picked one if it's still connected, else the first.
pub fn resolve_device(settings: &Settings, devices: &[String]) -> Option<String> {
    match &settings.device {
        Some(d) if devices.contains(d) => Some(d.clone()),
        _ => devices.first().cloned(),
    }
}

// ---------- capture ----------

/// One webcam recording process for one segment.
pub struct Capture {
    child: Child,
    stdin: Option<std::process::ChildStdin>,
    pub path: PathBuf,
    /// Host-clock time of the input's file time 0 (ffmpeg's "start: …").
    host_start: Arc<Mutex<Option<f64>>>,
    got_frame: Arc<AtomicBool>,
    spawned: Instant,
}

impl Capture {
    /// Asks the capture to finish its file (non-blocking).
    pub fn quit(&mut self) {
        use std::io::Write;
        if let Some(mut stdin) = self.stdin.take() {
            let _ = stdin.write_all(b"q");
            let _ = stdin.flush();
        }
    }

    /// The camera has delivered its first frame.
    pub fn has_frame(&self) -> bool {
        self.got_frame.load(Ordering::Relaxed)
    }

    /// Still running and, past the start-up deadline, delivering frames.
    pub fn healthy(&mut self) -> bool {
        let alive = matches!(self.child.try_wait(), Ok(None));
        alive && (self.got_frame.load(Ordering::Relaxed) || self.spawned.elapsed() < FIRST_FRAME_DEADLINE)
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Waits for the file to be finished; returns the segment if it holds frames.
    pub fn finish(mut self) -> Option<Segment> {
        let deadline = Instant::now() + Duration::from_secs(if self.got_frame.load(Ordering::Relaxed) { 10 } else { 1 });
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(40)),
                _ => {
                    self.kill();
                    break;
                }
            }
        }
        let host_start = (*self.host_start.lock().unwrap())?;
        let (v_start, dur, w, h) = probe_span(&self.path).ok()?;
        (dur > 0.0).then_some(Segment { path: self.path.clone(), host_start, v_start, dur, size: (w, h) })
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            self.kill();
        }
    }
}

/// A finished webcam segment file.
#[derive(Clone, Debug)]
pub struct Segment {
    pub path: PathBuf,
    /// File time `t` is host time `host_start + t`.
    pub host_start: f64,
    /// First frame and duration, in file time.
    pub v_start: f64,
    pub dur: f64,
    pub size: (u32, u32),
}

/// The webcam track of a recording, on the clip timeline (0 = first screen frame).
#[derive(Clone, Debug)]
pub struct Track {
    /// Constant-frame-rate video at `FPS`, as long as the clip.
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    /// Clip-time spans with real webcam frames. Outside them (camera unplugged, failed to
    /// start) the layout renders without the webcam.
    pub spans: Vec<(f64, f64)>,
}

impl Track {
    pub fn active_at(&self, t: f64) -> bool {
        self.spans.iter().any(|&(a, b)| t >= a && t < b)
    }

    /// ffmpeg `enable` expression for the spans, or `None` when they cover the whole clip.
    pub fn enable_expr(&self, duration: f64) -> Option<String> {
        if self.spans.len() == 1 && self.spans[0].0 <= 0.01 && self.spans[0].1 >= duration - 0.01 {
            return None;
        }
        if self.spans.is_empty() {
            return Some("0".into());
        }
        let terms: Vec<String> = self.spans.iter().map(|(a, b)| format!("between(t\\,{a:.3}\\,{b:.3})")).collect();
        Some(terms.join("+"))
    }
}

/// One screen part of the joined recording, as the webcam aligner needs it.
pub struct Part {
    /// Host-clock time of the part's first screen frame.
    pub host_first: f64,
    /// Length on the clip timeline.
    pub dur: f64,
}

/// Builds the clip-timeline webcam track from the webcam captures (`caps`, any number: the
/// webcam can be turned off and on while recording). For each screen part, every output frame
/// `t` shows the webcam frame from host time `host_first + t` (frames are duplicated or
/// dropped to match), black where no capture has frames; then the parts are joined. `None` if
/// no capture overlaps the recording.
pub fn align(dir: &Path, parts: &[Part], caps: &[Segment]) -> Result<Option<Track>> {
    let Some(first) = caps.first() else { return Ok(None) };
    let (w, h) = first.size;
    let host_span = |c: &Segment| (c.host_start + c.v_start, c.host_start + c.v_start + c.dur);
    let mut spans = Vec::new();
    let mut list = String::new();
    let mut offset = 0.0;
    let mut files = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        let out = dir.join(format!("webcam-part-{i:03}.mp4"));
        let d = format!("{:.6}", part.dur);
        let (p0, p1) = (part.host_first, part.host_first + part.dur);
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-hide_banner", "-v", "error", "-nostdin", "-y"])
            .args(["-f", "lavfi", "-i", &format!("color=black:s={w}x{h}:r={FPS}:d={d}")]);
        let mut graph = String::from("[0:v]null[base0]");
        let mut n = 0;
        for cap in caps {
            let (c0, c1) = host_span(cap);
            if c1 <= p0 || c0 >= p1 {
                continue;
            }
            spans.push((offset + (c0 - p0).max(0.0), offset + (c1 - p0).min(part.dur)));
            // Seek close to the part's start (a little early, for the keyframe); after an
            // input seek the decoded frames start at time 0 = file time `seek`.
            let seek = (p0 - cap.host_start - 1.0).max(0.0);
            let shift = cap.host_start + seek - p0;
            n += 1;
            cmd.args(["-ss", &format!("{seek:.6}"), "-i"]).arg(&cap.path);
            let _ = write!(
                graph,
                ";[{n}:v]setpts=PTS+({shift:.6})/TB,scale={w}:{h},setsar=1[c{n}];\
                 [base{}][c{n}]overlay=eof_action=pass:repeatlast=0[base{n}]",
                n - 1
            );
        }
        let _ = write!(graph, ";[base{n}]fps={FPS},trim=duration={d},setpts=PTS-STARTPTS,format=yuv420p[v]");
        let status = cmd
            .args(["-filter_complex", &graph, "-map", "[v]"])
            .args(["-an", "-c:v", "libx264", "-preset", "veryfast", "-crf", "20", "-r", &FPS.to_string()])
            // A keyframe every second, so the editor can seek to any frame quickly.
            .args(["-g", &FPS.to_string()])
            .arg(&out)
            .output()
            .context("Failed to launch ffmpeg")?;
        if !status.status.success() {
            return Err(anyhow!("Could not align webcam: {}", String::from_utf8_lossy(&status.stderr).trim()));
        }
        let _ = writeln!(list, "file '{}'\noutpoint {:.6}", out.display(), part.dur);
        files.push(out);
        offset += part.dur;
    }
    if spans.is_empty() {
        for f in &files {
            let _ = std::fs::remove_file(f);
        }
        return Ok(None);
    }
    let path = dir.join("webcam.mp4");
    if files.len() == 1 {
        std::fs::rename(&files[0], &path)?;
    } else {
        let list_path = dir.join("webcam-parts.txt");
        std::fs::write(&list_path, list)?;
        let out = Command::new("ffmpeg")
            .args(["-hide_banner", "-v", "error", "-nostdin", "-y", "-f", "concat", "-safe", "0", "-i"])
            .arg(&list_path)
            .args(["-c", "copy"])
            .arg(&path)
            .output()?;
        if !out.status.success() {
            return Err(anyhow!("Could not join webcam parts: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }
        for f in &files {
            let _ = std::fs::remove_file(f);
        }
        let _ = std::fs::remove_file(list_path);
    }
    // Sort and merge touching spans.
    spans.sort_by(|a: &(f64, f64), b| a.0.total_cmp(&b.0));
    let mut merged: Vec<(f64, f64)> = Vec::new();
    for (a, b) in spans {
        match merged.last_mut() {
            Some(last) if a - last.1 < 1.5 / FPS as f64 => last.1 = last.1.max(b),
            _ => merged.push((a, b)),
        }
    }
    Ok(Some(Track { path, width: w, height: h, spans: merged }))
}

/// A webcam frame for the preview, plus a tiny copy that, drawn large, is the blurred fill
/// behind a zoomed-out split webcam.
#[derive(Clone)]
pub struct CamImage {
    pub frame: Arc<RenderImage>,
    pub blur: Arc<RenderImage>,
}

impl CamImage {
    /// From a BGRA frame.
    pub fn from_bgra(img: RgbaImage) -> Self {
        let (w, h) = img.dimensions();
        let k = 24.0 / w.max(h).max(1) as f32;
        let tiny = image::imageops::resize(
            &img,
            ((w as f32 * k).round() as u32).max(2),
            ((h as f32 * k).round() as u32).max(2),
            image::imageops::FilterType::Triangle,
        );
        let wrap = |i: RgbaImage| Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(i)]));
        Self { frame: wrap(img), blur: wrap(tiny) }
    }

    pub fn images(self) -> [Arc<RenderImage>; 2] {
        [self.frame, self.blur]
    }
}

/// Webcam frames for the editor preview, one per preview frame (`fps`), mirrored if asked.
/// Small (16:9, 320 px wide) since the preview draws the webcam at a fraction of the canvas.
pub fn decode_preview(track: &Track, fps: f64, count: usize, mirror: bool) -> Result<Vec<CamImage>> {
    let (w, h) = (320u32, (320 * track.height / track.width.max(1)) / 2 * 2);
    let mut vf = format!("fps={fps:.4},scale={w}:{h}");
    if mirror {
        vf.push_str(",hflip");
    }
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-v", "error", "-nostdin", "-i"])
        .arg(&track.path)
        .args(["-vf", &vf, "-f", "rawvideo", "-pix_fmt", "bgra", "-"])
        .output()
        .context("Failed to launch ffmpeg")?;
    if !out.status.success() {
        return Err(anyhow!("Could not decode webcam: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let frame = (w * h * 4) as usize;
    let mut frames: Vec<CamImage> = out
        .stdout
        .chunks_exact(frame)
        .map(|c| CamImage::from_bgra(RgbaImage::from_raw(w, h, c.to_vec()).unwrap()))
        .collect();
    if frames.is_empty() {
        return Err(anyhow!("Webcam track has no frames"));
    }
    // Match the screen preview's frame count.
    while frames.len() < count {
        frames.push(frames.last().unwrap().clone());
    }
    frames.truncate(count.max(1));
    Ok(frames)
}

// ---------- live preview ----------

/// A running low-res feed of the camera; the newest frame is in `latest`.
pub struct Live {
    child: Child,
    latest: Arc<Mutex<Option<RgbaImage>>>,
    seq: Arc<AtomicU64>,
    taken: u64,
    failed: Arc<AtomicBool>,
}

impl Live {
    /// The newest frame (BGRA), if it's new since the last call.
    pub fn take(&mut self) -> Option<RgbaImage> {
        let seq = self.seq.load(Ordering::Relaxed);
        if seq == self.taken {
            return None;
        }
        self.taken = seq;
        self.latest.lock().unwrap().clone()
    }

    /// The feed ended or never produced a frame in time.
    pub fn failed(&mut self) -> bool {
        self.failed.load(Ordering::Relaxed) || !matches!(self.child.try_wait(), Ok(None))
    }
}

/// The idle live preview in the main window. `Failed` sticks (no retry loop against a
/// camera that's denied or gone) until the device or the webcam switch changes.
#[derive(Default)]
pub enum LivePreview {
    #[default]
    Off,
    Running(Live),
    Failed,
}

impl LivePreview {
    pub fn failed(&self) -> bool {
        matches!(self, LivePreview::Failed)
    }

    /// The newest frame (BGRA), if the feed runs and has a new one.
    pub fn take(&mut self) -> Option<RgbaImage> {
        match self {
            LivePreview::Running(l) => l.take(),
            _ => None,
        }
    }

    /// Stops a running feed (a failure stays recorded).
    pub fn stop(&mut self) {
        if let LivePreview::Running(_) = self {
            *self = LivePreview::Off;
        }
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---------- AVFoundation (macOS) ----------

/// Cameras through ffmpeg's avfoundation input, which timestamps frames on the host clock.
pub struct AvFoundation;

impl AvFoundation {
    fn input(device: &str) -> String {
        format!("{device}:none")
    }
}

impl Backend for AvFoundation {
    fn devices(&self) -> Vec<String> {
        let Ok(out) = Command::new("ffmpeg")
            .args(["-hide_banner", "-f", "avfoundation", "-list_devices", "true", "-i", ""])
            .output()
        else {
            return Vec::new();
        };
        parse_video_devices(&String::from_utf8_lossy(&out.stderr))
    }

    fn capture(&self, device: &str, path: PathBuf) -> Result<Capture> {
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-hide_banner", "-y", "-stats_period", "0.25"])
            .args(["-f", "avfoundation", "-thread_queue_size", "512", "-framerate", &FPS.to_string()])
            .args(["-video_size", &format!("{}x{}", CAPTURE.0, CAPTURE.1)])
            .args(["-i", &Self::input(device)])
            // Keep the camera's own timestamps; `align` re-times onto the clip.
            .args(["-fps_mode", "passthrough", "-an"])
            .args(["-c:v", "h264_videotoolbox", "-b:v", "8M", "-pix_fmt", "yuv420p"])
            .arg(&path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().context("Failed to launch ffmpeg for the webcam")?;
        let host_start = Arc::new(Mutex::new(None));
        let got_frame = Arc::new(AtomicBool::new(false));
        let stderr = child.stderr.take().unwrap();
        {
            let (host_start, got_frame) = (host_start.clone(), got_frame.clone());
            std::thread::spawn(move || {
                for line in split_lines(stderr) {
                    if line.starts_with("frame=") {
                        if !line.starts_with("frame=    0") {
                            got_frame.store(true, Ordering::Relaxed);
                        }
                    } else if let Some(s) = crate::recorder::parse_input_start(&line) {
                        host_start.lock().unwrap().get_or_insert(s);
                    }
                }
            });
        }
        let stdin = child.stdin.take();
        Ok(Capture { child, stdin, path, host_start, got_frame, spawned: Instant::now() })
    }

    fn live(&self, device: &str, mirror: Arc<AtomicBool>) -> Result<Live> {
        let (w, h) = LIVE_SIZE;
        let mut child = Command::new("ffmpeg")
            .args(["-hide_banner", "-v", "error", "-nostdin"])
            .args(["-f", "avfoundation", "-framerate", &FPS.to_string()])
            .args(["-video_size", &format!("{}x{}", CAPTURE.0, CAPTURE.1)])
            .args(["-i", &Self::input(device)])
            .args(["-vf", &format!("fps={LIVE_FPS},scale={w}:{h}:force_original_aspect_ratio=increase,crop={w}:{h}")])
            .args(["-f", "rawvideo", "-pix_fmt", "bgra", "-"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("Failed to launch ffmpeg for the webcam preview")?;
        let latest = Arc::new(Mutex::new(None));
        let seq = Arc::new(AtomicU64::new(0));
        let failed = Arc::new(AtomicBool::new(false));
        let mut stdout = child.stdout.take().unwrap();
        {
            let (latest, seq, failed) = (latest.clone(), seq.clone(), failed.clone());
            std::thread::spawn(move || {
                let mut buf = vec![0u8; (w * h * 4) as usize];
                while stdout.read_exact(&mut buf).is_ok() {
                    let mut img = RgbaImage::from_raw(w, h, buf.clone()).unwrap();
                    if mirror.load(Ordering::Relaxed) {
                        image::imageops::flip_horizontal_in_place(&mut img);
                    }
                    *latest.lock().unwrap() = Some(img);
                    seq.fetch_add(1, Ordering::Relaxed);
                }
                failed.store(true, Ordering::Relaxed);
            });
        }
        Ok(Live { child, latest, seq, taken: 0, failed })
    }
}

/// Camera names from `ffmpeg -list_devices` output (screens excluded).
fn parse_video_devices(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_video = false;
    for line in text.lines() {
        if line.contains("AVFoundation video devices") {
            in_video = true;
        } else if line.contains("AVFoundation audio devices") {
            in_video = false;
        } else if in_video {
            if let Some((_, rest)) = line.split_once("] [") {
                if let Some((_, name)) = rest.split_once("] ") {
                    let name = name.trim();
                    if !name.starts_with("Capture screen") {
                        out.push(name.to_string());
                    }
                }
            }
        }
    }
    out
}

/// Lines of ffmpeg's stderr, split on '\n' and '\r' (progress lines end in '\r').
fn split_lines(r: impl Read) -> impl Iterator<Item = String> {
    let mut reader = BufReader::new(r);
    let mut buf = Vec::new();
    std::iter::from_fn(move || {
        let mut byte = [0u8; 1];
        loop {
            match reader.read(&mut byte) {
                Ok(1) if byte[0] == b'\n' || byte[0] == b'\r' => {
                    let line = String::from_utf8_lossy(&buf).to_string();
                    buf.clear();
                    return Some(line);
                }
                Ok(1) => buf.push(byte[0]),
                _ if !buf.is_empty() => {
                    let line = String::from_utf8_lossy(&buf).to_string();
                    buf.clear();
                    return Some(line);
                }
                _ => return None,
            }
        }
    })
}

/// First video frame time, duration and size of a segment file.
fn probe_span(path: &Path) -> Result<(f64, f64, u32, u32)> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0"])
        .args(["-show_entries", "stream=start_time,duration,width,height", "-of", "default=nw=1"])
        .arg(path)
        .output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let (mut start, mut dur, mut w, mut h) = (0.0, None, 0, 0);
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("start_time=") {
            start = v.parse().unwrap_or(0.0);
        } else if let Some(v) = line.strip_prefix("duration=") {
            dur = v.parse().ok();
        } else if let Some(v) = line.strip_prefix("width=") {
            w = v.parse().unwrap_or(0);
        } else if let Some(v) = line.strip_prefix("height=") {
            h = v.parse().unwrap_or(0);
        }
    }
    let dur = dur.ok_or_else(|| anyhow!("Could not read webcam segment"))?;
    if w == 0 || h == 0 {
        return Err(anyhow!("Could not read webcam segment size"));
    }
    Ok((start, dur, w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cameras_not_screens() {
        let text = "[AVFoundation indev @ 0x1] AVFoundation video devices:\n\
                    [AVFoundation indev @ 0x1] [0] FaceTime HD Camera\n\
                    [AVFoundation indev @ 0x1] [1] Capture screen 0\n\
                    [AVFoundation indev @ 0x1] AVFoundation audio devices:\n\
                    [AVFoundation indev @ 0x1] [0] MacBook Pro Microphone\n";
        assert_eq!(parse_video_devices(text), vec!["FaceTime HD Camera".to_string()]);
    }

    #[test]
    fn device_falls_back_to_first() {
        let devices = vec!["A".to_string(), "B".to_string()];
        let mut s = Settings { device: Some("B".into()), ..Settings::default() };
        assert_eq!(resolve_device(&s, &devices).as_deref(), Some("B"));
        s.device = Some("gone".into());
        assert_eq!(resolve_device(&s, &devices).as_deref(), Some("A"));
        assert_eq!(resolve_device(&s, &[]), None);
    }

    fn color_count(path: &Path, t: f64, w: u32, h: u32) -> [u8; 3] {
        let out = Command::new("ffmpeg")
            .args(["-hide_banner", "-v", "error", "-ss", &format!("{t:.3}"), "-i"])
            .arg(path)
            .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
            .output()
            .unwrap();
        let i = ((h / 2 * w + w / 2) * 3) as usize;
        [out.stdout[i], out.stdout[i + 1], out.stdout[i + 2]]
    }

    /// Two screen parts (a pause between them). The webcam starts 0.5 s into part 1, is
    /// turned off at 2.5 s, and comes back (a second capture) for part 2. The aligned track is
    /// CFR, clip-length, shows the frame from the right host time, and its spans cover only
    /// the time the webcam was on.
    #[test]
    fn align_places_frames_by_host_time() {
        let dir = std::env::temp_dir().join(format!("clipper-webcam-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let clip = |name: &str, args: &[&str]| {
            let p = dir.join(name);
            assert!(Command::new("ffmpeg")
                .args(["-hide_banner", "-v", "error", "-y"])
                .args(args)
                .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
                .arg(&p)
                .status()
                .unwrap()
                .success());
            p
        };
        // Capture 1: red for 1 s then blue for 1 s; file time 0 = host 100.5 (ends 102.5).
        let cam1 = clip(
            "cam-000.mp4",
            &[
                "-f", "lavfi", "-i", "color=red:s=160x90:r=30:d=1",
                "-f", "lavfi", "-i", "color=blue:s=160x90:r=30:d=1",
                "-filter_complex", "[0:v][1:v]concat=n=2:v=1[v]", "-map", "[v]",
            ],
        );
        // Capture 2: green, host 199.0 .. 203.0 (covers part 2).
        let cam2 = clip("cam-001.mp4", &["-f", "lavfi", "-i", "color=green:s=160x90:r=30:d=4"]);
        let seg = |path: &Path, host_start| {
            let (v_start, dur, w, h) = probe_span(path).unwrap();
            Segment { path: path.to_path_buf(), host_start, v_start, dur, size: (w, h) }
        };
        let caps = vec![seg(&cam1, 100.5), seg(&cam2, 199.0)];
        let parts = vec![Part { host_first: 100.0, dur: 3.0 }, Part { host_first: 200.0, dur: 1.0 }];
        let track = align(&dir, &parts, &caps).unwrap().unwrap();
        let (_, total, tw, th) = probe_span(&track.path).unwrap();
        assert_eq!((tw, th), (160, 90));
        assert!((total - 4.0).abs() < 0.08, "track {total}s");
        // On from 0.5 s, off at 2.5 s, back on for part 2 (clip 3..4 s).
        assert_eq!(track.spans.len(), 2, "{:?}", track.spans);
        let near = |x: f64, y: f64| (x - y).abs() < 0.05;
        assert!(near(track.spans[0].0, 0.5) && near(track.spans[0].1, 2.5), "{:?}", track.spans);
        assert!(near(track.spans[1].0, 3.0) && near(track.spans[1].1, 4.0), "{:?}", track.spans);
        assert!(!track.active_at(0.2) && track.active_at(1.0) && !track.active_at(2.7) && track.active_at(3.5));
        // Clip 1.2 s = host 101.2 = webcam file 0.7 s: red. Clip 2.0 s = file 1.5 s: blue.
        let red = color_count(&track.path, 1.2, tw, th);
        let blue = color_count(&track.path, 2.0, tw, th);
        let off = color_count(&track.path, 2.7, tw, th);
        let green = color_count(&track.path, 3.5, tw, th);
        assert!(red[0] > 180 && red[2] < 80, "{red:?}");
        assert!(blue[2] > 180 && blue[0] < 80, "{blue:?}");
        assert!(off.iter().all(|&v| v < 40), "off {off:?}");
        assert!(green[1] > 90 && green[0] < 60, "{green:?}");
        let expr = track.enable_expr(4.0).unwrap();
        assert!(expr.starts_with("between(t"), "{expr}");

        let frames = decode_preview(&track, 5.0, 20, true).unwrap();
        assert_eq!(frames.len(), 20);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
