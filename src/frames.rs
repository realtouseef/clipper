//! Full-rate preview frames around the playhead, decoded on a background thread.
//!
//! The timeline thumbnails (`export::Preview`) are a few frames per second for the whole
//! clip; this cache holds every frame (at the recording's FPS) near where the playhead is
//! and where playback is heading, so scrubbing and frame stepping show the exact frame.
//! The app lists the frames it wants (most urgent first); the worker decodes short runs
//! with a fast keyframe seek (recordings have a keyframe every second) and drops frames
//! that are no longer wanted.

use crate::export::PREVIEW_WIDTH;
use crate::recorder::FPS;
use gpui::RenderImage;
use std::collections::HashMap;
use std::io::Read as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

/// Frames decoded per run (1.5 s).
const CHUNK: u64 = 45;
/// Most frames kept (~1 MB each).
const CAP: usize = 150;

#[derive(Default)]
struct Shared {
    frames: Mutex<HashMap<u64, Arc<RenderImage>>>,
    /// Wanted frame indices, most urgent first.
    want: Mutex<Vec<u64>>,
    wake: Condvar,
    stop: AtomicBool,
    /// Frames dropped from the cache, for the UI thread to free on the GPU.
    evicted: Mutex<Vec<Arc<RenderImage>>>,
}

pub struct FrameCache {
    shared: Arc<Shared>,
}

impl FrameCache {
    /// Starts the worker for `raw` (`width`×`height` source pixels; the clip starts at the
    /// first video frame, at file time `v_start`).
    pub fn new(raw: PathBuf, width: u32, height: u32, v_start: f64, frame_count: u64) -> Self {
        let shared = Arc::new(Shared::default());
        let s = shared.clone();
        let (w, h) = (PREVIEW_WIDTH, ((PREVIEW_WIDTH as f64 * height as f64 / width.max(1) as f64 / 2.0).round() as u32 * 2).max(2));
        std::thread::spawn(move || worker(s, raw, w, h, v_start, frame_count));
        Self { shared }
    }

    /// The frame at index `i` (time `i / FPS`), if decoded.
    pub fn get(&self, i: u64) -> Option<Arc<RenderImage>> {
        self.shared.frames.lock().ok()?.get(&i).cloned()
    }

    /// Sets the frames to have ready, most urgent first.
    pub fn want(&self, frames: Vec<u64>) {
        if let Ok(mut w) = self.shared.want.lock() {
            if *w != frames {
                *w = frames;
                self.shared.wake.notify_one();
            }
        }
    }

    /// Frames to free on the GPU.
    pub fn take_evicted(&self) -> Vec<Arc<RenderImage>> {
        self.shared.evicted.lock().map(|mut e| std::mem::take(&mut *e)).unwrap_or_default()
    }

    /// Stops the worker and returns every frame (to free on the GPU).
    pub fn close(self) -> Vec<Arc<RenderImage>> {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.shared.wake.notify_one();
        let mut all: Vec<_> = self.shared.frames.lock().map(|mut f| f.drain().map(|(_, v)| v).collect()).unwrap_or_default();
        all.extend(self.take_evicted());
        all
    }
}

fn worker(s: Arc<Shared>, raw: PathBuf, w: u32, h: u32, v_start: f64, frame_count: u64) {
    loop {
        // The first wanted frame that isn't decoded yet.
        let next = {
            let mut want = s.want.lock().unwrap();
            loop {
                if s.stop.load(Ordering::SeqCst) {
                    return;
                }
                let have = s.frames.lock().unwrap();
                if let Some(&i) = want.iter().find(|i| **i < frame_count && !have.contains_key(i)) {
                    break i;
                }
                drop(have);
                want = s.wake.wait(want).unwrap();
            }
        };
        let frames = decode(&raw, w, h, v_start, next, CHUNK.min(frame_count - next));
        if s.stop.load(Ordering::SeqCst) {
            return;
        }
        let want: Vec<u64> = s.want.lock().unwrap().clone();
        let mut have = s.frames.lock().unwrap();
        if frames.is_empty() {
            // Past the end of the video: don't ask for it again.
            let filler = have.values().next().cloned().unwrap_or_else(|| blank(w, h));
            have.insert(next, filler);
            continue;
        }
        for (k, img) in frames.into_iter().enumerate() {
            have.entry(next + k as u64).or_insert(img);
        }
        // Over the cap: drop the frames farthest from what's wanted.
        if have.len() > CAP {
            let center = want.first().copied().unwrap_or(next) as i64;
            let mut keys: Vec<u64> = have.keys().copied().filter(|k| !want.contains(k)).collect();
            keys.sort_by_key(|k| std::cmp::Reverse((*k as i64 - center).abs()));
            let excess = have.len() - CAP;
            let mut ev = s.evicted.lock().unwrap();
            for k in keys.into_iter().take(excess) {
                if let Some(img) = have.remove(&k) {
                    ev.push(img);
                }
            }
        }
    }
}

fn blank(w: u32, h: u32) -> Arc<RenderImage> {
    Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(image::RgbaImage::new(w, h))]))
}

/// `count` frames from frame `first` on, as BGRA images `w`×`h`.
fn decode(raw: &std::path::Path, w: u32, h: u32, v_start: f64, first: u64, count: u64) -> Vec<Arc<RenderImage>> {
    let t = first as f64 / FPS as f64;
    let child = Command::new("ffmpeg")
        .args(["-hide_banner", "-v", "error", "-nostdin", "-seek_timestamp", "1", "-ss", &format!("{:.6}", v_start + t), "-i"])
        .arg(raw)
        .args(["-an", "-frames:v", &count.to_string(), "-vf", &format!("fps={FPS},scale={w}:{h}:flags=bilinear"), "-pix_fmt", "bgra", "-f", "rawvideo", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else { return Vec::new() };
    let mut out = Vec::new();
    let size = (w * h * 4) as usize;
    let mut stdout = child.stdout.take().unwrap();
    loop {
        let mut buf = vec![0u8; size];
        if stdout.read_exact(&mut buf).is_err() {
            break;
        }
        let Some(img) = image::RgbaImage::from_raw(w, h, buf) else { break };
        out.push(Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(img)])));
    }
    let _ = child.wait();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frame `i` of the cache is the recording's frame at `i / FPS`: a counter video decodes
    /// to the right frames even when its streams start late in the file.
    #[test]
    fn decodes_the_requested_frames() {
        let dir = std::env::temp_dir().join(format!("clipper-frames-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let raw = dir.join("raw.mp4");
        // Brightness steps up every second (0.0 s dark … 2.x s bright), starting 0.1 s in.
        assert!(Command::new("ffmpeg")
            .args(["-hide_banner", "-v", "error", "-y", "-f", "lavfi", "-i", "color=black:s=320x200:r=30:d=3"])
            .args(["-vf", "geq=lum='floor(T)*100':cb=128:cr=128,setpts=PTS+0.1/TB", "-c:v", "libx264", "-g", "30", "-pix_fmt", "yuv420p"])
            .arg(&raw)
            .status()
            .unwrap()
            .success());
        let (v0, _) = crate::recorder::av_layout(&raw);
        let cache = FrameCache::new(raw, 320, 200, v0, 90);
        cache.want(vec![45, 75]);
        let luma = |img: &RenderImage| {
            let bytes = img.as_bytes(0).unwrap();
            bytes.chunks(4).map(|p| p[1] as f64).sum::<f64>() / (bytes.len() / 4) as f64
        };
        let start = std::time::Instant::now();
        while (cache.get(45).is_none() || cache.get(75).is_none()) && start.elapsed().as_secs() < 20 {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let (a, b) = (cache.get(45).expect("frame 45"), cache.get(75).expect("frame 75"));
        assert!((luma(&a) - 100.0).abs() < 20.0, "frame 45 (1.5 s) luma {}", luma(&a));
        assert!((luma(&b) - 200.0).abs() < 20.0, "frame 75 (2.5 s) luma {}", luma(&b));
        // The frames just before and after a second boundary.
        cache.want(vec![29, 30]);
        while (cache.get(29).is_none() || cache.get(30).is_none()) && start.elapsed().as_secs() < 20 {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(luma(&cache.get(29).unwrap()) < 50.0 && luma(&cache.get(30).unwrap()) > 50.0, "frame-exact around 1.0 s");
        drop(cache.close());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
