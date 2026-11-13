//! Screenshots in the main window: the capture flow (hotkey or Screenshot button), the
//! Screenshot tab (mode, delay, cursor, folder) and the recent screenshots strip.

use crate::theme as tk;
use crate::screenshot::{self, Mode, cards, capture, library, picker};
use crate::{Clipper, note};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::Sizable;
use gpui_kit::prelude::*;
use gpui_kit::{ClickEvent, Div, PathPromptOptions, div, px, rgb};
use std::path::PathBuf;
use std::time::Duration;

/// What to capture once the selection is made.
enum Target {
    Display(crate::region::Display, Option<crate::region::Region>),
    Window(capture::WindowInfo, crate::region::Display),
}


impl Clipper {
    /// At launch: the always-on screenshot hotkey, leftover deletes, recent shots, fonts.
    pub(crate) fn init_screenshots(&mut self, cx: &mut Context<Self>) {
        let (registration, failed) = crate::hotkeys::register_screenshot(&self.hotkey_config);
        if !failed.is_empty() {
            eprintln!("Clipper: could not register the screenshot hotkey {failed:?} (in use by another app?)");
        }
        self.shot_hotkey = Some(registration);
        library::flush_pending(&self.shot.folder);
        crate::home::flush_pending(&[&crate::recorder::sessions_root().join(".sessions"), &self.out_dir, &self.shot.folder]);
        crate::menubar::install(&self.hotkey_config.screenshot.label());
        self.system_shots = Some(crate::screenshot::watch::Watcher::new());
        crate::editor::text::warm_up();
        self.refresh_recent(cx);
        // Dev: take a screenshot at launch (CLIPPER_SHOT=full|region|window).
        #[cfg(debug_assertions)]
        if let Ok(m) = std::env::var("CLIPPER_SHOT") {
            self.shot.mode = match m.as_str() {
                "region" => Mode::Region,
                "window" => Mode::Window,
                _ => Mode::Full,
            };
            cx.defer(|_| {});
            let this = cx.entity();
            cx.spawn(async move |_, cx| {
                cx.background_executor().timer(Duration::from_millis(800)).await;
                let _ = cx.update(|cx| this.update(cx, |c, cx| c.take_screenshot(false, cx)));
            })
            .detach();
        }
        // Dev: merge pictures at launch (CLIPPER_MERGE=a.png,b.png; CLIPPER_MERGE_DIR for
        // where the merge goes, else the screenshots folder).
        #[cfg(debug_assertions)]
        if let Ok(list) = std::env::var("CLIPPER_MERGE") {
            let paths: Vec<PathBuf> = list.split(',').map(PathBuf::from).collect();
            let folder = std::env::var_os("CLIPPER_MERGE_DIR").map(PathBuf::from).unwrap_or_else(|| self.shot.folder.clone());
            cx.defer(move |cx| {
                if let Err(e) = crate::editor::view::open_merge(&paths, &folder, cx) {
                    eprintln!("Clipper: merge failed: {e}");
                }
            });
        }
        // Dev: open the editor on a file at launch.
        #[cfg(debug_assertions)]
        if let Some(p) = std::env::var_os("CLIPPER_EDIT") {
            let p = PathBuf::from(p);
            cx.defer(move |cx| crate::editor::view::open(p, cx));
        }
    }

    /// Starts a screenshot in the configured mode. From the button, the main window steps
    /// aside first so the selection can see what's under it (it's never in the shot anyway).
    /// Brings the main window (and the Dock icon) back from the menu bar.
    pub(crate) fn show_main(&mut self, cx: &mut Context<Self>) {
        crate::menubar::set_dock_icon_visible(true);
        let main = self.main_window;
        cx.defer(move |cx| {
            let _ = main.update(cx, |_, window, _| {
                crate::menubar::show_window(window);
                window.activate_window();
            });
            cx.activate(true);
        });
    }

    /// Opens images of the user's in the editor: each is copied into the screenshots folder
    /// first (off the UI thread), so the original is never changed.
    pub(crate) fn edit_images(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if paths.is_empty() {
            return;
        }
        let folder = self.shot.folder.clone();
        let job = cx.background_executor().spawn(async move {
            paths
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let stamp = format!("{}{}", crate::recorder::stamp(), if i > 0 { format!("-{i}") } else { String::new() });
                    library::import(&folder, p, &stamp)
                })
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let results = job.await;
            let _ = this.update(cx, |c, cx| {
                c.shot_error = results.iter().find_map(|r| r.as_ref().err().map(|e| e.to_string()));
                c.refresh_recent(cx);
                for p in results.into_iter().flatten() {
                    cx.defer(move |cx| crate::editor::view::open(p, cx));
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Asks for images to edit.
    pub(crate) fn choose_images(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some("Edit".into()) });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let _ = this.update(cx, |c, cx| c.edit_images(paths, cx));
        })
        .detach();
    }

    /// A screenshot in the Shots tab's mode and delay (the button and the hotkey).
    pub(crate) fn take_screenshot(&mut self, from_button: bool, cx: &mut Context<Self>) {
        self.capture(self.shot.mode, self.shot.delay, from_button, cx);
    }

    /// A screenshot in `mode` after `delay` seconds (the menu bar's items pass their own).
    pub(crate) fn capture(&mut self, mode: Mode, delay: u8, from_button: bool, cx: &mut Context<Self>) {
        if self.shot_busy || self.selector.is_some() {
            return;
        }
        let Some(display) = capture::display_under_cursor() else { return };
        self.shot_busy = true;
        let this = cx.entity();
        let settings = screenshot::Settings { mode, delay, ..self.shot.clone() };
        if from_button && mode != Mode::Full {
            let _ = self.main_window.update(cx, |_, window, _| window.minimize_window());
        }
        match mode {
            Mode::Full => shoot(this, settings, Target::Display(display, None), cx),
            Mode::Region => cx.defer(move |cx| {
                let done = this.clone();
                let settings = settings.clone();
                let opened = crate::selector::open_screenshot(
                    display,
                    Box::new(move |region, cx| match region {
                        Some(r) => shoot(done.clone(), settings.clone(), Target::Display(display, Some(r)), cx),
                        None => done.update(cx, |c, _| c.shot_busy = false),
                    }),
                    cx,
                );
                if opened.is_none() {
                    this.update(cx, |c, _| c.shot_busy = false);
                }
            }),
            Mode::Window => {
                let task = cx.background_executor().spawn(async { capture::windows() });
                cx.spawn(async move |_, cx| {
                    let windows = task.await;
                    let _ = cx.update(|cx| {
                        let done = this.clone();
                        let settings = settings.clone();
                        let opened = picker::open(
                            display,
                            windows,
                            Box::new(move |pick, cx| match pick {
                                Some(w) => shoot(done.clone(), settings.clone(), Target::Window(w, display), cx),
                                None => done.update(cx, |c, _| c.shot_busy = false),
                            }),
                            cx,
                        );
                        if opened.is_none() {
                            this.update(cx, |c, _| c.shot_busy = false);
                        }
                    });
                })
                .detach();
            }
        }
    }

    /// A screenshot was saved or deleted: the Library re-reads its folders.
    pub(crate) fn refresh_recent(&mut self, cx: &mut Context<Self>) {
        self.refresh_library(cx);
    }

    pub(crate) fn screenshot_panel(&self, cx: &mut Context<Self>) -> Div {
        let s = &self.shot;
        let modes = crate::ui::segmented(
            "shot-mode",
            Mode::ALL
                .into_iter()
                .map(|m| {
                    let on: crate::ui::OnClick = Box::new(cx.listener(move |c, _: &ClickEvent, _, cx| {
                        c.shot.mode = m;
                        c.shot.save();
                        cx.notify();
                    }));
                    (m.label().into(), s.mode == m, on)
                })
                .collect(),
        );
        let delays = crate::ui::segmented(
            "shot-delay",
            screenshot::DELAYS
                .into_iter()
                .map(|d| {
                    let label = if d == 0 { "Off".to_string() } else { format!("{d} s") };
                    let on: crate::ui::OnClick = Box::new(cx.listener(move |c, _: &ClickEvent, _, cx| {
                        c.shot.delay = d;
                        c.shot.save();
                        cx.notify();
                    }));
                    (label.into(), s.delay == d, on)
                })
                .collect(),
        );
        let this = cx.entity().downgrade();
        let this2 = cx.entity().downgrade();
        let system = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div().flex().items_center().justify_between().gap_3().child(div().flex_1().min_w_0().text_size(px(tk::T13)).child("Show the card for ⌘⇧3, ⌘⇧4 and ⌘⇧5")).child(
                    Switch::new("shot-system").cursor_pointer().checked(s.detect_system).on_click(move |on: &bool, _, cx| {
                        let on = *on;
                        let _ = this2.update(cx, |c, cx| {
                            c.shot.detect_system = on;
                            c.shot.save();
                            cx.notify();
                        });
                    }),
                ),
            )
            .child(note(&format!("Watching {}", crate::display_path(&screenshot::watch::system_dir()))));
        let cursor = div().flex().items_center().justify_between().child(div().text_size(px(tk::T13)).child("Show the mouse pointer")).child(
            Switch::new("shot-cursor").cursor_pointer().checked(s.cursor).on_click(move |on: &bool, _, cx| {
                let on = *on;
                let _ = this.update(cx, |c, cx| {
                    c.shot.cursor = on;
                    c.shot.save();
                    cx.notify();
                });
            }),
        );
        let folder = s.folder.clone();
        let folder_row = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().text_size(px(tk::T12)).text_color(rgb(tk::ink_soft())).truncate().child(folder.display().to_string()))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(Button::new("shot-folder").cursor_pointer().secondary().small().rounded_full().label("Change folder").on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                        let rx = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Use folder".into()) });
                        cx.spawn(async move |this, cx| {
                            let Ok(Ok(Some(paths))) = rx.await else { return };
                            let Some(dir) = paths.into_iter().next() else { return };
                            let _ = this.update(cx, |c, cx| {
                                c.shot.folder = dir;
                                c.shot.save();
                                c.refresh_recent(cx);
                            });
                        })
                        .detach();
                    })))
                    .child(Button::new("shot-reveal").cursor_pointer().ghost().small().rounded_full().label("Show in folder").on_click(move |_: &ClickEvent, _, cx| {
                        let _ = std::fs::create_dir_all(&folder);
                        cx.reveal_path(&folder)
                    })),
            );
        div()
            .flex()
            .flex_col()
            .gap(px(tk::S4))
            .child(crate::section("Screenshot mode", modes))
            .child(crate::section("Screenshot delay", delays))
            .child(crate::section("Screenshot pointer", cursor))
            .child(crate::section("macOS screenshots", system))
            .child(crate::section("Save screenshots to", folder_row))
            .child(crate::section(
                "Screenshot shortcut",
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(crate::ui::shortcut("Screenshot from anywhere", self.hotkey_config.screenshot.label()))
                    .child(note("Change it in ~/Library/Application Support/Clipper/hotkeys.conf.")),
            ))
    }
}

/// Waits for the delay (and for overlays to close), captures on a background thread, saves
/// the original and shows its card.
fn shoot(this: gpui_kit::Entity<Clipper>, s: screenshot::Settings, target: Target, cx: &mut gpui_kit::App) {
    let stamp = crate::recorder::stamp();
    cx.spawn(async move |cx| {
        // Overlays need a moment to leave the screen (they're excluded anyway).
        let wait = Duration::from_millis(150) + Duration::from_secs(s.delay as u64);
        cx.background_executor().timer(wait).await;
        let task = cx.background_executor().spawn(async move {
            let path = library::new_path(&s.folder, &stamp)?;
            let display = match &target {
                Target::Display(d, region) => {
                    capture::display(d, region.as_ref(), s.cursor, &path)?;
                    *d
                }
                Target::Window(w, d) => {
                    capture::window(w.id, s.cursor, &path)?;
                    *d
                }
            };
            anyhow::Ok((path, display))
        });
        let result = task.await;
        let _ = cx.update(|cx| {
            match result {
                Ok((path, display)) => cards::show(path, display, cx),
                Err(e) => {
                    let msg = format!("{e}. Check Screen Recording permission in System Settings, Privacy & Security.");
                    if let Some(d) = capture::display_under_cursor() {
                        cards::show_error(msg, d, cx);
                    }
                }
            }
            this.update(cx, |c, cx| {
                c.shot_busy = false;
                c.refresh_recent(cx);
            });
        });
    })
    .detach();
}
