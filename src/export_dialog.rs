//! The export dialog: pick MP4, GIF and/or WebP with their settings, see an estimated size
//! for each (from a quick sample encode), then export them all in one background run with
//! per-format progress and Cancel.

use crate::theme as tk;
use crate::formats::{self, COLOR_CHOICES, Dither, Estimate, ExportJob, FPS_CHOICES, Options, Owned, WIDTH_CHOICES};
use crate::knob::Knob;
use crate::{Clipper, Stage, button, note};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{ClickEvent, Div, FontWeight, div, px, rgb};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// A size estimate running in the background.
pub struct Estimating {
    pub opts: Options,
    cancel: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<anyhow::Result<Estimate>>>,
}

impl Drop for Estimating {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// An export in progress: the job and the formats it's making.
pub struct Running {
    pub job: ExportJob,
    pub opts: Options,
}

/// Dialog state.
#[derive(Default)]
pub struct ExportDialog {
    pub open: bool,
    pub estimate: Option<(Options, Estimate)>,
    pub estimating: Option<Estimating>,
    /// The export in progress (while `Stage::Exporting`).
    pub run: Option<Running>,
    pub saved: Vec<PathBuf>,
    pub error: Option<String>,
}

impl Clipper {
    /// The current recording and settings, owned, for an export or estimate.
    pub(crate) fn export_input(&self) -> Option<Owned> {
        let rec = self.recording.clone()?;
        let path = crate::camera::compute_zoomed(
            &rec.samples,
            rec.width,
            rec.height,
            rec.duration,
            self.layout.frame(&self.style, rec.webcam.is_some()).screen.aspect(),
            self.layout.zoom as f64,
            self.settings.follow(),
            &self.zooms,
            &self.motion,
        );
        let captions = self.captions.clone().filter(|c| self.cap.enabled && !c.lines.is_empty());
        // Edited: only the kept parts, re-timed onto the output timeline.
        let (rec, path, captions, cut) = if self.edit.is_identity(rec.duration) {
            (rec, path, captions, None)
        } else {
            let e = crate::editing::apply(&rec, &path, captions.as_ref(), self.caption_rules(), &self.edit);
            (std::sync::Arc::new(e.rec), e.path, e.captions, Some(e.cut))
        };
        Some(Owned {
            rec,
            path,
            style: self.style.clone(),
            layout: self.layout,
            mirror: self.webcam.mirror,
            fps: self.settings.fps,
            captions: captions.map(|c| (c, self.cap_style.clone())),
            srt: self.cap.srt,
            cut,
        })
    }

    pub(crate) fn open_export(&mut self, cx: &mut Context<Self>) {
        if self.recording.is_none() {
            return;
        }
        // Settings may have changed since the last estimate: start from a fresh sample.
        if let Some(rec) = &self.recording {
            let _ = std::fs::remove_file(rec.dir.join("sample.mp4"));
        }
        self.export.open = true;
        self.export.estimate = None;
        self.export.estimating = None;
        self.export.error = None;
        self.playing = false;
        cx.notify();
    }

    /// Polls the estimate and the export (from the app ticker); re-estimates after changes.
    pub(crate) fn poll_export(&mut self, cx: &mut Context<Self>) {
        let d = &mut self.export;
        if let Some(e) = d.estimating.as_mut() {
            if e.handle.as_ref().is_some_and(|h| h.is_finished()) {
                let opts = e.opts.clone();
                let r = e.handle.take().unwrap().join().unwrap_or_else(|_| Err(anyhow::anyhow!("estimate crashed")));
                d.estimating = None;
                match r {
                    Ok(est) => d.estimate = Some((opts, est)),
                    Err(e) if e.to_string() != crate::export::CANCELLED => d.error = Some(e.to_string()),
                    Err(_) => {}
                }
                cx.notify();
            }
        }
        // Re-estimate when the options changed (not while exporting).
        let stale = d.estimate.as_ref().is_none_or(|(o, _)| *o != self.export_opts);
        if d.open && d.run.is_none() && d.estimating.is_none() && stale && d.error.is_none() {
            if let Some(input) = self.export_input() {
                let opts = self.export_opts.clone();
                let sample = input.rec.dir.join("sample.mp4");
                let cancel = Arc::new(AtomicBool::new(false));
                let c = cancel.clone();
                let o = opts.clone();
                let handle = std::thread::spawn(move || formats::estimate(&input, &o, &sample, &c));
                self.export.estimating = Some(Estimating { opts, cancel, handle: Some(handle) });
            }
        }
        let d = &mut self.export;
        if let Some(run) = d.run.as_mut() {
            match run.job.poll() {
                Some(Ok(saved)) => {
                    d.run = None;
                    self.saved = saved.first().cloned();
                    d.saved = saved;
                    self.stage = Stage::Review;
                }
                Some(Err(e)) => {
                    d.run = None;
                    if e.to_string() != crate::export::CANCELLED {
                        d.error = Some(e.to_string());
                    }
                    self.stage = Stage::Review;
                }
                None => {}
            }
            cx.notify();
        }
    }

    fn start_export(&mut self, cx: &mut Context<Self>) {
        let opts = self.export_opts.clone();
        if !(opts.mp4 || opts.gif || opts.webp) {
            return;
        }
        let Some(input) = self.export_input() else { return };
        self.export.estimating = None;
        self.export.error = None;
        self.export.saved.clear();
        self.export.run = Some(Running { job: ExportJob::start(input, opts.clone(), self.out_dir.clone()), opts });
        self.stage = Stage::Exporting;
        cx.notify();
    }

    // ---------- UI ----------

    pub(crate) fn export_dialog(&self, cx: &mut Context<Self>) -> Option<Div> {
        if !self.export.open {
            return None;
        }
        let o = &self.export_opts;
        let d = &self.export;
        let est = d.estimate.as_ref().map(|(_, e)| *e);
        let estimating = d.estimating.is_some();
        let limit = (o.limit_mb * 1e6) as u64;
        let size = |v: Option<u64>, heavy: bool| -> Div {
            match v {
                _ if estimating && v.is_none() => div().text_xs().text_color(rgb(tk::muted())).child("Estimating…"),
                Some(b) => {
                    let over = heavy && b > limit;
                    div()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(if over { 0xffb020 } else { tk::muted() }))
                        .child(format!("{}~{}", if over { "Over · " } else { "" }, formats::size_label(b)))
                }
                None => div(),
            }
        };
        let busy = d.run.is_some();

        let format_row = |id: &'static str, title: &'static str, sub: &'static str, on: bool, est: Option<u64>, heavy: bool, f: fn(&mut Options, bool), cx: &mut Context<Self>| {
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    Checkbox::new(id).cursor_pointer()
                        .checked(on)
                        .disabled(busy)
                        .label(title)
                        .on_click(cx.listener(move |this, v: &bool, _, cx| {
                            f(&mut this.export_opts, *v);
                            cx.notify();
                        })),
                )
                .child(div().flex().items_center().gap_2().child(div().text_xs().text_color(rgb(tk::muted())).child(sub)).child(size(est, heavy)))
        };

        let seg = |id: &'static str, labels: Vec<String>, selected: usize, f: fn(&mut Options, usize), cx: &mut Context<Self>| {
            crate::ui::segmented(
                id,
                labels
                    .into_iter()
                    .enumerate()
                    .map(|(ix, l)| {
                        let on: crate::ui::OnClick = Box::new(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            f(&mut this.export_opts, ix);
                            cx.notify();
                        }));
                        (l.into(), ix == selected, on)
                    })
                    .collect(),
            )
        };
        let pos = |list: &[u32], v: u32| list.iter().position(|x| *x == v).unwrap_or(0);
        let fps_labels = || FPS_CHOICES.iter().map(|f| format!("{f}")).collect::<Vec<_>>();
        let width_labels = || WIDTH_CHOICES.iter().map(|w| format!("{w}")).collect::<Vec<_>>();
        let field = |label: &'static str, c: gpui_kit::AnyElement| {
            div().flex().items_center().gap_3().child(div().w(px(64.)).text_xs().text_color(rgb(tk::muted())).child(label)).child(div().flex_1().child(c))
        };

        let gif_opts = o.gif.then(|| {
            div()
                .flex()
                .flex_col()
                .gap_2()
                .pl_6()
                .child(field("FPS", seg("gif-fps", fps_labels(), pos(&FPS_CHOICES, o.gif_opts.fps), |o, i| o.gif_opts.fps = FPS_CHOICES[i], cx).into_any_element()))
                .child(field("Width", seg("gif-width", width_labels(), pos(&WIDTH_CHOICES, o.gif_opts.width), |o, i| o.gif_opts.width = WIDTH_CHOICES[i], cx).into_any_element()))
                .child(field(
                    "Colors",
                    seg("gif-colors", COLOR_CHOICES.iter().map(|c| c.to_string()).collect(), pos(&COLOR_CHOICES, o.gif_opts.colors), |o, i| o.gif_opts.colors = COLOR_CHOICES[i], cx).into_any_element(),
                ))
                .child(field(
                    "Dither",
                    seg(
                        "gif-dither",
                        Dither::ALL.iter().map(|d| d.label().to_string()).collect(),
                        Dither::ALL.iter().position(|d| *d == o.gif_opts.dither).unwrap_or(1),
                        |o, i| o.gif_opts.dither = Dither::ALL[i],
                        cx,
                    )
                    .into_any_element(),
                ))
                .child(
                    Switch::new("gif-loop").cursor_pointer().checked(o.gif_opts.looped).small().label("Loop forever").on_click(cx.listener(|this, v: &bool, _, cx| {
                        this.export_opts.gif_opts.looped = *v;
                        cx.notify();
                    })),
                )
        });
        let webp_opts = o.webp.then(|| {
            div()
                .flex()
                .flex_col()
                .gap_2()
                .pl_6()
                .child(field("FPS", seg("webp-fps", fps_labels(), pos(&FPS_CHOICES, o.webp_opts.fps), |o, i| o.webp_opts.fps = FPS_CHOICES[i], cx).into_any_element()))
                .child(field("Width", seg("webp-width", width_labels(), pos(&WIDTH_CHOICES, o.webp_opts.width), |o, i| o.webp_opts.width = WIDTH_CHOICES[i], cx).into_any_element()))
                .child(self.knob(Knob::WebpQuality, "Quality", cx))
                .child(
                    Switch::new("webp-loop").cursor_pointer().checked(o.webp_opts.looped).small().label("Loop forever").on_click(cx.listener(|this, v: &bool, _, cx| {
                        this.export_opts.webp_opts.looped = *v;
                        cx.notify();
                    })),
                )
        });

        let over = est.is_some_and(|e| (o.gif && e.gif.is_some_and(|b| b > limit)) || (o.webp && e.webp.is_some_and(|b| b > limit)));
        let progress = d.run.as_ref().map(|Running { job, opts: r }| {
            let p = |a: &std::sync::atomic::AtomicU32| a.load(Ordering::Relaxed) as f32 / 10.0;
            let bar = |label: &'static str, v: f32| {
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().w(px(84.)).text_xs().child(label))
                    .child(div().flex_1().child(Progress::new(label).value(v).color(rgb(tk::green()))))
                    .child(div().w(px(36.)).text_xs().text_color(rgb(tk::muted())).child(format!("{v:.0}%")))
            };
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(bar(if r.mp4 { "MP4" } else { "Rendering" }, p(&job.progress.render)))
                .when(r.gif, |d| d.child(bar("GIF", p(&job.progress.gif))))
                .when(r.webp, |d| d.child(bar("WebP", p(&job.progress.webp))))
        });
        let n = [o.mp4, o.gif, o.webp].iter().filter(|b| **b).count();
        let saved = (!d.saved.is_empty()).then(|| {
            let first = d.saved[0].clone();
            div()
                .flex()
                .flex_col()
                .gap_1()
                .p_3()
                .rounded(px(12.))
                .bg(rgb(tk::raised()))
                .child(div().text_xs().text_color(rgb(tk::green_ink())).child("Saved"))
                .children(d.saved.iter().map(|p| {
                    div().text_xs().truncate().child(p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default())
                }))
                .child(
                    button("export-reveal", "Show in Finder", cx)
                        .small()
                        .on_click(move |_, _, _| {
                            let _ = std::process::Command::new("open").arg("-R").arg(&first).spawn();
                        }),
                )
        });

        let body = div()
            .id("export-body")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_5()
            .pt_5()
            .pb_3()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_base().font_weight(FontWeight::BOLD).child("Export"))
                    .child(div().text_xs().text_color(rgb(tk::muted())).child(crate::display_path(&self.out_dir))),
            )
            .child(format_row("fmt-mp4", "MP4", "H.264 + audio", o.mp4, est.and_then(|e| e.mp4), false, |o, v| o.mp4 = v, cx))
            .child(format_row("fmt-gif", "GIF", "no audio", o.gif, est.and_then(|e| e.gif), true, |o, v| o.gif = v, cx))
            .children(gif_opts)
            .child(format_row("fmt-webp", "WebP", "animated, no audio", o.webp, est.and_then(|e| e.webp), true, |o, v| o.webp = v, cx))
            .children(webp_opts)
            .child(div().h(px(1.)).bg(rgb(tk::line())))
            .child(self.knob(Knob::SizeLimit, "Warn when a GIF or WebP is over", cx))
            .when(over, |d| {
                d.child(div().text_xs().text_color(rgb(tk::warn())).child("Over your size limit. Try a lower frame rate, a smaller width, or fewer colors."))
            })
            .child(note("GIF and WebP don't carry sound. Captions, webcam and click highlights are in every format. Sizes are estimated from a short sample."))
            .when_some(d.error.clone(), |c, e| c.child(div().text_xs().text_color(rgb(tk::error())).child(e)))
            .children(progress)
            .children(saved);
        // The buttons stay visible under the scrolling settings.
        let card = div()
            .id("export-dialog")
            .occlude()
            .w(px(460.))
            .max_h(gpui_kit::relative(0.9))
            .flex()
            .flex_col()
            .rounded(px(24.))
            .bg(rgb(tk::card()))
            .border_1()
            .border_color(crate::ui::hairline_strong())
            .shadow(crate::ui::float_shadow())
            .child(body)
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .px_5()
                    .py_4()
                    .border_t_1()
                    .border_color(crate::ui::hairline())
                    .child(if busy {
                        button("export-cancel", "Cancel export", cx).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            if let Some(r) = &this.export.run {
                                r.job.cancel();
                            }
                            cx.notify();
                        }))
                    } else {
                        button("export-close", "Close", cx).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.export.open = false;
                            this.export.estimating = None;
                            cx.notify();
                        }))
                    })
                    .when(!busy, |d| {
                        d.child(
                            gpui_kit::component::button::Button::new("export-go").cursor_pointer()
                                .primary()
                                .rounded_full()
                                .px_5()
                                .font_weight(FontWeight::SEMIBOLD)
                                .disabled(n == 0)
                                .label(match n {
                                    0 => "Pick a format".to_string(),
                                    1 => "Export".to_string(),
                                    n => format!("Export {n} formats"),
                                })
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.start_export(cx))),
                        )
                    }),
            );
        Some(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(tk::shadow(0.55))
                .child(card)
                .text_color(rgb(tk::text())),
        )
    }
}

use gpui_kit::component::button::ButtonVariants;
