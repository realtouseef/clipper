//! Local auto captions: words from speech-to-text, grouped into caption lines, edited in the
//! settings panel, drawn by the compositor, and optionally saved as .srt.
//!
//! - `transcribe`: whisper.cpp on the recorded mic track (after Stop, on a background thread).
//! - `segment`: words → captions (line length, pauses, punctuation, display time).
//! - `render`: caption style and rasterization, shared by the preview and the export.

pub mod render;
pub mod segment;
pub mod transcribe;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::path::Path;

/// One spoken word on the clip timeline (seconds).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Word {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// One caption: up to two lines (split by '\n'), with per-word timing for highlighting.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Caption {
    pub start: f64,
    pub end: f64,
    pub text: String,
    pub words: Vec<Word>,
}

impl Caption {
    /// Builds a caption from words, wrapping its text into lines of at most `max_chars`.
    pub fn from_words(words: Vec<Word>, max_chars: usize) -> Self {
        let start = words.first().map_or(0.0, |w| w.start);
        let end = words.last().map_or(start, |w| w.end);
        let text = segment::wrap(&words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(), max_chars).join("\n");
        Self { start, end, text, words }
    }

    /// Replaces the text (an inline edit). Word timing is redistributed over the caption's
    /// span by length, so word highlighting keeps working.
    pub fn set_text(&mut self, text: &str, max_chars: usize) {
        let tokens: Vec<&str> = text.split_whitespace().collect();
        let total: usize = tokens.iter().map(|t| t.chars().count().max(1)).sum::<usize>().max(1);
        let span = (self.end - self.start).max(0.0);
        let mut t = self.start;
        self.words = tokens
            .iter()
            .map(|tok| {
                let d = span * tok.chars().count().max(1) as f64 / total as f64;
                let w = Word { start: t, end: t + d, text: tok.to_string() };
                t += d;
                w
            })
            .collect();
        self.text = segment::wrap(&tokens, max_chars).join("\n");
    }

    /// Index of the word being spoken at `t`.
    pub fn word_at(&self, t: f64) -> Option<usize> {
        self.words.iter().rposition(|w| w.start <= t)
    }

    /// Splits at the word nearest the middle; `None` for a single word.
    pub fn split(&self, max_chars: usize) -> Option<(Caption, Caption)> {
        if self.words.len() < 2 {
            return None;
        }
        let mid = (self.start + self.end) / 2.0;
        let at = self.words.iter().position(|w| w.start >= mid).unwrap_or(self.words.len() / 2).clamp(1, self.words.len() - 1);
        let (a, b) = self.words.split_at(at);
        let mut first = Caption::from_words(a.to_vec(), max_chars);
        let mut second = Caption::from_words(b.to_vec(), max_chars);
        first.start = self.start;
        second.end = self.end;
        first.end = first.end.min(second.start);
        Some((first, second))
    }

    /// Joins `next` onto this caption.
    pub fn merge(&self, next: &Caption, max_chars: usize) -> Caption {
        let mut words = self.words.clone();
        words.extend(next.words.iter().cloned());
        let mut c = Caption::from_words(words, max_chars);
        c.start = self.start.min(next.start);
        c.end = self.end.max(next.end);
        c
    }
}

/// The captions of a recording, saved next to it as `captions.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Captions {
    pub lines: Vec<Caption>,
    /// Model and language used, for the record.
    pub model: String,
    pub language: String,
}

impl Captions {
    pub fn at(&self, t: f64) -> Option<usize> {
        self.lines.iter().position(|c| t >= c.start && t < c.end)
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        std::fs::write(dir.join("captions.json"), serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }

    #[allow(dead_code)] // reopening a session's captions
    pub fn load(dir: &Path) -> Option<Self> {
        serde_json::from_slice(&std::fs::read(dir.join("captions.json")).ok()?).ok()
    }

    /// SubRip text: numbered cues, `HH:MM:SS,mmm --> HH:MM:SS,mmm`, the caption lines, a
    /// blank line.
    pub fn to_srt(&self) -> String {
        let mut out = String::new();
        for (i, c) in self.lines.iter().enumerate() {
            let _ = write!(out, "{}\n{} --> {}\n{}\n\n", i + 1, srt_time(c.start), srt_time(c.end), c.text.trim());
        }
        out
    }
}

fn srt_time(t: f64) -> String {
    let ms = (t.max(0.0) * 1000.0).round() as u64;
    format!("{:02}:{:02}:{:02},{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
}

/// "0:07.4" for the caption list.
pub fn short_time(t: f64) -> String {
    format!("{}:{:04.1}", (t / 60.0) as u32, t % 60.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(start: f64, end: f64, text: &str) -> Word {
        Word { start, end, text: text.into() }
    }

    #[test]
    fn srt_format() {
        let caps = Captions {
            lines: vec![
                Caption::from_words(vec![w(1.0, 1.5, "Hello"), w(1.5, 2.25, "world.")], 32),
                Caption {
                    start: 3661.007,
                    end: 3663.5,
                    text: "Two\nlines".into(),
                    words: vec![],
                },
            ],
            ..Default::default()
        };
        assert_eq!(
            caps.to_srt(),
            "1\n00:00:01,000 --> 00:00:02,250\nHello world.\n\n2\n01:01:01,007 --> 01:01:03,500\nTwo\nlines\n\n"
        );
    }

    #[test]
    fn edit_split_merge() {
        let mut c = Caption::from_words(vec![w(0.0, 0.5, "one"), w(0.5, 1.0, "two"), w(1.0, 1.5, "three"), w(1.5, 2.0, "four")], 32);
        let (a, b) = c.split(32).unwrap();
        assert_eq!((a.text.as_str(), b.text.as_str()), ("one two", "three four"));
        assert_eq!((a.start, a.end, b.start, b.end), (0.0, 1.0, 1.0, 2.0));
        let m = a.merge(&b, 32);
        assert_eq!(m.text, "one two three four");
        assert_eq!((m.start, m.end), (0.0, 2.0));
        c.set_text("uno dos", 32);
        assert_eq!(c.words.len(), 2);
        assert_eq!((c.words[0].start, c.words[1].end), (0.0, 2.0));
        assert_eq!(c.word_at(1.2), Some(1));
        let json = serde_json::to_string(&Captions { lines: vec![c.clone()], ..Default::default() }).unwrap();
        let back: Captions = serde_json::from_str(&json).unwrap();
        assert_eq!(back.lines[0], c);
    }
}
