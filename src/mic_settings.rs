//! Microphone settings: on/off, device picker and a live input level meter.
//!
//! Self-contained so it can be mounted in any settings layout: `MicSettings` is an entity that
//! renders the Audio section body (`.child(mic_settings.clone())`). The meter runs a small
//! ffmpeg process only while the section is on screen and the mic is enabled, and stops on its
//! own once the section is no longer rendered (e.g. while recording).
//!
//! The mic on/off here decides whether audio is captured at all; the control bar's mute is
//! separate (it records silence inside a captured track).

use crate::theme as tk;
use crate::recorder::{self, DEFAULT_MIC, sessions_root};
use gpui_kit::component::progress::Progress;
use crate::dropdown::{Dropdown, DropdownEvent};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{px, Entity, FontWeight, SharedString, Subscription, Task, Window, div, rgb};
use std::io::{BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

const SYSTEM_DEFAULT: &str = "System default";

pub struct MicSettings {
    enabled: bool,
    /// Picked device name; `None` follows the system default input.
    device: Option<String>,
    /// Available mics (names as avfoundation reports them).
    devices: Vec<String>,
    select: Entity<Dropdown>,
    meter: Option<LevelMeter>,
    /// Displayed level, 0..=1 (peak with a short decay).
    level: f32,
    /// Set by `render`; the meter stops when the section hasn't been drawn for a moment.
    last_render: Instant,
    refreshing: bool,
    _ticker: Task<()>,
    _select_sub: Subscription,
}

impl MicSettings {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (enabled, device) = load();
        let select = cx.new(|cx| {
            let mut d = Dropdown::new("mic-device", "Microphone");
            d.set_items(vec![SharedString::from(SYSTEM_DEFAULT)], Some(0), cx);
            d.set_disabled(!enabled, cx);
            d
        });
        let sub = cx.subscribe(&select, |this, _, ev: &DropdownEvent, cx| {
            let DropdownEvent::Select(i) = ev;
            // Item 0 is the system default, then the devices in order.
            this.device = i.checked_sub(1).and_then(|i| this.devices.get(i).cloned());
            this.meter = None; // restart on the new device next tick
            save(this.enabled, this.device.as_deref());
            cx.notify();
        });
        let ticker = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(50)).await;
                if this.update_in(cx, |this, window, cx| this.tick(window, cx)).is_err() {
                    break;
                }
            }
        });
        Self {
            enabled,
            device,
            devices: Vec::new(),
            select,
            meter: None,
            level: 0.0,
            last_render: Instant::now() - Duration::from_secs(1),
            refreshing: false,
            _ticker: ticker,
            _select_sub: sub,
        }
    }

    /// The mic to record with, for `Recorder::start_with`: `None` when the mic is
    /// off, otherwise the picked device name or `DEFAULT_MIC`.
    pub fn mic_arg(&self) -> Option<String> {
        self.enabled.then(|| self.device.clone().unwrap_or_else(|| DEFAULT_MIC.to_string()))
    }

    fn visible(&self) -> bool {
        self.last_render.elapsed() < Duration::from_millis(300)
    }

    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The level meter opens the mic: only while the Audio section is on screen, never
        // with the window closed.
        let want = self.enabled && self.visible() && !crate::menubar::window_hidden();
        if !want {
            if self.meter.take().is_some() {
                self.level = 0.0;
                cx.notify();
            }
            return;
        }
        if self.meter.is_none() {
            // Just became visible (or the device changed): refresh the list, then meter.
            self.refresh_devices(window, cx);
            self.meter = Some(LevelMeter::start(&self.mic_arg().unwrap_or_default()));
        }
        if let Some(m) = &self.meter {
            let peak = m.peak();
            self.level = peak.max(self.level * 0.8);
        }
        cx.notify();
    }

    fn refresh_devices(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.refreshing {
            return;
        }
        self.refreshing = true;
        let list = cx.background_executor().spawn(async move { recorder::list_mics() });
        cx.spawn_in(window, async move |this, cx| {
            let devices = list.await;
            let _ = this.update(cx, |this, cx| {
                this.refreshing = false;
                if devices == this.devices {
                    return;
                }
                this.devices = devices;
                let mut items: Vec<SharedString> = vec![SYSTEM_DEFAULT.into()];
                items.extend(this.devices.iter().map(|d| SharedString::from(d.clone())));
                let current = this
                    .device
                    .as_ref()
                    .and_then(|d| this.devices.iter().position(|x| x == d))
                    .map_or(0, |i| i + 1);
                this.select.update(cx, |s, cx| s.set_items(items, Some(current), cx));
                cx.notify();
            });
        })
        .detach();
    }

    fn set_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.enabled = enabled;
        self.select.update(cx, |s, cx| s.set_disabled(!enabled, cx));
        save(self.enabled, self.device.as_deref());
        cx.notify();
    }
}

impl Render for MicSettings {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.last_render = Instant::now();
        let muted_fg = cx.theme().muted_foreground;
        let failed = self.meter.as_ref().is_some_and(|m| m.failed());
        let missing = self.device.as_ref().is_some_and(|d| !self.devices.is_empty() && !self.devices.contains(d));
        let note = if !self.enabled {
            Some("Recording without audio")
        } else if failed {
            Some("Can't open the microphone. Check Microphone permission in System Settings, Privacy & Security.")
        } else if missing {
            Some("Picked microphone isn't connected; the system default will be used.")
        } else {
            None
        };

        div()
            .flex()
            .flex_col()
            .gap_3()
            .w_full()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_size(px(tk::T13)).font_weight(FontWeight::MEDIUM).child("Microphone"))
                    .child(
                        Switch::new("mic-enabled").cursor_pointer()
                            .checked(self.enabled)
                            .small()
                            .on_click(cx.listener(|this, checked: &bool, _, cx| this.set_enabled(*checked, cx))),
                    ),
            )
            .child(
                self.select.clone(),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().flex_none().text_size(px(tk::T12)).text_color(muted_fg).child("Level"))
                    .child(
                        div().flex_1().child(
                            Progress::new("mic-level")
                                .value(if self.enabled { self.level * 100.0 } else { 0.0 })
                                .color(rgb(if self.level > 0.95 { tk::error_text() } else { tk::selected() }))
                                .accessibility_label("Microphone input level"),
                        ),
                    ),
            )
            .when_some(note, |d, note| d.child(div().text_size(px(tk::T12)).text_color(muted_fg).child(note)))
    }
}

// ---------- level meter ----------

/// Reads peak input levels from a separate ffmpeg process (avfoundation lets several
/// readers share a mic).
struct LevelMeter {
    child: Child,
    /// Latest peak, 0..=1 (f32 bits).
    peak: Arc<AtomicU32>,
    failed: Arc<AtomicBool>,
}

impl LevelMeter {
    fn start(mic: &str) -> Self {
        let peak = Arc::new(AtomicU32::new(0));
        let failed = Arc::new(AtomicBool::new(false));
        // ~20 readings a second at 48 kHz.
        let filter = "asetnsamples=n=2400:p=0,astats=metadata=1:reset=1,\
                      ametadata=mode=print:key=lavfi.astats.Overall.Peak_level";
        let child = Command::new("ffmpeg")
            .args(["-hide_banner", "-nostats", "-nostdin", "-f", "avfoundation"])
            .args(["-i", &format!(":{mic}"), "-af", filter, "-f", "null", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(_) => {
                failed.store(true, Ordering::Relaxed);
                // A placeholder that exits at once keeps the type simple.
                Command::new("true").spawn().expect("spawn true")
            }
        };
        if let Some(stderr) = child.stderr.take() {
            let (peak, failed) = (peak.clone(), failed.clone());
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stderr);
                let (mut buf, mut byte, mut got_any) = (Vec::new(), [0u8; 1], false);
                while let Ok(1) = reader.read(&mut byte) {
                    if byte[0] != b'\n' && byte[0] != b'\r' {
                        buf.push(byte[0]);
                        continue;
                    }
                    let line = String::from_utf8_lossy(&buf).to_string();
                    buf.clear();
                    if let Some(v) = line.split("Peak_level=").nth(1) {
                        got_any = true;
                        let db: f32 = v.trim().parse().unwrap_or(-120.0); // "-inf" → silent
                        let level = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
                        peak.store(level.to_bits(), Ordering::Relaxed);
                    }
                }
                if !got_any {
                    failed.store(true, Ordering::Relaxed);
                }
            });
        }
        Self { child, peak, failed }
    }

    fn peak(&self) -> f32 {
        f32::from_bits(self.peak.load(Ordering::Relaxed))
    }

    fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }
}

impl Drop for LevelMeter {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---------- persistence ----------

fn prefs_path() -> std::path::PathBuf {
    sessions_root().join(".mic")
}

/// (enabled, device); the mic is on and follows the system default unless changed.
fn load() -> (bool, Option<String>) {
    let text = std::fs::read_to_string(prefs_path()).unwrap_or_default();
    let (mut enabled, mut device) = (true, None);
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("enabled=") {
            enabled = v.trim() != "0";
        } else if let Some(v) = line.strip_prefix("device=") {
            device = Some(v.trim().to_string()).filter(|d| !d.is_empty());
        }
    }
    (enabled, device)
}

fn save(enabled: bool, device: Option<&str>) {
    let _ = std::fs::create_dir_all(sessions_root());
    let text = format!("enabled={}\ndevice={}\n", enabled as u8, device.unwrap_or(""));
    let _ = std::fs::write(prefs_path(), text);
}
