//! The mic track cut to the edit, shared by the preview player and the export so both sound
//! the same. Each cut gets a 15 ms crossfade centered on it: the audio before the cut runs
//! 7.5 ms past it while fading out, the audio after starts 7.5 ms early while fading in.
//! The fades sum to one, and every kept sample stays at its exact output time, so audio
//! and video stay in sync after any number of cuts.

use super::EditList;
use anyhow::Result;
use std::io::Write as _;
use std::path::Path;

/// Crossfade length at each cut, in seconds.
pub const CROSSFADE: f64 = 0.015;

/// PCM on the raw timeline (sample `i` at `i / rate` s) → PCM on the output timeline.
pub fn cut(src: &[f32], rate: u32, edit: &EditList) -> Vec<f32> {
    let r = rate as f64;
    let n = (edit.output_duration() * r).round() as usize;
    let mut out = vec![0.0f32; n];
    let runs = edit.runs();
    let half = ((CROSSFADE / 2.0) * r).round() as i64;
    let mut acc = 0.0;
    for (k, &(a, b)) in runs.iter().enumerate() {
        let o0 = (acc * r).round() as i64;
        let len = ((acc + (b - a)) * r).round() as i64 - o0;
        let s0 = (a * r).round() as i64;
        acc += b - a;
        let hl = if k > 0 { half } else { 0 };
        let hr = if k + 1 < runs.len() { half } else { 0 };
        for i in -hl..len + hr {
            let (oi, si) = (o0 + i, s0 + i);
            if oi < 0 || oi as usize >= n {
                continue;
            }
            let v = if si >= 0 && (si as usize) < src.len() { src[si as usize] } else { 0.0 };
            let mut g = 1.0f64;
            if i < hl {
                // Fade in across [-hl, hl).
                g *= (std::f64::consts::FRAC_PI_2 * (i + hl) as f64 / (2 * hl) as f64).sin().powi(2);
            }
            if i >= len - hr {
                // Fade out across [len - hr, len + hr).
                g *= (std::f64::consts::FRAC_PI_2 * (i - (len - hr)) as f64 / (2 * hr) as f64).cos().powi(2);
            }
            out[oi as usize] += v * g as f32;
        }
    }
    out
}

/// A mono 32-bit float WAV, for the export's audio input.
pub fn write_wav(path: &Path, samples: &[f32], rate: u32) -> Result<()> {
    let data = samples.len() as u32 * 4;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&3u16.to_le_bytes())?; // IEEE float
    f.write_all(&1u16.to_le_bytes())?; // mono
    f.write_all(&rate.to_le_bytes())?;
    f.write_all(&(rate * 4).to_le_bytes())?;
    f.write_all(&4u16.to_le_bytes())?;
    f.write_all(&32u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data.to_le_bytes())?;
    for s in samples {
        f.write_all(&s.to_le_bytes())?;
    }
    f.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn tone(secs: f64, f: impl Fn(f64) -> f32) -> Vec<f32> {
        (0..(secs * RATE as f64) as usize).map(|i| f(i as f64 / RATE as f64)).collect()
    }

    /// Largest jump between neighboring samples in `s[from..to]`.
    fn max_step(s: &[f32], from: usize, to: usize) -> f32 {
        s[from..to].windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max)
    }

    #[test]
    fn no_pop_at_cuts_and_stays_in_sync() {
        // A low tone whose phase is opposite on either side of the removed second: a hard
        // cut there would jump by about 1.6.
        let src = tone(4.0, |t| if t < 1.5 { (t * 2.0 * std::f64::consts::PI * 50.0).sin() as f32 * 0.8 } else { -(t * 2.0 * std::f64::consts::PI * 50.0).sin() as f32 * 0.8 });
        let edit = EditList { segments: vec![(0.0, 1.005), (2.0, 4.0)] };
        let out = cut(&src, RATE, &edit);
        assert_eq!(out.len(), (edit.output_duration() * RATE as f64).round() as usize);
        let at = (1.005 * RATE as f64) as usize;
        let hard = (src[at - 1] - src[(2.0 * RATE as f64) as usize]).abs();
        let smooth = max_step(&out, at - 600, at + 600);
        let normal = max_step(&src, 10_000, 20_000);
        assert!(hard > 0.5, "the test needs a discontinuity ({hard})");
        assert!(smooth < normal * 4.0, "pop at the cut: step {smooth} vs normal {normal}");

        // Away from the crossfade, every kept sample is at its exact output time.
        for (o, s) in [(0.5, 0.5), (1.5, 2.495), (2.9, 3.895)] {
            let (oi, si) = ((o * RATE as f64) as usize, (s * RATE as f64) as usize);
            assert!((out[oi] - src[si]).abs() < 1e-3, "output {o}s ≠ raw {s}s");
        }
    }

    #[test]
    fn plain_splits_and_unity_gain() {
        let src = tone(2.0, |_| 0.5);
        // A split with nothing removed: identical audio.
        let split = EditList { segments: vec![(0.0, 1.0), (1.0, 2.0)] };
        assert_eq!(cut(&src, RATE, &split), src);
        // A real cut through constant audio: the fades sum to one.
        let edit = EditList { segments: vec![(0.0, 0.8), (1.2, 2.0)] };
        let out = cut(&src, RATE, &edit);
        assert!(out.iter().all(|v| (v - 0.5).abs() < 1e-3), "crossfade changes the level");
    }

    #[test]
    fn writes_a_wav_ffmpeg_reads() {
        let dir = std::env::temp_dir().join(format!("clipper-wav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.wav");
        write_wav(&p, &tone(0.5, |t| (t * 2000.0).sin() as f32), RATE).unwrap();
        let probe = std::process::Command::new("ffprobe")
            .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
            .arg(&p)
            .output()
            .unwrap();
        let dur: f64 = String::from_utf8_lossy(&probe.stdout).trim().parse().unwrap();
        assert!((dur - 0.5).abs() < 0.01, "duration {dur}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
