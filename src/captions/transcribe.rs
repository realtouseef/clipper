//! Speech to text with whisper.cpp, after recording (never during it), on a background thread.
//!
//! 1. The mic track is decoded at 16 kHz mono f32 on the clip timeline (`audio::decode_at`):
//!    pauses are already cut out and it's shifted like the export, so sample time is video time.
//! 2. Muted stretches (digital silence) are dropped, and Silero VAD (whisper.cpp's built-in)
//!    drops other silence. The speech pieces are packed into one buffer, remembering where
//!    each came from (`Piece`).
//! 3. whisper.cpp transcribes the packed buffer with token timestamps; tokens are joined into
//!    words, and each word's time is mapped back through the pieces to clip time.

use super::Word;
use crate::models::{self, Model};
use anyhow::{Result, anyhow};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::JoinHandle;

pub const RATE: u32 = 16_000;

/// Languages offered for multilingual models: (whisper code, name). "auto" detects.
pub const LANGUAGES: [(&str, &str); 18] = [
    ("auto", "Auto-detect"),
    ("en", "English"),
    ("ur", "Urdu"),
    ("hi", "Hindi"),
    ("ar", "Arabic"),
    ("es", "Spanish"),
    ("fr", "French"),
    ("de", "German"),
    ("it", "Italian"),
    ("pt", "Portuguese"),
    ("ru", "Russian"),
    ("tr", "Turkish"),
    ("id", "Indonesian"),
    ("bn", "Bengali"),
    ("zh", "Chinese"),
    ("ja", "Japanese"),
    ("ko", "Korean"),
    ("nl", "Dutch"),
];

/// Threads to use by default: all cores but one, so the app stays responsive.
pub fn default_threads() -> usize {
    std::thread::available_parallelism().map_or(2, |n| n.get()).saturating_sub(1).max(1)
}

pub fn max_threads() -> usize {
    std::thread::available_parallelism().map_or(2, |n| n.get())
}

/// A stretch of speech: `len` seconds that start at `src` on the clip timeline and at `dst`
/// in the packed buffer whisper sees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Piece {
    pub src: f64,
    pub dst: f64,
    pub len: f64,
}

/// Silence put between packed pieces so words don't run together.
const GAP: f64 = 0.2;

/// Packs `spans` (clip-time (start, end) of speech) back to back with `GAP` between them.
pub fn pack(spans: &[(f64, f64)]) -> Vec<Piece> {
    let mut dst = 0.0;
    spans
        .iter()
        .filter(|(a, b)| b > a)
        .map(|&(a, b)| {
            let p = Piece { src: a, dst, len: b - a };
            dst += p.len + GAP;
            p
        })
        .collect()
}

/// Packed-buffer time → clip time. A time in a gap between pieces snaps to the nearer edge.
pub fn map_time(pieces: &[Piece], t: f64) -> f64 {
    let Some(first) = pieces.first() else { return t };
    if t <= first.dst {
        return first.src;
    }
    for (i, p) in pieces.iter().enumerate() {
        if t <= p.dst + p.len {
            return p.src + (t - p.dst);
        }
        if let Some(next) = pieces.get(i + 1) {
            if t < next.dst {
                // In the gap: closer to the end of this piece or the start of the next.
                return if t - (p.dst + p.len) < next.dst - t { p.src + p.len } else { next.src };
            }
        }
    }
    let last = pieces.last().unwrap();
    last.src + last.len
}

/// A word's packed-buffer (start, end) → clip time. The end stays inside the piece its
/// start is in, so a word never stretches across removed silence.
pub fn map_word(pieces: &[Piece], start: f64, end: f64) -> (f64, f64) {
    let s = map_time(pieces, start);
    let piece_end = pieces
        .iter()
        .find(|p| s >= p.src - 1e-9 && s <= p.src + p.len + 1e-9)
        .map_or(f64::INFINITY, |p| p.src + p.len);
    (s, map_time(pieces, end).clamp(s, piece_end.max(s)))
}

/// Clip-time spans that aren't digital silence (a muted mic records exact zeros). Zero runs
/// shorter than `min_gap` seconds are kept (ordinary zero crossings and tiny dropouts).
pub fn unmuted_spans(samples: &[f32], rate: u32, min_gap: f64) -> Vec<(f64, f64)> {
    let min_run = (min_gap * rate as f64) as usize;
    let mut spans = Vec::new();
    let (mut start, mut zeros) = (None::<usize>, 0usize);
    for (i, &s) in samples.iter().enumerate() {
        if s == 0.0 {
            zeros += 1;
            if zeros == min_run {
                if let Some(a) = start.take() {
                    let end = i + 1 - min_run;
                    if end > a {
                        spans.push((a, end));
                    }
                }
            }
        } else {
            zeros = 0;
            start.get_or_insert(i);
        }
    }
    if let Some(a) = start {
        spans.push((a, samples.len()));
    }
    spans.into_iter().map(|(a, b)| (a as f64 / rate as f64, b as f64 / rate as f64)).collect()
}

/// Intersection of two sorted span lists.
fn intersect(a: &[(f64, f64)], b: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    for &(a0, a1) in a {
        for &(b0, b1) in b {
            let (s, e) = (a0.max(b0), a1.min(b1));
            if e > s {
                out.push((s, e));
            }
        }
    }
    out.sort_by(|x, y| x.0.total_cmp(&y.0));
    out
}

/// What to transcribe with.
#[derive(Clone, Debug)]
pub struct Settings {
    pub model: &'static Model,
    /// Whisper language code, or "auto".
    pub language: String,
    pub threads: usize,
}

/// Progress callback, 0..=1.
pub type Progress = Arc<dyn Fn(f32) + Send + Sync>;

/// Speech-to-text backend; whisper.cpp on the CPU, or the GPU with the `gpu` feature.
pub trait Engine: Send {
    /// Words with times relative to the start of `audio` (16 kHz mono).
    fn transcribe(&self, audio: &[f32], settings: &Settings, progress: Progress, cancel: Arc<AtomicBool>) -> Result<Vec<Word>>;
}

pub struct Whisper;

impl Engine for Whisper {
    fn transcribe(
        &self,
        audio: &[f32],
        settings: &Settings,
        progress: Progress,
        cancel: Arc<AtomicBool>,
    ) -> Result<Vec<Word>> {
        use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};
        let mut cp = WhisperContextParameters::default();
        cp.use_gpu(cfg!(feature = "gpu"));
        let path = settings.model.path();
        let ctx = WhisperContext::new_with_params(&path, cp)
            .map_err(|e| anyhow!("Could not load {}: {e}", settings.model.name))?;
        let mut state = ctx.create_state().map_err(|e| anyhow!("whisper: {e}"))?;
        let mut p = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        p.set_n_threads(settings.threads.max(1) as i32);
        let lang = if settings.model.english_only { "en" } else { settings.language.as_str() };
        p.set_language(Some(lang));
        p.set_translate(false);
        p.set_token_timestamps(true);
        p.set_split_on_word(true);
        p.set_no_context(true);
        p.set_suppress_blank(true);
        p.set_print_progress(false);
        p.set_print_realtime(false);
        p.set_print_special(false);
        p.set_print_timestamps(false);
        // whisper.cpp reports 0..=100.
        p.set_progress_callback_safe(move |pct: i32| progress(pct as f32 / 100.0));
        // whisper-rs 0.16's `set_abort_callback_safe` casts its boxed closure to the wrong type
        // (whisper.cpp then reads garbage and aborts), so point the C callback straight at the
        // cancel flag. `cancel` outlives `full` below.
        unsafe extern "C" fn abort(data: *mut std::ffi::c_void) -> bool {
            unsafe { (*(data as *const AtomicBool)).load(Ordering::Relaxed) }
        }
        unsafe {
            p.set_abort_callback(Some(abort));
            p.set_abort_callback_user_data(Arc::as_ptr(&cancel) as *mut std::ffi::c_void);
        }
        state.full(p, audio).map_err(|e| {
            if cancel.load(Ordering::Relaxed) { anyhow!(CANCELLED) } else { anyhow!("Transcription failed: {e}") }
        })?;
        if cancel.load(Ordering::Relaxed) {
            return Err(anyhow!(CANCELLED));
        }

        // Tokens → words: a token starting with a space begins a new word.
        let eot = ctx.token_eot();
        let mut words: Vec<Word> = Vec::new();
        for seg in state.as_iter() {
            for i in 0..seg.n_tokens() {
                let Some(tok) = seg.get_token(i) else { continue };
                let data = tok.token_data();
                if data.id >= eot {
                    continue; // special tokens (timestamps, language, end)
                }
                let text = tok.to_str_lossy().map(|s| s.to_string()).unwrap_or_default();
                if text.trim().is_empty() {
                    continue;
                }
                let (t0, t1) = (data.t0 as f64 / 100.0, data.t1 as f64 / 100.0);
                match words.last_mut() {
                    Some(w) if !text.starts_with(' ') => {
                        w.text.push_str(&text);
                        w.end = w.end.max(t1);
                    }
                    _ => words.push(Word { start: t0, end: t1.max(t0), text: text.trim().to_string() }),
                }
            }
        }
        Ok(words)
    }
}

pub const CANCELLED: &str = "Transcription cancelled";

/// Clip-time spans of speech: Silero VAD on the unmuted audio (or just the unmuted audio if
/// the VAD model isn't there).
fn speech_spans(samples: &[f32], threads: usize) -> Vec<(f64, f64)> {
    let unmuted = unmuted_spans(samples, RATE, 0.3);
    if !models::VAD.installed() {
        return unmuted;
    }
    use whisper_rs::{WhisperVadContext, WhisperVadContextParams, WhisperVadParams};
    let mut cp = WhisperVadContextParams::new();
    cp.set_n_threads(threads.max(1) as i32);
    cp.set_use_gpu(false);
    let Ok(mut vad) = WhisperVadContext::new(&models::VAD.path().to_string_lossy(), cp) else { return unmuted };
    let mut vp = WhisperVadParams::new();
    vp.set_speech_pad(200); // ms around each stretch of speech
    vp.set_min_silence_duration(300);
    let Ok(segs) = vad.segments_from_samples(vp, samples) else { return unmuted };
    let speech: Vec<(f64, f64)> = segs.map(|s| (s.start as f64 / 100.0, s.end as f64 / 100.0)).collect();
    intersect(&speech, &unmuted)
}

/// Produces the audio to transcribe (e.g. decodes the recording's mic track).
pub type Source = Box<dyn FnOnce() -> Result<Vec<f32>> + Send>;

/// The audio of a recording, for `Job::start`.
pub fn recording_audio(rec: Arc<crate::recorder::Recording>) -> Source {
    Box::new(move || crate::audio::decode_at(&rec, RATE)?.ok_or_else(|| anyhow!("This recording has no microphone audio.")))
}

/// A transcription running on a background thread.
pub struct Job {
    /// 0..=1000.
    pub progress: Arc<AtomicU32>,
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<Result<Vec<Word>>>>,
}

impl Job {
    /// Transcribes the audio from `source` (16 kHz mono on the clip timeline, produced on the
    /// job's thread) into clip-time words.
    pub fn start(source: Source, settings: Settings, engine: Box<dyn Engine>) -> Self {
        // Keep whisper.cpp / ggml quiet (their logs go nowhere without a log backend).
        whisper_rs::install_logging_hooks();
        let progress = Arc::new(AtomicU32::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (prog, c) = (progress.clone(), cancel.clone());
        let handle = std::thread::spawn(move || {
            let samples = source()?;
            prog.store(20, Ordering::Relaxed);
            let spans = speech_spans(&samples, settings.threads);
            prog.store(50, Ordering::Relaxed);
            if spans.is_empty() {
                return Ok(Vec::new());
            }
            let pieces = pack(&spans);
            let gap = vec![0.0f32; (GAP * RATE as f64) as usize];
            let mut packed = Vec::with_capacity(samples.len());
            for p in &pieces {
                let a = ((p.src * RATE as f64) as usize).min(samples.len());
                let b = (((p.src + p.len) * RATE as f64) as usize).min(samples.len());
                packed.extend_from_slice(&samples[a..b]);
                packed.extend_from_slice(&gap);
            }
            // whisper.cpp wants at least a second of audio.
            packed.resize(packed.len().max(RATE as usize + 1), 0.0);
            let p2 = prog.clone();
            let report: Progress = Arc::new(move |f: f32| p2.store(50 + (f.clamp(0.0, 1.0) * 940.0) as u32, Ordering::Relaxed));
            let words = engine.transcribe(&packed, &settings, report, c)?;
            prog.store(1000, Ordering::Relaxed);
            Ok(words
                .into_iter()
                .map(|w| {
                    let (start, end) = map_word(&pieces, w.start, w.end);
                    Word { start, end, ..w }
                })
                .collect())
        });
        Self { progress, cancel, handle: Some(handle) }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn fraction(&self) -> f32 {
        self.progress.load(Ordering::Relaxed) as f32 / 1000.0
    }

    pub fn poll(&mut self) -> Option<Result<Vec<Word>>> {
        if self.handle.as_ref().is_some_and(|h| h.is_finished()) {
            let h = self.handle.take()?;
            return Some(h.join().unwrap_or_else(|_| Err(anyhow!("transcription crashed"))));
        }
        None
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Speech at 1–2 s and 5–6.5 s of the clip; the packed buffer has them back to back. Word
    /// times in the packed buffer map back to clip time, including across the removed gap.
    #[test]
    fn maps_packed_time_back_across_gaps() {
        let pieces = pack(&[(1.0, 2.0), (5.0, 6.5)]);
        assert_eq!(pieces[1], Piece { src: 5.0, dst: 1.0 + GAP, len: 1.5 });
        assert!((map_time(&pieces, 0.25) - 1.25).abs() < 1e-9);
        assert!((map_time(&pieces, 1.0 + GAP + 0.5) - 5.5).abs() < 1e-9);
        // Inside the inserted gap: snaps to the nearer piece edge.
        assert!((map_time(&pieces, 1.0 + GAP * 0.25) - 2.0).abs() < 1e-9);
        assert!((map_time(&pieces, 1.0 + GAP * 0.9) - 5.0).abs() < 1e-9);
        // A word whose end falls past its piece is cut at the piece's end.
        assert_eq!(map_word(&pieces, 0.8, 1.0 + GAP + 0.3), (1.8, 2.0));
        // Past the end clamps.
        assert!((map_time(&pieces, 99.0) - 6.5).abs() < 1e-9);
        // Monotonic.
        let ts: Vec<f64> = (0..400).map(|i| map_time(&pieces, i as f64 * 0.01)).collect();
        assert!(ts.windows(2).all(|w| w[1] >= w[0]));
    }

    /// The clip timeline already has recording pauses cut out (segments joined back to back):
    /// a word spoken just after a resume, at clip time 3.2 s, packs and maps back to 3.2 s,
    /// not to its wall-clock time.
    #[test]
    fn timestamps_survive_pause_gaps() {
        // Two recorded segments of 3 s joined at clip time 3.0 (the pause between them is gone).
        // Speech: 2.0–2.9 (end of segment 1) and 3.1–4.0 (start of segment 2).
        let pieces = pack(&[(2.0, 2.9), (3.1, 4.0)]);
        let word_in_packed = pieces[1].dst + 0.1;
        assert!((map_time(&pieces, word_in_packed) - 3.2).abs() < 1e-9);
    }

    #[test]
    fn muted_stretches_are_skipped() {
        let rate = 1000;
        // 1 s of signal, 1 s muted (exact zeros), 1 s of signal with a 50 ms zero dropout.
        let mut s: Vec<f32> = (0..1000).map(|i| ((i as f32) * 0.3).sin() * 0.5 + 0.01).collect();
        s.extend(std::iter::repeat_n(0.0, 1000));
        s.extend((0..1000).map(|i| if (400..450).contains(&i) { 0.0 } else { 0.2 }));
        let spans = unmuted_spans(&s, rate, 0.3);
        assert_eq!(spans.len(), 2, "{spans:?}");
        assert!((spans[0].0 - 0.0).abs() < 1e-3 && (spans[0].1 - 1.0).abs() < 1e-3);
        assert!((spans[1].0 - 2.0).abs() < 1e-3 && (spans[1].1 - 3.0).abs() < 1e-3);
        assert_eq!(intersect(&[(0.5, 2.5)], &spans), vec![(0.5, 1.0), (2.0, 2.5)]);
    }

    #[test]
    fn thread_count_leaves_a_core() {
        assert!(default_threads() >= 1);
        assert!(default_threads() < max_threads() || max_threads() == 1);
    }

    /// Downloads Tiny (English) if needed, then transcribes generated speech with a 2 s muted
    /// stretch in the middle. Run with `cargo test real_transcription -- --ignored --nocapture`
    /// (uses the network for the model download).
    #[test]
    #[ignore]
    fn real_transcription() {
        let model = models::by_id("tiny.en").unwrap();
        if !model.installed() || !models::VAD.installed() {
            let mut d = models::Download::start(model);
            let r = loop {
                if let Some(r) = d.poll() {
                    break r;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            };
            r.unwrap();
        }
        let dir = std::env::temp_dir().join(format!("clipper-stt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let speech = |text: &str, name: &str| -> Vec<f32> {
            let aiff = dir.join(format!("{name}.aiff"));
            assert!(std::process::Command::new("say").arg("-o").arg(&aiff).arg(text).status().unwrap().success());
            let out = std::process::Command::new("ffmpeg")
                .args(["-hide_banner", "-v", "error", "-i"])
                .arg(&aiff)
                .args(["-ac", "1", "-ar", "16000", "-f", "f32le", "-"])
                .output()
                .unwrap();
            out.stdout.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect()
        };
        let a = speech("Hello and welcome. Today I will show you the new caption feature.", "a");
        let b = speech("This sentence comes after the muted part.", "b");
        let gap_start = a.len() as f64 / 16000.0;
        let mut samples = a;
        samples.extend(std::iter::repeat_n(0.0, 32000)); // muted mic: exact zeros
        let b_start = samples.len() as f64 / 16000.0;
        samples.extend(b);
        let settings = Settings { model, language: "en".into(), threads: default_threads() };
        let t0 = std::time::Instant::now();
        let mut job = Job::start(Box::new(move || Ok(samples)), settings, Box::new(Whisper));
        let words = loop {
            if let Some(r) = job.poll() {
                break r.unwrap();
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        let text: Vec<String> = words.iter().map(|w| w.text.to_lowercase()).collect();
        println!("{:.1}s: {}", t0.elapsed().as_secs_f64(), text.join(" "));
        for w in &words {
            println!("  {:6.2}-{:6.2} {}", w.start, w.end, w.text);
        }
        assert!(text.iter().any(|w| w.contains("welcome")));
        assert!(text.iter().any(|w| w.contains("muted")));
        // Nothing inside the muted stretch; words after it are on the right side of it.
        assert!(words.iter().all(|w| w.start < gap_start + 0.1 || w.start >= b_start - 0.25), "a word inside the muted gap");
        let after = words.iter().find(|w| w.text.to_lowercase().contains("comes")).unwrap();
        let feature = words.iter().find(|w| w.text.to_lowercase().contains("feature")).unwrap();
        assert!(feature.end <= gap_start + 0.3, "word stretched across the gap: {feature:?}");
        assert!(after.start >= b_start - 0.25, "word after the gap at {} < {b_start}", after.start);
        let caps = crate::captions::segment::segment(&words, crate::captions::segment::Rules::for_ratio(crate::layout::Ratio::Portrait));
        for c in &caps {
            println!("[{:.2}-{:.2}] {}", c.start, c.end, c.text.replace('\n', " / "));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
