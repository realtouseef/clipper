//! ffmpeg graph pieces that play only the kept parts of a raw video track.

use super::EditList;
use crate::recorder::FPS;
use std::fmt::Write as _;

/// The start of a filter chain reading `input` (e.g. `0:v`) and keeping only the edit's
/// runs, frame-exactly, rebased to 0. The result ends with a trailing comma (or label
/// handoff) so the caller appends its next filter. The track is first made constant-rate
/// at the recording's FPS so cuts land on whole frames (edit points are on that grid).
///
/// `tag` keeps labels unique when several inputs are trimmed in one graph.
pub fn trimmed(input: &str, edit: Option<&EditList>, tag: &str) -> String {
    let runs = edit.map(|e| e.runs()).unwrap_or_default();
    let frame = |t: f64| (t * FPS as f64).round() as u64;
    match runs.as_slice() {
        [] => format!("[{input}]setpts=PTS-STARTPTS,"),
        [(a, b)] => format!("[{input}]setpts=PTS-STARTPTS,fps={FPS},trim=start_frame={}:end_frame={},setpts=PTS-STARTPTS,", frame(*a), frame(*b)),
        _ => {
            let n = runs.len();
            let mut g = format!("[{input}]setpts=PTS-STARTPTS,fps={FPS},split={n}");
            for i in 0..n {
                let _ = write!(g, "[{tag}s{i}]");
            }
            for (i, (a, b)) in runs.iter().enumerate() {
                let _ = write!(g, ";[{tag}s{i}]trim=start_frame={}:end_frame={},setpts=PTS-STARTPTS[{tag}t{i}]", frame(*a), frame(*b));
            }
            g.push(';');
            for i in 0..n {
                let _ = write!(g, "[{tag}t{i}]");
            }
            let _ = write!(g, "concat=n={n}:v=1:a=0[{tag}cut];[{tag}cut]");
            g
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_shapes() {
        assert_eq!(trimmed("0:v", None, "s"), "[0:v]setpts=PTS-STARTPTS,");
        let one = EditList { segments: vec![(1.0, 2.0)] };
        assert_eq!(trimmed("0:v", Some(&one), "s"), "[0:v]setpts=PTS-STARTPTS,fps=30,trim=start_frame=30:end_frame=60,setpts=PTS-STARTPTS,");
        // Adjacent segments (a split) are one run.
        let split = EditList { segments: vec![(0.0, 1.0), (1.0, 2.0)] };
        assert!(!trimmed("0:v", Some(&split), "s").contains("concat"));
        let two = EditList { segments: vec![(0.0, 1.0), (2.0, 3.0)] };
        let g = trimmed("4:v", Some(&two), "w");
        assert!(g.starts_with("[4:v]setpts=PTS-STARTPTS,fps=30,split=2[ws0][ws1];"));
        assert!(g.contains("[ws1]trim=start_frame=60:end_frame=90"));
        assert!(g.ends_with("concat=n=2:v=1:a=0[wcut];[wcut]"));
    }
}
