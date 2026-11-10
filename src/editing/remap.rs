//! Reading the recording's timed data through an edit: cursor samples, webcam spans, the
//! camera path, click ripples and captions, all from raw time onto the output timeline.

use super::{EditList, FRAME};
use crate::camera::{self, CameraPath};
use crate::captions::segment::Rules;
use crate::captions::{Caption, Captions, Word};
use crate::clicks::Click;
use crate::cursor::Sample;
use crate::recorder::FPS;

/// Each run (continuous raw span) with where it starts on the output timeline.
pub fn runs_with_offsets(edit: &EditList) -> Vec<(f64, f64, f64)> {
    let mut acc = 0.0;
    edit.runs()
        .into_iter()
        .map(|(a, b)| {
            let r = (a, b, acc);
            acc += b - a;
            r
        })
        .collect()
}

/// Cursor samples on the output timeline. Each run gets exact samples at its start and just
/// before its end, so the cursor at output time `t` is the cursor at raw time
/// `output_to_source(t)`, including the first frame after a cut (no interpolation across it).
pub fn samples(src: &[Sample], edit: &EditList) -> Vec<Sample> {
    if src.is_empty() {
        return Vec::new();
    }
    let at = |t: f64| {
        let (x, y) = camera::cursor_at(src, t).unwrap_or((0.0, 0.0));
        let i = src.partition_point(|s| s.t <= t).saturating_sub(1);
        Sample { t, x, y, down: src[i].down }
    };
    let mut out = Vec::new();
    for (a, b, o) in runs_with_offsets(edit) {
        out.push(Sample { t: o, ..at(a) });
        let lo = src.partition_point(|s| s.t <= a);
        let hi = src.partition_point(|s| s.t < b);
        out.extend(src[lo..hi].iter().map(|s| Sample { t: o + s.t - a, ..*s }));
        let end = b - 1e-6;
        if end > a {
            out.push(Sample { t: o + end - a, ..at(end) });
        }
    }
    out
}

/// Time spans (e.g. when the webcam has frames) on the output timeline.
pub fn spans(src: &[(f64, f64)], edit: &EditList) -> Vec<(f64, f64)> {
    let mut out: Vec<(f64, f64)> = Vec::new();
    for (a, b, o) in runs_with_offsets(edit) {
        for &(s, e) in src {
            let (lo, hi) = (s.max(a), e.min(b));
            if hi > lo {
                let span = (o + lo - a, o + hi - a);
                match out.last_mut() {
                    Some(last) if (span.0 - last.1).abs() < 1e-6 => last.1 = span.1,
                    _ => out.push(span),
                }
            }
        }
    }
    out
}

/// The camera path read through the edit: output frame `j` uses the crop the original path
/// has at raw time `output_to_source(j / FPS)`. Computed once on the original timeline, so
/// a cut is a hard cut to where the camera already is at the new raw time.
pub fn path(src: &CameraPath, edit: &EditList) -> CameraPath {
    let n = ((edit.output_duration() * FPS as f64).ceil() as usize).max(1);
    let frames = (0..n).map(|j| src.at_time(edit.output_to_source(j as f64 * FRAME) + 1e-6)).collect();
    CameraPath { crop_w: src.crop_w, crop_h: src.crop_h, frames }
}

/// Click ripples on the output timeline, each with the time it must be gone by: the end of
/// the continuous run it is in. A ripple never carries across a cut, and a click in removed
/// time is dropped (its ripple would start halfway).
pub fn clicks(src: &[Click], edit: &EditList) -> Vec<(Click, f64)> {
    let runs = runs_with_offsets(edit);
    src.iter()
        .filter_map(|c| {
            let &(a, b, o) = runs.iter().find(|&&(a, b, _)| c.t >= a && c.t < b)?;
            Some((Click { t: o + c.t - a, ..*c }, o + b - a))
        })
        .collect()
}

/// Ripples showing at output time `t` (like `clicks::active`, cut off at each one's end).
pub fn active_clicks(clicks: &[(Click, f64)], t: f64, duration: f64) -> Vec<(Click, f32)> {
    let d = duration.max(0.01);
    clicks
        .iter()
        .filter(|(c, end)| t >= c.t && t < (c.t + d).min(*end))
        .map(|(c, _)| (*c, ((t - c.t) / d) as f32))
        .collect()
}

/// Captions on the output timeline. Words fully inside removed time go; a word across a cut
/// is clipped to the side it mostly belongs to. A caption that straddles a cut becomes one
/// caption per side, so no text shows without its spoken words. Line breaks and display
/// times (minimum, maximum, never past the next caption or a cut) are recomputed. The
/// transcript itself is not re-run.
pub fn captions(src: &Captions, edit: &EditList, rules: Rules) -> Captions {
    let runs = runs_with_offsets(edit);
    // A word's run: the one holding most of it (or its instant, for a zero-length word).
    let place = |w: &Word| -> Option<(usize, Word)> {
        let best = runs
            .iter()
            .enumerate()
            .map(|(i, &(a, b, _))| {
                let overlap = w.end.min(b) - w.start.max(a);
                let inside = w.start >= a && w.start < b;
                (i, if overlap > 0.0 { overlap } else if inside { 0.0 } else { -1.0 })
            })
            .filter(|&(_, o)| o >= 0.0)
            .max_by(|x, y| x.1.total_cmp(&y.1))?;
        let (a, b, o) = runs[best.0];
        let (s, e) = (w.start.clamp(a, b), w.end.clamp(a, b));
        Some((best.0, Word { start: o + s - a, end: o + e - a, text: w.text.clone() }))
    };
    let mut lines: Vec<(Caption, f64)> = Vec::new();
    for c in &src.lines {
        let mut group: Vec<Word> = Vec::new();
        let mut group_run = None;
        let mut flush = |group: &mut Vec<Word>, run: Option<usize>| {
            if let (false, Some(r)) = (group.is_empty(), run) {
                let (a, b, o) = runs[r];
                lines.push((Caption::from_words(std::mem::take(group), rules.max_chars), o + b - a));
            }
        };
        for (run, w) in c.words.iter().filter(|w| !w.text.trim().is_empty()).filter_map(place) {
            if group_run.is_some_and(|r| r != run) {
                flush(&mut group, group_run);
            }
            group_run = Some(run);
            group.push(w);
        }
        flush(&mut group, group_run);
    }
    lines.sort_by(|x, y| x.0.start.total_cmp(&y.0.start));
    for i in 0..lines.len() {
        let next = lines.get(i + 1).map_or(f64::INFINITY, |l| l.0.start);
        let (c, run_end) = &mut lines[i];
        c.end = c.end.max(c.start + rules.min_dur).min(c.start + rules.max_dur).min(next).min(*run_end);
    }
    Captions { lines: lines.into_iter().map(|(c, _)| c).collect(), model: src.model.clone(), language: src.language.clone() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Ratio;

    fn word(start: f64, end: f64, text: &str) -> Word {
        Word { start, end, text: text.into() }
    }

    fn rules() -> Rules {
        Rules::for_ratio(Ratio::Landscape)
    }

    #[test]
    fn caption_words_inside_outside_and_across_a_cut() {
        // Remove 2.0..3.0.
        let edit = EditList { segments: vec![(0.0, 2.0), (3.0, 6.0)] };
        let caps = Captions {
            lines: vec![
                // Fully before the cut: unchanged.
                Caption::from_words(vec![word(0.2, 0.6, "Hello"), word(0.7, 1.1, "there")], 42),
                // Straddles the cut: "one" before, "two" removed, "three" after.
                Caption::from_words(vec![word(1.5, 1.9, "one"), word(2.2, 2.6, "two"), word(3.1, 3.5, "three")], 42),
                // Fully inside the removed time: gone.
                Caption::from_words(vec![word(2.1, 2.9, "gone")], 42),
                // Mostly after the cut: clipped to start at the cut.
                Caption::from_words(vec![word(2.9, 3.6, "later")], 42),
            ],
            ..Default::default()
        };
        let out = captions(&caps, &edit, rules());
        let texts: Vec<&str> = out.lines.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["Hello there", "one", "later", "three"]);
        let first = &out.lines[0];
        assert!((first.start - 0.2).abs() < 1e-9);
        // "one" ends at the cut (2.0 output), never showing after it.
        let one = &out.lines[1];
        assert!(one.end <= 2.0 + 1e-9, "one ends at {}", one.end);
        // "later" starts exactly at the cut; "three" at 3.1 raw → 2.1 output.
        assert!((out.lines[2].start - 2.0).abs() < 1e-9);
        assert!((out.lines[3].start - 2.1).abs() < 1e-9);
        // No removed word survives, and everything is in order without overlaps.
        assert!(out.lines.iter().all(|c| c.words.iter().all(|w| w.text != "two" && w.text != "gone")));
        for w in out.lines.windows(2) {
            assert!(w[0].end <= w[1].start + 1e-9);
        }
        // Minimum display time, but not past the next caption.
        assert!(out.lines[0].end - out.lines[0].start >= rules().min_dur - 1e-9);
    }

    #[test]
    fn click_ripples_clip_at_cuts() {
        let edit = EditList { segments: vec![(0.0, 2.0), (2.0, 3.0), (5.0, 8.0)] };
        let src = vec![
            Click { t: 1.9, x: 1.0, y: 1.0 },  // ripple would run 1.9..2.3, across a split: kept whole
            Click { t: 2.8, x: 2.0, y: 2.0 },  // ripple would run past the cut at 3.0: clipped there
            Click { t: 4.0, x: 3.0, y: 3.0 },  // in removed time: dropped
            Click { t: 4.9, x: 4.0, y: 4.0 },  // removed; its ripple must not appear after the cut
            Click { t: 6.0, x: 5.0, y: 5.0 },
        ];
        let out = clicks(&src, &edit);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0], (Click { t: 1.9, x: 1.0, y: 1.0 }, 3.0));
        assert_eq!(out[1], (Click { t: 2.8, x: 2.0, y: 2.0 }, 3.0));
        assert!((out[2].0.t - 4.0).abs() < 1e-9);
        let d = 0.4;
        // Across a plain split the ripple keeps going.
        assert_eq!(active_clicks(&out, 2.2, d).len(), 1);
        // The ripple from 2.8 stops at the cut; nothing from before the cut shows after it.
        assert_eq!(active_clicks(&out, 2.95, d).len(), 1);
        assert!(active_clicks(&out, 3.0, d).is_empty());
        assert!(active_clicks(&out, 3.1, d).is_empty());
        assert_eq!(active_clicks(&out, 4.1, d).len(), 1);
    }

    #[test]
    fn spans_follow_the_edit() {
        let edit = EditList { segments: vec![(0.0, 2.0), (3.0, 6.0)] };
        assert_eq!(spans(&[(0.0, 6.0)], &edit), vec![(0.0, 5.0)]);
        assert_eq!(spans(&[(1.0, 2.5), (4.0, 5.0)], &edit), vec![(1.0, 2.0), (3.0, 4.0)]);
    }

    /// The cursor is inside the crop on every output frame, including the first one after
    /// each cut, where the cursor jumps.
    #[test]
    fn camera_keeps_the_cursor_in_frame_across_cuts() {
        let (w, h, dur) = (1920u32, 1080u32, 9.0);
        // The cursor sweeps left to right, then jumps to the far corner at 3 s and 6 s.
        let samples: Vec<Sample> = (0..(dur * 120.0) as usize)
            .map(|i| {
                let t = i as f64 / 120.0;
                let (x, y) = match t {
                    t if t < 3.0 => (100.0 + t * 500.0, 200.0),
                    t if t < 6.0 => (1850.0, 1000.0 - (t - 3.0) * 50.0),
                    t => (60.0 + (t - 6.0) * 30.0, 60.0),
                };
                Sample { t, x, y, down: false }
            })
            .collect();
        let src_path = camera::compute_with(&samples, w, h, dur, 9.0 / 16.0, camera::FollowConfig::default());
        // Cut right around the jumps, so the frame after each cut lands far from the last one.
        let edit = EditList { segments: vec![(0.0, 2.5), (3.4, 5.5), (6.5, 9.0)] };
        let out_samples = samples_fn(&samples, &edit);
        let out_path = path(&src_path, &edit);
        let n = out_path.frames.len();
        assert_eq!(n, (edit.output_duration() * FPS as f64).ceil() as usize);
        for j in 0..n {
            let t = j as f64 * FRAME;
            let (cx, cy) = camera::cursor_at(&out_samples, t).unwrap();
            let (x, y, cw, ch) = out_path.frames[j];
            assert!(
                cx >= x as f64 && cx <= (x + cw) as f64 && cy >= y as f64 && cy <= (y + ch) as f64,
                "frame {j} ({t:.3}s): cursor ({cx:.0},{cy:.0}) outside crop {:?}",
                out_path.frames[j]
            );
            // And it is exactly the raw recording's cursor at the mapped time.
            let (sx, sy) = camera::cursor_at(&samples, edit.output_to_source(t)).unwrap();
            assert!((sx - cx).abs() < 0.5 && (sy - cy).abs() < 0.5, "frame {j}: ({cx},{cy}) vs raw ({sx},{sy})");
        }
        // The first frame after each cut shows the new raw time's crop (a hard cut).
        for (o, real) in edit.joins() {
            assert!(real);
            let j = (o * FPS as f64).round() as usize;
            assert_eq!(out_path.frames[j], src_path.at_time(edit.output_to_source(o) + 1e-6));
        }
    }

    fn samples_fn(src: &[Sample], edit: &EditList) -> Vec<Sample> {
        samples(src, edit)
    }
}
