//! The Captions tab: auto captions on/off, the speech models (download, resume, delete),
//! language and threads, transcription progress, the caption list editor, and caption style.

use crate::theme as tk;
use crate::captions::render::{Area, CaptionStyle, Position};
use crate::captions::segment::Rules;
use crate::captions::transcribe::{self, LANGUAGES};
use crate::captions::{self, Caption, Captions};
use crate::dropdown::{Dropdown, DropdownEvent};
use crate::knob::Knob;
use crate::models::{self, Download, MODELS, Model};
use crate::{Clipper, Stage, button, color_hex, hex_color, note, section};
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{ClickEvent, Div, Entity, FontWeight, SharedString, Window, div, px, rgb};

const TEXT_COLORS: [u32; 6] = tk::content::CAPTION_TEXT;
const PILL_COLORS: [u32; 5] = tk::content::CAPTION_PILL;

/// Captions settings behind the tab.
#[derive(Clone, Copy, Debug)]
pub struct CaptionSettings {
    /// Transcribe new recordings automatically and burn the captions into the export.
    pub enabled: bool,
    pub model: &'static Model,
    /// Index into `LANGUAGES`.
    pub language: usize,
    pub threads: usize,
    /// Also write an .srt next to the exported MP4.
    pub srt: bool,
}

impl Default for CaptionSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            model: models::by_id(models::RECOMMENDED).unwrap(),
            language: 0,
            threads: transcribe::default_threads(),
            srt: false,
        }
    }
}

/// The captions dropdowns (created once, with the Clipper entity).
pub struct CaptionDropdowns {
    pub model: Entity<Dropdown>,
    pub language: Entity<Dropdown>,
    pub font: Entity<Dropdown>,
}

impl CaptionDropdowns {
    pub fn new(cx: &mut Context<Clipper>, subs: &mut Vec<gpui_kit::Subscription>) -> Self {
        let settings = CaptionSettings::default();
        let model = cx.new(|cx| {
            let mut d = Dropdown::new("caption-model", "Model");
            let items = MODELS.iter().map(|m| SharedString::from(m.name)).collect();
            d.set_items(items, MODELS.iter().position(|m| m.id == settings.model.id), cx);
            d
        });
        let language = cx.new(|cx| {
            let mut d = Dropdown::new("caption-language", "Language");
            d.set_items(LANGUAGES.iter().map(|(_, n)| SharedString::from(*n)).collect(), Some(0), cx);
            d.set_disabled(settings.model.english_only, cx);
            d
        });
        let font = cx.new(|cx| {
            let mut d = Dropdown::new("caption-font", "Font");
            let items = captions::render::fonts().iter().map(|f| SharedString::from(f.name.clone())).collect();
            d.set_items(items, Some(0), cx);
            d
        });
        subs.push(cx.subscribe(&model, |this, _, ev: &DropdownEvent, cx| {
            let DropdownEvent::Select(i) = ev;
            this.cap.model = &MODELS[*i];
            let en = this.cap.model.english_only;
            this.cap_dropdowns.language.update(cx, |d, cx| d.set_disabled(en, cx));
            cx.notify();
        }));
        subs.push(cx.subscribe(&language, |this, _, ev: &DropdownEvent, cx| {
            let DropdownEvent::Select(i) = ev;
            this.cap.language = *i;
            cx.notify();
        }));
        subs.push(cx.subscribe(&font, |this, _, ev: &DropdownEvent, cx| {
            let DropdownEvent::Select(i) = ev;
            this.cap_style.font = *i;
            cx.notify();
        }));
        Self { model, language, font }
    }
}

/// A caption line open for typing: which line, its text field, and the subscription that
/// saves it on Enter or blur. One value, so the field and its subscription come and go
/// together.
pub struct CaptionEdit {
    pub index: usize,
    pub input: Entity<InputState>,
    _sub: gpui_kit::Subscription,
}

impl CaptionEdit {
    pub fn is(&self, i: usize) -> bool {
        self.index == i
    }
}

impl Clipper {
    // ---------- actions ----------

    /// Caption line rules for the current output ratio.
    pub(crate) fn caption_rules(&self) -> Rules {
        Rules::for_ratio(self.layout.ratio)
    }

    pub(crate) fn start_download(&mut self, model: &'static Model, cx: &mut Context<Self>) {
        if self.download.is_some() {
            return;
        }
        self.cap_error = None;
        self.download = Some(Download::start(model));
        cx.notify();
    }

    /// Transcribes the current recording (after Stop only; never while recording).
    pub(crate) fn start_transcription(&mut self, cx: &mut Context<Self>) {
        let Some(rec) = self.recording.clone() else { return };
        if !matches!(self.stage, Stage::Review) || self.stt.is_some() {
            return;
        }
        if !self.cap.model.installed() {
            self.cap_error = Some(format!("Download {} first.", self.cap.model.name));
            cx.notify();
            return;
        }
        self.cap_error = None;
        let settings = transcribe::Settings {
            model: self.cap.model,
            language: LANGUAGES[self.cap.language].0.to_string(),
            threads: self.cap.threads,
        };
        self.stt = Some(transcribe::Job::start(transcribe::recording_audio(rec), settings, Box::new(transcribe::Whisper)));
        cx.notify();
    }

    /// Polls the model download and the transcription (from the app ticker).
    pub(crate) fn poll_captions(&mut self, cx: &mut Context<Self>) {
        if let Some(d) = self.download.as_mut() {
            match d.poll() {
                Some(Ok(())) => {
                    self.download = None;
                    // Captions are on and a recording is waiting for its transcript.
                    if self.cap.enabled && self.captions.is_none() && self.recording.is_some() {
                        self.start_transcription(cx);
                    }
                }
                Some(Err(e)) => {
                    self.download = None;
                    if e.to_string() != models::CANCELLED {
                        self.cap_error = Some(e.to_string());
                    }
                }
                None => {}
            }
            cx.notify();
        }
        if let Some(job) = self.stt.as_mut() {
            match job.poll() {
                Some(Ok(words)) => {
                    self.stt = None;
                    let mut caps = Captions {
                        lines: captions::segment::segment(&words, self.caption_rules()),
                        model: self.cap.model.id.into(),
                        language: LANGUAGES[self.cap.language].0.into(),
                    };
                    caps.lines.retain(|c| !c.text.trim().is_empty());
                    if caps.lines.is_empty() {
                        self.cap_error = Some("No speech found in the recording.".into());
                    }
                    self.set_captions(caps);
                }
                Some(Err(e)) => {
                    self.stt = None;
                    if e.to_string() != transcribe::CANCELLED {
                        self.cap_error = Some(e.to_string());
                    }
                }
                None => {}
            }
            cx.notify();
        }
    }

    /// Replaces the captions and saves them with the recording.
    pub(crate) fn set_captions(&mut self, caps: Captions) {
        if let Some(rec) = &self.recording {
            if let Err(e) = caps.save(&rec.dir) {
                eprintln!("Clipper: could not save captions: {e}");
            }
        }
        self.captions = Some(caps);
        self.cap_art = None;
        self.refresh_edit_derived();
    }

    fn edit_captions(&mut self, f: impl FnOnce(&mut Vec<Caption>, usize)) {
        let max = self.caption_rules().max_chars;
        if let Some(mut caps) = self.captions.clone() {
            f(&mut caps.lines, max);
            self.set_captions(caps);
        }
    }

    /// Saves the caption being typed into (if any) and closes its editor.
    fn commit_caption_edit(&mut self, cx: &mut Context<Self>) {
        let Some(CaptionEdit { index: i, input, .. }) = self.cap_edit.take() else { return };
        let text = input.read(cx).value().to_string();
        self.edit_captions(|lines, max| {
            if let Some(c) = lines.get_mut(i) {
                if !text.trim().is_empty() && text.trim() != c.text.replace('\n', " ").trim() {
                    c.set_text(&text, max);
                }
            }
        });
        cx.notify();
    }

    /// Opens caption `i` for typing (saving any other caption being edited first).
    fn begin_edit(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.cap_edit.as_ref().is_some_and(|e| e.is(i)) {
            return;
        }
        self.commit_caption_edit(cx);
        let Some(c) = self.captions.as_ref().and_then(|c| c.lines.get(i)) else { return };
        let text = c.text.replace('\n', " ");
        let input = cx.new(|cx| InputState::new(window, cx).default_value(text));
        input.update(cx, |s, cx| s.focus(window, cx));
        let sub = cx.subscribe_in(&input, window, move |this, _, ev: &InputEvent, _, cx| match ev {
            InputEvent::PressEnter { .. } | InputEvent::Blur => {
                if this.cap_edit.as_ref().is_some_and(|e| e.is(i)) {
                    this.commit_caption_edit(cx);
                }
            }
            _ => {}
        });
        self.cap_edit = Some(CaptionEdit { index: i, input, _sub: sub });
        cx.notify();
    }

    // ---------- UI ----------

    pub(crate) fn captions_panel(&self, cx: &mut Context<Self>) -> Div {
        let toggle = Switch::new("captions-on").cursor_pointer()
            .checked(self.cap.enabled)
            .small()
            .on_click(cx.listener(|this, v: &bool, _, cx| {
                this.cap.enabled = *v;
                this.cap_art = None;
                if *v && this.captions.is_none() && this.recording.is_some() && this.cap.model.installed() {
                    this.start_transcription(cx);
                }
                cx.notify();
            }));
        let head = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_size(px(tk::T13)).font_weight(FontWeight::MEDIUM).child("Auto captions"))
                    .child(toggle),
            )
            .child(note("Transcribed on this Mac after you stop recording. Nothing is uploaded."));
        let mut body = div().flex().flex_col().gap_5().child(head);
        if self.cap.enabled {
            body = body
                .child(section("Transcript", self.transcript_section(cx)))
                .child(section("Model", self.model_section(cx)))
                .child(section("Style", self.caption_style_section(cx)));
        } else {
            body = body.child(section("Model", self.model_section(cx)));
        }
        body.when_some(self.cap_error.clone(), |d, e| {
            d.child(div().p_3().rounded(px(12.)).bg(rgb(tk::raised())).text_size(px(tk::T12)).text_color(rgb(tk::error_text())).child(e))
        })
    }

    fn model_section(&self, cx: &mut Context<Self>) -> Div {
        let rows = MODELS.iter().map(|m| self.model_row(m, cx));
        // The list is the model picker (no second dropdown for the same choice).
        let lang_note = self.cap.model.english_only.then(|| note("This model only understands English."));
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().flex().flex_col().gap_1p5().children(rows))
            .child(field("Language", self.cap_dropdowns.language.clone()))
            .children(lang_note)
            .child(self.knob(Knob::Threads, "CPU threads", cx))
            .child(note(&format!(
                "Models are stored in {}.",
                crate::display_path(&models::models_dir())
            )))
    }

    fn model_row(&self, m: &'static Model, cx: &mut Context<Self>) -> gpui_kit::Stateful<Div> {
        let selected = self.cap.model.id == m.id;
        let installed = m.installed();
        let dl = self.download.as_ref().filter(|d| d.model.id == m.id);
        let busy = self.download.is_some();
        let partial = m.partial();
        let info = format!("{}, about {} MB of memory", m.size_label(), m.ram_mb);
        let recommended = m.id == models::RECOMMENDED;
        let action: gpui_kit::AnyElement = match (dl, installed) {
            (Some(d), _) => {
                let pct = d.fraction() * 100.0;
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().w(px(70.)).child(Progress::new(("dl", m.size as usize)).value(pct).color(rgb(tk::selected()))))
                    .child(div().w(px(30.)).text_size(px(tk::T12)).text_color(rgb(tk::ink_soft())).child(format!("{pct:.0}%")))
                    .child(
                        button("dl-cancel", "Cancel", cx)
                            .xsmall()
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                if let Some(d) = &this.download {
                                    d.cancel();
                                }
                                cx.notify();
                            })),
                    )
                    .into_any_element()
            }
            (None, true) => div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().text_size(px(tk::T12)).font_weight(gpui_kit::FontWeight::MEDIUM).text_color(rgb(tk::selected())).child("Ready"))
                .child(
                    gpui_kit::component::button::Button::new(SharedString::from(format!("del-{}", m.id))).cursor_pointer()
                        .ghost()
                        .xsmall()
                        .rounded_full()
                        .icon(crate::editor::icons::trash())
                        .text_color(rgb(tk::error_text()))
                        .tooltip("Delete model")
                        .disabled(self.stt.is_some() && selected)
                        .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                            m.delete();
                            cx.notify();
                        })),
                )
                .into_any_element(),
            (None, false) => gpui_kit::component::button::Button::new(SharedString::from(format!("dl-{}", m.id))).cursor_pointer()
                .primary()
                .xsmall()
                .rounded_full()
                .px_3()
                .label(if partial > 0 { "Resume" } else { "Download" })
                .disabled(busy)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.start_download(m, cx)))
                .into_any_element(),
        };
        div()
            .id(SharedString::from(format!("model-{}", m.id)))
            .p_2()
            .pl_3()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .rounded(px(12.))
            .bg(rgb(tk::raised()))
            .border_1()
            .border_color(rgb(if selected { tk::text() } else { tk::raised() }))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(tk::hover())))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.cap.model = m;
                let i = MODELS.iter().position(|x| x.id == m.id);
                let en = m.english_only;
                this.cap_dropdowns.model.update(cx, |d, cx| {
                    let items = MODELS.iter().map(|m| SharedString::from(m.name)).collect();
                    d.set_items(items, i, cx);
                });
                this.cap_dropdowns.language.update(cx, |d, cx| d.set_disabled(en, cx));
                cx.notify();
            }))
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().text_size(px(tk::T13)).truncate().child(m.name))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().text_size(px(tk::T12)).text_color(rgb(tk::ink_soft())).truncate().child(info))
                            .when(recommended, |d| {
                                d.child(
                                    div()
                                        .px(px(6.))
                                        .rounded_full()
                                        .bg(crate::ui::accent_tint())
                                        .text_color(rgb(tk::selected()))
                                        .text_size(px(tk::T12))
                                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                                        .child("Recommended"),
                                )
                            }),
                    ),
            )
            .child(div().flex_none().child(action))
    }

    fn transcript_section(&self, cx: &mut Context<Self>) -> Div {
        let mut d = div().flex().flex_col().gap_2();
        if let Some(job) = &self.stt {
            let pct = job.fraction() * 100.0;
            return d
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .text_size(px(tk::T13))
                        .child("Transcribing...")
                        .child(div().text_size(px(tk::T12)).text_color(rgb(tk::ink_soft())).child(format!("{pct:.0}%"))),
                )
                .child(Progress::new("stt").value(pct).color(rgb(tk::selected())))
                .child(
                    button("stt-cancel", "Cancel", cx)
                        .small()
                        .w_full()
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            if let Some(j) = &this.stt {
                                j.cancel();
                            }
                            cx.notify();
                        })),
                );
        }
        let can = self.recording.is_some() && matches!(self.stage, Stage::Review);
        let label = if self.captions.is_some() { "Transcribe again" } else { "Transcribe" };
        let transcribe = button("stt-run", label, cx)
            .small()
            .w_full()
            .disabled(!can || self.download.is_some())
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.start_transcription(cx)));
        match &self.captions {
            Some(caps) if !caps.lines.is_empty() => {
                d = d.child(self.caption_list(caps, cx));
                let srt = Switch::new("captions-srt").cursor_pointer()
                    .checked(self.cap.srt)
                    .small()
                    .label("Also save .srt with the export")
                    .on_click(cx.listener(|this, v: &bool, _, cx| {
                        this.cap.srt = *v;
                        cx.notify();
                    }));
                d.child(srt).child(transcribe)
            }
            _ => {
                let msg = if self.recording.is_none() {
                    "Captions are made after you stop recording."
                } else if !self.cap.model.installed() {
                    "Download a model below, then transcribe."
                } else {
                    "No captions yet."
                };
                d.child(note(msg)).child(transcribe)
            }
        }
    }

    fn caption_list(&self, caps: &Captions, cx: &mut Context<Self>) -> Div {
        let current = caps.at(self.playhead);
        let selected = self.cap_selected;
        let n = caps.lines.len();
        let rows = caps.lines.iter().enumerate().map(|(i, c)| {
            let editing = self.cap_edit.as_ref().filter(|e| e.is(i)).map(|e| e.input.clone());
            let is_sel = selected == Some(i);
            let is_now = current == Some(i);
            let text: gpui_kit::AnyElement = match editing {
                Some(state) => Input::new(&state).small().into_any_element(),
                None => div()
                    .text_size(px(tk::T13))
                    .text_color(rgb(if is_now { tk::text() } else { tk::text() }))
                    .child(c.text.replace('\n', " "))
                    .into_any_element(),
            };
            let actions = is_sel.then(|| {
                div()
                    .flex()
                    .gap_1()
                    .pt_1()
                    .child(button("cap-split", "Split", cx).xsmall().disabled(c.words.len() < 2).on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.edit_captions(|lines, max| {
                                if let Some((a, b)) = lines.get(i).and_then(|c| c.split(max)) {
                                    lines.splice(i..=i, [a, b]);
                                }
                            });
                            cx.notify();
                        },
                    )))
                    .child(button("cap-merge", "Merge ↓", cx).xsmall().disabled(i + 1 >= n).on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.edit_captions(|lines, max| {
                                if i + 1 < lines.len() {
                                    let m = lines[i].merge(&lines[i + 1], max);
                                    lines.splice(i..=i + 1, [m]);
                                }
                            });
                            cx.notify();
                        },
                    )))
                    .child(button("cap-delete", "Delete", cx).xsmall().on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.edit_captions(|lines, _| {
                                if i < lines.len() {
                                    lines.remove(i);
                                }
                            });
                            this.cap_selected = None;
                            cx.notify();
                        },
                    )))
            });
            div()
                .id(("caption", i))
                .p_2()
                .flex()
                .flex_col()
                .gap_0p5()
                .rounded(px(10.))
                .bg(rgb(if is_sel { tk::pressed() } else if is_now { tk::hover() } else { tk::raised() }))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(tk::hover())))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    // Seek the preview to the caption and open it for typing.
                    if this.cap_edit.as_ref().is_some_and(|e| e.is(i)) {
                        return;
                    }
                    if let Some(c) = this.captions.as_ref().and_then(|c| c.lines.get(i)) {
                        this.playhead = c.start + 0.001;
                        this.audio_restart = true;
                    }
                    this.cap_selected = Some(i);
                    this.begin_edit(i, window, cx);
                    cx.notify();
                }))
                .child(
                    div()
                        .text_size(px(tk::T12))
                        .text_color(rgb(if is_now { tk::selected() } else { tk::ink_soft() }))
                        .child(format!("{} – {}", captions::short_time(c.start), captions::short_time(c.end))),
                )
                .child(text)
                .children(actions)
        });
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .justify_between()
                    .text_size(px(tk::T12))
                    .text_color(rgb(tk::ink_soft()))
                    .child(format!("{n} caption{}", if n == 1 { "" } else { "s" }))
                    .child("Click a caption to fix it. Enter saves."),
            )
            // The kit's scroll area: the panel scrolls too, and plain nested scrolling would
            // lose the wheel to it.
            .child(
                div()
                    .max_h(px(360.))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .pr(px(8.))
                    .children(rows)
                    .overflow_y_scrollbar()
                    .id("caption-list"),
            )
    }

    fn caption_style_section(&self, cx: &mut Context<Self>) -> Div {
        let st = &self.cap_style;
        let colors = |id: &'static str, palette: &'static [u32], current: [u8; 3], set: fn(&mut CaptionStyle, [u8; 3]), cx: &mut Context<Self>| {
            div().flex().gap_1p5().children(palette.iter().enumerate().map(move |(i, &v)| {
                let selected = color_hex(current) == v;
                div()
                    .id((id, i))
                    .size(px(26.))
                    .p(px(2.))
                    .rounded(px(9.))
                    .border_2()
                    .border_color(rgb(if selected { tk::text() } else { tk::mist() }))
                    .cursor_pointer()
                    .child(div().size_full().rounded(px(6.)).border_1().border_color(rgb(tk::stone())).bg(rgb(v)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        set(&mut this.cap_style, hex_color(v));
                        cx.notify();
                    }))
            }))
        };
        let segmented = |id: &'static str, labels: Vec<&'static str>, selected: usize, f: fn(&mut CaptionStyle, usize), cx: &mut Context<Self>| {
            crate::ui::segmented(
                id,
                labels
                    .into_iter()
                    .enumerate()
                    .map(|(ix, l)| {
                        let on: crate::ui::OnClick = Box::new(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            f(&mut this.cap_style, ix);
                            cx.notify();
                        }));
                        (l.into(), ix == selected, on)
                    })
                    .collect(),
            )
        };
        let switch = |id: &'static str, label: &'static str, on: bool, f: fn(&mut CaptionStyle, bool), cx: &mut Context<Self>| {
            Switch::new(id).cursor_pointer().checked(on).small().label(label).on_click(cx.listener(move |this, v: &bool, _, cx| {
                f(&mut this.cap_style, *v);
                cx.notify();
            }))
        };
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(field("Font", self.cap_dropdowns.font.clone()))
            .child(self.knob(Knob::CapSize, "Size", cx))
            .child(row("Text color", colors("cap-color", &TEXT_COLORS, st.color, |s, c| s.color = c, cx)))
            .child(self.knob(Knob::CapOutline, "Outline", cx))
            .child(field(
                "Position",
                segmented(
                    "cap-position",
                    Position::ALL.iter().map(|p| p.label()).collect(),
                    Position::ALL.iter().position(|p| *p == st.position).unwrap_or(2),
                    |s, i| s.position = Position::ALL[i],
                    cx,
                ),
            ))
            .child(field(
                "Place in",
                segmented(
                    "cap-area",
                    Area::ALL.iter().map(|a| a.label()).collect(),
                    Area::ALL.iter().position(|a| *a == st.area).unwrap_or(1),
                    |s, i| s.area = Area::ALL[i],
                    cx,
                ),
            ))
            .child(switch("cap-pill", "Background pill", st.pill, |s, v| s.pill = v, cx))
            .when(st.pill, |d| {
                d.child(row("Pill color", colors("cap-pill-color", &PILL_COLORS, st.pill_color, |s, c| s.pill_color = c, cx)))
                    .child(self.knob(Knob::PillOpacity, "Pill opacity", cx))
            })
            .child(switch("cap-highlight", "Highlight the spoken word", st.highlight, |s, v| s.highlight = v, cx))
            .when(st.highlight, |d| {
                d.child(row(
                    "Highlight color",
                    colors("cap-hl-color", &TEXT_COLORS, st.highlight_color, |s, c| s.highlight_color = c, cx),
                ))
            })
            .child(switch("cap-upper", "Uppercase", st.uppercase, |s, v| s.uppercase = v, cx))
    }
}

fn field(label: &'static str, control: impl IntoElement) -> Div {
    div().flex().flex_col().gap_1().child(div().text_size(px(tk::T12)).text_color(rgb(tk::ink_soft())).child(label)).child(control)
}

fn row(label: &'static str, control: impl IntoElement) -> Div {
    div().flex().items_center().justify_between().gap_2().child(div().text_size(px(tk::T13)).child(label)).child(control)
}

impl Clipper {
    /// The caption to draw on the preview right now (the one at the playhead, or a sample
    /// before there's a transcript), rendered with the export's renderer and cached until
    /// its text, highlighted word, style or layout changes. Logical rect.
    pub(crate) fn caption_layer(
        &mut self,
        frame: &crate::layout::Frame,
        display_w: f32,
        window: &mut Window,
    ) -> Option<(std::sync::Arc<gpui_kit::RenderImage>, (f32, f32, f32, f32))> {
        let reviewing = matches!(self.stage, Stage::Review | Stage::Exporting);
        let pick = if !self.cap.enabled {
            None
        } else {
            // Captions read through the edit, at the playhead's edited time.
            let t = self.out_time();
            match (&self.edited_caps, &self.captions) {
                (Some(c), _) if reviewing => c.at(t).map(|i| {
                    let line = c.lines[i].clone();
                    let w = line.word_at(t);
                    (line, w)
                }),
                (None, Some(_)) if reviewing => None,
                (_, Some(_)) => None,
                (_, None) => Some((sample_caption(self.caption_rules().max_chars), Some(1))),
            }
        };
        let Some((caption, word)) = pick else {
            if let Some((_, img, _)) = self.cap_art.take() {
                let _ = window.drop_image(img);
            }
            return None;
        };
        let sf = window.scale_factor();
        let key = format!("{:?}|{word:?}|{}|{display_w}|{frame:?}|{sf}", caption.text, self.cap_style.key());
        if let Some((k, img, rect)) = &self.cap_art {
            if *k == key {
                return Some((img.clone(), *rect));
            }
        }
        let scale = display_w / frame.canvas.0 as f32 * sf;
        let placed = crate::captions::render::render(&caption, word, &self.cap_style, frame, scale)?;
        let (w, h) = placed.image.dimensions();
        let rect = (placed.x as f32 / sf, placed.y as f32 / sf, w as f32 / sf, h as f32 / sf);
        let img = crate::canvas_preview::to_render_image(placed.image);
        if let Some((_, old, _)) = self.cap_art.replace((key, img.clone(), rect)) {
            let _ = window.drop_image(old);
        }
        Some((img, rect))
    }
}

/// A sample caption for the preview before there's a transcript.
pub fn sample_caption(max_chars: usize) -> Caption {
    let words = ["Your", "captions", "will", "look", "like", "this"]
        .iter()
        .enumerate()
        .map(|(i, w)| captions::Word { start: i as f64 * 0.4, end: i as f64 * 0.4 + 0.35, text: (*w).into() })
        .collect();
    Caption::from_words(words, max_chars)
}
