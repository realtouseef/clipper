//! Raw full-screen + mic capture, the webcam track and the cursor track.
//!
//! Screen and mic: `clipper-capture` (helpers/capture.swift, ScreenCaptureKit) records the
//! whole display while leaving out Clipper's own windows, so the floating control bar is
//! never in the video. Pausing ends the current segment; resuming starts a new one. On stop
//! the segments are joined back to back (stream copy), and the cursor and webcam tracks are
//! re-timed onto that joined timeline, so a pause leaves no gap or frozen stretch.
//!
//! Webcam: one capture for the whole recording, started (and given time to deliver its first
//! frame) before the screen, so it's in the clip from the first frame. Turning it off from the
//! control bar ends that capture; turning it on starts a new one.
//!
//! Sync: screen frames, mic buffers, camera frames and cursor samples are all stamped on the
//! macOS host clock (see `clock`). Each segment reports the host time of its file time 0
//! ("start: …"), which places every track on one timeline; `stop` uses it to re-time them.

use crate::clock::{self, Nanos};
use crate::cursor::{self, Sample, Tracker};
use crate::region::{Display, Source};
use crate::webcam;
use anyhow::{Context as _, Result, anyhow};
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const FPS: u32 = 30;

/// How long Start waits for the webcam's first frame before starting the screen anyway.
const WEBCAM_WARMUP: Duration = Duration::from_secs(4);

pub struct Recording {
    pub dir: PathBuf,
    pub raw: PathBuf,
    pub width: u32,
    pub height: u32,
    pub duration: f64,
    /// Cursor samples converted to video pixel coordinates.
    pub samples: Vec<Sample>,
    /// The webcam track on the clip timeline, if the webcam was on and delivered frames.
    pub webcam: Option<webcam::Track>,
    /// Video pixels per display point (2.0 on Retina); sizes the cursor sprite.
    pub px_per_pt: f64,
}

/// One continuous screen capture between a start/resume and a pause/stop.
struct Segment {
    child: Child,
    stdin: Option<ChildStdin>,
    /// Set when the first frame is in; drives the live timer.
    t0: Arc<Mutex<Option<Instant>>>,
    /// Shared state parsed from the capture's log (file start on the host clock, etc.).
    sync: Arc<Mutex<SegmentSync>>,
    path: PathBuf,
    audio: bool,
}

#[derive(Default)]
struct SegmentSync {
    /// File time `t` in this segment is host time `host_start + t` (seconds).
    host_start: Option<f64>,
    /// Host-clock time when the first frame was logged; a coarse fallback.
    first_log: Option<Nanos>,
    /// The microphone couldn't be opened; the segment is video only.
    no_mic: bool,
}

/// Parses `start: 87540.958500` out of a "Duration: …" line (ffmpeg's input line, which the
/// capture helper prints too).
pub fn parse_input_start(line: &str) -> Option<f64> {
    if !line.contains("Duration:") {
        return None;
    }
    let v = line.split("start:").nth(1)?.split(',').next()?.trim();
    v.parse().ok()
}

/// A helper program: next to the app's executable when installed (Clipper.app/Contents/MacOS),
/// else where the build put it (`cargo run`).
pub fn helper(name: &str, built: &str) -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join(name)))
        .filter(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from(built))
}

/// The ScreenCaptureKit recorder built from helpers/capture.swift.
fn capture_helper() -> PathBuf {
    helper("clipper-capture", env!("CLIPPER_CAPTURE_HELPER"))
}

impl Segment {
    fn spawn(source: &Source, mic: Option<&str>, muted: bool, path: PathBuf, log: Arc<Mutex<String>>) -> Result<Self> {
        let mut cmd = Command::new(capture_helper());
        cmd.arg("--out").arg(&path).args([
            "--exclude-pid",
            &std::process::id().to_string(),
            "--display-id",
            &source.display_id().to_string(),
            "--fps",
            &FPS.to_string(),
        ]);
        if let Source::Region(r) = source {
            if let Some(d) = cursor::displays().into_iter().find(|d| d.id == r.display) {
                let (x, y, w, h) = r.to_points(&d);
                cmd.args(["--rect", &format!("{x},{y},{w},{h}")]);
            }
        }
        if let Some(m) = mic {
            cmd.args(["--mic", m]);
        }
        if muted {
            cmd.arg("--muted");
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("Failed to launch the screen recorder")?;

        let t0 = Arc::new(Mutex::new(None));
        let sync = Arc::new(Mutex::new(SegmentSync::default()));
        let stderr = child.stderr.take().unwrap();
        {
            let (t0, sync) = (t0.clone(), sync.clone());
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    if line.starts_with("frame=") {
                        let mut t = t0.lock().unwrap();
                        if t.is_none() {
                            *t = Some(Instant::now());
                            sync.lock().unwrap().first_log = Some(clock::now());
                        }
                    } else if line == "nomic" {
                        sync.lock().unwrap().no_mic = true;
                        log.lock().unwrap().push_str("Microphone unavailable; recording without audio.\n");
                    } else if let Some(start) = parse_input_start(&line) {
                        sync.lock().unwrap().host_start.get_or_insert(start);
                    } else if !line.is_empty() {
                        let mut l = log.lock().unwrap();
                        l.push_str(line.trim_start_matches("error: "));
                        l.push('\n');
                    }
                }
            });
        }
        let stdin = child.stdin.take();
        Ok(Self { child, stdin, t0, sync, path, audio: mic.is_some() })
    }

    fn send(&mut self, text: &str) {
        if let Some(stdin) = self.stdin.as_mut() {
            let _ = stdin.write_all(text.as_bytes());
            let _ = stdin.flush();
        }
    }

    /// Asks the capture to finish the file (non-blocking).
    fn quit(&mut self) {
        self.send("q\n");
        self.stdin = None;
    }

    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Waits for the capture to exit after `quit`. A segment that never got a frame has
    /// nothing to save, so give up after a deadline and kill it rather than blocking Stop.
    fn finish(&mut self) -> bool {
        let limit = if self.t0.lock().unwrap().is_some() { 10 } else { 2 };
        let deadline = Instant::now() + Duration::from_secs(limit);
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return status.success(),
                Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    return false;
                }
            }
        }
    }
}

pub struct Recorder {
    /// What's recorded (a display or a region of one) and that display.
    source: Source,
    display: Display,
    /// Mic name for each segment ("default" = system default); `None` records video only.
    mic: Option<String>,
    muted: bool,
    dir: PathBuf,
    active: Option<Segment>,
    finished: Vec<Segment>,
    tracker: Option<Tracker>,
    log: Arc<Mutex<String>>,
    /// Recorded time of finished segments (wall clock, for the on-screen timer).
    banked: Duration,
    /// Camera recorded alongside the screen (`None`: no webcam this recording).
    webcam: Option<String>,
    /// The running webcam capture, and earlier ones (turned off, or failed).
    cam: Option<webcam::Capture>,
    cam_done: Vec<webcam::Capture>,
    /// The webcam stopped delivering frames (unplugged, no permission).
    webcam_lost: bool,
}

/// The system default microphone.
pub const DEFAULT_MIC: &str = "default";

/// Names of the available microphones, for the settings picker.
pub fn list_mics() -> Vec<String> {
    let Ok(out) = Command::new("ffmpeg")
        .args(["-hide_banner", "-f", "avfoundation", "-list_devices", "true", "-i", ""])
        .output()
    else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&out.stderr);
    let mut in_audio = false;
    let mut mics = Vec::new();
    for line in text.lines() {
        if line.contains("AVFoundation audio devices") {
            in_audio = true;
        } else if in_audio {
            // "[AVFoundation indev @ 0x…] [0] MacBook Pro Microphone"
            if let Some((_, rest)) = line.split_once("] [") {
                if let Some((_, name)) = rest.split_once("] ") {
                    mics.push(name.trim().to_string());
                }
            }
        }
    }
    mics
}

pub fn sessions_root() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join("Movies").join("Clipper")
}

pub fn stamp() -> String {
    let out = Command::new("date").arg("+%Y%m%d-%H%M%S").output();
    out.ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "session".into())
}

impl Recorder {
    #[cfg_attr(not(test), allow(dead_code))] // used by the e2e tests
    pub fn start(muted: bool) -> Result<Self> {
        let main = cursor::displays().first().map_or(0, |d| d.id);
        Self::start_with(Source::Display(main), muted, Some(DEFAULT_MIC.to_string()), None)
    }

    /// Starts recording `source` (a whole display or a region). `mic` is a device name from `list_mics`,
    /// `DEFAULT_MIC`, or `None` to record without audio. `webcam` is a camera name to record
    /// as its own track, or `None`. Blocks until the webcam (if any) is delivering frames, so
    /// it's in the clip from the first frame.
    pub fn start_with(source: Source, muted: bool, mic: Option<String>, webcam: Option<String>) -> Result<Self> {
        let displays = cursor::displays();
        let display = displays
            .iter()
            .copied()
            .find(|d| d.id == source.display_id())
            .or(displays.first().copied())
            .ok_or_else(|| anyhow!("No display found"))?;
        let source = match source {
            Source::Region(r) if r.display == display.id => Source::Region(r.clamped(&display)),
            Source::Region(_) => return Err(anyhow!("The region's display isn't connected. Select the region again.")),
            s => s,
        };
        let mics = list_mics();
        let log = Arc::new(Mutex::new(String::new()));
        // A picked mic that was unplugged falls back to the system default.
        let mic = match mic {
            _ if mics.is_empty() => None,
            Some(m) if m != DEFAULT_MIC && !mics.contains(&m) => {
                log.lock().unwrap().push_str(&format!("Microphone \"{m}\" not found; using the default.\n"));
                Some(DEFAULT_MIC.to_string())
            }
            m => m,
        };
        let dir = sessions_root().join(".sessions").join(stamp());
        std::fs::create_dir_all(&dir)?;
        let mut r = Self {
            source,
            display,
            mic,
            muted,
            dir,
            active: None,
            finished: Vec::new(),
            tracker: None,
            log,
            banked: Duration::ZERO,
            webcam,
            cam: None,
            cam_done: Vec::new(),
            webcam_lost: false,
        };
        if r.webcam.is_some() {
            r.start_webcam();
            let deadline = Instant::now() + WEBCAM_WARMUP;
            while r.cam.as_mut().is_some_and(|c| !c.has_frame() && c.healthy()) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        r.tracker = Some(Tracker::start());
        r.spawn_segment()?;
        Ok(r)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn spawn_segment(&mut self) -> Result<()> {
        let n = self.finished.len() + self.active.is_some() as usize;
        let path = self.dir.join(format!("seg-{n:03}.mp4"));
        self.active = Some(Segment::spawn(&self.source, self.mic.as_deref(), self.muted, path, self.log.clone())?);
        Ok(())
    }

    fn start_webcam(&mut self) {
        let Some(device) = self.webcam.clone() else { return };
        let n = self.cam_done.len();
        match webcam::backend().capture(&device, self.dir.join(format!("cam-{n:03}.mp4"))) {
            Ok(cam) => {
                self.cam = Some(cam);
                self.webcam_lost = false;
            }
            Err(e) => self.lose_webcam(&e.to_string()),
        }
    }

    fn stop_webcam(&mut self) {
        if let Some(mut cam) = self.cam.take() {
            cam.quit();
            self.cam_done.push(cam);
        }
    }

    /// Stops using the webcam until it's turned on again; screen and audio carry on.
    fn lose_webcam(&mut self, why: &str) {
        self.stop_webcam();
        if !self.webcam_lost {
            self.webcam_lost = true;
            self.log.lock().unwrap().push_str(&format!("Webcam stopped: {why}\n"));
        }
    }

    /// This recording has a webcam (it may be turned off right now).
    pub fn has_webcam(&self) -> bool {
        self.webcam.is_some()
    }

    /// The webcam is being recorded right now.
    pub fn webcam_on(&self) -> bool {
        self.cam.is_some()
    }

    /// Turns the webcam off (the clip shows no webcam from here) or back on.
    pub fn toggle_webcam(&mut self) {
        if self.cam.is_some() {
            self.stop_webcam();
            self.webcam_lost = false;
        } else {
            self.start_webcam();
        }
    }

    /// The webcam was on and stopped by itself (the rest of the clip renders without it).
    pub fn webcam_lost(&self) -> bool {
        self.webcam.is_some() && self.webcam_lost
    }

    /// Checks the captures. Errors if the screen capture died.
    pub fn poll(&mut self) -> Result<()> {
        // A dead webcam never stops the recording: finish its file (what it has so far is
        // kept) and carry on without it.
        if self.cam.as_mut().is_some_and(|c| !c.healthy()) {
            self.lose_webcam("the camera stopped delivering frames");
        }
        let Some(seg) = self.active.as_mut() else { return Ok(()) };
        if seg.audio && seg.sync.lock().unwrap().no_mic {
            seg.audio = false;
            self.mic = None;
        }
        let Some(seg) = self.active.as_mut() else { return Ok(()) };
        if seg.alive() {
            return Ok(());
        }
        self.active = None;
        Err(anyhow!("{}", self.error_tail()))
    }

    pub fn has_audio(&self) -> bool {
        self.mic.is_some()
    }

    pub fn is_paused(&self) -> bool {
        self.active.is_none()
    }

    /// Recorded time so far: counts only while frames are actually being captured, so it
    /// matches the length of the final file (capture takes a moment to start after resume).
    pub fn elapsed(&self) -> Duration {
        let live = self.active.as_ref().and_then(|s| *s.t0.lock().unwrap());
        self.banked + live.map(|t| t.elapsed()).unwrap_or_default()
    }

    /// True until the first frame of the recording arrives (even across a pause, if no
    /// segment has produced a frame yet).
    pub fn is_warming_up(&self) -> bool {
        self.banked.is_zero()
            && self.active.as_ref().is_some_and(|s| s.t0.lock().unwrap().is_none())
    }

    /// Stops video, audio and the cursor log until `resume`. The webcam keeps running so it's
    /// there the moment recording resumes (paused time isn't in the clip).
    pub fn pause(&mut self) {
        let Some(mut seg) = self.active.take() else { return };
        seg.quit();
        if let Some(t0) = *seg.t0.lock().unwrap() {
            self.banked += t0.elapsed();
        }
        self.finished.push(seg);
        if let Some(t) = &self.tracker {
            t.set_paused(true);
        }
    }

    pub fn resume(&mut self) -> Result<()> {
        if self.active.is_some() {
            return Ok(());
        }
        if let Some(t) = &self.tracker {
            t.set_paused(false);
        }
        self.spawn_segment()
    }

    /// Mute keeps recording the audio track (as silence) so A/V stay in sync.
    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
        if let Some(seg) = self.active.as_mut().filter(|s| s.audio) {
            seg.send(if muted { "m1\n" } else { "m0\n" });
        }
    }

    pub fn error_tail(&self) -> String {
        let log = self.log.lock().unwrap();
        log.lines().rev().take(3).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")
    }

    /// Finalizes every segment, joins them and re-times the cursor track. Blocking.
    pub fn stop(mut self) -> Result<Recording> {
        if let Some(mut seg) = self.active.take() {
            seg.quit();
            self.finished.push(seg);
        }
        self.stop_webcam();
        let raw_samples = self.tracker.take().unwrap().finish();

        // Each usable segment, in order. Both streams of a segment share its file timeline,
        // and file time `t` is host time `host_start + t`.
        struct Part {
            path: PathBuf,
            host_start: f64,
            /// Video span in file time (the end is the concat outpoint).
            v_start: f64,
            v_end: f64,
        }
        let mut parts: Vec<Part> = Vec::new();
        for seg in self.finished.iter_mut() {
            let ok = seg.finish();
            if !ok || seg.t0.lock().unwrap().is_none() {
                continue;
            }
            let Ok((v_start, dur)) = video_span(&seg.path) else { continue };
            if dur <= 0.0 {
                continue;
            }
            let sync = seg.sync.lock().unwrap();
            // Fallback if the capture never printed its start: the first "frame=" log time,
            // which lags the real first frame by a few tens of ms.
            let host_start = sync
                .host_start
                .or(sync.first_log.map(|n| n as f64 / 1e9 - v_start))
                .unwrap_or(0.0);
            parts.push(Part { path: seg.path.clone(), host_start, v_start, v_end: v_start + dur });
        }
        if parts.is_empty() {
            return Err(anyhow!("Recording failed.\n{}", self.error_tail()));
        }

        let raw = self.dir.join("raw.mp4");
        if parts.len() == 1 {
            std::fs::rename(&parts[0].path, &raw)?;
        } else {
            // `outpoint` ends each part at its last video frame so the next part's frames
            // follow immediately (audio is often a few ms longer). Concat keeps both streams
            // of a part on the same offset, so A/V stay in sync across the joins.
            let mut list = String::new();
            for p in &parts {
                let _ = writeln!(list, "file '{}'\noutpoint {:.6}", p.path.display(), p.v_end);
            }
            let list_path = self.dir.join("segments.txt");
            std::fs::write(&list_path, list)?;
            let out = Command::new("ffmpeg")
                .args(["-hide_banner", "-v", "error", "-nostdin", "-y"])
                .args(["-f", "concat", "-safe", "0", "-i"])
                .arg(&list_path)
                .args(["-c", "copy", "-movflags", "+faststart"])
                .arg(&raw)
                .output()?;
            if !out.status.success() {
                return Err(anyhow!("Could not join segments: {}", String::from_utf8_lossy(&out.stderr).trim()));
            }
            for p in &parts {
                let _ = std::fs::remove_file(&p.path);
            }
            let _ = std::fs::remove_file(&list_path);
        }
        let (width, height, duration) = probe(&raw)?;

        // The webcam track, re-timed onto the joined clip by host time. A failure here
        // keeps the screen recording.
        // Part i starts on the clip at (Σ v_end before it) + v_start_i − v_start_0, the
        // same placement the cursor re-timing below uses; each webcam part runs until the
        // next part starts.
        let cam_parts: Vec<webcam::Part> = parts
            .iter()
            .enumerate()
            .map(|(i, p)| webcam::Part {
                host_first: p.host_start + p.v_start,
                dur: match parts.get(i + 1) {
                    Some(next) => p.v_end - p.v_start + next.v_start,
                    None => p.v_end - p.v_start,
                },
            })
            .collect();
        let cams: Vec<webcam::Segment> = std::mem::take(&mut self.cam_done).into_iter().filter_map(webcam::Capture::finish).collect();
        let webcam = match webcam::align(&self.dir, &cam_parts, &cams) {
            Ok(track) => track,
            Err(e) => {
                eprintln!("Clipper: {e}");
                None
            }
        };
        for c in &cams {
            let _ = std::fs::remove_file(&c.path);
        }

        // Re-time cursor samples onto the clip timeline (0 = first video frame, which is
        // what export and preview use) and convert display points → video pixels.
        // Display points → video pixels: relative to the region (or the display), times the
        // display's scale. Positions outside the region stay outside the frame (not drawn).
        let (source, display) = (self.source, self.display);
        let to_video = |x: f64, y: f64| match source {
            Source::Region(r) => r.cursor_to_video(&display, (x, y)),
            Source::Display(_) => ((x - display.origin.0) * display.scale, (y - display.origin.1) * display.scale),
        };
        let mut samples = Vec::with_capacity(raw_samples.len());
        let mut sync_log = String::from("segment,host_start,video_start,video_end,clip_offset\n");
        let mut offset = 0.0; // clip time of the current part's file time 0
        for (i, p) in parts.iter().enumerate() {
            let _ = writeln!(
                sync_log,
                "{i},{:.6},{:.6},{:.6},{:.6}",
                p.host_start, p.v_start, p.v_end, offset
            );
            for (at, s) in &raw_samples {
                let t = *at as f64 / 1e9 - p.host_start; // file time in this part
                if t >= p.v_start && t < p.v_end {
                    let t = offset + t - parts[0].v_start;
                    let (x, y) = to_video(s.x, s.y);
                    samples.push(Sample { t, x, y, down: s.down });
                }
            }
            offset += p.v_end;
        }
        samples.sort_by(|a, b| a.t.total_cmp(&b.t));
        let _ = std::fs::write(self.dir.join("sync.csv"), sync_log);
        cursor::write_csv(&self.dir.join("cursor.csv"), &samples)?;

        Ok(Recording { dir: self.dir.clone(), raw, width, height, duration, samples, webcam, px_per_pt: display.scale })
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        for seg in self.active.iter_mut().chain(self.finished.iter_mut()) {
            if seg.alive() {
                let _ = seg.child.kill();
            }
        }
    }
}

/// Start time and duration of the first video stream, in file time.
fn video_span(path: &Path) -> Result<(f64, f64)> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0"])
        .args(["-show_entries", "stream=start_time,duration", "-of", "default=nw=1"])
        .arg(path)
        .output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let (mut start, mut dur) = (0.0, None);
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("start_time=") {
            start = v.parse().unwrap_or(0.0);
        } else if let Some(v) = line.strip_prefix("duration=") {
            dur = v.parse().ok();
        }
    }
    Ok((start, dur.ok_or_else(|| anyhow!("Could not read segment duration"))?))
}

/// Where the clip starts inside `raw` (the first video frame's time) and whether it has an
/// audio track. Export shifts audio by the same amount it shifts video, keeping them in sync.
pub fn av_layout(path: &Path) -> (f64, bool) {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "stream=codec_type,start_time", "-of", "csv=p=0"])
        .arg(path)
        .output();
    let text = out.map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
    let (mut v_start, mut audio) = (0.0, false);
    for line in text.lines() {
        match line.split_once(',') {
            Some(("video", t)) => v_start = t.trim().parse().unwrap_or(0.0),
            Some(("audio", _)) => audio = true,
            _ => {}
        }
    }
    (v_start, audio)
}

pub fn probe(path: &Path) -> Result<(u32, u32, f64)> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0"])
        .args(["-show_entries", "stream=width,height:format=duration", "-of", "default=nw=1"])
        .arg(path)
        .output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let (mut w, mut h, mut d) = (0u32, 0u32, 0f64);
    for line in BufReader::new(text.as_bytes()).lines().map_while(Result::ok) {
        if let Some(v) = line.strip_prefix("width=") {
            w = v.parse().unwrap_or(0);
        } else if let Some(v) = line.strip_prefix("height=") {
            h = v.parse().unwrap_or(0);
        } else if let Some(v) = line.strip_prefix("duration=") {
            d = v.parse().unwrap_or(0.0);
        }
    }
    if w == 0 || h == 0 {
        return Err(anyhow!("Could not read recording"));
    }
    Ok((w, h, d))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_host_clock_input_start() {
        let line = "  Duration: N/A, start: 87540.958500, bitrate: 1536 kb/s";
        assert_eq!(parse_input_start(line), Some(87540.9585));
        assert_eq!(parse_input_start("frame=  10 fps=30"), None);
        assert_eq!(parse_input_start("  Duration: N/A, start: N/A, bitrate: N/A"), None);
    }

    /// `clock::now` is in nanoseconds (the unit cursor and segment times are compared in).
    #[test]
    fn clock_counts_nanoseconds() {
        let a = clock::now();
        std::thread::sleep(Duration::from_millis(20));
        let b = clock::now();
        let ms = (b - a) as f64 / 1e6;
        assert!((19.0..60.0).contains(&ms), "{ms} ms");
    }

    /// Records ~5 s with a 1.5 s pause and a muted stretch, then checks the joined file.
    /// Run with `cargo test pause_and_mute -- --ignored --nocapture` (needs Screen Recording
    /// and Microphone permission).
    #[test]
    #[ignore]
    fn pause_and_mute() {
        let mut r = Recorder::start(false).unwrap();
        std::thread::sleep(Duration::from_millis(2500)); // includes ffmpeg spin-up
        r.set_muted(true);
        std::thread::sleep(Duration::from_secs(1));
        r.pause();
        assert!(r.is_paused());
        let paused_at = r.elapsed();
        std::thread::sleep(Duration::from_millis(1500));
        assert_eq!(r.elapsed(), paused_at, "timer must not run while paused");
        r.set_muted(false); // applied to the next segment
        r.resume().unwrap();
        std::thread::sleep(Duration::from_millis(2500));
        r.poll().unwrap();
        let has_audio = r.has_audio();
        let rec = r.stop().unwrap();
        println!("{}x{} {:.2}s, {} samples, audio={has_audio}", rec.width, rec.height, rec.duration, rec.samples.len());

        // The 1.5 s pause is not in the file.
        assert!(rec.duration < 6.0, "duration {} includes the pause", rec.duration);

        // No gap and no frozen stretch in the video timeline.
        let out = Command::new("ffprobe")
            .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "packet=pts_time"])
            .args(["-of", "csv=p=0"])
            .arg(&rec.raw)
            .output()
            .unwrap();
        let mut pts: Vec<f64> =
            String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.trim().parse().ok()).collect();
        pts.sort_by(f64::total_cmp);
        let max_gap = pts.windows(2).map(|w| w[1] - w[0]).fold(0.0, f64::max);
        println!("{} frames, max frame gap {:.3}s", pts.len(), max_gap);
        assert!(max_gap < 2.5 / FPS as f64, "gap of {max_gap}s in video");

        // Cursor track lives on the joined timeline.
        assert!(!rec.samples.is_empty());
        assert!(rec.samples.windows(2).all(|w| w[0].t <= w[1].t));
        assert!(rec.samples.last().unwrap().t <= rec.duration + 0.05);

        if has_audio {
            // Per-0.25 s RMS levels; the muted stretch must be digital silence.
            let out = Command::new("ffmpeg")
                .args(["-hide_banner", "-nostdin", "-i"])
                .arg(&rec.raw)
                .args(["-af", "asetnsamples=12000,astats=metadata=1:reset=1,ametadata=print:key=lavfi.astats.Overall.RMS_level"])
                .args(["-f", "null", "-"])
                .output()
                .unwrap();
            let levels: Vec<String> = String::from_utf8_lossy(&out.stderr)
                .lines()
                .filter_map(|l| l.split("RMS_level=").nth(1).map(str::to_string))
                .collect();
            println!("audio RMS per 0.25s: {levels:?}");
            assert!(levels.iter().any(|l| l == "-inf"), "no silent (muted) stretch");
        }
    }

    /// Records ~6 s with the first camera: the webcam is in the clip from the first frame,
    /// is off between ~2 s and ~3.5 s (toggled), and back on after. Run with
    /// `cargo test webcam_from_first_frame -- --ignored --nocapture` (needs Camera permission).
    #[test]
    #[ignore]
    fn webcam_from_first_frame() {
        let cam = webcam::backend().devices().into_iter().next().expect("no camera");
        let main = cursor::displays()[0].id;
        let mut r = Recorder::start_with(Source::Display(main), false, None, Some(cam)).unwrap();
        while r.is_warming_up() {
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_secs(2));
        r.toggle_webcam();
        assert!(!r.webcam_on());
        std::thread::sleep(Duration::from_millis(1500));
        r.toggle_webcam();
        assert!(r.webcam_on());
        std::thread::sleep(Duration::from_millis(2500));
        r.poll().unwrap();
        let rec = r.stop().unwrap();
        let track = rec.webcam.expect("no webcam track");
        println!("clip {:.2}s, webcam spans {:?}", rec.duration, track.spans);
        assert!(track.spans[0].0 < 0.05, "webcam starts late: {:?}", track.spans);
        assert_eq!(track.spans.len(), 2, "{:?}", track.spans);
        let off = track.spans[0].1..track.spans[1].0;
        assert!((1.2..2.6).contains(&(off.end - off.start)), "off for {:?}", off);
    }

    /// Records a region of the main display with a pause: the video is exactly the region's
    /// pixel size and the cursor samples are relative to the region. Run with
    /// `cargo test region_recording -- --ignored --nocapture` (needs Screen Recording permission).
    #[test]
    #[ignore]
    fn region_recording() {
        let d = cursor::displays()[0];
        let region = crate::region::Region::from_points(&d, (100.0, 120.0), (500.0, 420.0));
        let mut r = Recorder::start_with(Source::Region(region), false, None, None).unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        r.pause();
        std::thread::sleep(Duration::from_millis(500));
        r.resume().unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        let elapsed = r.elapsed().as_secs_f64();
        let rec = r.stop().unwrap();
        println!("region {region:?} -> {}x{} {:.2}s, px/pt {}", rec.width, rec.height, rec.duration, rec.px_per_pt);
        assert_eq!((rec.width, rec.height), (region.w, region.h));
        assert_eq!(rec.px_per_pt, d.scale);
        // Cursor samples map through the region: a sample at the region's global origin is 0,0.
        let (gx, gy) = region.global_origin(&d);
        assert_eq!(region.cursor_to_video(&d, (gx, gy)), (0.0, 0.0));
        // The file is as long as the recorded time (the pause and start-up aren't in it).
        assert!((rec.duration - elapsed).abs() < 0.4, "file {:.2}s vs recorded {elapsed:.2}s", rec.duration);
    }
}
