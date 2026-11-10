//! Audio for the editor preview: the recording's mic track decoded to PCM once, played
//! from any point on the clip timeline.

use crate::recorder::{self, Recording};
use anyhow::{Context as _, Result, anyhow};
use rodio::buffer::SamplesBuffer;
use rodio::{OutputStream, OutputStreamHandle, Sink, Source};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

pub const RATE: u32 = 48_000;

/// Mono PCM on the clip timeline: sample `i` plays at `i / RATE` seconds.
pub struct Track {
    pub samples: Arc<Vec<f32>>,
}

/// Decodes the mic track with the same alignment the export uses (shifted by the video start,
/// gaps filled), so the preview sounds like the exported file. `None` when there is no audio.
pub fn decode(rec: &Recording) -> Result<Option<Track>> {
    Ok(decode_at(rec, RATE)?.map(|samples| Track { samples: Arc::new(samples) }))
}

/// The mic track as mono f32 PCM at `rate` Hz on the clip timeline (sample `i` at `i / rate`
/// seconds of the final video), or `None` when there is no audio.
pub fn decode_at(rec: &Recording, rate: u32) -> Result<Option<Vec<f32>>> {
    let (v0, has_audio) = recorder::av_layout(&rec.raw);
    if !has_audio {
        return Ok(None);
    }
    let dur = format!("{:.3}", rec.duration.max(0.1));
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-v", "error", "-nostdin", "-i"])
        .arg(&rec.raw)
        .args([
            "-filter_complex",
            &format!(
                "[0:a]asetpts=PTS-{v0:.6}/TB,aresample=async=1:min_hard_comp=0.01:first_pts=0,atrim=end={dur}[a]"
            ),
        ])
        .args(["-map", "[a]", "-ac", "1", "-ar", &rate.to_string(), "-f", "f32le", "-"])
        .output()
        .context("Failed to launch ffmpeg")?;
    if !out.status.success() {
        return Err(anyhow!("Could not decode audio: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(Some(out.stdout.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect()))
}

/// Plays a `Track` on the default output. Lives on the main thread (the output stream is
/// not `Send`); the device is opened on first use.
#[derive(Default)]
pub struct Player {
    output: Option<(OutputStream, OutputStreamHandle)>,
    sink: Option<Sink>,
}

impl Player {
    /// Starts playback at clip time `t`, replacing whatever was playing.
    pub fn play_from(&mut self, track: &Track, t: f64) {
        self.stop();
        if self.output.is_none() {
            match OutputStream::try_default() {
                Ok(o) => self.output = Some(o),
                Err(e) => {
                    eprintln!("Clipper: no audio output ({e})");
                    return;
                }
            }
        }
        let Some((_, handle)) = &self.output else { return };
        let Ok(sink) = Sink::try_new(handle) else { return };
        let start = ((t.max(0.0) * RATE as f64) as usize).min(track.samples.len());
        if start >= track.samples.len() {
            return;
        }
        // The track can be minutes long; copy only from the start point.
        let buf = SamplesBuffer::new(1, RATE, track.samples[start..].to_vec());
        sink.append(buf.fade_in(Duration::from_millis(8)));
        sink.play();
        self.sink = Some(sink);
    }

    pub fn stop(&mut self) {
        if let Some(sink) = self.sink.take() {
            sink.stop();
        }
    }

    pub fn is_playing(&self) -> bool {
        self.sink.as_ref().is_some_and(|s| !s.empty() && !s.is_paused())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A beep at 1.0 s in a raw whose streams start late must land at 1.0 s on the clip timeline.
    #[test]
    fn decode_aligns_to_clip_timeline() {
        let dir = std::env::temp_dir().join(format!("clipper-audio-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let raw = dir.join("raw.mp4");
        assert!(Command::new("ffmpeg")
            .args(["-hide_banner", "-v", "error", "-y"])
            .args(["-f", "lavfi", "-i", "color=black:s=320x240:r=30:d=2"])
            .args(["-f", "lavfi", "-i", "sine=f=1000:r=48000:d=2"])
            .args([
                "-filter_complex",
                "[0:v]setpts=PTS+0.1/TB[v];[1:a]volume=enable='not(between(t,1,1.2))':volume=0,adelay=100:all=1[a]",
            ])
            .args(["-map", "[v]", "-map", "[a]", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac"])
            .arg(&raw)
            .status()
            .unwrap()
            .success());
        let rec = Recording { dir: dir.clone(), raw, width: 320, height: 240, duration: 2.0, samples: Vec::new(), webcam: None, px_per_pt: 1.0 };
        let track = decode(&rec).unwrap().expect("audio track");
        let s = &track.samples;
        assert!((s.len() as f64 / RATE as f64 - 2.0).abs() < 0.05, "length {}", s.len());
        let onset = s.iter().position(|v| v.abs() > 0.05).unwrap() as f64 / RATE as f64;
        println!("beep at {onset:.3}s");
        assert!((onset - 1.0).abs() < 0.04, "beep at {onset}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
