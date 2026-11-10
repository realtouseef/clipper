//! Groups transcribed words into caption lines.
//!
//! Rules: at most 2 lines per caption, each at most `max_chars` (32 in 9:16, 42 in 16:9);
//! a new caption starts after sentence punctuation, after a comma once the caption has
//! `min_chars`, after a pause over 400 ms, or when it would run past 4 s. Each caption shows
//! for at least 0.7 s (extended into the following gap) and at most 4 s.

use super::{Caption, Word};
use crate::layout::Ratio;

#[derive(Clone, Copy, Debug)]
pub struct Rules {
    pub max_chars: usize,
    pub min_chars: usize,
    pub max_lines: usize,
    /// Seconds of silence that end a caption.
    pub pause: f64,
    pub min_dur: f64,
    pub max_dur: f64,
}

impl Rules {
    pub fn for_ratio(ratio: Ratio) -> Self {
        Self {
            max_chars: match ratio {
                Ratio::Portrait => 32,
                Ratio::Landscape => 42,
            },
            min_chars: 20,
            max_lines: 2,
            pause: 0.4,
            min_dur: 0.7,
            max_dur: 4.0,
        }
    }
}

fn len(tokens: &[&str]) -> usize {
    tokens.iter().map(|t| t.chars().count()).sum::<usize>() + tokens.len().saturating_sub(1)
}

/// Lines of at most `max_chars`. Two lines are balanced (the break that makes the longer
/// line shortest); longer texts wrap greedily. A single word longer than the limit gets a
/// line of its own.
pub fn wrap(tokens: &[&str], max_chars: usize) -> Vec<String> {
    if tokens.is_empty() {
        return Vec::new();
    }
    if len(tokens) <= max_chars {
        return vec![tokens.join(" ")];
    }
    let best = (1..tokens.len())
        .map(|i| (i, len(&tokens[..i]).max(len(&tokens[i..]))))
        .filter(|&(_, m)| m <= max_chars)
        .min_by_key(|&(_, m)| m);
    if let Some((i, _)) = best {
        return vec![tokens[..i].join(" "), tokens[i..].join(" ")];
    }
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for t in tokens {
        if !cur.is_empty() && cur.chars().count() + 1 + t.chars().count() > max_chars {
            lines.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(t);
    }
    lines.push(cur);
    lines
}

/// Words → captions under `rules`.
pub fn segment(words: &[Word], rules: Rules) -> Vec<Caption> {
    let mut groups: Vec<Vec<Word>> = Vec::new();
    let mut cur: Vec<Word> = Vec::new();
    for w in words.iter().filter(|w| !w.text.trim().is_empty()) {
        if let Some(prev) = cur.last() {
            let mut tokens: Vec<&str> = cur.iter().map(|w| w.text.as_str()).collect();
            let chars = len(&tokens);
            tokens.push(&w.text);
            let overflow = wrap(&tokens, rules.max_chars).len() > rules.max_lines;
            let too_long = w.end - cur[0].start > rules.max_dur;
            let pause = w.start - prev.end > rules.pause;
            let sentence = prev.text.ends_with(['.', '?', '!', '…']);
            let clause = prev.text.ends_with([',', ';', ':']) && chars >= rules.min_chars;
            if overflow || too_long || pause || sentence || clause {
                groups.push(std::mem::take(&mut cur));
            }
        }
        cur.push(w.clone());
    }
    if !cur.is_empty() {
        groups.push(cur);
    }

    let mut caps: Vec<Caption> = groups.into_iter().map(|g| Caption::from_words(g, rules.max_chars)).collect();
    // Display time: at least `min_dur` (into the gap before the next caption), at most `max_dur`.
    for i in 0..caps.len() {
        let next = caps.get(i + 1).map_or(f64::INFINITY, |c| c.start);
        let c = &mut caps[i];
        if c.end - c.start < rules.min_dur {
            c.end = (c.start + rules.min_dur).min(next);
        }
        c.end = c.end.min(c.start + rules.max_dur).min(next);
    }
    caps
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A paragraph of speech: ~3 words/s, a 0.6 s pause after word 9, some punctuation.
    fn speech() -> Vec<Word> {
        let text = "So today I want to show you how this works. First we open the settings panel, \
                    then we pick a layout and a background that fits the video we are making, and \
                    finally we press record and talk through the whole thing without stopping at all";
        let mut t = 0.5;
        text.split_whitespace()
            .enumerate()
            .map(|(i, s)| {
                if i == 9 {
                    t += 0.6;
                }
                let d = 0.12 + s.len() as f64 * 0.03;
                let w = Word { start: t, end: t + d, text: s.into() };
                t += d + 0.05;
                w
            })
            .collect()
    }

    #[test]
    fn line_length_and_count() {
        for ratio in [Ratio::Portrait, Ratio::Landscape] {
            let rules = Rules::for_ratio(ratio);
            let caps = segment(&speech(), rules);
            assert!(caps.len() > 3);
            for c in &caps {
                let lines: Vec<&str> = c.text.split('\n').collect();
                assert!(lines.len() <= 2, "{ratio:?}: {:?}", c.text);
                for l in lines {
                    assert!(l.chars().count() <= rules.max_chars, "{ratio:?}: line {l:?} too long");
                }
            }
            // Every word lands in exactly one caption, in order.
            let n: usize = caps.iter().map(|c| c.words.len()).sum();
            assert_eq!(n, speech().len());
        }
    }

    #[test]
    fn display_time_limits() {
        let rules = Rules::for_ratio(Ratio::Portrait);
        // A lone short word between pauses gets stretched to 0.7 s.
        let mut words = vec![Word { start: 0.0, end: 0.2, text: "Hi.".into() }];
        // A long run of quick words without punctuation must be cut at 4 s.
        words.extend((0..40).map(|i| Word { start: 2.0 + i as f64 * 0.25, end: 2.2 + i as f64 * 0.25, text: "go".into() }));
        let caps = segment(&words, rules);
        assert!((caps[0].end - caps[0].start - 0.7).abs() < 1e-9, "{:?}", caps[0]);
        for c in &caps {
            assert!(c.end - c.start <= rules.max_dur + 1e-9, "{c:?}");
            assert!(c.end - c.start >= rules.min_dur - 1e-9, "{c:?}");
        }
        for pair in caps.windows(2) {
            assert!(pair[0].end <= pair[1].start + 1e-9, "overlap {pair:?}");
        }
    }

    #[test]
    fn breaks_on_pause_and_punctuation() {
        let rules = Rules::for_ratio(Ratio::Landscape);
        let caps = segment(&speech(), rules);
        // "...how this works." ends a caption.
        assert!(caps.iter().any(|c| c.text.replace('\n', " ").ends_with("works.")), "{caps:#?}");
        // The 0.6 s pause before word 9 ("works.") ... and after the sentence.
        let pause_at = speech()[9].start;
        assert!(caps.iter().any(|c| (c.start - pause_at).abs() < 1e-9) || caps.iter().any(|c| c.words.first().unwrap().start == pause_at));
    }

    #[test]
    fn wrap_balances_two_lines() {
        let t: Vec<&str> = "the quick brown fox jumps over the lazy dog".split(' ').collect();
        assert_eq!(wrap(&t, 32), vec!["the quick brown fox", "jumps over the lazy dog"]);
        assert_eq!(wrap(&t, 60), vec!["the quick brown fox jumps over the lazy dog"]);
        assert_eq!(wrap(&t, 10).len(), 5);
    }
}
