//! The edit list in the app: applying edits (with undo), keeping the playhead inside kept
//! time, and the preview data read through the edit (captions, ripples, cut audio,
//! waveform). The editing logic itself is in `editing`.

use crate::editing::{self, EditList, remap};
use crate::{Clipper, audio};
use std::sync::Arc;

/// Waveform resolution: peaks per second of raw time.
pub const WAVE_RATE: f64 = 100.0;

/// Peak level per 1/`WAVE_RATE` s of the mic track.
pub fn waveform(track: &audio::Track) -> Vec<f32> {
    let per = (audio::RATE as f64 / WAVE_RATE) as usize;
    track.samples.chunks(per).map(|c| c.iter().fold(0.0f32, |m, v| m.max(v.abs()))).collect()
}

impl Clipper {
    /// Length of the edited clip.
    pub(crate) fn out_duration(&self) -> f64 {
        if self.recording.is_some() { self.edit.output_duration() } else { 0.0 }
    }

    /// The playhead on the edited timeline.
    pub(crate) fn out_time(&self) -> f64 {
        self.edit.source_to_output(self.playhead)
    }

    /// Moves the playhead to output time `t`.
    pub(crate) fn seek_output(&mut self, t: f64) {
        let d = self.out_duration();
        self.playhead = self.edit.output_to_source(t.clamp(0.0, d));
        self.audio_restart = true;
    }

    /// A raw-time seek (e.g. from the caption list): lands on kept time.
    pub(crate) fn keep_playhead_in_edit(&mut self) {
        if self.recording.is_some() && self.edit.segment_at_source(self.playhead).is_none() {
            let o = self.edit.source_to_output(self.playhead);
            self.playhead = self.edit.output_to_source(o.min((self.out_duration() - editing::FRAME).max(0.0)));
        }
    }

    /// Replaces the edit, recording the old one for undo.
    pub(crate) fn commit_edit(&mut self, new: EditList) {
        if new == self.edit {
            return;
        }
        let old = std::mem::replace(&mut self.edit, new);
        self.edit_history.push(old);
        self.after_edit();
    }

    /// After the edit changed: save the sidecar, keep the playhead and selection valid,
    /// and refresh what the preview reads through the edit.
    pub(crate) fn after_edit(&mut self) {
        if let Some(rec) = &self.recording {
            if let Err(e) = self.edit.save(&rec.dir) {
                eprintln!("Clipper: could not save edits: {e}");
            }
        }
        self.keep_playhead_in_edit();
        if self.edit_sel.is_some_and(|i| i >= self.edit.segments.len()) {
            self.edit_sel = None;
        }
        self.confirm_reset = false;
        self.refresh_edit_derived();
        self.audio_restart = true;
    }

    /// Captions, ripples and audio on the output timeline.
    pub(crate) fn refresh_edit_derived(&mut self) {
        let Some(rec) = self.recording.clone() else {
            self.edited_caps = None;
            self.edited_clicks.clear();
            self.edited_audio = None;
            return;
        };
        let identity = self.edit.is_identity(rec.duration);
        self.edited_caps = self.captions.as_ref().map(|c| if identity { c.clone() } else { remap::captions(c, &self.edit, self.caption_rules()) });
        self.edited_clicks = remap::clicks(&crate::clicks::clicks(&rec.samples), &self.edit);
        self.edited_audio = self.audio.as_ref().map(|t| {
            if identity {
                audio::Track { samples: t.samples.clone() }
            } else {
                audio::Track { samples: Arc::new(editing::audio::cut(&t.samples, audio::RATE, &self.edit)) }
            }
        });
        self.cap_art = None;
    }

    pub(crate) fn split_at_playhead(&mut self) {
        let mut e = self.edit.clone();
        if e.split_at(self.playhead) {
            // Select the part that starts at the playhead.
            self.edit_sel = e.segment_at_source(editing::snap(self.playhead));
            self.commit_edit(e);
        }
    }

    /// Ripple-deletes the selected segment.
    pub(crate) fn delete_segment(&mut self) {
        let Some(i) = self.edit_sel else { return };
        let mut e = self.edit.clone();
        if e.delete(i) {
            // The playhead goes to where the removed part was.
            let at = self.edit.output_start(i);
            self.edit_sel = None;
            self.commit_edit(e);
            self.seek_output(at);
        }
    }

    pub(crate) fn undo_edit(&mut self) {
        if let Some(prev) = self.edit_history.undo(&self.edit) {
            self.edit = prev;
            self.after_edit();
        }
    }

    pub(crate) fn redo_edit(&mut self) {
        if let Some(next) = self.edit_history.redo(&self.edit) {
            self.edit = next;
            self.after_edit();
        }
    }

    /// Back to the whole recording (undoable).
    pub(crate) fn reset_edits(&mut self) {
        let d = self.duration();
        self.edit_sel = None;
        self.commit_edit(EditList::full(d));
        self.confirm_reset = false;
    }

    /// A new recording: its saved edit (if any), fresh history.
    pub(crate) fn load_edit(&mut self) {
        let Some(rec) = self.recording.clone() else { return };
        self.edit = EditList::load(&rec.dir, rec.duration).unwrap_or_else(|| EditList::full(rec.duration));
        self.edit_history.clear();
        self.edit_sel = None;
        self.confirm_reset = false;
        self.tl_zoom = 1.0;
        self.tl_scroll = 0.0;
        self.waveform = self.audio.as_ref().map(|t| Arc::new(waveform(t)));
        if let Some(old) = self.frame_cache.take() {
            self.stale_images.extend(old.close());
        }
        let (v0, _) = crate::recorder::av_layout(&rec.raw);
        let frames = (rec.duration * crate::recorder::FPS as f64).round() as u64;
        self.frame_cache = Some(crate::frames::FrameCache::new(rec.raw.clone(), rec.width, rec.height, v0, frames));
        self.refresh_edit_derived();
    }
}
