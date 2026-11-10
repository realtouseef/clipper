mod audio;
mod backgrounds;
mod bg_picker;
mod credits;
mod cam_bubble;
mod camera;
mod captions;
mod captions_panel;
mod canvas_preview;
mod composite;
mod clicks;
mod clock;
mod control_bar;
mod dropdown;
mod cutting;
mod editing;
mod editor;
mod cursor;
mod export;
mod export_dialog;
mod formats;
mod frames;
mod hotkeys;
mod knob;
mod layout;
mod layout_panel;
mod mascot;
mod menubar;
mod models;
mod mic_settings;
mod recorder;
mod region;
mod screenshot;
mod selector;
mod shots_panel;
mod style;
mod timeline;
mod ui;
mod webcam;
mod zoom;

use camera::{CameraPath, FollowConfig};
use zoom::{ZoomMotion, ZoomRegion};
use canvas_preview::{ArtPixels, CamShow, CanvasArt};
use layout::Layout;
#[cfg(test)]
use layout::Res;
use knob::{Knob, KnobBounds};
use style::{Color, Style};
use control_bar::BarState;
use export::Preview;
use gpui::PathPromptOptions;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme, Selectable, Sizable, Theme, ThemeMode};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyWindowHandle, App, Bounds, ClickEvent, Div, FocusHandle, FontWeight, Hsla, MouseButton, Pixels,
    Subscription, Task, TitlebarOptions, Window, WindowBounds, WindowOptions, div, img, point, px, rgb, size,
};
use std::cell::Cell;
use std::rc::Rc;
use mic_settings::MicSettings;
use recorder::{Recorder, Recording};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
#[cfg(test)]
use std::sync::atomic::AtomicU32;
use std::time::{Duration, Instant};

// Palette: near-black window, dark cards, lighter round controls, one green accent.
const BG: u32 = 0x0a0a0b;
/// Cards (preview, settings panel, timeline).
const CARD: u32 = 0x18181a;
/// Controls on a card: buttons, rows, chips.
const RAISED: u32 = 0x262629;
const HOVER: u32 = 0x313135;
const PRESSED: u32 = 0x3d3d42;
const TEXT: u32 = 0xf4f4f5;
const MUTED: u32 = 0x9a9aa0;
const LINE: u32 = 0x2a2a2e;
/// Primary: actions, active tab, sliders, switches, zooms.
const GREEN: u32 = 0xa4f47c;
const GREEN_HOVER: u32 = 0xb9f89b;
const GREEN_PRESSED: u32 = 0x8ee163;
/// Text on green.
const ON_GREEN: u32 = 0x0a0a0b;
const RED: u32 = 0xff453a;
/// Dark red disc behind the stop square.
const RED_DEEP: u32 = 0x3a1412;
const RED_DEEP_HOVER: u32 = 0x4d1a17;
/// Behind the 9:16 preview inside its card.
const STAGE: u32 = CARD;
const R_CARD: f32 = 24.0;
const GAP: f32 = 10.0;
const HEADER_H: f32 = 52.0;
const PANEL_W: f32 = 320.0;
/// The vertical tab rail right of the settings panel.
const RAIL_W: f32 = 76.0;

// Squircle-like radii (GPUI draws circular-arc corners; large radii read as soft squircles).
const R_BIG: f32 = 28.0;
const R_MID: f32 = 14.0;

fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}

/// The app's top-level lifecycle (see docs/state-machines.md for every machine).
///
/// Transitions:
/// - Idle → Starting (Record) → Recording (capture up) → Processing (Stop) → Review
/// - Starting → Idle (cancelled before capture started)
/// - Starting / Recording / Processing → Error (capture or processing failed)
/// - Review → Exporting (Export) → Review (done, failed or cancelled)
/// - Review / Error → Idle (new recording, discard)
///
/// Invariants (checked in debug builds by `Clipper::stage_violations`):
/// - `Recording` has a `recorder`.
/// - `Review` and `Exporting` have a `recording` and its `preview`.
/// - `Exporting` exactly while `export.run` is some.
/// - Timeline gestures (scrub, zoom, trim) only happen in `Review`.
enum Stage {
    Idle,
    /// Main window is minimizing and ffmpeg is spinning up.
    Starting,
    Recording,
    /// Recording stopped; joining segments, building the preview.
    Processing,
    /// Editing the finished recording.
    Review,
    /// An export runs (the preview still plays).
    Exporting,
    Error(String),
}

/// Tabs of the settings panel.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Panel {
    /// Output format and webcam layout.
    Layout,
    Scene,
    /// Zoom regions and camera follow.
    Zoom,
    Captions,
    /// Source, audio and export settings.
    Output,
    /// Screenshot settings and recent screenshots.
    Shots,
}

impl Panel {
    const ALL: [Panel; 6] = [Panel::Layout, Panel::Scene, Panel::Zoom, Panel::Captions, Panel::Output, Panel::Shots];

    fn label(self) -> &'static str {
        match self {
            Panel::Layout => "Layout",
            Panel::Scene => "Scene",
            Panel::Captions => "Captions",
            Panel::Zoom => "Zoom",
            Panel::Output => "Output",
            Panel::Shots => "Shots",
        }
    }

    fn icon(self) -> gpui_kit::component::Icon {
        use editor::icons;
        match self {
            Panel::Layout => icons::layout(),
            Panel::Scene => icons::background(),
            Panel::Zoom => icons::zoom(),
            Panel::Captions => icons::captions(),
            Panel::Output => icons::settings(),
            Panel::Shots => icons::camera(),
        }
    }
}

struct Clipper {
    stage: Stage,
    recorder: Option<Recorder>,
    recording: Option<Arc<Recording>>,
    preview: Option<Preview>,
    path: Option<CameraPath>,
    settings: Settings,
    /// Background, frame and cursor look, shared by the preview and the export.
    style: Style,
    /// Output ratio, resolution and webcam placement.
    layout: Layout,
    /// Webcam on/off, device and mirror.
    webcam: webcam::Settings,
    cam_devices: Vec<String>,
    cam_select: gpui_kit::Entity<dropdown::Dropdown>,
    /// Live camera feed while idle with the webcam on (or that it failed to open), and
    /// its newest frame.
    live: webcam::LivePreview,
    /// The live webcam bubble while recording.
    bubble: Option<AnyWindowHandle>,
    preview_bounds: Rc<Cell<Bounds<Pixels>>>,
    live_frame: Option<webcam::CamImage>,
    live_mirror: Arc<std::sync::atomic::AtomicBool>,
    /// Webcam frames matching the preview's screen frames.
    cam_frames: Option<Vec<webcam::CamImage>>,
    /// GPU images to free on the next render.
    stale_images: Vec<Arc<gpui_kit::RenderImage>>,
    /// Auto captions: settings, style, the transcript, and the model download / transcription
    /// running in the background.
    cap: captions_panel::CaptionSettings,
    cap_style: captions::render::CaptionStyle,
    captions: Option<captions::Captions>,
    download: Option<models::Download>,
    stt: Option<captions::transcribe::Job>,
    cap_error: Option<String>,
    /// Caption list: the line being edited inline, and the selected line.
    cap_edit: Option<captions_panel::CaptionEdit>,
    cap_selected: Option<usize>,
    /// Rendered caption for the preview and the inputs it was made from.
    cap_art: Option<(String, Arc<gpui_kit::RenderImage>, (f32, f32, f32, f32))>,
    cap_dropdowns: captions_panel::CaptionDropdowns,
    /// Export dialog (formats, estimates, the running export) and its options.
    export: export_dialog::ExportDialog,
    export_opts: formats::Options,
    /// Clicks of the current recording (for the preview's ripples).
    clicks: Vec<clicks::Click>,
    /// Record a region of a display instead of the whole display, and that region.
    use_region: bool,
    region: Option<region::Region>,
    /// The region selector overlay window, while open.
    selector: Option<AnyWindowHandle>,
    /// Thin border around the region while recording (never captured).
    region_border: Option<AnyWindowHandle>,
    /// The last export of this recording; the editor stays open for changes and re-exports.
    saved: Option<PathBuf>,
    /// Non-blocking webcam message (lost mid-recording, didn't start).
    cam_notice: Option<String>,
    /// Track bounds of the settings sliders and the one being dragged.
    knob_bounds: KnobBounds,
    /// Background picker thumbnails.
    /// This moment's animated background frame and corner patches: (background key, frame
    /// time in 1/60 s, frame, patches).
    anim_frame: Option<(u64, i64, Arc<gpui_kit::RenderImage>, Vec<(Arc<gpui_kit::RenderImage>, (f32, f32, f32, f32))>)>,
    /// Clock for animated backgrounds before there is a recording (its timeline drives them after).
    idle_clock: Instant,
    watch_scan: Instant,
    /// The shared background picker (Scene tab).
    bg_picker: gpui_kit::Entity<bg_picker::BgPicker>,
    /// Thumbnail of the user's own background photo.
    /// Cached preview layers and the inputs they were built from.
    art: Option<(String, CanvasArt)>,
    /// Background render of new preview layers; at most one runs at a time.
    art_job: Option<Task<()>>,
    /// The recording's mic track for preview playback, and the player.
    audio: Option<audio::Track>,
    player: audio::Player,
    /// Restart preview audio at the playhead on the next tick (after a seek or loop).
    audio_restart: bool,
    /// 1×1 fill shown in the clip area before there is footage.
    empty_frame: Arc<gpui_kit::RenderImage>,
    out_dir: PathBuf,
    /// Zoom regions on the clip timeline, sorted and non-overlapping.
    zooms: Vec<ZoomRegion>,
    motion: ZoomMotion,
    /// Index into `zooms` of the region being edited.
    selected: Option<usize>,
    /// Suggest zooms from the clicks when a recording finishes.
    auto_zoom: bool,
    /// Result of the last "Suggest zooms".
    suggest_note: Option<String>,
    /// The edit (kept segments of the raw recording), its undo history, and the selected
    /// segment on the timeline.
    edit: editing::EditList,
    edit_history: editing::History,
    edit_sel: Option<usize>,
    /// Derived from the edit for the preview: captions and click ripples on the output
    /// timeline, the mic track cut with crossfades, and the waveform (peaks per 1/100 s of
    /// raw time).
    edited_caps: Option<captions::Captions>,
    edited_clicks: Vec<(clicks::Click, f64)>,
    edited_audio: Option<audio::Track>,
    waveform: Option<Arc<Vec<f32>>>,
    /// Timeline zoom (1 = the whole clip fits) and horizontal scroll (px).
    tl_zoom: f32,
    tl_scroll: f32,
    /// "Reset all edits" asked; waiting for the confirmation click.
    confirm_reset: bool,
    /// Full-rate frames around the playhead, and whether the preview shows one (rather
    /// than the nearest thumbnail while it decodes).
    frame_cache: Option<frames::FrameCache>,
    showing_exact: bool,
    /// Preview playback position in raw recording time (always inside a kept segment), and
    /// whether it advances.
    playhead: f64,
    playing: bool,
    last_tick: Instant,
    panel: Panel,
    /// The pointer gesture in progress (timeline scrub, zoom/trim drag, webcam drag, slider).
    gesture: timeline::Gesture,
    /// Window bounds of the focus picker, from the last paint.
    picker_bounds: Rc<Cell<Bounds<Pixels>>>,
    focus: FocusHandle,
    /// Mic mute for the current/next recording (toggled from the control bar).
    muted: bool,
    /// Mic on/off + device picker + level meter (the Audio settings section).
    mic: gpui_kit::Entity<MicSettings>,
    main_window: AnyWindowHandle,
    bar: Option<AnyWindowHandle>,
    hotkey_config: hotkeys::Config,
    hotkeys: Option<hotkeys::Registration>,
    /// Screenshot settings, a capture in progress, the always-on hotkey and recent shots.
    shot: screenshot::Settings,
    shot_busy: bool,
    /// Why an image couldn't be opened for editing.
    shot_error: Option<String>,
    /// Recent shots picked for merging.
    shot_pick: Vec<PathBuf>,
    /// Watches for macOS's own screenshots (⌘⇧3/4/5).
    system_shots: Option<screenshot::watch::Watcher>,
    system_poll: Instant,
    shot_hotkey: Option<hotkeys::Registration>,
    recent_shots: Vec<(PathBuf, Arc<gpui_kit::RenderImage>)>,
    _ticker: Task<()>,
    _job: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

/// Values behind the settings panel (sliders write here; preview, camera and export read).
#[derive(Clone, Copy)]
struct Settings {
    /// Index into `displays()`.
    display: usize,
    /// Camera deadzone margin, fraction of crop width.
    deadzone: f64,
    /// Camera settling time in seconds.
    smoothing: f64,
    fps: u32,
}

impl Default for Settings {
    fn default() -> Self {
        let follow = FollowConfig::default();
        Self {
            display: 0,
            deadzone: follow.margin,
            smoothing: follow.settle,
            fps: 30,
        }
    }
}

impl Settings {
    fn follow(&self) -> FollowConfig {
        FollowConfig { margin: self.deadzone, settle: self.smoothing }
    }
}

const FPS_CHOICES: [u32; 2] = [30, 60];


/// A short key for a background (part of the cached canvas layers' key).
fn bg_key(bg: &backgrounds::Background) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(bg).unwrap_or_default().hash(&mut h);
    h.finish()
}

fn color_hex(c: Color) -> u32 {
    (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32
}

fn hex_color(v: u32) -> Color {
    [(v >> 16) as u8, (v >> 8) as u8, v as u8]
}

/// Active displays as (label, width × height in points).
fn displays() -> Vec<(String, String)> {
    (0..8)
        .map_while(cursor::display_bounds)
        .enumerate()
        .map(|(i, (_, _, w, h))| {
            let name = if i == 0 { "Main display".to_string() } else { format!("Display {}", i + 1) };
            (name, format!("{w:.0} × {h:.0}"))
        })
        .collect()
}

impl Clipper {
    fn new(main_window: AnyWindowHandle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // One light ticker drives the timer, preview playback and export progress.
        let ticker = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(33)).await;
                let alive = this.update(cx, |this, cx| this.tick(cx));
                if alive.is_err() {
                    break;
                }
            }
        });
        let settings = Settings::default();
        let style = Style::default();
        let motion = ZoomMotion::default();
        let mut subs = Vec::new();
        let cap_dropdowns = captions_panel::CaptionDropdowns::new(cx, &mut subs);
        let bg_picker = bg_picker::new_picker(style.bg.clone(), true, None, window, cx);
        subs.push(cx.subscribe(&bg_picker, |this, _, ev: &bg_picker::PickerEvent, cx| {
            if let bg_picker::PickerEvent::Changed(bg) = ev {
                this.style.bg = bg.clone();
                cx.notify();
            }
        }));
        let cam_select = cx.new(|_| dropdown::Dropdown::new("webcam-device", "Camera"));
        subs.push(cx.subscribe(&cam_select, |this, _, ev: &dropdown::DropdownEvent, cx| {
            let dropdown::DropdownEvent::Select(i) = ev;
            if let Some(name) = this.cam_devices.get(*i) {
                this.webcam.device = Some(name.clone());
                this.restart_live();
                cx.notify();
            }
        }));
        cx.spawn_in(window, async move |this, cx| {
            let devices = cx.background_executor().spawn(async { webcam::backend().devices() }).await;
            let _ = this.update(cx, |this, cx| this.set_cam_devices(devices, cx));
        })
        .detach();
        Self {
            stage: Stage::Idle,
            recorder: None,
            recording: None,
            preview: None,
            path: None,
            settings,
            style,
            layout: Layout::default(),
            webcam: webcam::Settings::default(),
            cam_devices: Vec::new(),
            cam_select,
            live: webcam::LivePreview::Off,
            bubble: None,
            preview_bounds: Rc::new(Cell::new(Bounds::default())),
            live_frame: None,
            live_mirror: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            cam_frames: None,
            stale_images: Vec::new(),
            cam_notice: None,
            saved: None,
            clicks: Vec::new(),
            export: export_dialog::ExportDialog::default(),
            export_opts: formats::Options::default(),
            use_region: false,
            region: region::load_last(),
            selector: None,
            region_border: None,
            cap: captions_panel::CaptionSettings::default(),
            cap_style: captions::render::CaptionStyle::default(),
            captions: None,
            download: None,
            stt: None,
            cap_error: None,
            cap_edit: None,
            cap_selected: None,
            cap_art: None,
            cap_dropdowns,
            knob_bounds: knob::new_bounds(),
            bg_picker,
            anim_frame: None,
            idle_clock: Instant::now(),
            watch_scan: Instant::now(),
            art: None,
            art_job: None,
            audio: None,
            player: audio::Player::default(),
            audio_restart: false,
            empty_frame: canvas_preview::to_render_image(image::RgbaImage::from_pixel(
                1,
                1,
                image::Rgba([0x2c, 0x2c, 0x2e, 0xff]),
            )),
            out_dir: export::load_export_dir(),
            zooms: Vec::new(),
            motion,
            selected: None,
            auto_zoom: true,
            suggest_note: None,
            edit: editing::EditList::full(0.0),
            edit_history: editing::History::default(),
            edit_sel: None,
            edited_caps: None,
            edited_clicks: Vec::new(),
            edited_audio: None,
            waveform: None,
            tl_zoom: 1.0,
            tl_scroll: 0.0,
            confirm_reset: false,
            frame_cache: None,
            showing_exact: true,
            playhead: 0.0,
            playing: false,
            last_tick: Instant::now(),
            panel: Panel::Layout,
            gesture: timeline::Gesture::None,
            picker_bounds: Rc::new(Cell::new(Bounds::default())),
            focus: cx.focus_handle(),
            muted: false,
            mic: cx.new(|cx| MicSettings::new(window, cx)),
            main_window,
            bar: None,
            hotkey_config: hotkeys::load(),
            hotkeys: None,
            shot: screenshot::Settings::load(),
            shot_busy: false,
            shot_pick: Vec::new(),
            shot_error: None,
            system_shots: None,
            system_poll: Instant::now(),
            shot_hotkey: None,
            recent_shots: Vec::new(),
            _ticker: ticker,
            _job: None,
            _subs: subs,
        }
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        #[cfg(debug_assertions)]
        {
            static LAST: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());
            let now = self.stage_violations();
            let mut last = LAST.lock().unwrap();
            for v in now.iter().filter(|v| !last.contains(v)) {
                eprintln!("Clipper: state invariant broken: {v}");
            }
            *last = now;
        }
        // Watched background folder: look for new images every few seconds, off the UI thread.
        if self.watch_scan.elapsed() > Duration::from_secs(5) {
            self.watch_scan = Instant::now();
            if !backgrounds::library::scanning() {
                std::thread::spawn(backgrounds::library::scan_watched_global);
            }
        }
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f64();
        self.last_tick = now;
        self.sync_audio();
        self.sync_live();
        self.poll_captions(cx);
        self.poll_export(cx);
        if let Some(rec) = &self.recorder {
            if rec.webcam_lost() && self.cam_notice.is_none() {
                self.cam_notice = Some("Webcam disconnected. Still recording screen and audio; the rest of the clip has no webcam.".into());
            }
        }
        // A screenshot taken with macOS's shortcuts: show its card right away.
        if self.shot.detect_system && self.system_poll.elapsed() >= Duration::from_millis(250) {
            self.system_poll = Instant::now();
            let new = self.system_shots.as_mut().map(|w| w.poll()).unwrap_or_default();
            if !new.is_empty() {
                cx.defer(move |cx| {
                    let display = screenshot::capture::display_under_cursor();
                    for path in new {
                        if let Some(d) = display {
                            screenshot::cards::show(path, d, cx);
                        }
                    }
                });
            }
        }
        for action in menubar::take_actions() {
            match action {
                menubar::Action::Screenshot => self.take_screenshot(false, cx),
                menubar::Action::EditImage => {
                    cx.activate(true);
                    self.choose_images(cx);
                }
                menubar::Action::Open => self.show_main(cx),
                menubar::Action::Record => {
                    if matches!(self.stage, Stage::Idle) {
                        self.show_main(cx);
                        self.start_recording(cx);
                    } else {
                        self.show_main(cx);
                    }
                }
                menubar::Action::Quit => cx.quit(),
            }
        }
        let pressed = hotkeys::take_pressed();
        if pressed.contains(&hotkeys::Action::Screenshot) {
            self.take_screenshot(false, cx);
        }
        match self.stage {
            Stage::Starting | Stage::Recording => {
                for action in pressed {
                    match action {
                        hotkeys::Action::PauseResume => self.toggle_pause(cx),
                        hotkeys::Action::Stop => self.stop_recording(cx),
                        hotkeys::Action::Screenshot => {}
                    }
                }
                if let Some(Err(e)) = self.recorder.as_mut().map(|r| r.poll()) {
                    self.recorder = None;
                    self.end_session(cx);
                    self.stage = Stage::Error(format!(
                        "Screen capture stopped.\nCheck Screen Recording permission in System Settings → Privacy & Security.\n\n{e}"
                    ));
                }
                cx.notify();
            }
            Stage::Review | Stage::Exporting => {
                self.keep_playhead_in_edit();
                // Paused on a frame that was still decoding: show it once it's there.
                if !self.showing_exact && !self.playing {
                    let k = (self.playhead * recorder::FPS as f64 + 1e-6) as u64;
                    if self.frame_cache.as_ref().is_some_and(|c| c.get(k).is_some()) {
                        cx.notify();
                    }
                }
                // Playback runs on the edited timeline, so it plays straight through cuts.
                if self.playing && !self.gesture.holds_playback() {
                    let d = self.out_duration().max(0.01);
                    let mut next = self.out_time() + dt;
                    if next >= d {
                        self.audio_restart = true;
                        next %= d;
                    }
                    self.playhead = self.edit.output_to_source(next);
                    cx.notify();
                }
                if matches!(self.stage, Stage::Exporting) {
                    cx.notify();
                }
            }
            _ => {}
        }
    }

    /// Runs the live camera feed while idle with the webcam on, and stops it otherwise
    /// (always before recording, so only the recorder holds the camera).
    fn sync_live(&mut self) {
        // Never while the window is closed: in the menu bar Clipper leaves the camera alone.
        let want = matches!(self.stage, Stage::Idle) && self.webcam.enabled && !menubar::window_hidden();
        if !want {
            self.live.stop();
            return;
        }
        match &mut self.live {
            webcam::LivePreview::Failed => {}
            webcam::LivePreview::Running(l) => {
                if l.failed() {
                    self.live = webcam::LivePreview::Failed;
                    self.cam_notice = Some("Can't open the camera. Check Camera permission in System Settings → Privacy & Security.".into());
                }
            }
            webcam::LivePreview::Off => {
                let Some(device) = webcam::resolve_device(&self.webcam, &self.cam_devices) else { return };
                self.live_mirror.store(self.webcam.mirror, Ordering::Relaxed);
                match webcam::backend().live(&device, self.live_mirror.clone()) {
                    Ok(live) => self.live = webcam::LivePreview::Running(live),
                    Err(e) => {
                        self.live = webcam::LivePreview::Failed;
                        self.cam_notice = Some(e.to_string());
                    }
                }
            }
        }
    }

    /// Restarts the live feed (new device, switched on again).
    fn restart_live(&mut self) {
        self.live = webcam::LivePreview::Off;
        self.cam_notice = None;
    }

    fn set_cam_devices(&mut self, devices: Vec<String>, cx: &mut Context<Self>) {
        self.cam_devices = devices;
        let items: Vec<gpui_kit::SharedString> = self.cam_devices.iter().map(|d| d.clone().into()).collect();
        let current = webcam::resolve_device(&self.webcam, &self.cam_devices)
            .and_then(|cur| self.cam_devices.iter().position(|d| *d == cur));
        self.cam_select.update(cx, |s, cx| s.set_items(items, current, cx));
        cx.notify();
    }

    /// The display to record (by the Source setting), and the source.
    fn source(&self) -> region::Source {
        let displays = cursor::displays();
        let display = displays.get(self.settings.display).or(displays.first()).map_or(0, |d| d.id);
        match self.region.filter(|r| self.use_region && displays.iter().any(|d| d.id == r.display)) {
            Some(r) => region::Source::Region(r),
            None => region::Source::Display(display),
        }
    }

    /// Pixel size of what will be captured, and its pixels per point.
    fn capture_size(&self) -> (u32, u32, f64) {
        let displays = cursor::displays();
        match self.source() {
            region::Source::Region(r) => {
                let s = displays.iter().find(|d| d.id == r.display).map_or(2.0, |d| d.scale);
                (r.w, r.h, s)
            }
            region::Source::Display(id) => displays
                .iter()
                .find(|d| d.id == id)
                .map(|d| {
                    let (w, h) = d.pixels();
                    (region::even(w), region::even(h), d.scale)
                })
                .unwrap_or((1920, 1080, 1.0)),
        }
    }

    /// Whether the canvas has a webcam to lay out: the recorded track once there is a
    /// recording, otherwise the webcam switch.
    fn has_webcam(&self) -> bool {
        match &self.recording {
            Some(rec) => rec.webcam.is_some(),
            None => self.webcam.enabled,
        }
    }

    /// Re-decodes the preview's webcam frames (after recording, or when Mirror changes).
    fn load_cam_frames(&mut self, cx: &mut Context<Self>) {
        let (Some(rec), Some(preview)) = (self.recording.clone(), self.preview.as_ref()) else { return };
        let Some(track) = rec.webcam.clone() else { return };
        let (fps, count, mirror) = (preview.fps, preview.frames.len(), self.webcam.mirror);
        cx.spawn(async move |this, cx| {
            let frames = cx
                .background_executor()
                .spawn(async move { webcam::decode_preview(&track, fps, count, mirror) })
                .await;
            let _ = this.update(cx, |this, cx| {
                match frames {
                    Ok(frames) => {
                        if let Some(old) = this.cam_frames.replace(frames) {
                            this.stale_images.extend(old.into_iter().flat_map(webcam::CamImage::images));
                        }
                    }
                    Err(e) => eprintln!("Clipper: {e}"),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Keeps preview audio in step with playback: playing from the playhead while the
    /// preview plays, silent otherwise.
    fn sync_audio(&mut self) {
        let want = self.playing
            && !self.gesture.holds_playback()
            && matches!(self.stage, Stage::Review | Stage::Exporting);
        // The mic track already cut to the edit (with crossfades), on the output timeline.
        match &self.edited_audio {
            Some(track) if want => {
                if self.audio_restart || !self.player.is_playing() {
                    self.player.play_from(track, self.edit.source_to_output(self.playhead));
                }
            }
            _ => self.player.stop(),
        }
        self.audio_restart = false;
    }

    /// Minimizes the main window, shows the floating control bar, registers the global
    /// hotkeys, then starts capture once the window is out of the way.
    fn start_recording(&mut self, cx: &mut Context<Self>) {
        self.stage = Stage::Starting;
        self.hotkey_config = hotkeys::load();
        let (registration, failed) = hotkeys::register(&self.hotkey_config);
        if !failed.is_empty() {
            eprintln!("Clipper: could not register hotkeys {failed:?} (in use by another app?)");
        }
        self.hotkeys = Some(registration);

        let (main, this) = (self.main_window, cx.entity());
        let webcam_bar = self.webcam.enabled;
        let border = match self.source() {
            region::Source::Region(r) => cursor::displays().into_iter().find(|d| d.id == r.display).map(|d| (d, r)),
            _ => None,
        };
        cx.defer(move |cx| {
            let _ = main.update(cx, |_, window, _| window.minimize_window());
            // A thin outline just outside the region shows what's being recorded.
            if let Some((d, r)) = border {
                let handle = selector::open_border(d, r, cx);
                this.update(cx, |c, _| c.region_border = handle);
            }
            let bar = control_bar::open(this.clone(), webcam_bar, cx);
            this.update(cx, |c, _| c.bar = bar);
        });

        let muted = self.muted;
        let mic = self.mic.read(cx).mic_arg();
        self.live.stop();
        self.cam_notice = None;
        let cam_settings = self.webcam.clone();
        let source = self.source();
        self._job = Some(cx.spawn(async move |this, cx| {
            // Let the minimize animation finish so it isn't recorded.
            cx.background_executor().timer(Duration::from_millis(450)).await;
            let result = cx.background_executor()
                .spawn(async move {
                    let cam = cam_settings
                        .enabled
                        .then(|| webcam::resolve_device(&cam_settings, &webcam::backend().devices()))
                        .flatten();
                    Recorder::start_with(source, muted, mic, cam)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if !matches!(this.stage, Stage::Starting) {
                    // Stopped while starting: throw the capture away.
                    if let Ok(r) = result {
                        let dir = r.dir().to_path_buf();
                        drop(r);
                        let _ = std::fs::remove_dir_all(dir);
                    }
                    return;
                }
                match result {
                    Ok(r) => {
                        let with_cam = r.has_webcam();
                        this.recorder = Some(r);
                        this.stage = Stage::Recording;
                        // The camera is warm now: show the live bubble.
                        if let Some(device) = with_cam.then(|| webcam::resolve_device(&this.webcam, &this.cam_devices)).flatten() {
                            this.live_mirror.store(this.webcam.mirror, Ordering::Relaxed);
                            let (me, mirror) = (cx.entity(), this.live_mirror.clone());
                            cx.defer(move |cx| {
                                let handle = cam_bubble::open(me.clone(), device, mirror, cx);
                                me.update(cx, |c, _| c.bubble = handle);
                            });
                        }
                    }
                    Err(e) => {
                        this.end_session(cx);
                        this.stage = Stage::Error(e.to_string());
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Closes the control bar, drops the hotkeys and brings the main window back.
    fn end_session(&mut self, cx: &mut Context<Self>) {
        self.hotkeys = None;
        let (bar, main, border) = (self.bar.take(), self.main_window, self.region_border.take());
        let bubble = self.bubble.take();
        cx.defer(move |cx| {
            if let Some(b) = bubble {
                cam_bubble::close(b, cx);
            }
            if let Some(bar) = bar {
                control_bar::close(bar, cx);
            }
            if let Some(b) = border {
                let _ = b.update(cx, |_, window, _| window.remove_window());
            }
            // Restore on a later tick: un-minimizing in the same pass as the bar's teardown
            // trips an AppKit Touch Bar KVO exception.
            cx.spawn(async move |cx| {
                cx.background_executor().timer(Duration::from_millis(150)).await;
                let _ = cx.update(|cx| {
                    menubar::set_dock_icon_visible(true);
                    let _ = main.update(cx, |_, window, _| {
                        menubar::show_window(window);
                        window.activate_window();
                    });
                    cx.activate(true);
                });
            })
            .detach();
        });
    }

    fn toggle_pause(&mut self, cx: &mut Context<Self>) {
        let Some(rec) = self.recorder.as_mut() else { return };
        if rec.is_paused() {
            if let Err(e) = rec.resume() {
                self.recorder = None;
                self.end_session(cx);
                self.stage = Stage::Error(e.to_string());
            }
        } else {
            rec.pause();
        }
        cx.notify();
    }

    fn toggle_webcam(&mut self, cx: &mut Context<Self>) {
        if let Some(rec) = self.recorder.as_mut() {
            rec.toggle_webcam();
            if rec.webcam_on() {
                self.cam_notice = None;
            }
        }
        cx.notify();
    }

    fn toggle_mute(&mut self, cx: &mut Context<Self>) {
        self.muted = !self.muted;
        if let Some(rec) = self.recorder.as_mut() {
            rec.set_muted(self.muted);
        }
        cx.notify();
    }

    fn bar_state(&self) -> BarState {
        let rec = self.recorder.as_ref();
        let elapsed = rec.map(|r| r.elapsed()).unwrap_or_default();
        BarState {
            starting: rec.is_none_or(|r| r.is_warming_up()),
            paused: rec.is_some_and(|r| r.is_paused()),
            muted: self.muted,
            has_audio: rec.is_none_or(|r| r.has_audio()),
            secs: elapsed.as_secs(),
            blink_on: elapsed.as_millis() / 600 % 2 == 0,
            cam_lost: rec.is_some_and(|r| r.webcam_lost()),
            has_webcam: rec.map_or(self.webcam.enabled, |r| r.has_webcam()),
            webcam_on: rec.map_or(self.webcam.enabled, |r| r.webcam_on()),
            pause_key: self.hotkey_config.pause.label(),
            stop_key: self.hotkey_config.stop.label(),
        }
    }

    fn stop_recording(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.stage, Stage::Starting | Stage::Recording) {
            return;
        }
        self.end_session(cx);
        let Some(recorder) = self.recorder.take() else {
            // Still starting: cancel.
            self.stage = Stage::Idle;
            cx.notify();
            return;
        };
        self.stage = Stage::Processing;
        let job = cx.background_executor().spawn(async move {
            let rec = recorder.stop()?;
            let preview = export::build_preview(&rec)?;
            let track = audio::decode(&rec).unwrap_or_else(|e| {
                eprintln!("Clipper: {e}");
                None
            });
            anyhow::Ok((rec, preview, track))
        });
        self._job = Some(cx.spawn(async move |this, cx| {
            let result = job.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok((rec, preview, track)) => {
                        if this.webcam.enabled && rec.webcam.is_none() && this.cam_notice.is_none() {
                            this.cam_notice = Some("The webcam didn't record (no camera or no permission). The clip has screen and audio only.".into());
                        }
                        this.show_recording(rec, preview, track, None, cx);
                    }
                    Err(e) => this.stage = Stage::Error(e.to_string()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Broken invariants between `stage` and the state that belongs to it (empty when all
    /// hold). Debug builds log each new one from the ticker, so drift shows up right away.
    #[cfg(debug_assertions)]
    fn stage_violations(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        let reviewing = matches!(self.stage, Stage::Review | Stage::Exporting);
        if matches!(self.stage, Stage::Recording) && self.recorder.is_none() {
            v.push("Recording without a recorder");
        }
        if reviewing && (self.recording.is_none() || self.preview.is_none()) {
            v.push("Review/Exporting without a recording and preview");
        }
        if matches!(self.stage, Stage::Exporting) != self.export.run.is_some() {
            v.push("Exporting and export.run disagree");
        }
        if self.gesture.holds_playback() && !matches!(self.stage, Stage::Review) {
            v.push("timeline gesture outside Review");
        }
        v
    }

    /// Opens a finished recording in the editor. `captions` are its saved captions; without
    /// them, transcription starts (when captions are on).
    fn show_recording(&mut self, rec: Recording, preview: Preview, track: Option<audio::Track>, captions: Option<captions::Captions>, cx: &mut Context<Self>) {
        self.audio = track;
        self.zooms = if self.auto_zoom { zoom::suggest(&rec.samples, rec.width, rec.height, rec.duration) } else { Vec::new() };
        self.selected = None;
        self.suggest_note = None;
        self.recording = Some(Arc::new(rec));
        self.preview = Some(preview);
        self.cam_frames = None;
        self.load_cam_frames(cx);
        self.recompute();
        self.playhead = 0.0;
        self.playing = true;
        self.panel = Panel::Zoom;
        self.stage = Stage::Review;
        self.cap_art = None;
        let have_captions = captions.is_some();
        self.captions = captions;
        self.load_edit();
        // Transcription runs only now, after Stop, in the background.
        if self.cap.enabled && !have_captions {
            self.panel = Panel::Captions;
            self.start_transcription(cx);
        }
    }

    fn recompute(&mut self) {
        self.clicks = self.recording.as_ref().map(|r| clicks::clicks(&r.samples)).unwrap_or_default();
        if let Some(rec) = &self.recording {
            self.path = Some(camera::compute_zoomed(
                &rec.samples,
                rec.width,
                rec.height,
                rec.duration,
                self.layout.frame(&self.style, self.has_webcam()).screen.aspect(),
                self.layout.zoom as f64,
                self.settings.follow(),
                &self.zooms,
                &self.motion,
            ));
        }
    }

    fn choose_folder(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Export Here".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(dir) = paths.into_iter().next() else { return };
            let _ = this.update(cx, |this, cx| {
                export::save_export_dir(&dir);
                this.out_dir = dir;
                cx.notify();
            });
        })
        .detach();
    }

    /// Picks a background photo and decodes it off the main thread.
    fn reset(&mut self, discard: bool, cx: &mut Context<Self>) {
        if discard {
            if let Some(rec) = &self.recording {
                let _ = std::fs::remove_dir_all(&rec.dir);
            }
        }
        self.recording = None;
        self.preview = None;
        self.path = None;
        self.recorder = None;
        self.audio = None;
        if let Some(old) = self.cam_frames.take() {
            self.stale_images.extend(old.into_iter().flat_map(webcam::CamImage::images));
        }
        self.cam_notice = None;
        self.saved = None;
        self.captions = None;
        self.stt = None;
        self.cap_edit = None;
        self.cap_selected = None;
        self.cap_art = None;
        self.cap_error = None;
        self.player.stop();
        self.zooms.clear();
        self.selected = None;
        self.suggest_note = None;
        self.edit = editing::EditList::full(0.0);
        if let Some(cache) = self.frame_cache.take() {
            self.stale_images.extend(cache.close());
        }
        self.edit_history.clear();
        self.edit_sel = None;
        self.waveform = None;
        self.refresh_edit_derived();
        self.playhead = 0.0;
        self.playing = false;
        self.gesture = timeline::Gesture::None;
        self.panel = Panel::Layout;
        self.stage = Stage::Idle;
        cx.notify();
    }

    // ---------- preview (center) ----------

    /// The composited 9:16 output, centered on the light stage.
    fn preview_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let vp = window.viewport_size();
        let area_w = f32::from(vp.width) - PANEL_W - RAIL_W - GAP * 4.0 - 48.0;
        let area_h = f32::from(vp.height) - HEADER_H - self.timeline_height() - GAP * 2.0 - 48.0;
        let (ow, oh) = self.layout.canvas();
        let aspect = ow as f32 / oh as f32;
        let ch = area_h.min(area_w / aspect).max(120.0).floor();
        let cw = (ch * aspect).floor();

        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .rounded(px(R_CARD))
            .bg(rgb(STAGE))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .child(self.canvas(cw, ch, window, cx))
    }

    /// Background, framed clip and cursor via `canvas_preview`, the same layers the export uses.
    fn canvas(&mut self, cw: f32, ch: f32, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let has_cam = self.has_webcam();
        let frame = self.layout.frame(&self.style, has_cam);
        let output = frame.canvas;
        let aspect = frame.screen.aspect();
        let caption = self.caption_layer(&frame, cw, window);
        let playing = matches!(self.stage, Stage::Review | Stage::Exporting);
        let footage = match (&self.preview, &self.path, &self.recording) {
            (Some(p), Some(path), Some(rec)) if playing => Some((p, path, rec.clone())),
            _ => None,
        };
        // Before there is footage, frame a placeholder the size of what will be captured
        // (the region, or the chosen display).
        let (pw, ph, ppp) = self.capture_size();
        let rec = footage.as_ref().map(|f| f.2.clone()).unwrap_or_else(|| {
            Arc::new(Recording {
                dir: PathBuf::new(),
                raw: PathBuf::new(),
                width: pw,
                height: ph,
                duration: 0.0,
                samples: Vec::new(),
                webcam: None,
                px_per_pt: ppp,
            })
        });
        let (crop_w, crop_h) = match &footage {
            Some((_, path, _)) => (path.crop_w, path.crop_h),
            None => camera::crop_size(rec.width, rec.height, aspect, self.layout.zoom as f64),
        };

        // Rebuild the cached layers only when something they depend on changed.
        let st = &self.style;
        let key = format!(
            "{:?}|{:?}|{:?}|{:?}|{}|{:?}|{:?}|{}|{}|{}|{}|{}|{}x{}|{}|{}|{}",
            frame,
            (self.layout.cam_border_color, self.layout.cam_shadow),
            bg_key(&st.bg),
            0,
            0,
            0,
            0,
            st.padding,
            st.radius,
            st.shadow_blur,
            st.shadow_opacity,
            st.cursor_scale,
            output.0,
            output.1,
            cw,
            crop_w,
            window.scale_factor(),
        );
        let (border_color, cam_shadow) = (self.layout.cam_border_color, self.layout.cam_shadow);
        if self.art.is_none() {
            let art = CanvasArt::new(&self.style, &frame, border_color, cam_shadow, size(px(cw), px(ch)), &rec, crop_w, window);
            self.art = Some((key, art));
        } else if self.art.as_ref().is_some_and(|(k, _)| *k != key) && self.art_job.is_none() {
            // Re-render off the UI thread and keep showing the current layers meanwhile, so
            // dragging a slider stays smooth. When this lands, the next render starts if the
            // settings moved on in the meantime.
            let (style, scale_factor, ppp) = (self.style.clone(), window.scale_factor(), canvas_preview::px_per_pt(&rec));
            let display = size(px(cw), px(ch));
            self.art_job = Some(cx.spawn_in(window, async move |this, cx| {
                let pixels = cx
                    .background_executor()
                    .spawn(async move {
                        ArtPixels::render(&style, &frame, border_color, cam_shadow, display, scale_factor, ppp, crop_w)
                    })
                    .await;
                let _ = this.update_in(cx, |this, window, cx| {
                    this.art_job = None;
                    if let Some((_, old)) = this.art.replace((key, CanvasArt::upload(pixels))) {
                        old.release(window);
                    }
                    cx.notify();
                });
            }));
        }
        // Newest live camera frame; the replaced one is freed below.
        if let Some(img) = self.live.take() {
            if let Some(old) = self.live_frame.replace(webcam::CamImage::from_bgra(img)) {
                self.stale_images.extend(old.images());
            }
        }
        if !self.webcam.enabled || !matches!(self.stage, Stage::Idle) {
            self.stale_images.extend(self.live_frame.take().into_iter().flat_map(webcam::CamImage::images));
        }
        if let Some(cache) = &self.frame_cache {
            self.stale_images.extend(cache.take_evicted());
        }
        for old in self.stale_images.drain(..) {
            let _ = window.drop_image(old);
        }
        // Animated background: render this moment's low-resolution frame (timeline time while
        // reviewing, so pausing freezes it and the export matches; a free-running clock before
        // recording) and re-tint the rounded corners from it.
        let anim_on = matches!(&self.style.bg, backgrounds::Background::Animated(a) if !a.frozen)
            && self.art.as_ref().is_some_and(|(_, a)| a.animated());
        if anim_on {
            if let backgrounds::Background::Animated(a) = &self.style.bg {
                // Edited timeline time, so the background never jumps at a cut.
                let t = if playing { self.out_time() } else { self.idle_clock.elapsed().as_secs_f64() };
                let key = (bg_key(&self.style.bg), (t * 60.0).round() as i64);
                if self.anim_frame.as_ref().is_none_or(|f| (f.0, f.1) != key) {
                    let (lw, lh) = backgrounds::render::low_size(output.0, output.1);
                    let low = backgrounds::render::animated_frame(a, lw, lh, t);
                    let patches = self.art.as_ref().unwrap().1.anim_corners(&low);
                    let frame_img = canvas_preview::to_render_image(image::DynamicImage::ImageRgb8(low).to_rgba8());
                    if let Some((_, _, old, old_patches)) = self.anim_frame.replace((key.0, key.1, frame_img, patches)) {
                        self.stale_images.push(old);
                        self.stale_images.extend(old_patches.into_iter().map(|(p, _)| p));
                    }
                }
                // Keep it moving (before recording; while reviewing the playhead drives it).
                if (!playing || self.playing) && !menubar::window_hidden() {
                    window.request_animation_frame();
                }
            }
        } else if let Some((_, _, old, old_patches)) = self.anim_frame.take() {
            self.stale_images.push(old);
            self.stale_images.extend(old_patches.into_iter().map(|(p, _)| p));
        }
        for old in self.stale_images.drain(..) {
            let _ = window.drop_image(old);
        }
        let anim = self.anim_frame.as_ref().filter(|_| anim_on).map(|(_, _, f, p)| (f.clone(), p.as_slice()));
        let art = &self.art.as_ref().unwrap().1;

        let content = match footage {
            Some((preview, path, rec)) => {
                let n = preview.frames.len();
                let t = self.playhead.clamp(0.0, rec.duration);
                let idx = ((t * preview.fps) as usize).min(n - 1);
                // The exact frame from the cache when it's decoded, else the nearest thumbnail.
                // Ask for what comes next: the frames ahead on the edited timeline while
                // playing, the neighbors while paused (for frame stepping).
                let k = (t * recorder::FPS as f64 + 1e-6) as u64;
                let exact = self.frame_cache.as_ref().and_then(|c| {
                    let fps = recorder::FPS as f64;
                    let want: Vec<u64> = if self.playing {
                        let o = self.edit.source_to_output(t);
                        (0..60).map(|j| (self.edit.output_to_source(o + j as f64 / fps) * fps + 1e-6) as u64).collect()
                    } else {
                        std::iter::once(k).chain((1..=15).flat_map(|j| [k + j, k.saturating_sub(j)])).collect()
                    };
                    c.want(want);
                    c.get(k)
                });
                self.showing_exact = exact.is_some();
                let screen_frame = exact.unwrap_or_else(|| preview.frames[idx].clone());
                let cam = match (&rec.webcam, &self.cam_frames) {
                    (Some(track), Some(frames)) if track.active_at(t) => {
                        CamShow::Frame(frames[idx.min(frames.len() - 1)].clone())
                    }
                    (Some(track), None) if track.active_at(t) => CamShow::Placeholder,
                    _ => CamShow::Hidden,
                };
                canvas_preview::canvas_preview(
                    art,
                    screen_frame,
                    &rec,
                    path.at_time(t),
                    camera::cursor_at(&rec.samples, t),
                    cam,
                    caption,
                    &if self.style.click.enabled { editing::remap::active_clicks(&self.edited_clicks, self.edit.source_to_output(t), self.style.click.duration) } else { Vec::new() },
                    self.style.click,
                    anim,
                )
            }
            None => canvas_preview::canvas_preview(
                art,
                self.empty_frame.clone(),
                &rec,
                (0, 0, crop_w, crop_h),
                None,
                match &self.live_frame {
                    Some(img) => CamShow::Frame(img.clone()),
                    None => CamShow::Placeholder,
                },
                caption,
                &[],
                self.style.click,
                anim,
            ),
        };

        let status = match self.stage {
            _ if playing => None,
            Stage::Starting => Some(("Starting…", true)),
            Stage::Recording => Some(("Recording…", false)),
            Stage::Processing => Some(("Preparing preview…", true)),
            Stage::Error(_) => Some(("Something went wrong", false)),
            _ => Some(("Preview appears after recording", false)),
        };
        let idle_empty = matches!(self.stage, Stage::Idle) && !playing;
        // The floating webcam can be dragged anywhere on the canvas (before and after recording).
        let k = cw / output.0 as f32;
        let movable = matches!(self.stage, Stage::Idle | Stage::Review) && self.has_webcam();
        let cam_handle = frame.cam.filter(|c| c.floating && movable).map(|c| {
            let dragging = matches!(self.gesture, timeline::Gesture::Cam { .. });
            let (x, y, d) = (c.rect.x as f32 * k, c.rect.y as f32 * k, c.rect.w as f32 * k);
            div()
                .id("cam-handle")
                .absolute()
                .left(px(x))
                .top(px(y))
                .size(px(d))
                .rounded(px(c.radius as f32 * k))
                .border_2()
                .border_color(if dragging { gpui_kit::hsla(0., 0., 1.0, 0.7) } else { gpui_kit::transparent_black() })
                .hover(|s| s.border_color(gpui_kit::hsla(0., 0., 1.0, 0.45)))
                .cursor(if dragging { gpui_kit::CursorStyle::ClosedHand } else { gpui_kit::CursorStyle::OpenHand })
                .tooltip(tip("Drag to place the webcam anywhere"))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, ev: &gpui_kit::MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        let b = this.preview_bounds.get();
                        let px_out = |v: Pixels, o: Pixels| (f32::from(v) - f32::from(o)) / k;
                        let (cx0, cy0) = ((c.rect.x + c.rect.w / 2) as f32, (c.rect.y + c.rect.h / 2) as f32);
                        this.gesture = timeline::Gesture::Cam { grab: (px_out(ev.position.x, b.origin.x) - cx0, px_out(ev.position.y, b.origin.y) - cy0) };
                        cx.notify();
                    }),
                )
        });
        let store = self.preview_bounds.clone();
        div()
            .relative()
            .w(px(cw))
            .h(px(ch))
            .rounded(px(canvas_preview::STAGE_RADIUS))
            .overflow_hidden()
            .child(content)
            .child(gpui_kit::canvas(move |b, _, _| store.set(b), |_, _, _, _| {}).absolute().size_full())
            .children(cam_handle)
            .when_some(status, |d, (text, spin)| {
            d.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .text_xs()
                    .text_color(rgb(0xc7c7cc))
                    .when(spin, |d| d.child(Spinner::new().color(c(0xc7c7cc))))
                    // The mascot greets you before the first recording.
                    .when(idle_empty, |d| d.child(img(mascot::image(180)).h(px(84.)).w(px(84. * mascot::aspect())).mb_1()))
                    .child(text),
            )
        })
    }

    // ---------- settings (right) ----------

    fn settings_pane(&self, cx: &mut Context<Self>) -> Div {
        // Images are ordered for the output's shape (portrait first for 9:16).
        let portrait = self.layout.ratio == layout::Ratio::Portrait;
        self.bg_picker.update(cx, |p, cx| p.set_portrait(portrait, cx));
        // Tabs live in a vertical rail at the right edge.
        let mut rail = div()
            .w(px(RAIL_W))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(4.))
            .py(px(10.))
            .rounded(px(R_CARD))
            .bg(rgb(CARD));
        for p in Panel::ALL {
            let on = p == self.panel;
            rail = rail.child(
                div()
                    .id(("panel-tab", p as usize))
                    .w(px(RAIL_W - 12.0))
                    .py(px(8.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(4.))
                    .rounded(px(14.))
                    .cursor_pointer()
                    .when(on, |d| d.bg(ui::accent_tint()).text_color(rgb(GREEN)))
                    .when(!on, |d| d.text_color(rgb(MUTED)).hover(|d| d.bg(rgb(RAISED)).text_color(rgb(TEXT))))
                    .child(p.icon().size(px(20.)))
                    .child(div().text_size(px(11.)).font_weight(FontWeight::MEDIUM).child(p.label()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.panel = p;
                        cx.notify();
                    })),
            );
        }
        let body = match self.panel {
            Panel::Layout => self.layout_panel(cx),
            Panel::Scene => div()
                .flex()
                .flex_col()
                .gap_5()
                .child(section("Background", self.background_section(cx)))
                .child(section("Frame", self.frame_section(cx))),
            Panel::Zoom => div()
                .flex()
                .flex_col()
                .gap_5()
                .child(self.zoom_panel(cx))
                .child(section("Camera follow", self.camera_section(cx)))
                .child(section("Cursor", self.cursor_section(cx))),
            Panel::Captions => self.captions_panel(cx),
            Panel::Shots => self.screenshot_panel(cx),
            Panel::Output => div()
                .flex()
                .flex_col()
                .gap_5()
                .child(section("Display", self.source_section(cx)))
                .child(section("Audio", div().child(self.mic.clone())))
                .child(section("Export", self.output_section(cx)))
                .child(section(
                    "Shortcuts while recording",
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(ui::shortcut("Pause / resume", self.hotkey_config.pause.label()))
                        .child(ui::shortcut("Stop", self.hotkey_config.stop.label())),
                )),
        };
        let panel = div()
            .w(px(PANEL_W))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .rounded(px(R_CARD))
            .overflow_hidden()
            .bg(rgb(CARD))
            .child(div().px_4().pt_4().pb_1().text_base().font_weight(FontWeight::SEMIBOLD).child(self.panel.label()))
            .child(
                div()
                    .id("settings-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(div().flex().flex_col().px_4().pt_3().pb_4().child(body)),
            );
        div().h_full().flex_none().flex().gap(px(GAP)).child(panel).child(rail)
    }

    fn source_section(&self, cx: &mut Context<Self>) -> Div {
        let list = displays();
        let picked = self.settings.display.min(list.len().saturating_sub(1));
        let rows = list.iter().enumerate().map(|(i, (name, dims))| {
            Button::new(format!("display-{i}")).cursor_pointer()
                .secondary()
                .rounded(px(R_MID))
                .w_full()
                .selected(i == picked)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.settings.display = i;
                    cx.notify();
                }))
                .child(
                    div()
                        .w_full()
                        .flex()
                        .justify_between()
                        .text_sm()
                        .child(name.clone())
                        .child(div().text_xs().text_color(rgb(MUTED)).child(dims.clone())),
                )
        });
        div()
            .flex()
            .flex_col()
            .gap_2()
            .children(rows)
            .child(note("Full-screen and region recordings use this display. Pick Full screen or Region next to Record."))
    }

    /// Opens the region selector on the chosen display.
    fn open_selector(&mut self, cx: &mut Context<Self>) {
        if self.selector.is_some() || !matches!(self.stage, Stage::Idle) {
            return;
        }
        let displays = cursor::displays();
        let Some(display) = displays.get(self.settings.display).or(displays.first()).copied() else { return };
        let (this, last) = (cx.entity(), self.region);
        cx.defer(move |cx| {
            let handle = selector::open(this.clone(), display, last, cx);
            this.update(cx, |c, _| c.selector = handle);
        });
    }

    /// The selector closed: `Some` with the confirmed region, `None` if cancelled.
    pub(crate) fn region_selected(&mut self, region: Option<region::Region>, cx: &mut Context<Self>) {
        self.selector = None;
        if let Some(r) = region {
            self.region = Some(r);
            self.use_region = true;
            if let Some(i) = cursor::displays().iter().position(|d| d.id == r.display) {
                self.settings.display = i;
            }
        } else if self.region.is_none() {
            self.use_region = false;
        }
        self.art = None;
        self.recompute();
        let main = self.main_window;
        cx.defer(move |cx| {
            let _ = main.update(cx, |_, window, _| window.activate_window());
        });
        cx.notify();
    }

    fn camera_section(&self, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(self.knob(Knob::Deadzone, "Deadzone margin", cx))
            .child(self.knob(Knob::Smoothing, "Smoothing", cx))
    }

    /// Cursor size and the click highlight.
    fn cursor_section(&self, cx: &mut Context<Self>) -> Div {
        const RIPPLE_COLORS: [u32; 5] = [0xa4f47c, 0xffffff, 0xfacc15, 0x22d3ee, 0xf472b6];
        let click = self.style.click;
        let toggle = gpui_kit::component::switch::Switch::new("click-highlight").cursor_pointer()
            .checked(click.enabled)
            .small()
            .label("Highlight clicks")
            .on_click(cx.listener(|this, v: &bool, _, cx| {
                this.style.click.enabled = *v;
                cx.notify();
            }));
        let colors = div().flex().gap_1p5().children(RIPPLE_COLORS.iter().enumerate().map(|(i, &v)| {
            let selected = color_hex(click.color) == v;
            div()
                .id(("click-color", i))
                .size(px(26.))
                .p(px(2.))
                .rounded(px(9.))
                .border_2()
                .border_color(rgb(if selected { GREEN } else { CARD }))
                .cursor_pointer()
                .child(div().size_full().rounded_full().border_1().border_color(rgb(LINE)).bg(rgb(v)))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.style.click.color = hex_color(v);
                    cx.notify();
                }))
        }));
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(self.knob(Knob::Cursor, "Cursor size", cx))
            .child(toggle)
            .when(click.enabled, |d| {
                d.child(div().flex().items_center().justify_between().child(div().text_sm().child("Ripple color")).child(colors))
                    .child(self.knob(Knob::ClickSize, "Ripple size", cx))
                    .child(self.knob(Knob::ClickDuration, "Ripple duration", cx))
                    .child(note("Drawn from the recorded clicks (left and right), in the preview and every export."))
            })
    }

    fn background_section(&self, _: &mut Context<Self>) -> Div {
        div().child(self.bg_picker.clone())
    }

    fn frame_section(&self, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(self.knob(Knob::Padding, "Padding", cx))
            .child(self.knob(Knob::Radius, "Corner radius", cx))
            .child(self.knob(Knob::ShadowBlur, "Shadow blur", cx))
            .child(self.knob(Knob::ShadowOpacity, "Shadow opacity", cx))
    }

    fn output_section(&self, cx: &mut Context<Self>) -> Div {
        let st = self.settings;
        let fps = ui::segmented(
            "fps",
            FPS_CHOICES
                .map(|f| {
                    let on: ui::OnClick = Box::new(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.settings.fps = f;
                        cx.notify();
                    }));
                    (format!("{f} fps").into(), st.fps == f, on)
                })
                .into_iter()
                .collect(),
        );
        let folder = Button::new("folder").cursor_pointer()
            .secondary()
            .rounded(px(R_MID))
            .w_full()
            .small()
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.choose_folder(cx)))
            .child(
                div()
                    .w_full()
                    .flex()
                    .justify_between()
                    .gap_2()
                    .text_xs()
                    .child(div().truncate().child(display_path(&self.out_dir)))
                    .child(div().text_color(rgb(MUTED)).child("Change")),
            );
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(field("Frame rate", fps))
            .child(field("Save folder", folder))
    }

    // ---------- header ----------

    fn header(&self, cx: &mut Context<Self>) -> Div {
        let subtitle = match &self.stage {
            Stage::Idle => "New recording".to_string(),
            Stage::Starting | Stage::Recording => "Recording".to_string(),
            Stage::Processing => "Preparing…".to_string(),
            _ if self.recording.is_some() => {
                let d = self.out_duration();
                let n = self.zooms.len();
                format!(
                    "{}:{:02}  ·  {n} zoom{}",
                    (d / 60.0) as u32,
                    (d % 60.0) as u32,
                    if n == 1 { "" } else { "s" }
                )
            }
            _ => String::new(),
        };
        div()
            .h(px(HEADER_H))
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .pl(px(88.))
            .pr(px(GAP + 2.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(img(mascot::image(56)).h(px(26.)).w(px(26. * mascot::aspect())))
                    .child(div().text_base().font_weight(FontWeight::BOLD).child("Clipper"))
                    .child(div().text_xs().text_color(rgb(MUTED)).child(subtitle)),
            )
            .child(self.header_actions(cx))
    }

    /// What Record captures: the whole display or a region, right next to Record. In
    /// Region mode the region's size is shown; click it to pick a new one.
    fn source_picker(&self, cx: &mut Context<Self>) -> Div {
        let seg = |id: &'static str, icon: gpui_kit::component::Icon, label: &'static str, on: bool| {
            div()
                .id(id)
                .h(px(28.))
                .px_3()
                .flex()
                .items_center()
                .gap_1p5()
                .rounded_full()
                .cursor_pointer()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .when(on, |d| d.bg(rgb(TEXT)).text_color(rgb(BG)))
                .when(!on, |d| d.text_color(rgb(MUTED)).hover(|d| d.bg(rgb(HOVER)).text_color(rgb(TEXT))))
                .child(icon.size(px(15.)))
                .child(label)
        };
        let region_chip = self.region.filter(|_| self.use_region).map(|r| {
            div()
                .id("region-chip")
                .h(px(34.))
                .px_3()
                .flex()
                .items_center()
                .gap_1p5()
                .rounded_full()
                .bg(rgb(RAISED))
                .border_1()
                .border_color(ui::hairline())
                .cursor_pointer()
                .hover(|d| d.bg(rgb(HOVER)))
                .text_xs()
                .text_color(rgb(TEXT))
                .child(format!("{} × {}", r.w, r.h))
                .child(editor::icons::edit().size(px(13.)).text_color(rgb(MUTED)))
                .tooltip(tip("Select a new region"))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.open_selector(cx)))
        });
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex()
                    .p(px(3.))
                    .rounded_full()
                    .bg(rgb(RAISED))
                    .border_1()
                    .border_color(ui::hairline())
                    .child(seg("src-display", editor::icons::monitor(), "Full screen", !self.use_region).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.use_region = false;
                        this.recompute();
                        cx.notify();
                    })))
                    .child(seg("src-region", editor::icons::region(), "Region", self.use_region).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        // Always lets you draw: a new region, or adjust the current one.
                        if this.use_region || this.region.is_none() {
                            this.open_selector(cx);
                        } else {
                            this.use_region = true;
                            this.recompute();
                        }
                        cx.notify();
                    }))),
            )
            .children(region_chip)
    }

    fn header_actions(&self, cx: &mut Context<Self>) -> Div {
        let row = div().flex().items_center().gap_2();
        match &self.stage {
            Stage::Idle => row.child(self.source_picker(cx)).child(div().w(px(1.)).h(px(20.)).mx_1().bg(ui::hairline_strong())).child(
                header_button("screenshot", cx)
                    .icon(editor::icons::camera())
                    .child("Screenshot")
                    .tooltip(format!("Screenshot  {}", self.hotkey_config.screenshot.label()))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.take_screenshot(true, cx))),
            ).child(
                accent_button("record", cx)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.start_recording(cx)))
                    .child(div().size(px(10.)).rounded_full().bg(rgb(RED)).border_2().border_color(rgb(ON_GREEN)))
                    .child("Record"),
            ),
            Stage::Starting | Stage::Recording => {
                let elapsed = self.recorder.as_ref().map(|r| r.elapsed()).unwrap_or_default();
                let paused = self.recorder.as_ref().is_some_and(|r| r.is_paused());
                let secs = elapsed.as_secs();
                let starting = matches!(self.stage, Stage::Starting);
                let blink = !paused && elapsed.as_millis() / 600 % 2 == 0;
                row.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_sm()
                        .when(starting, |d| d.child(Spinner::new().color(c(MUTED))))
                        .when(!starting, |d| d.child(dot(if blink { RED } else { BG }, 8.)))
                        .child(if starting {
                            "Starting…"
                        } else if paused {
                            "Paused"
                        } else {
                            "Recording"
                        })
                        .child(
                            div()
                                .font_weight(FontWeight::MEDIUM)
                                .child(format!("{:02}:{:02}", secs / 60, secs % 60)),
                        ),
                )
                .child(
                    header_button("stop", cx)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.stop_recording(cx)))
                        .child(div().size(px(10.)).rounded(px(3.)).bg(rgb(RED)))
                        .child("Stop"),
                )
            }
            Stage::Processing => row
                .text_sm()
                .text_color(rgb(MUTED))
                .child(Spinner::new().color(c(MUTED)))
                .child("Preparing preview…"),
            Stage::Review => {
                // After an export: where it went, and the editor stays open for changes and
                // re-exports.
                let saved = self.saved.clone().map(|out| {
                    let name = out.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .max_w(px(260.))
                                .text_xs()
                                .text_color(rgb(MUTED))
                                .child(gpui_kit::component::Icon::new(gpui_kit::assets::IconName::CircleCheck).size_3p5().text_color(rgb(GREEN)))
                                .child(div().truncate().child(format!("Saved {name}"))),
                        )
                        .child(header_button("reveal", cx).child("Show in Finder").on_click(move |_, _, _| {
                            let _ = std::process::Command::new("open").arg("-R").arg(&out).spawn();
                        }))
                });
                row.children(saved)
                    .child(
                        header_button("discard", cx)
                            .text_color(rgb(MUTED))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                // Keep the recording files once something was exported from it.
                                let discard = this.saved.is_none();
                                this.reset(discard, cx)
                            }))
                            .child(if self.saved.is_some() { "New recording" } else { "Discard" }),
                    )
                    .child(
                        accent_button("export", cx)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.open_export(cx)))
                            .child(if self.saved.is_some() { "Export again" } else { "Export" }),
                    )
            }
            Stage::Exporting => {
                let p = self.export.run.as_ref().map_or(0.0, |r| r.job.progress.render.load(Ordering::Relaxed) as f32 / 10.0);
                row.child(div().text_sm().child("Exporting…"))
                    .child(div().w(px(140.)).child(Progress::new("export-progress").value(p).color(c(GREEN))))
                    .child(
                        header_button("export-open", cx)
                            .child("Details")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.export.open = true;
                                cx.notify();
                            })),
                    )
            }
            Stage::Error(_) => row.child(
                header_button("back", cx)
                    .child("Back")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.reset(false, cx))),
            ),
        }
    }
}

impl Render for Clipper {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Keep keyboard shortcuts working when nothing else has focus.
        if window.focused(cx).is_none() {
            self.focus.focus(window, cx);
        }
        let error = match &self.stage {
            Stage::Error(msg) => Some(msg.clone()),
            _ => None,
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .on_mouse_move(cx.listener(Self::on_drag_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_drag_end))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_drag_end))
            .child(self.header(cx))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .gap(px(GAP))
                    .px(px(GAP))
                    .child(self.preview_pane(window, cx))
                    .child(self.settings_pane(cx))
                    .when_some(error, |d, msg| {
                        d.child(
                            div()
                                .absolute()
                                .left_6()
                                .bottom_6()
                                .max_w(px(420.))
                                .p_4()
                                .rounded(px(R_MID))
                                .bg(rgb(RAISED))
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Something went wrong"))
                                .child(note(&msg)),
                        )
                    }),
            )
            .child(div().p(px(GAP)).child(self.timeline_pane(window, cx)))
            .children(self.export_dialog(cx))
    }
}

/// Section header + body.
fn section(title: &'static str, body: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(rgb(MUTED)).child(title))
        .child(body)
}

/// Small label above a control.
fn field(label: &'static str, control: impl IntoElement) -> Div {
    div().flex().flex_col().gap_1().child(div().text_xs().child(label)).child(control)
}

/// Hover label from an owned string.
fn tip_owned(text: String) -> impl Fn(&mut Window, &mut App) -> gpui_kit::AnyView {
    move |window, cx| gpui_kit::component::tooltip::Tooltip::new(text.clone()).build(window, cx)
}

/// Hover label for a picker tile.
fn tip(text: &'static str) -> impl Fn(&mut Window, &mut App) -> gpui_kit::AnyView {
    move |window, cx| gpui_kit::component::tooltip::Tooltip::new(text).build(window, cx)
}

/// A `cols`-wide grid of picker tiles.
fn grid(cols: u16) -> Div {
    div().grid().grid_cols(cols).gap_2()
}

fn note(text: &str) -> Div {
    div().text_xs().text_color(rgb(MUTED)).child(text.to_string())
}

fn dot(color: u32, d: f32) -> Div {
    div().size(px(d)).rounded_full().bg(rgb(color))
}

/// `/Users/me/Movies/Clipper` → `~/Movies/Clipper`.
fn display_path(p: &Path) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    match p.strip_prefix(&home) {
        Ok(rest) if !home.is_empty() => format!("~/{}", rest.display()),
        _ => p.display().to_string(),
    }
}

// ---------- GPUI Kit components, styled for Clipper ----------

/// Secondary header action: a dark pill.
fn header_button(id: &'static str, _: &App) -> Button {
    Button::new(id).cursor_pointer().secondary().rounded_full().h(px(34.)).px_4().gap_2().text_sm()
}

/// The one green call to action in the header (Record, Export).
fn accent_button(id: &'static str, _: &App) -> Button {
    Button::new(id).cursor_pointer()
        .primary()
        .rounded_full()
        .h(px(34.))
        .px_5()
        .gap_2()
        .text_sm()
        .font_weight(FontWeight::SEMIBOLD)
}

fn button(id: &'static str, label: &'static str, _: &App) -> Button {
    Button::new(id).cursor_pointer().secondary().rounded(px(R_MID)).label(label)
}

/// Dark theme tuned to the app: near-black surfaces, green accent, round corners.
/// Edited through `Theme::update` so the kit's component tokens pick the colors up too.
fn apply_theme(cx: &mut App) {
    Theme::change(ThemeMode::Dark, None, cx);
    Theme::update(cx, |theme| {
        theme.radius = px(R_MID);
        theme.radius_lg = px(R_BIG);
        theme.shadow = false;
        let t = &mut theme.colors;
        t.background = c(BG);
        t.foreground = c(TEXT);
        t.border = c(LINE);
        t.input = c(LINE);
        t.ring = c(GREEN);
        t.muted = c(RAISED);
        t.muted_foreground = c(MUTED);
        t.primary = c(GREEN);
        t.primary_hover = c(GREEN_HOVER);
        t.primary_active = c(GREEN_PRESSED);
        t.primary_foreground = c(ON_GREEN);
        t.secondary = c(RAISED);
        t.secondary_hover = c(HOVER);
        t.secondary_active = c(PRESSED);
        t.secondary_foreground = c(TEXT);
        t.accent = c(HOVER);
        t.accent_foreground = c(TEXT);
        t.popover = c(RAISED);
        t.popover_foreground = c(TEXT);
        t.list = c(RAISED);
        t.list_hover = c(HOVER);
        t.list_active = c(PRESSED);
        t.list_active_border = c(PRESSED);
        // Buttons: primary = green pill, secondary = dark control, danger = stop.
        t.button_primary = c(GREEN);
        t.button_primary_hover = c(GREEN_HOVER);
        t.button_primary_active = c(GREEN_PRESSED);
        t.button_primary_foreground = c(ON_GREEN);
        t.button_secondary = c(RAISED);
        t.button_secondary_hover = c(HOVER);
        t.button_secondary_active = c(PRESSED);
        t.button_secondary_foreground = c(TEXT);
        t.button_danger = c(RED_DEEP);
        t.button_danger_hover = c(RED_DEEP_HOVER);
        t.button_danger_active = c(RED_DEEP_HOVER);
        t.button_danger_foreground = c(RED);
        t.slider_bar = c(GREEN);
        t.slider_thumb = c(TEXT);
        t.switch = c(PRESSED);
        t.switch_thumb = c(TEXT);
        t.progress_bar = c(GREEN);
        t.tab_bar = c(CARD);
        t.tab_bar_segmented = c(RAISED);
        t.tab = c(RAISED);
        t.tab_active = c(BG);
        t.tab_foreground = c(MUTED);
        t.tab_active_foreground = c(TEXT);
        t.scrollbar_thumb = c(PRESSED);
        t.scrollbar_thumb_hover = c(MUTED);
    });
}

/// Dev only: `CLIPPER_DEMO=1 cargo run` drives a scripted session (record, pause, mute +
/// resume, stop) so the control bar flow can be checked without clicking.
#[cfg(debug_assertions)]
fn demo(clipper: gpui_kit::Entity<Clipper>, cx: &mut App) {
    cx.spawn(async move |cx| {
        let ex = cx.background_executor().clone();
        let wait = |ms| ex.timer(Duration::from_millis(ms));
        let _ = clipper.update(cx, |c, cx| c.start_recording(cx));
        while clipper.update(cx, |c, _| !matches!(c.stage, Stage::Recording)) {
            wait(100).await;
        }
        eprintln!("demo: recording");
        wait(3000).await;
        eprintln!("demo: pause");
        let _ = clipper.update(cx, |c, cx| c.toggle_pause(cx));
        wait(3000).await;
        eprintln!("demo: mute + resume");
        let _ = clipper.update(cx, |c, cx| {
            c.toggle_mute(cx);
            c.toggle_pause(cx);
        });
        wait(3000).await;
        eprintln!("demo: stop");
        let _ = clipper.update(cx, |c, cx| c.stop_recording(cx));
        if std::env::var_os("CLIPPER_EXPORT").is_some() {
            while clipper.update(cx, |c, _| !matches!(c.stage, Stage::Review)) {
                wait(100).await;
            }
            let _ = clipper.update(cx, |c, cx| {
                c.export_opts.gif = true;
                c.export_opts.webp = true;
                c.open_export(cx);
            });
        }
    })
    .detach();
}

fn main() {
    // ffmpeg: the copy bundled next to the executable (Clipper.app/Contents/MacOS) first, then
    // Homebrew's (an app opened from Finder doesn't get the shell's PATH).
    let path = std::env::var("PATH").unwrap_or_default();
    let exe_dir = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.to_string_lossy().to_string()));
    let bundled = exe_dir.filter(|d| std::path::Path::new(d).join("ffmpeg").exists());
    let extra = bundled
        .iter()
        .map(String::as_str)
        .chain(["/opt/homebrew/bin", "/usr/local/bin"])
        .filter(|d| !path.split(':').any(|p| p == *d))
        .collect::<Vec<_>>()
        .join(":");
    if !extra.is_empty() {
        // SAFETY: at the very start of main, before any other thread exists.
        unsafe { std::env::set_var("PATH", if path.is_empty() { extra } else { format!("{extra}:{path}") }) };
    }
    // Packaging: `Clipper --write-iconset DIR` writes the app icon at every size `iconutil` needs.
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--write-iconset") {
        let dir = PathBuf::from(args.get(2).expect("--write-iconset DIR"));
        std::fs::create_dir_all(&dir).expect("create iconset dir");
        for size in [16u32, 32, 128, 256, 512] {
            mascot::icon(size).save(dir.join(format!("icon_{size}x{size}.png"))).expect("write icon");
            mascot::icon(size * 2).save(dir.join(format!("icon_{size}x{size}@2x.png"))).expect("write icon");
        }
        return;
    }
    gpui_kit::application().with_assets(gpui_kit::assets::Assets).run(|cx: &mut App| {
        gpui_kit::init(cx);
        apply_theme(cx);

        let bounds = Bounds::centered(None, size(px(1360.), px(860.)), cx);
        let main = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Clipper".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(18.), px(19.))),
                }),
                is_resizable: true,
                // Room for the panel, a usable preview and the timeline.
                window_min_size: Some(size(px(1040.), px(640.))),
                ..Default::default()
            },
            cx,
            |window, cx| {
                let handle = window.window_handle();
                cx.new(|cx| Clipper::new(handle, window, cx))
            },
        )
        .expect("failed to open window");
        mascot::set_dock_icon(cx);
        let _ = main.1.update(cx, |c, cx| c.init_screenshots(cx));
        // Quit with the main window (not when the control bar closes).
        #[cfg(debug_assertions)]
        if std::env::var_os("CLIPPER_DEMO").is_some() {
            demo(main.1.clone(), cx);
        }
        // Dev only: open on a settings tab / background type for screenshots.
        #[cfg(debug_assertions)]
        {
            let tab = std::env::var("CLIPPER_PANEL").ok();
            let bg = std::env::var("CLIPPER_BG").ok();
            let _ = main.1.update(cx, |c, cx| {
                if let Some(p) = Panel::ALL.into_iter().find(|p| Some(p.label().to_lowercase()) == tab) {
                    c.panel = p;
                }
                // CLIPPER_BG=gradient|animated|image|color picks a background type.
                let pick = match bg.as_deref() {
                    Some("animated") => Some(backgrounds::Background::Animated(backgrounds::presets::animated()[0].anim.clone())),
                    Some("image") => Some(backgrounds::Background::Image(backgrounds::ImageBg::new(backgrounds::ImageSource::Builtin(0)))),
                    Some("color") => Some(backgrounds::Background::Solid { color: [0xf2, 0xf2, 0xf4], grain: 0.0 }),
                    Some("gradient") => Some(backgrounds::Background::default()),
                    _ => None,
                };
                if let Some(b) = pick {
                    c.style.bg = b.clone();
                    c.bg_picker.update(cx, |p, cx| p.set_background(b, cx));
                }
                // CLIPPER_RATIO=16:9, CLIPPER_PLACEMENT=circle|square|split (shows the
                // webcam spot without turning the camera on).
                if std::env::var("CLIPPER_RATIO").as_deref() == Ok("16:9") {
                    c.layout.set_ratio(layout::Ratio::Landscape);
                }
                let side = if c.layout.ratio == layout::Ratio::Portrait { layout::Side::Top } else { layout::Side::Left };
                let placement = match std::env::var("CLIPPER_PLACEMENT").as_deref() {
                    Ok("circle") => Some(layout::Placement::FALLBACK),
                    Ok("square") => Some(layout::Placement::Float(layout::Shape::Square, layout::Corner::TopRight)),
                    Ok("split") => Some(layout::Placement::Split(side)),
                    _ => None,
                };
                if std::env::var_os("CLIPPER_SELECT").is_some() {
                    c.open_selector(cx);
                }
                if std::env::var_os("CLIPPER_CAPTIONS").is_some() {
                    c.cap.enabled = true;
                }
                // CLIPPER_OPEN=<session dir>: opens a saved recording in the editor (its raw,
                // cursor track, webcam and captions).
                if let Some(dir) = std::env::var_os("CLIPPER_OPEN").map(PathBuf::from) {
                    let job = cx.background_executor().spawn(async move {
                        let raw = dir.join("raw.mp4");
                        let (width, height, duration) = recorder::probe(&raw)?;
                        let samples = std::fs::read_to_string(dir.join("cursor.csv"))
                            .unwrap_or_default()
                            .lines()
                            .skip(1)
                            .filter_map(|l| {
                                let v: Vec<&str> = l.split(',').collect();
                                Some(cursor::Sample { t: v.first()?.parse().ok()?, x: v.get(1)?.parse().ok()?, y: v.get(2)?.parse().ok()?, down: *v.get(3)? == "1" })
                            })
                            .collect();
                        let cam = dir.join("webcam.mp4");
                        let webcam = recorder::probe(&cam).ok().map(|(w, h, _)| webcam::Track { path: cam, width: w, height: h, spans: vec![(0.0, duration)] });
                        let rec = Recording { dir: dir.clone(), raw, width, height, duration, samples, webcam, px_per_pt: 2.0 };
                        let preview = export::build_preview(&rec)?;
                        let track = audio::decode(&rec).ok().flatten();
                        let caps = captions::Captions::load(&dir);
                        anyhow::Ok((rec, preview, track, caps))
                    });
                    cx.spawn(async move |this, cx| {
                        if let Ok((rec, preview, track, caps)) = job.await {
                            let _ = this.update(cx, |c, cx| {
                                c.show_recording(rec, preview, track, caps, cx);
                                c.playing = false;
                                // CLIPPER_EDIT_DEMO: a few cuts, to look at the timeline.
                                if std::env::var_os("CLIPPER_EDIT_DEMO").is_some() {
                                    for t in [2.0, 4.0, 7.0] {
                                        c.playhead = t;
                                        c.split_at_playhead();
                                    }
                                    c.edit_sel = Some(1);
                                    c.delete_segment();
                                    c.edit_sel = Some(2);
                                    c.seek_output(1.0);
                                }
                                cx.notify();
                            });
                        }
                    })
                    .detach();
                }
                // CLIPPER_FAKE_CAPTIONS=<session dir>: that session's captions, repeated into a
                // long list (nothing is saved: there's no recording).
                if let Some(dir) = std::env::var_os("CLIPPER_FAKE_CAPTIONS") {
                    if let Some(mut caps) = captions::Captions::load(std::path::Path::new(&dir)) {
                        let one = caps.lines.clone();
                        let span = one.last().map(|c| c.end).unwrap_or(1.0);
                        for k in 1..5 {
                            caps.lines.extend(one.iter().cloned().map(|mut c| {
                                c.start += span * k as f64;
                                c.end += span * k as f64;
                                c
                            }));
                        }
                        c.cap.enabled = true;
                        c.captions = Some(caps);
                    }
                }
                if let Some(m) = std::env::var("CLIPPER_CAP_MODEL").ok().and_then(|id| models::by_id(&id)) {
                    c.cap.model = m;
                }
                if let Some(v) = std::env::var("CLIPPER_SPLIT").ok().and_then(|v| v.parse().ok()) {
                    c.layout.split = v;
                }
                if let Some(p) = placement {
                    c.layout.placement = p;
                    c.webcam.enabled = true;
                    c.live = webcam::LivePreview::Failed; // placeholder only
                    // A red|blue test card in place of the camera.
                    if std::env::var_os("CLIPPER_FAKE_CAM").is_some() {
                        let card = image::RgbaImage::from_fn(320, 180, |x, y| {
                            let ring = ((x as i32 - 160).pow(2) + (y as i32 - 90).pow(2)) < 60 * 60;
                            match (ring, x < 160) {
                                (true, _) => image::Rgba([255, 255, 255, 255]),
                                (false, true) => image::Rgba([255, 0, 0, 255]), // BGRA: blue
                                (false, false) => image::Rgba([0, 0, 255, 255]), // BGRA: red
                            }
                        });
                        c.live_frame = Some(webcam::CamImage::from_bgra(card));
                    }
                }
            });
        }
        // Closing the main window keeps Clipper running in the menu bar (Quit is in its menu
        // and ⌘Q).
        let _ = main.0.update(cx, |_, window, cx| {
            window.on_window_should_close(cx, |window, _| {
                menubar::hide_window(window);
                menubar::set_dock_icon_visible(false);
                false
            });
        });
        // Dev: CLIPPER_NO_ACTIVATE opens without taking focus from other apps.
        if std::env::var_os("CLIPPER_NO_ACTIVATE").is_none() {
            cx.activate(true);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end: records 3s of the real screen and exports each background type at both
    /// output sizes. Run with `cargo test -- --ignored --nocapture` (needs Screen Recording permission).
    #[test]
    #[ignore]
    fn record_and_export() {
        let r = Recorder::start(false).unwrap();
        std::thread::sleep(Duration::from_secs(3));
        let rec = r.stop().unwrap();
        println!("raw {}x{} {:.2}s, {} cursor samples", rec.width, rec.height, rec.duration, rec.samples.len());
        assert!(!rec.samples.is_empty());
        let preview = export::build_preview(&rec).unwrap();
        println!("preview frames: {} @ {:.1}fps", preview.frames.len(), preview.fps);
        let out_dir = rec.dir.join("exports");
        // A gradient, a built-in image and a solid color, at both resolutions.
        let backgrounds = [
            backgrounds::Background::default(),
            backgrounds::Background::Image(backgrounds::ImageBg::new(backgrounds::ImageSource::Builtin(0))),
            backgrounds::Background::Solid { color: [20, 20, 20], grain: 0.0 },
        ];
        for (kind, res) in backgrounds.into_iter().zip([Res::P1080, Res::P720, Res::P1080]) {
            let style = Style { bg: kind.clone(), ..Style::default() };
            let layout = Layout { res, ..Layout::default() };
            let output = layout.canvas();
            let path = camera::compute_with(
                &rec.samples,
                rec.width,
                rec.height,
                rec.duration,
                layout.frame(&style, false).screen.aspect(),
                FollowConfig::default(),
            );
            let out = export::export(&rec, &path, &style, &layout, true, recorder::FPS, &out_dir, Arc::new(AtomicU32::new(0)), None, false).unwrap();
            assert!(out.starts_with(&out_dir));
            assert!(out.file_name().unwrap().to_string_lossy().starts_with("clip_"));
            let (w, h, d) = recorder::probe(&out).unwrap();
            assert_eq!((w, h), output);
            println!("{:?} -> {}x{} {:.2}s crop {}x{} {}", kind, w, h, d, path.crop_w, path.crop_h, out.display());
        }
    }
}
