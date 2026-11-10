//! Editor timeline (playback, ruler, edited clip with trim/split/ripple delete, waveform,
//! webcam and captions rows, zoom track) and the Zoom settings tab.
//!
//! The timeline shows the edited clip: kept segments end to end on the output timeline.
//! Zoom regions and the playhead live in raw recording time and are mapped for display.

use crate::editing::{self, remap};
use crate::zoom::{self, DEPTHS, ZoomRegion};
use crate::{
    CARD, Clipper, GREEN, LINE, MUTED, ON_GREEN, R_CARD, RAISED, RED, Stage, TEXT, button, field, note, section,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable, Icon, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, Bounds, ClickEvent, CursorStyle, Div, FontWeight, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ObjectFit, Pixels, ScrollEvent, Window, canvas, div, hsla, img, point, px, rgb, size,
};

const PAD_X: f32 = 16.0;
const LABEL_W: f32 = 60.0;
const GAP: f32 = 8.0;
const TOOLBAR_H: f32 = 32.0;
const RULER_H: f32 = 18.0;
const VIDEO_H: f32 = 40.0;
const AUDIO_H: f32 = 30.0;
const CAM_H: f32 = 14.0;
const CAP_H: f32 = 20.0;
const ZOOM_H: f32 = 26.0;
const ROW_GAP: f32 = 6.0;
const SCROLL_H: f32 = 4.0;
/// Most zoomed in: pixels per second.
const MAX_PPS: f64 = 600.0;

/// Space between the window edge and the timeline card.
const OUTER: f32 = crate::GAP;

// Zoom blocks: deep green, solid green when selected.
const ZOOM_BG: u32 = 0x233119;
const ZOOM_LINE: u32 = 0x3d5a2c;

const DEPTH_LABELS: [&str; 5] = ["1.25×", "1.5×", "2×", "2.5×", "3×"];

/// How a zoom block is being dragged.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ZoomDrag {
    /// Moving it; `grab` is the pointer's offset from the region start (raw time).
    Move { grab: f64 },
    Start,
    End,
}

/// The one pointer gesture in progress in the main window. Only one can run at a time, so
/// it's one value rather than a flag per kind; it ends on mouse up (or when the button is
/// found released on a move).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Gesture {
    #[default]
    None,
    /// Scrubbing the playhead on the timeline.
    Seek,
    /// Moving or resizing zoom `index`.
    Zoom { index: usize, how: ZoomDrag },
    /// Dragging the clip's start or end edge: the pointer's x and the scale when it started
    /// (the timeline rescales as the clip gets shorter), and the edit before the drag, which
    /// becomes the one undo step for the whole drag.
    Trim { end: bool, x: f32, pps: f64, orig: editing::EditList },
    /// Dragging the floating webcam on the preview: pointer offset from its center (output px).
    Cam { grab: (f32, f32) },
    /// Holding a settings slider.
    Knob(crate::knob::Knob),
}

impl Gesture {
    /// Timeline gestures hold playback (and preview audio) while they run.
    pub fn holds_playback(&self) -> bool {
        matches!(self, Gesture::Seek | Gesture::Zoom { .. } | Gesture::Trim { .. })
    }
}

fn clock(t: f64) -> String {
    let t = t.max(0.0);
    format!("{}:{:04.1}", (t / 60.0) as u32, t % 60.0)
}

fn icon(svg: &'static [u8]) -> Icon {
    Icon::default().data(svg)
}

impl Clipper {
    /// Length of the raw recording.
    pub(crate) fn duration(&self) -> f64 {
        self.recording.as_ref().map_or(0.0, |r| r.duration)
    }

    /// Track origin and width in window coordinates (the track spans the window).
    fn track_geom(&self, window: &Window) -> (f32, f32) {
        let x0 = OUTER + PAD_X + LABEL_W + GAP;
        (x0, (f32::from(window.viewport_size().width) - x0 - PAD_X - OUTER).max(1.0))
    }

    /// Pixels per second of output time for a visible track `w` wide.
    fn pps(&self, w: f32) -> f64 {
        let d = self.out_duration();
        if d > 0.0 { w as f64 * self.tl_zoom as f64 / d } else { 1.0 }
    }

    fn clamp_scroll(&mut self, w: f32) {
        let content = w * self.tl_zoom;
        self.tl_scroll = self.tl_scroll.clamp(0.0, (content - w).max(0.0));
    }

    /// Output time under window x (clamped to the clip).
    fn out_at(&self, x: Pixels, window: &Window) -> f64 {
        let (x0, w) = self.track_geom(window);
        (((f32::from(x) - x0 + self.tl_scroll) as f64) / self.pps(w)).clamp(0.0, self.out_duration())
    }

    /// Raw time under window x.
    fn time_at(&self, x: Pixels, window: &Window) -> f64 {
        self.edit.output_to_source(self.out_at(x, window))
    }

    fn editing(&self) -> bool {
        matches!(self.stage, Stage::Review) && self.recording.is_some()
    }

    /// The preview can play (also while an export runs).
    fn previewing(&self) -> bool {
        matches!(self.stage, Stage::Review | Stage::Exporting) && self.recording.is_some()
    }

    /// Height of the timeline card: the webcam and captions rows only when there's
    /// something to show.
    pub(crate) fn timeline_height(&self) -> f32 {
        let mut rows = RULER_H + VIDEO_H + AUDIO_H + ZOOM_H + ROW_GAP * 3.0;
        if self.recording.as_ref().is_some_and(|r| r.webcam.is_some()) {
            rows += CAM_H + ROW_GAP;
        }
        if self.edited_caps.as_ref().is_some_and(|c| !c.lines.is_empty()) && self.cap.enabled {
            rows += CAP_H + ROW_GAP;
        }
        8.0 + TOOLBAR_H + 8.0 + rows + SCROLL_H + 4.0 + 12.0
    }

    // ---------- edits ----------

    pub(crate) fn toggle_play(&mut self) {
        self.playing = !self.playing;
        if self.playing && self.out_time() >= self.out_duration() - 0.05 {
            self.seek_output(0.0);
        }
        self.sync_audio();
    }

    /// Adds a zoom starting at raw time `t` in the free space around it, focused on the cursor.
    pub(crate) fn add_zoom_at(&mut self, t: f64) {
        let d = self.duration();
        let Some((lo, hi)) = zoom::free_slot(&self.zooms, t, d) else { return };
        let start = t.clamp(lo, (hi - zoom::DEFAULT_LEN).max(lo));
        let mut r = ZoomRegion::new(start, (start + zoom::DEFAULT_LEN).min(hi));
        if let Some(rec) = &self.recording {
            if let Some((x, y)) = crate::camera::cursor_at(&rec.samples, t) {
                r.focus = ((x / rec.width as f64).clamp(0.0, 1.0), (y / rec.height as f64).clamp(0.0, 1.0));
            }
        }
        self.zooms.push(r);
        zoom::normalize(&mut self.zooms, d);
        self.selected = self.zooms.iter().position(|z| z.start == r.start);
        self.recompute();
    }

    /// Adds zooms around the clicks, in the gaps between existing zooms.
    pub(crate) fn suggest_zooms(&mut self) {
        let Some(rec) = &self.recording else { return };
        let found = zoom::suggest(&rec.samples, rec.width, rec.height, rec.duration);
        let fresh: Vec<ZoomRegion> = found
            .into_iter()
            .filter(|s| !self.zooms.iter().any(|z| s.start < z.end && z.start < s.end))
            .collect();
        self.suggest_note = Some(match fresh.len() {
            0 if self.zooms.is_empty() => "No clicks to zoom on in this recording.".into(),
            0 => "No new moments found.".into(),
            1 => "Added 1 zoom.".into(),
            n => format!("Added {n} zooms."),
        });
        self.zooms.extend(fresh);
        let d = self.duration();
        zoom::normalize(&mut self.zooms, d);
        self.selected = None;
        self.recompute();
    }

    pub(crate) fn delete_selected(&mut self) {
        if let Some(i) = self.selected.take() {
            if i < self.zooms.len() {
                self.zooms.remove(i);
                self.recompute();
            }
        }
    }

    fn neighbors(&self, i: usize) -> (f64, f64) {
        let lo = i.checked_sub(1).map_or(0.0, |j| self.zooms[j].end);
        let hi = self.zooms.get(i + 1).map_or(self.duration(), |n| n.start);
        (lo, hi)
    }

    /// Zooms the timeline by `k` around window x `at` (or the playhead).
    fn zoom_timeline(&mut self, k: f32, at: Option<f32>, window: &Window) {
        let (x0, w) = self.track_geom(window);
        let d = self.out_duration();
        if d <= 0.0 {
            return;
        }
        let max = ((MAX_PPS * d) / w as f64).max(1.0) as f32;
        let anchor_x = at.map(|x| x - x0).unwrap_or_else(|| (self.out_time() * self.pps(w)) as f32 - self.tl_scroll).clamp(0.0, w);
        let t = (anchor_x + self.tl_scroll) as f64 / self.pps(w);
        self.tl_zoom = (self.tl_zoom * k).clamp(1.0, max);
        self.tl_scroll = (t * self.pps(w)) as f32 - anchor_x;
        self.clamp_scroll(w);
    }

    fn fit_timeline(&mut self) {
        self.tl_zoom = 1.0;
        self.tl_scroll = 0.0;
    }

    // ---------- input ----------

    pub(crate) fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.previewing() {
            return;
        }
        let key = ev.keystroke.key.as_str();
        // While exporting only play/pause works.
        if !self.editing() && key != "space" {
            return;
        }
        let m = ev.keystroke.modifiers;
        let cmd = m.platform || m.control;
        let frame = editing::FRAME;
        let step = if m.shift { 1.0 } else { frame };
        match key {
            "z" if cmd && m.shift => self.redo_edit(),
            "z" if cmd => self.undo_edit(),
            "y" if cmd => self.redo_edit(),
            _ if cmd || m.alt => return,
            "space" => self.toggle_play(),
            "s" => self.split_at_playhead(),
            "z" => self.add_zoom_at(self.playhead),
            "backspace" | "delete" => {
                if self.selected.is_some() {
                    self.delete_selected();
                } else {
                    self.delete_segment();
                }
            }
            "escape" => {
                self.selected = None;
                self.edit_sel = None;
                self.confirm_reset = false;
            }
            "left" => self.seek_output(self.out_time() - step),
            "right" => self.seek_output((self.out_time() + step).min(self.out_duration() - frame)),
            "home" => self.seek_output(0.0),
            "end" => self.seek_output(self.out_duration() - frame),
            "-" => self.zoom_timeline(1.0 / 1.5, None, window),
            "=" | "+" => self.zoom_timeline(1.5, None, window),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn on_track_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editing() {
            return;
        }
        self.selected = None;
        self.playing = false;
        self.seek_output(self.out_at(ev.position.x, window));
        self.gesture = Gesture::Seek;
        cx.notify();
    }

    fn on_zoom_down(&mut self, i: usize, how: ZoomDrag, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        if !self.editing() || i >= self.zooms.len() {
            return;
        }
        let how = match how {
            ZoomDrag::Move { .. } => ZoomDrag::Move { grab: self.time_at(ev.position.x, window) - self.zooms[i].start },
            k => k,
        };
        self.selected = Some(i);
        self.edit_sel = None;
        self.panel = crate::Panel::Zoom;
        self.gesture = Gesture::Zoom { index: i, how };
        cx.notify();
    }

    fn on_trim_down(&mut self, end: bool, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        if !self.editing() {
            return;
        }
        let (_, w) = self.track_geom(window);
        let (x, pps) = (f32::from(ev.position.x), self.pps(w));
        self.playing = false;
        self.gesture = Gesture::Trim { end, x, pps, orig: self.edit.clone() };
        cx.notify();
    }

    fn on_timeline_scroll(&mut self, ev: &ScrollEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editing() {
            return;
        }
        let d = ev.delta.pixel_delta(px(16.));
        let (dx, dy) = (f32::from(d.x), f32::from(d.y));
        if ev.modifiers.control || ev.modifiers.platform {
            self.zoom_timeline((dy * 0.01).exp(), Some(f32::from(ev.position.x)), window);
        } else {
            let (_, w) = self.track_geom(window);
            self.tl_scroll -= if dx.abs() > dy.abs() { dx } else { dy };
            self.clamp_scroll(w);
        }
        cx.stop_propagation();
        cx.notify();
    }

    pub(crate) fn on_drag_move(&mut self, ev: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.gesture == Gesture::None {
            return;
        }
        if ev.pressed_button != Some(MouseButton::Left) {
            self.finish_gesture();
            cx.notify();
            return;
        }
        match self.gesture.clone() {
            Gesture::None => {}
            Gesture::Knob(k) => self.knob_to(k, ev.position),
            Gesture::Cam { grab: (ox, oy) } => {
                // Pointer → canvas fraction, keeping where on the webcam it was grabbed.
                let b = self.preview_bounds.get();
                let (cw, ch) = self.layout.canvas();
                let k = f32::from(b.size.width) / cw as f32;
                if k > 0.0 {
                    let x = (f32::from(ev.position.x) - f32::from(b.origin.x)) / k - ox;
                    let y = (f32::from(ev.position.y) - f32::from(b.origin.y)) / k - oy;
                    self.layout.cam_pos = Some((x / cw as f32, y / ch as f32));
                }
            }
            Gesture::Seek => self.seek_output(self.out_at(ev.position.x, window)),
            Gesture::Trim { end, x, pps, orig } => {
                let dt = (f32::from(ev.position.x) - x) as f64 / pps;
                let mut e = orig.clone();
                if end {
                    let last = orig.segments.last().unwrap().1;
                    e.set_end(last + dt, self.duration());
                    self.playhead = (e.segments.last().unwrap().1 - editing::FRAME).max(e.segments.last().unwrap().0);
                } else {
                    e.set_start(orig.segments[0].0 + dt);
                    self.playhead = e.segments[0].0;
                }
                // Applied live; saved, undoable and re-derived when the drag ends.
                self.edit = e;
            }
            Gesture::Zoom { index: i, how } => {
                if i >= self.zooms.len() {
                    return;
                }
                let t = self.time_at(ev.position.x, window);
                let (lo, hi) = self.neighbors(i);
                let r = &mut self.zooms[i];
                match how {
                    ZoomDrag::Move { grab } => {
                        let len = r.len();
                        r.start = (t - grab).clamp(lo, (hi - len).max(lo));
                        r.end = r.start + len;
                    }
                    ZoomDrag::Start => r.start = t.clamp(lo, r.end - zoom::MIN_LEN),
                    ZoomDrag::End => r.end = t.clamp(r.start + zoom::MIN_LEN, hi),
                }
                self.recompute();
            }
        }
        cx.notify();
    }

    /// Ends the gesture; a trim becomes one undoable edit.
    pub(crate) fn finish_gesture(&mut self) {
        if let Gesture::Trim { orig, .. } = std::mem::take(&mut self.gesture) {
            if orig != self.edit {
                self.edit_history.push(orig);
                self.after_edit();
            }
        }
    }

    pub(crate) fn on_drag_end(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.gesture != Gesture::None {
            self.finish_gesture();
            cx.notify();
        }
    }

    // ---------- timeline ----------

    pub(crate) fn timeline_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let (_, w) = self.track_geom(window);
        self.clamp_scroll(w);
        let editing = self.editing();
        let d = self.out_duration();
        // Keep the playhead in view while playing.
        if self.playing && editing {
            let x = (self.out_time() * self.pps(w)) as f32 - self.tl_scroll;
            if x < 0.0 || x > w {
                self.tl_scroll = (self.out_time() * self.pps(w)) as f32 - w * 0.1;
                self.clamp_scroll(w);
            }
        }
        let pps = self.pps(w);
        let scroll = self.tl_scroll;
        let x_of = |t: f64| (t * pps) as f32 - scroll;
        let visible = |x: f32, bw: f32| x + bw >= -2.0 && x <= w + 2.0;
        let has_cam = self.recording.as_ref().is_some_and(|r| r.webcam.is_some());
        let caps = self.edited_caps.clone().filter(|c| !c.lines.is_empty() && self.cap.enabled);

        let label = |text: &'static str, h: f32| div().h(px(h)).flex().items_center().child(text);
        let labels = div()
            .w(px(LABEL_W))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(ROW_GAP))
            .text_xs()
            .text_color(rgb(MUTED))
            .child(div().h(px(RULER_H)))
            .child(label("Video", VIDEO_H))
            .child(label("Audio", AUDIO_H))
            .when(has_cam, |d| d.child(label("Webcam", CAM_H)))
            .when(caps.is_some(), |d| d.child(label("Captions", CAP_H)))
            .child(label("Zoom", ZOOM_H));

        // Ruler: a label every `step` seconds of output time, at least ~70 px apart.
        let step = [0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0]
            .into_iter()
            .find(|s| s * pps >= 70.0)
            .unwrap_or(600.0);
        let first = ((scroll as f64 / pps) / step).floor().max(0.0) as usize;
        let last = if d > 0.0 { (((scroll + w) as f64 / pps).min(d) / step).floor() as usize } else { 0 };
        let ruler = div()
            .id("ruler")
            .relative()
            .h(px(RULER_H))
            .overflow_hidden()
            .children((first..=last).filter(|_| d > 0.0).map(|k| {
                let t = k as f64 * step;
                div()
                    .absolute()
                    .left(px(x_of(t)))
                    .top_0()
                    .h_full()
                    .flex()
                    .items_end()
                    .gap_1()
                    .text_size(px(10.))
                    .text_color(rgb(MUTED))
                    .child(div().w(px(1.)).h(px(6.)).bg(rgb(LINE)))
                    .child(if step < 1.0 { clock(t) } else { format!("{}:{:02}", (t / 60.0) as u32, (t % 60.0).round() as u32 % 60) })
            }));

        // Video: the kept segments end to end, with thumbnails; trim handles at the ends.
        let n_seg = self.edit.segments.len();
        let tw = VIDEO_H * 16.0 / 10.0;
        let mut video = div().relative().h(px(VIDEO_H)).w_full();
        if editing {
            for (i, &(a, b)) in self.edit.segments.iter().enumerate() {
                let x = x_of(self.edit.output_start(i));
                let bw = ((b - a) * pps) as f32;
                if !visible(x, bw) {
                    continue;
                }
                let sel = self.edit_sel == Some(i);
                let inner = (bw - if i + 1 < n_seg { 2.0 } else { 0.0 }).max(1.0);
                let mut block = div()
                    .id(("seg", i))
                    .absolute()
                    .left(px(x))
                    .top_0()
                    .w(px(inner))
                    .h_full()
                    .rounded(px(8.))
                    .bg(rgb(RAISED))
                    .overflow_hidden()
                    .cursor_pointer();
                if let Some(p) = self.preview.as_ref().filter(|p| !p.frames.is_empty()) {
                    let j0 = ((-x).max(0.0) / tw).floor() as usize;
                    let j1 = ((inner.min(w - x)) / tw).ceil().max(0.0) as usize;
                    for j in j0..j1 {
                        let left = j as f32 * tw;
                        let wj = tw.min(inner - left);
                        if wj <= 0.0 {
                            break;
                        }
                        let t = (a + ((left + wj / 2.0) as f64 / pps)).min(b);
                        let idx = ((t * p.fps) as usize).min(p.frames.len() - 1);
                        let mut im = img(p.frames[idx].clone()).absolute().left(px(left)).top_0().w(px(wj)).h_full().object_fit(ObjectFit::Cover);
                        if j == 0 {
                            im = im.rounded_l(px(8.));
                        }
                        if left + wj >= inner - 0.5 {
                            im = im.rounded_r(px(8.));
                        }
                        block = block.child(im);
                    }
                }
                block = block
                    .when(sel, |d| d.child(div().absolute().inset_0().rounded(px(8.)).bg(hsla(96. / 360., 0.85, 0.72, 0.18)).border_2().border_color(rgb(GREEN))))
                    .when(!sel, |d| d.child(div().absolute().inset_0().rounded(px(8.)).border_1().border_color(hsla(0., 0., 1., 0.10))))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            this.edit_sel = Some(i);
                            this.selected = None;
                            cx.notify();
                        }),
                    );
                // Trim handles on the clip's first and last edge.
                let handle = |end: bool, cx: &mut Context<Self>| {
                    div()
                        .id(if end { "trim-end" } else { "trim-start" })
                        .absolute()
                        .top_0()
                        .h_full()
                        .w(px(10.))
                        .when(end, |d| d.right_0().rounded_r(px(8.)))
                        .when(!end, |d| d.left_0().rounded_l(px(8.)))
                        .bg(hsla(0., 0., 1., 0.92))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor(CursorStyle::ResizeLeftRight)
                        .child(div().w(px(2.)).h(px(14.)).rounded_full().bg(hsla(0., 0., 0., 0.55)))
                        .on_mouse_down(MouseButton::Left, cx.listener(move |this, ev: &MouseDownEvent, window, cx| this.on_trim_down(end, ev, window, cx)))
                };
                if i == 0 {
                    block = block.child(handle(false, cx));
                }
                if i + 1 == n_seg {
                    block = block.child(handle(true, cx));
                }
                video = video.child(block);
            }
        }

        // Audio: the waveform read through the edit, over each segment's block.
        let mut audio = div().relative().h(px(AUDIO_H)).w_full();
        if editing {
            for (i, &(a, b)) in self.edit.segments.iter().enumerate() {
                let x = x_of(self.edit.output_start(i));
                let bw = ((b - a) * pps) as f32 - if i + 1 < n_seg { 2.0 } else { 0.0 };
                if visible(x, bw) {
                    let sel = self.edit_sel == Some(i);
                    audio = audio.child(
                        div()
                            .absolute()
                            .left(px(x))
                            .top_0()
                            .w(px(bw.max(1.0)))
                            .h_full()
                            .rounded(px(6.))
                            .bg(if sel { hsla(96. / 360., 0.85, 0.72, 0.14) } else { hsla(0., 0., 1., 0.04) }),
                    );
                }
            }
            if let Some(wave) = self.waveform.clone() {
                let edit = self.edit.clone();
                let peak = wave.iter().fold(0.0f32, |m, v| m.max(*v)).max(1e-4);
                audio = audio.child(
                    canvas(
                        |_, _, _| {},
                        move |bounds: Bounds<Pixels>, _, window, _| {
                            let (bx, by) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
                            let (bw, bh) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
                            let col = 2.0f32;
                            let per_col = (col as f64 / pps * crate::cutting::WAVE_RATE).ceil().max(1.0) as usize;
                            let mut cx_ = 0.0;
                            while cx_ < bw {
                                let t = (cx_ + scroll) as f64 / pps;
                                if t >= d {
                                    break;
                                }
                                let src = edit.output_to_source(t);
                                let i0 = (src * crate::cutting::WAVE_RATE) as usize;
                                let v = wave.get(i0..(i0 + per_col).min(wave.len())).map_or(0.0, |s| s.iter().fold(0.0f32, |m, v| m.max(*v)));
                                let h = ((v / peak).sqrt() * (bh - 4.0)).max(1.0);
                                window.paint_quad(gpui_kit::fill(
                                    Bounds::new(point(px(bx + cx_), px(by + (bh - h) / 2.0)), size(px(col - 0.5), px(h))),
                                    hsla(96. / 360., 0.6, 0.66, 0.85),
                                ));
                                cx_ += col;
                            }
                        },
                    )
                    .absolute()
                    .inset_0(),
                );
            } else {
                audio = audio.child(div().absolute().inset_0().flex().items_center().pl_2().text_xs().text_color(rgb(MUTED)).child("No audio"));
            }
        }

        // Webcam: where the webcam has frames, on the edited timeline.
        let cam_row = has_cam.then(|| {
            let spans = self.recording.as_ref().and_then(|r| r.webcam.as_ref()).map(|t| remap::spans(&t.spans, &self.edit)).unwrap_or_default();
            div().relative().h(px(CAM_H)).w_full().children(spans.into_iter().filter_map(|(a, b)| {
                let (x, bw) = (x_of(a), ((b - a) * pps) as f32);
                visible(x, bw).then(|| div().absolute().left(px(x)).top(px(3.)).w(px(bw.max(2.0))).h(px(CAM_H - 6.0)).rounded_full().bg(hsla(210. / 360., 0.7, 0.65, 0.7)))
            }))
        });

        // Captions (display only): the captions as they'll appear after the edit.
        let cap_row = caps.as_ref().map(|caps| {
            div().relative().h(px(CAP_H)).w_full().overflow_hidden().children(caps.lines.iter().filter_map(|c| {
                let (x, bw) = (x_of(c.start), ((c.end - c.start) * pps) as f32);
                visible(x, bw).then(|| {
                    div()
                        .absolute()
                        .left(px(x))
                        .top_0()
                        .w(px((bw - 1.0).max(2.0)))
                        .h_full()
                        .rounded(px(5.))
                        .bg(hsla(0., 0., 1., 0.08))
                        .overflow_hidden()
                        .px_1()
                        .flex()
                        .items_center()
                        .text_size(px(10.))
                        .text_color(rgb(0xd4d4d8))
                        .child(div().truncate().child(c.text.replace('\n', " ")))
                })
            }))
        });

        // Zoom track: each zoom where it falls on the edited timeline (one block per kept
        // part of it).
        let runs = remap::runs_with_offsets(&self.edit);
        let mut zoom_row = div()
            .id("zoom-row")
            .relative()
            .h(px(ZOOM_H))
            .w_full()
            .rounded(px(8.))
            .border_1()
            .border_color(rgb(LINE))
            .border_dashed()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                    if ev.click_count >= 2 && this.editing() {
                        cx.stop_propagation();
                        let t = this.time_at(ev.position.x, window);
                        this.add_zoom_at(t);
                        cx.notify();
                    }
                }),
            )
            .when(editing && self.zooms.is_empty(), |d| {
                d.flex().items_center().justify_center().text_xs().text_color(rgb(MUTED)).child("Double-click to add a zoom, or press Z")
            });
        if editing {
            for (i, r) in self.zooms.iter().enumerate() {
                for &(a, b, o) in &runs {
                    let (lo, hi) = (r.start.max(a), r.end.min(b));
                    if hi <= lo {
                        continue;
                    }
                    let (x, bw) = (x_of(o + lo - a), (((hi - lo) * pps) as f32).max(6.0));
                    if !visible(x, bw) {
                        continue;
                    }
                    let selected = self.selected == Some(i);
                    let text = format!("{}{}", DEPTH_LABELS[depth_index(r.depth)], if r.manual { " · Fixed" } else { "" });
                    let zhandle = |side: ZoomDrag, id: &'static str, cx: &mut Context<Self>| {
                        div()
                            .id((id, i * 1000 + (o * 10.0) as usize))
                            .absolute()
                            .top_0()
                            .h_full()
                            .w(px(8.))
                            .cursor(CursorStyle::ResizeLeftRight)
                            .on_mouse_down(MouseButton::Left, cx.listener(move |this, ev: &MouseDownEvent, window, cx| this.on_zoom_down(i, side, ev, window, cx)))
                    };
                    zoom_row = zoom_row.child(
                        div()
                            .id(("zoom", i * 1000 + (o * 10.0) as usize))
                            .absolute()
                            .left(px(x))
                            .top_0()
                            .w(px(bw))
                            .h_full()
                            .rounded(px(8.))
                            .bg(rgb(if selected { GREEN } else { ZOOM_BG }))
                            .border_1()
                            .border_color(rgb(if selected { GREEN } else { ZOOM_LINE }))
                            .overflow_hidden()
                            .cursor(CursorStyle::OpenHand)
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(if selected { ON_GREEN } else { GREEN }))
                            .child(div().truncate().px_2().child(text))
                            .on_mouse_down(MouseButton::Left, cx.listener(move |this, ev: &MouseDownEvent, window, cx| this.on_zoom_down(i, ZoomDrag::Move { grab: 0.0 }, ev, window, cx)))
                            .when(lo == r.start, |d| d.child(zhandle(ZoomDrag::Start, "zs", cx).left_0()))
                            .when(hi == r.end, |d| d.child(zhandle(ZoomDrag::End, "ze", cx).right_0())),
                    );
                }
            }
        }

        // Cut markers across the video and audio rows: solid red where time was removed,
        // a faint line at a plain split.
        let marks_top = RULER_H + ROW_GAP;
        let marks_h = VIDEO_H + ROW_GAP + AUDIO_H;
        let markers = editing.then(|| {
            self.edit.joins().into_iter().filter_map(move |(o, real)| {
                let x = x_of(o);
                (x >= -2.0 && x <= w + 2.0).then(|| {
                    div()
                        .absolute()
                        .top(px(marks_top - 4.0))
                        .h(px(marks_h + 8.0))
                        .left(px(x - if real { 1.5 } else { 0.5 }))
                        .w(px(if real { 3.0 } else { 1.0 }))
                        .rounded_full()
                        .bg(if real { gpui_kit::Hsla::from(rgb(RED)) } else { hsla(0., 0., 1., 0.5) })
                })
            })
        });

        let playhead = editing.then(|| {
            let x = x_of(self.out_time());
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(x - 1.0))
                .w(px(2.))
                .bg(rgb(TEXT))
                .child(div().absolute().top(px(-3.)).left(px(-4.)).size(px(10.)).rounded_full().bg(rgb(TEXT)))
        });

        // A thin bar showing what part of the clip is in view while zoomed in.
        let scrollbar = div().relative().h(px(SCROLL_H)).w_full().when(self.tl_zoom > 1.001, |dd| {
            let content = w * self.tl_zoom;
            dd.child(
                div()
                    .absolute()
                    .top_0()
                    .h_full()
                    .left(px(scroll / content * w))
                    .w(px((w / content * w).max(24.0)))
                    .rounded_full()
                    .bg(hsla(0., 0., 1., 0.25)),
            )
        });

        let track = div()
            .id("track")
            .relative()
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .flex()
            .flex_col()
            .gap(px(ROW_GAP))
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_track_down))
            .on_scroll(cx.listener(Self::on_timeline_scroll))
            .child(ruler)
            .child(video)
            .child(audio)
            .children(cam_row)
            .children(cap_row)
            .child(zoom_row)
            .child(scrollbar)
            .children(markers.into_iter().flatten())
            .children(playhead);

        div()
            .h(px(self.timeline_height()))
            .flex_none()
            .flex()
            .flex_col()
            .gap_2()
            .px(px(PAD_X))
            .pt_2()
            .pb_3()
            .rounded(px(R_CARD))
            .bg(rgb(CARD))
            .child(self.timeline_toolbar(cx))
            .child(div().flex().gap(px(GAP)).child(labels).child(track))
    }

    fn timeline_toolbar(&self, cx: &mut Context<Self>) -> Div {
        let editing = self.editing();
        let d = self.out_duration();
        let edited = editing && !self.edit.is_identity(self.duration());
        let small = |id: &'static str, svg: &'static [u8], tip: &'static str| {
            Button::new(id).cursor_pointer().secondary().rounded_full().size(px(32.)).icon(icon(svg)).tooltip(tip)
        };
        let play = Button::new("play").cursor_pointer()
            .primary()
            .rounded_full()
            .size(px(32.))
            .icon(if self.playing { IconName::Pause } else { IconName::Play })
            .disabled(!self.previewing())
            .tooltip("Play / Pause (Space)")
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.toggle_play();
                cx.notify();
            }));
        let time = div()
            .text_sm()
            .font_weight(FontWeight::BOLD)
            .child(if editing { format!("{} / {}", clock(self.out_time()), clock(d)) } else { "0:00.0 / 0:00.0".into() });
        // Before and after the edit, and how much was removed.
        let lengths = edited.then(|| {
            div()
                .text_xs()
                .text_color(rgb(MUTED))
                .child(format!("{} → {}  ·  {:.1} s removed", clock(self.duration()), clock(d), self.edit.removed(self.duration())))
        });

        let undo = small("undo", include_bytes!("../assets/icons/editor/undo-2.svg"), "Undo (⌘Z)")
            .disabled(!editing || !self.edit_history.can_undo())
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.undo_edit();
                cx.notify();
            }));
        let redo = small("redo", include_bytes!("../assets/icons/editor/redo-2.svg"), "Redo (⌘⇧Z)")
            .disabled(!editing || !self.edit_history.can_redo())
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.redo_edit();
                cx.notify();
            }));
        let can_split = editing && {
            let mut e = self.edit.clone();
            e.split_at(self.playhead)
        };
        let split = Button::new("split").cursor_pointer()
            .secondary()
            .rounded_full()
            .h(px(32.))
            .px_3()
            .icon(icon(include_bytes!("../assets/icons/scissors.svg")))
            .label("Split")
            .tooltip("Split at the playhead (S)")
            .disabled(!can_split)
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.split_at_playhead();
                cx.notify();
            }));
        // Deletes the selected zoom, or ripple-deletes the selected part of the clip.
        let (del_tip, can_del) = if self.selected.is_some() {
            ("Delete the selected zoom (⌫)", true)
        } else {
            ("Delete the selected part, closing the gap (⌫)", self.edit_sel.is_some() && self.edit.segments.len() > 1)
        };
        let delete = Button::new("delete").cursor_pointer()
            .secondary()
            .rounded_full()
            .size(px(32.))
            .icon(IconName::Delete)
            .tooltip(del_tip)
            .disabled(!editing || !can_del)
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                if this.selected.is_some() {
                    this.delete_selected();
                } else {
                    this.delete_segment();
                }
                cx.notify();
            }));
        // Reset asks first.
        let reset = edited.then(|| {
            if self.confirm_reset {
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child("Undo all cuts?")
                    .child(Button::new("reset-yes").cursor_pointer().danger().xsmall().rounded_full().label("Reset").on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.reset_edits();
                        cx.notify();
                    })))
                    .child(Button::new("reset-no").cursor_pointer().ghost().xsmall().rounded_full().label("Cancel").on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.confirm_reset = false;
                        cx.notify();
                    })))
            } else {
                div().child(Button::new("reset").cursor_pointer().ghost().xsmall().rounded_full().label("Reset all edits").on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.confirm_reset = true;
                    cx.notify();
                })))
            }
        });
        let zoom_out = small("tl-out", include_bytes!("../assets/icons/editor/zoom-out.svg"), "Zoom out (−, or ⌃ + scroll)")
            .disabled(!editing || self.tl_zoom <= 1.0)
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.zoom_timeline(1.0 / 1.5, None, window);
                cx.notify();
            }));
        let zoom_in = small("tl-in", include_bytes!("../assets/icons/editor/zoom-in.svg"), "Zoom in (+, or ⌃ + scroll)")
            .disabled(!editing)
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.zoom_timeline(1.5, None, window);
                cx.notify();
            }));
        let fit = small("tl-fit", include_bytes!("../assets/icons/move-horizontal.svg"), "Fit the whole clip")
            .disabled(!editing || self.tl_zoom <= 1.0)
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.fit_timeline();
                cx.notify();
            }));
        let add = Button::new("add-zoom").cursor_pointer()
            .secondary()
            .rounded_full()
            .h(px(32.))
            .px_3()
            .icon(icon(include_bytes!("../assets/icons/zoom-in.svg")))
            .label("Add zoom")
            .tooltip("Add a zoom at the playhead (Z)")
            .disabled(!editing || zoom::free_slot(&self.zooms, self.playhead, self.duration()).is_none())
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.add_zoom_at(this.playhead);
                cx.notify();
            }));
        let suggest = Button::new("suggest-zooms").cursor_pointer()
            .secondary()
            .rounded_full()
            .h(px(32.))
            .px_3()
            .icon(icon(include_bytes!("../assets/icons/wand-sparkles.svg")))
            .label("Suggest zooms")
            .tooltip("Add zooms around the clicks in the recording")
            .disabled(!editing)
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.suggest_zooms();
                cx.notify();
            }));
        let sep = || div().w(px(1.)).h(px(20.)).bg(rgb(LINE));
        div()
            .h(px(TOOLBAR_H))
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .child(div().flex().items_center().gap_3().min_w_0().child(play).child(time).children(lengths))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .children(reset)
                    .child(undo)
                    .child(redo)
                    .child(split)
                    .child(delete)
                    .child(sep())
                    .child(zoom_out)
                    .child(zoom_in)
                    .child(fit)
                    .child(sep())
                    .child(add)
                    .child(suggest),
            )
    }

    // ---------- Zoom tab ----------

    pub(crate) fn zoom_panel(&self, cx: &mut Context<Self>) -> Div {
        let selected = self.selected.and_then(|i| self.zooms.get(i).map(|r| (i, *r)));
        // The "Selected zoom" section only exists while one is selected; otherwise a one-line
        // hint sits under Zooms.
        let editor = selected.map(|(i, r)| self.zoom_editor(i, r, cx));
        let hint = selected.is_none().then(|| {
            note(if self.editing() { "Click a zoom on the timeline to adjust it. Double-click the track or press Z to add one." } else { "Zooms appear on the timeline after recording." })
        });

        let auto = Switch::new("auto-zoom").cursor_pointer()
            .checked(self.auto_zoom)
            .label("Auto-zoom new recordings")
            .on_click(cx.listener(|this, v: &bool, _, cx| {
                this.auto_zoom = *v;
                cx.notify();
            }));
        // Destructive: small, red, and only there when there is something to remove.
        let clear = (!self.zooms.is_empty()).then(|| gpui_kit::component::button::Button::new("clear-zooms").cursor_pointer()
            .ghost()
            .xsmall()
            .rounded_full()
            .text_color(rgb(crate::RED))
            .label(match self.zooms.len() {
                1 => "Remove the zoom".to_string(),
                n => format!("Remove all {n} zooms"),
            })
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.zooms.clear();
                this.selected = None;
                this.suggest_note = None;
                this.recompute();
                cx.notify();
            })));
        let connect = Switch::new("connect-zooms").cursor_pointer()
            .checked(self.motion.connect)
            .label("Connect nearby zooms")
            .on_click(cx.listener(|this, v: &bool, _, cx| {
                this.motion.connect = *v;
                this.recompute();
                cx.notify();
            }));

        div()
            .flex()
            .flex_col()
            .gap_5()
            .children(editor.map(|e| section("Selected zoom", e)))
            .child(section(
                "Zooms",
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .children(hint)
                    .child(auto)
                    .when_some(self.suggest_note.clone(), |d, n| d.child(note(&n)))
                    .children(clear.map(|c| div().flex().justify_end().child(c))),
            ))
            .child(section(
                "Motion",
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .gap_4()
                    .child(self.knob(crate::knob::Knob::ZoomIn, "Zoom in", cx))
                    .child(self.knob(crate::knob::Knob::ZoomOut, "Zoom out", cx))
                    .child(connect)
                    .child(note("Zooms less than 1.5 s apart pan across instead of zooming out and back in.")),
            ))
    }

    fn zoom_editor(&self, i: usize, r: ZoomRegion, cx: &mut Context<Self>) -> Div {
        let depth = crate::ui::segmented(
            "zoom-depth",
            DEPTH_LABELS
                .iter()
                .enumerate()
                .map(|(k, l)| {
                    let on: crate::ui::OnClick = Box::new(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if let Some(z) = this.zooms.get_mut(i) {
                            z.depth = DEPTHS[k];
                            this.recompute();
                            cx.notify();
                        }
                    }));
                    ((*l).into(), depth_index(r.depth) == k, on)
                })
                .collect(),
        );
        let focus = |manual: bool| -> crate::ui::OnClick {
            Box::new(cx.listener(move |this, _: &ClickEvent, _, cx| {
                if let Some(z) = this.zooms.get_mut(i) {
                    z.manual = manual;
                    this.recompute();
                    cx.notify();
                }
            }))
        };
        let mode = crate::ui::segmented(
            "zoom-focus",
            vec![("Follow cursor".into(), !r.manual, focus(false)), ("Fixed point".into(), r.manual, focus(true))],
        );
        let span = note(&format!("{} – {}  ·  {:.1} s", clock(r.start), clock(r.end), r.len()));
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(span)
            .child(field("Zoom level", depth))
            .child(field("Focus", mode))
            .when(r.manual, |d| d.child(field("Focus point", self.focus_picker(i, r, cx))))
            .child(
                button("delete-selected", "Delete zoom", cx)
                    .small()
                    .w_full()
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.delete_selected();
                        cx.notify();
                    })),
            )
    }

    /// The full screen at the zoom's start; click to set the point the zoom holds.
    fn focus_picker(&self, i: usize, r: ZoomRegion, cx: &mut Context<Self>) -> AnyElement {
        let (Some(preview), Some(rec)) = (&self.preview, &self.recording) else { return div().into_any_element() };
        let n = preview.frames.len();
        let t = (r.start + r.len() / 2.0).min(rec.duration);
        let frame = preview.frames[((t * preview.fps) as usize).min(n.saturating_sub(1))].clone();
        let (pw, ph) = (288.0_f32, 288.0 * rec.height as f32 / rec.width.max(1) as f32);
        let bounds = self.picker_bounds.clone();
        let store = self.picker_bounds.clone();
        div()
            .id("focus-picker")
            .relative()
            .w(px(pw))
            .h(px(ph))
            .rounded(px(12.))
            .overflow_hidden()
            .cursor(CursorStyle::Crosshair)
            .child(img(frame).size_full().rounded(px(12.)).object_fit(ObjectFit::Fill))
            .child(canvas(move |b, _, _| store.set(b), |_, _, _, _| {}).absolute().size_full())
            .child(
                div()
                    .absolute()
                    .left(px(r.focus.0 as f32 * pw - 7.0))
                    .top(px(r.focus.1 as f32 * ph - 7.0))
                    .size(px(14.))
                    .rounded_full()
                    .border_2()
                    .border_color(rgb(0xffffff))
                    .bg(rgb(GREEN)),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                    let b = bounds.get();
                    let (bw, bh) = (f32::from(b.size.width).max(1.0), f32::from(b.size.height).max(1.0));
                    let fx = ((f32::from(ev.position.x) - f32::from(b.origin.x)) / bw).clamp(0.0, 1.0) as f64;
                    let fy = ((f32::from(ev.position.y) - f32::from(b.origin.y)) / bh).clamp(0.0, 1.0) as f64;
                    if let Some(z) = this.zooms.get_mut(i) {
                        z.focus = (fx, fy);
                        this.recompute();
                        cx.notify();
                    }
                }),
            )
            .into_any_element()
    }
}

/// Closest offered zoom level.
fn depth_index(depth: f64) -> usize {
    DEPTHS
        .iter()
        .enumerate()
        .min_by(|a, b| (a.1 - depth).abs().total_cmp(&(b.1 - depth).abs()))
        .map_or(0, |(k, _)| k)
}
