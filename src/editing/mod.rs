//! Non-destructive video editing: trim, split, ripple delete, undo/redo.
//!
//! An `EditList` is the ordered list of kept segments, each `(source_start, source_end)` in
//! raw recording time. The raw files are never touched; everything downstream (preview,
//! export, captions, clicks, camera, webcam, audio) reads through `source_to_output` and
//! `output_to_source`. Output time is the edited timeline, the kept segments laid end to end.
//!
//! Pure logic, no UI.

pub mod audio;
pub mod filter;
pub mod remap;

use crate::recorder::FPS;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// One frame of the recording: the smallest segment and the grid every cut snaps to.
pub const FRAME: f64 = 1.0 / FPS as f64;
const EPS: f64 = 1e-9;

/// Snaps a time to the recording's frame grid.
pub fn snap(t: f64) -> f64 {
    (t * FPS as f64).round() / FPS as f64
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EditList {
    /// Kept segments in raw recording time: ordered, non-overlapping, each at least a frame.
    pub segments: Vec<(f64, f64)>,
}

impl EditList {
    /// The whole recording, unedited.
    pub fn full(duration: f64) -> Self {
        Self { segments: vec![(0.0, duration.max(FRAME))] }
    }

    /// Nothing removed (splits alone don't change the output).
    pub fn is_identity(&self, duration: f64) -> bool {
        (self.output_duration() - duration).abs() < 1e-6
    }

    pub fn output_duration(&self) -> f64 {
        self.segments.iter().map(|(a, b)| b - a).sum()
    }

    /// Output time where segment `i` starts.
    pub fn output_start(&self, i: usize) -> f64 {
        self.segments[..i.min(self.segments.len())].iter().map(|(a, b)| b - a).sum()
    }

    /// Segment containing raw time `t` (a segment owns its start, not its end).
    pub fn segment_at_source(&self, t: f64) -> Option<usize> {
        self.segments.iter().position(|&(a, b)| t >= a - EPS && t < b - EPS)
    }

    /// Segment playing at output time `t` (the last one at the very end).
    pub fn segment_at_output(&self, t: f64) -> Option<usize> {
        let mut acc = 0.0;
        for (i, (a, b)) in self.segments.iter().enumerate() {
            let len = b - a;
            if t < acc + len - EPS {
                return Some(i);
            }
            acc += len;
        }
        self.segments.len().checked_sub(1)
    }

    /// Raw time → output time. Removed time maps to where the next kept segment starts
    /// (the end of the output after the last one).
    pub fn source_to_output(&self, t: f64) -> f64 {
        let mut acc = 0.0;
        for &(a, b) in &self.segments {
            if t < a {
                return acc;
            }
            if t < b {
                return acc + (t - a);
            }
            acc += b - a;
        }
        acc
    }

    /// Output time → raw time. A cut point maps to the start of the segment after it, so
    /// the first frame after a cut is that segment's first frame.
    pub fn output_to_source(&self, t: f64) -> f64 {
        let Some(i) = self.segment_at_output(t.max(0.0)) else { return 0.0 };
        let (a, b) = self.segments[i];
        (a + (t.max(0.0) - self.output_start(i))).clamp(a, b)
    }

    /// Output times of the joins between segments, and whether each is a real cut
    /// (raw time removed there) rather than a plain split.
    pub fn joins(&self) -> Vec<(f64, bool)> {
        let mut acc = 0.0;
        let mut out = Vec::new();
        for w in self.segments.windows(2) {
            acc += w[0].1 - w[0].0;
            out.push((acc, w[1].0 - w[0].1 > EPS));
        }
        out
    }

    /// Runs of segments with no raw time removed between them, merged: what actually
    /// plays continuously.
    pub fn runs(&self) -> Vec<(f64, f64)> {
        let mut out: Vec<(f64, f64)> = Vec::new();
        for &(a, b) in &self.segments {
            match out.last_mut() {
                Some(last) if (a - last.1).abs() < EPS => last.1 = b,
                _ => out.push((a, b)),
            }
        }
        out
    }

    /// Raw seconds removed by the edit.
    pub fn removed(&self, duration: f64) -> f64 {
        (duration - self.output_duration()).max(0.0)
    }

    /// Splits the segment under raw time `t` at the nearest frame. False when that would
    /// leave a piece shorter than a frame.
    pub fn split_at(&mut self, t: f64) -> bool {
        let t = snap(t);
        let Some(i) = self.segment_at_source(t) else { return false };
        let (a, b) = self.segments[i];
        if t - a < FRAME - EPS || b - t < FRAME - EPS {
            return false;
        }
        self.segments.splice(i..=i, [(a, t), (t, b)]);
        true
    }

    /// Removes segment `i`; later segments shift left (ripple). The last segment can't go.
    pub fn delete(&mut self, i: usize) -> bool {
        if self.segments.len() <= 1 || i >= self.segments.len() {
            return false;
        }
        self.segments.remove(i);
        true
    }

    /// Moves the clip's start to raw time `t` (trimming, or bringing trimmed time back),
    /// within the recording and at least a frame before the first segment's end.
    pub fn set_start(&mut self, t: f64) {
        if let Some(first) = self.segments.first_mut() {
            first.0 = snap(t).clamp(0.0, first.1 - FRAME);
        }
    }

    /// Moves the clip's end to raw time `t`, at most `duration`.
    pub fn set_end(&mut self, t: f64, duration: f64) {
        if let Some(last) = self.segments.last_mut() {
            let t = if (t - duration).abs() < FRAME / 2.0 || t > duration { duration } else { snap(t) };
            last.1 = t.clamp(last.0 + FRAME, duration);
        }
    }

    /// Repairs a list read from disk: sorted, inside the recording, no overlaps, no
    /// segment shorter than a frame. Falls back to the whole recording.
    pub fn sanitized(mut self, duration: f64) -> Self {
        self.segments.retain(|(a, b)| a.is_finite() && b.is_finite());
        self.segments.sort_by(|x, y| x.0.total_cmp(&y.0));
        let mut out: Vec<(f64, f64)> = Vec::new();
        for (a, b) in self.segments {
            let a = a.max(out.last().map_or(0.0, |l| l.1)).max(0.0);
            let b = b.min(duration);
            if b - a >= FRAME - EPS {
                out.push((a, b));
            }
        }
        if out.is_empty() { Self::full(duration) } else { Self { segments: out } }
    }

    /// The sidecar file next to the recording.
    pub fn save(&self, dir: &Path) -> anyhow::Result<()> {
        std::fs::write(dir.join("edits.json"), serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }

    pub fn load(dir: &Path, duration: f64) -> Option<Self> {
        let list: Self = serde_json::from_slice(&std::fs::read(dir.join("edits.json")).ok()?).ok()?;
        Some(list.sanitized(duration))
    }
}

/// What a render needs besides the re-timed recording: the kept runs to cut the raw
/// screen, webcam and mic tracks to, and the click ripples on the output timeline.
#[derive(Clone, Debug)]
pub struct Cut {
    pub edit: EditList,
    /// Length of the raw recording.
    pub source_duration: f64,
    pub clicks: Vec<(crate::clicks::Click, f64)>,
}

/// A recording read through an edit, ready for the compositor: its duration, cursor
/// samples and webcam spans on the output timeline (the raw files are the same; the render
/// cuts them with `Cut`), the camera path and captions remapped.
pub struct Edited {
    pub rec: crate::recorder::Recording,
    pub path: crate::camera::CameraPath,
    pub captions: Option<crate::captions::Captions>,
    pub cut: Cut,
}

pub fn apply(
    rec: &crate::recorder::Recording,
    path: &crate::camera::CameraPath,
    captions: Option<&crate::captions::Captions>,
    rules: crate::captions::segment::Rules,
    edit: &EditList,
) -> Edited {
    let webcam = rec.webcam.clone().map(|mut t| {
        t.spans = remap::spans(&t.spans, edit);
        t
    });
    Edited {
        rec: crate::recorder::Recording {
            dir: rec.dir.clone(),
            raw: rec.raw.clone(),
            width: rec.width,
            height: rec.height,
            duration: edit.output_duration(),
            samples: remap::samples(&rec.samples, edit),
            webcam,
            px_per_pt: rec.px_per_pt,
        },
        path: remap::path(path, edit),
        captions: captions.map(|c| remap::captions(c, edit, rules)),
        cut: Cut { edit: edit.clone(), source_duration: rec.duration, clicks: remap::clicks(&crate::clicks::clicks(&rec.samples), edit) },
    }
}

/// Unlimited undo and redo of edit lists.
#[derive(Default)]
pub struct History {
    past: Vec<EditList>,
    future: Vec<EditList>,
}

impl History {
    /// Records `before` as the state an edit started from.
    pub fn push(&mut self, before: EditList) {
        self.past.push(before);
        self.future.clear();
    }

    pub fn undo(&mut self, current: &EditList) -> Option<EditList> {
        let prev = self.past.pop()?;
        self.future.push(current.clone());
        Some(prev)
    }

    pub fn redo(&mut self, current: &EditList) -> Option<EditList> {
        let next = self.future.pop()?;
        self.past.push(current.clone());
        Some(next)
    }

    pub fn can_undo(&self) -> bool {
        !self.past.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.future.is_empty()
    }

    pub fn clear(&mut self) {
        self.past.clear();
        self.future.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    fn list(segs: &[(f64, f64)]) -> EditList {
        EditList { segments: segs.to_vec() }
    }

    #[test]
    fn mapping_round_trips() {
        let e = list(&[(1.0, 3.0), (4.0, 4.5), (6.0, 10.0)]);
        assert!(close(e.output_duration(), 6.5));
        // Every kept raw time survives source → output → source.
        let mut t = 0.0;
        while t < 10.0 {
            if e.segment_at_source(t).is_some() {
                let back = e.output_to_source(e.source_to_output(t));
                assert!(close(back, t), "{t} → {back}");
            }
            t += 0.01;
        }
        // Every output time survives output → source → output.
        let mut o = 0.0;
        while o < 6.5 {
            let back = e.source_to_output(e.output_to_source(o));
            assert!(close(back, o), "{o} → {back}");
            o += 0.01;
        }
        // Removed time maps to the next kept segment; a cut maps to the segment after it.
        assert!(close(e.source_to_output(0.5), 0.0));
        assert!(close(e.source_to_output(3.5), 2.0));
        assert!(close(e.source_to_output(12.0), 6.5));
        assert!(close(e.output_to_source(2.0), 4.0));
        assert!(close(e.output_to_source(2.5), 6.0));
        assert_eq!(e.joins(), vec![(2.0, true), (2.5, true)]);
    }

    #[test]
    fn ripple_delete_first_middle_last() {
        let base = list(&[(0.0, 2.0), (2.0, 5.0), (5.0, 9.0)]);
        for (i, want) in [(0, vec![(2.0, 5.0), (5.0, 9.0)]), (1, vec![(0.0, 2.0), (5.0, 9.0)]), (2, vec![(0.0, 2.0), (2.0, 5.0)])] {
            let mut e = base.clone();
            assert!(e.delete(i));
            assert_eq!(e.segments, want);
            // No gap on the output timeline.
            assert!(close(e.output_duration(), 9.0 - (base.segments[i].1 - base.segments[i].0)));
            assert!(close(e.output_start(1), e.segments[0].1 - e.segments[0].0));
        }
        // After deleting the middle, what followed now plays right after the first part.
        let mut e = base.clone();
        e.delete(1);
        assert!(close(e.output_to_source(2.0), 5.0));
        // The last segment stays.
        let mut one = list(&[(0.0, 2.0)]);
        assert!(!one.delete(0));
    }

    #[test]
    fn split_at_boundaries_and_minimum_length() {
        let mut e = EditList::full(4.0);
        assert!(e.split_at(1.0));
        assert_eq!(e.segments, vec![(0.0, 1.0), (1.0, 4.0)]);
        // At an existing join, or less than a frame from an edge: no split.
        assert!(!e.split_at(1.0));
        assert!(!e.split_at(1.0 + FRAME * 0.4));
        assert!(!e.split_at(0.0));
        assert!(!e.split_at(4.0 - FRAME * 0.4));
        // Exactly one frame from an edge is allowed: the minimum segment.
        assert!(e.split_at(4.0 - FRAME));
        assert!(close(e.segments.last().unwrap().1 - e.segments.last().unwrap().0, FRAME));
        // Splits snap to the frame grid.
        let mut e = EditList::full(4.0);
        assert!(e.split_at(2.012));
        assert!(close(e.segments[0].1, 2.0));
        // Splitting alone doesn't change the output.
        assert!(e.is_identity(4.0));
    }

    #[test]
    fn trims_keep_a_frame() {
        let mut e = EditList::full(5.0);
        e.set_start(1.03);
        assert!(close(e.segments[0].0, snap(1.03)));
        e.set_start(9.0);
        assert!(close(e.segments[0].0, 5.0 - FRAME));
        e.set_start(-1.0);
        assert!(close(e.segments[0].0, 0.0));
        e.set_end(2.0, 5.0);
        assert!(close(e.segments[0].1, 2.0));
        e.set_end(0.0, 5.0);
        assert!(close(e.segments[0].1, FRAME));
        e.set_end(99.0, 5.0);
        assert!(close(e.segments[0].1, 5.0));
        assert!(close(e.removed(5.0), 0.0));
    }

    #[test]
    fn undo_redo() {
        let mut h = History::default();
        let mut e = EditList::full(4.0);
        h.push(e.clone());
        e.split_at(2.0);
        h.push(e.clone());
        e.delete(0);
        let undone = h.undo(&e).unwrap();
        assert_eq!(undone.segments.len(), 2);
        let undone2 = h.undo(&undone).unwrap();
        assert_eq!(undone2, EditList::full(4.0));
        assert!(h.undo(&undone2).is_none());
        let redone = h.redo(&undone2).unwrap();
        assert_eq!(redone.segments.len(), 2);
        // A new edit drops the redo stack.
        h.push(redone.clone());
        assert!(!h.can_redo());
    }

    #[test]
    fn sanitizes_saved_lists() {
        let e = list(&[(3.0, 2.0), (1.0, 2.5), (-1.0, 0.5), (9.0, 12.0)]).sanitized(10.0);
        assert_eq!(e.segments, vec![(0.0, 0.5), (1.0, 2.5), (9.0, 10.0)]);
        assert_eq!(list(&[]).sanitized(3.0), EditList::full(3.0));
        let dir = std::env::temp_dir().join(format!("clipper-edits-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        e.save(&dir).unwrap();
        assert_eq!(EditList::load(&dir, 10.0), Some(e));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
