//! The Library: the start screen. Lists the recordings, screenshots and exports already on
//! disk (the recordings folder, the save folder and the screenshots folder; no new storage)
//! as a masonry grid of cards that keep each item's true shape.
//!
//! Performance: the scan, thumbnails and video probes run in the background and are cached
//! on disk (`~/Library/Caches/Clipper/thumbs`); only the cards in view are laid out as
//! elements, and decoded thumbnails far from the view are released.

use crate::editor::icons;
use crate::screenshot::library as shots;
use crate::theme as tk;
use crate::ui;
use gpui_kit::component::Disableable;
use gpui_kit::component::button::{ButtonCustomVariant, ButtonVariants};
use gpui_kit::{AnimationExt, Focusable};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, ClickEvent, Context, Entity, FocusHandle, FontWeight, KeyDownEvent, ObjectFit, RenderImage, ScrollHandle, SharedString,
    Subscription, Task, WeakEntity, Window, div, img, point, px, rgb,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

// ---------- items ----------

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Kind {
    /// A recording session folder (raw.mp4 and its tracks); opens in the video editor.
    Video,
    /// An original screenshot or imported image; opens in the image editor.
    Screenshot,
    /// A finished file (MP4, GIF, WebP, edited PNG/JPEG); opens in the default app.
    Export,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    /// The session folder for videos, else the file.
    pub path: PathBuf,
    pub kind: Kind,
    pub name: String,
    pub modified: SystemTime,
    pub bytes: u64,
    /// Width / height, once known (cheap for images; from the thumbnail for videos).
    pub aspect: Option<f32>,
}

impl Item {
    /// The file a thumbnail is made from.
    fn source(&self) -> PathBuf {
        match self.kind {
            Kind::Video => self.path.join("raw.mp4"),
            _ => self.path.clone(),
        }
    }

    fn is_movie(&self) -> bool {
        self.kind == Kind::Video || matches!(ext(&self.path).as_str(), "mp4" | "mov" | "m4v")
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Filter {
    All,
    Videos,
    Screenshots,
    Exports,
}

impl Filter {
    pub const ALL: [Filter; 4] = [Filter::All, Filter::Videos, Filter::Screenshots, Filter::Exports];
    fn label(self) -> &'static str {
        match self {
            Filter::All => "All",
            Filter::Videos => "Videos",
            Filter::Screenshots => "Screenshots",
            Filter::Exports => "GIFs and exports",
        }
    }
    fn admits(self, k: Kind) -> bool {
        match self {
            Filter::All => true,
            Filter::Videos => k == Kind::Video,
            Filter::Screenshots => k == Kind::Screenshot,
            Filter::Exports => k == Kind::Export,
        }
    }
}

fn ext(p: &Path) -> String {
    p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

fn hidden(p: &Path) -> bool {
    p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.'))
}

/// `20261114-020712` → "Nov 14, 02:07".
fn friendly_stamp(stamp: &str) -> Option<String> {
    let (date, time) = stamp.split_once('-')?;
    if date.len() != 8 || time.len() < 4 || !date.chars().all(|c| c.is_ascii_digit()) || !time[..4].chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let m: usize = date[4..6].parse().ok()?;
    let d: u32 = date[6..8].parse().ok()?;
    Some(format!("{} {d}, {}:{}", MONTHS.get(m.checked_sub(1)?)?, &time[..2], &time[2..4]))
}

/// The name shown on a card: a readable date for the app's own timestamped names, else the
/// file or folder name.
pub fn display_name(path: &Path, kind: Kind) -> String {
    let stem = match kind {
        Kind::Video => path.file_name(),
        _ => path.file_stem(),
    }
    .and_then(|s| s.to_str())
    .unwrap_or("Untitled")
    .to_string();
    let pretty = |prefix: &str, rest: &str| friendly_stamp(rest).map(|d| format!("{prefix} {d}"));
    match kind {
        Kind::Video => friendly_stamp(&stem).map(|d| format!("Recording {d}")).unwrap_or(stem),
        _ => {
            for (p, label) in [("Screenshot_", "Screenshot"), ("Image_", "Image"), ("Merge_", "Merge")] {
                if let Some(rest) = stem.strip_prefix(p)
                    && let Some(n) = pretty(label, rest)
                {
                    return n;
                }
            }
            stem
        }
    }
}

/// Everything in the three folders, newest first. Runs off the UI thread.
pub fn scan(sessions: &Path, exports: &Path, screenshots: &Path) -> Vec<Item> {
    let mut out = Vec::new();
    let meta = |p: &Path| std::fs::metadata(p).ok();
    for e in std::fs::read_dir(sessions).into_iter().flatten().flatten() {
        let dir = e.path();
        let raw = dir.join("raw.mp4");
        if hidden(&dir) || !raw.exists() {
            continue;
        }
        let m = meta(&raw);
        out.push(Item {
            name: display_name(&dir, Kind::Video),
            modified: m.as_ref().and_then(|m| m.modified().ok()).unwrap_or(SystemTime::UNIX_EPOCH),
            bytes: m.map_or(0, |m| m.len()),
            path: dir,
            kind: Kind::Video,
            aspect: None,
        });
    }
    let mut seen = HashSet::new();
    for folder in [exports, screenshots] {
        for e in std::fs::read_dir(folder).into_iter().flatten().flatten() {
            let p = e.path();
            if hidden(&p) || !p.is_file() || !seen.insert(p.clone()) {
                continue;
            }
            let x = ext(&p);
            let image = matches!(x.as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif");
            let movie = matches!(x.as_str(), "mp4" | "mov" | "m4v");
            if !image && !movie {
                continue;
            }
            let kind = if image && shots::is_original(&p) { Kind::Screenshot } else { Kind::Export };
            let m = meta(&p);
            // Image headers are cheap; videos get their shape from the thumbnail.
            let aspect = if image { image::image_dimensions(&p).ok().filter(|d| d.1 > 0).map(|(w, h)| w as f32 / h as f32) } else { None };
            out.push(Item {
                name: display_name(&p, kind),
                modified: m.as_ref().and_then(|m| m.modified().ok()).unwrap_or(SystemTime::UNIX_EPOCH),
                bytes: m.map_or(0, |m| m.len()),
                path: p,
                kind,
                aspect,
            });
        }
    }
    out.sort_by(|a, b| b.modified.cmp(&a.modified).then(a.path.cmp(&b.path)));
    out
}

// ---------- masonry ----------

/// Card placement for a grid `width` wide: 2 to 6 columns, `gap` between, each card in the
/// shortest column at its own aspect (clamped so a sliver or a banner stays usable).
/// Returns (columns, rects as (x, y, w, h), total height).
pub fn masonry(aspects: &[f32], width: f32, gap: f32) -> (usize, Vec<(f32, f32, f32, f32)>, f32) {
    const MIN_CARD: f32 = 220.0;
    let cols = (((width + gap) / (MIN_CARD + gap)).floor() as usize).clamp(2, 6);
    let cw = ((width - gap * (cols as f32 - 1.0)) / cols as f32).max(1.0);
    let mut heights = vec![0.0f32; cols];
    let rects = aspects
        .iter()
        .map(|&a| {
            let a = if a.is_finite() && a > 0.0 { a.clamp(0.4, 3.0) } else { 16.0 / 10.0 };
            let (c, &y) = heights.iter().enumerate().min_by(|a, b| a.1.total_cmp(b.1)).unwrap();
            let h = (cw / a).round();
            heights[c] = y + h + gap;
            (c as f32 * (cw + gap), y, cw, h)
        })
        .collect();
    let total = heights.iter().fold(0.0f32, |m, &h| m.max(h)) - gap;
    (cols, rects, total.max(0.0))
}

// ---------- thumbnails ----------

const THUMB_W: u32 = 480;
/// Decoded thumbnails kept in memory at most (the rest reload from the disk cache).
const KEEP: usize = 240;
const IN_FLIGHT: usize = 4;

fn cache_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join("Library/Caches/Clipper/thumbs")
}

/// Cache key: the file, its size and time, and its edits (a screenshot's project file).
fn cache_key(item: &Item) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let src = item.source();
    src.hash(&mut h);
    let stamp = |p: &Path| std::fs::metadata(p).ok().map(|m| (m.len(), m.modified().ok()));
    stamp(&src).hash(&mut h);
    if item.kind == Kind::Screenshot {
        stamp(&crate::editor::doc::Document::sidecar_path(&src)).hash(&mut h);
    }
    format!("{:016x}", h.finish())
}

pub struct Thumb {
    pub image: Arc<RenderImage>,
    pub aspect: f32,
    /// Seconds, for videos.
    pub duration: Option<f64>,
}

/// Loads the cached thumbnail or makes one. Runs on a background thread.
fn make_thumb(item: &Item) -> Option<Thumb> {
    let dir = cache_dir();
    let key = cache_key(item);
    let (jpg, meta) = (dir.join(format!("{key}.jpg")), dir.join(format!("{key}.txt")));
    let duration_of = |s: &str| s.trim().parse::<f64>().ok().filter(|d| *d > 0.0);
    let rgba = match image::open(&jpg) {
        Ok(i) => i.to_rgba8(),
        Err(_) => {
            let src = item.source();
            let (pic, duration) = if item.is_movie() {
                let d = crate::recorder::probe(&src).ok().map(|(_, _, d)| d);
                let at = d.map_or(0.0, |d| (d * 0.1).min(1.0));
                let out = std::process::Command::new("ffmpeg")
                    .args(["-v", "error", "-ss", &format!("{at:.2}"), "-i"])
                    .arg(&src)
                    .args(["-frames:v", "1", "-vf", &format!("scale={THUMB_W}:-2"), "-f", "image2pipe", "-vcodec", "png", "-"])
                    .output()
                    .ok()?;
                (image::load_from_memory(&out.stdout).ok()?, d)
            } else if item.kind == Kind::Screenshot {
                (image::DynamicImage::ImageRgba8(crate::editor::baked(&src).ok()?), None)
            } else {
                (image::open(&src).ok()?, None)
            };
            let small = pic.thumbnail(THUMB_W, THUMB_W * 4).to_rgb8();
            let _ = std::fs::create_dir_all(&dir);
            let _ = small.save_with_format(&jpg, image::ImageFormat::Jpeg);
            let _ = std::fs::write(&meta, duration.map(|d| d.to_string()).unwrap_or_default());
            image::DynamicImage::ImageRgb8(small).to_rgba8()
        }
    };
    let duration = std::fs::read_to_string(&meta).ok().and_then(|s| duration_of(&s));
    let aspect = rgba.width() as f32 / rgba.height().max(1) as f32;
    Some(Thumb { image: crate::canvas_preview::to_render_image(rgba), aspect, duration })
}

enum Slot {
    Loading,
    Ready(Thumb),
    Failed,
}

// ---------- the view ----------

/// A delete waiting out its undo window.
struct Deleted {
    item: Item,
    undo: Undo,
    _timer: Task<()>,
}

enum Undo {
    /// A screenshot and its project file, moved aside.
    Shot(shots::Pending),
    /// A file or session folder moved into the folder's hidden pending area.
    Moved { from: PathBuf, to: PathBuf },
}

/// Moves `path` (file or folder) into `<parent>/.pending-delete` until undone or finished.
fn park(path: &Path) -> anyhow::Result<Undo> {
    let pending = path.parent().unwrap_or(Path::new(".")).join(".pending-delete");
    std::fs::create_dir_all(&pending)?;
    let to = pending.join(path.file_name().unwrap_or_default());
    std::fs::rename(path, &to)?;
    Ok(Undo::Moved { from: path.to_path_buf(), to })
}

fn restore(u: &Undo) -> anyhow::Result<()> {
    match u {
        Undo::Shot(p) => shots::undo(p),
        Undo::Moved { from, to } => Ok(std::fs::rename(to, from)?),
    }
}

fn finish(u: &Undo) {
    match u {
        Undo::Shot(p) => shots::finish(p),
        Undo::Moved { to, .. } => {
            if trash::delete(to).is_err() {
                let _ = if to.is_dir() { std::fs::remove_dir_all(to) } else { std::fs::remove_file(to) };
            }
        }
    }
}

/// Deletes left waiting when the app quit go to the Trash now.
pub fn flush_pending(folders: &[&Path]) {
    for f in folders {
        for e in std::fs::read_dir(f.join(".pending-delete")).into_iter().flatten().flatten() {
            let _ = trash::delete(e.path());
        }
    }
}

enum Toast {
    Deleted,
    Note(String),
}

/// What's open over the grid.
#[derive(Clone, PartialEq)]
enum Overlay {
    None,
    Create,
    /// The "…" menu of a card.
    Menu(PathBuf),
    Rename(PathBuf),
}

pub struct Library {
    clipper: WeakEntity<crate::Clipper>,
    items: Vec<Item>,
    scanned: bool,
    filter: Filter,
    query: Entity<InputState>,
    rename: Entity<InputState>,
    thumbs: HashMap<PathBuf, Slot>,
    in_flight: usize,
    /// GPU images to free on the next render.
    stale: Vec<Arc<RenderImage>>,
    scroll: ScrollHandle,
    overlay: Overlay,
    /// Keyboard selection (index into the filtered list).
    cursor: Option<usize>,
    deleted: Option<Deleted>,
    toast: Option<(Toast, u64)>,
    toast_seq: u64,
    focus: FocusHandle,
    /// Recordings, exports and screenshots folders (set by the main window).
    folders: (PathBuf, PathBuf, PathBuf),
    /// The screenshot shortcut, shown in the Create menu.
    shot_key: String,
    _scan: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

impl Library {
    pub fn new(clipper: WeakEntity<crate::Clipper>, exports: PathBuf, screenshots: PathBuf, shot_key: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search by name"));
        let rename = cx.new(|cx| InputState::new(window, cx).placeholder("Name"));
        let subs = vec![
            cx.subscribe(&query, |this: &mut Self, _, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    this.cursor = None;
                    this.scroll.set_offset(point(px(0.), px(0.)));
                    cx.notify();
                }
            }),
            cx.subscribe_in(&rename, window, |this: &mut Self, _, ev: &InputEvent, window, cx| {
                if matches!(ev, InputEvent::PressEnter { .. }) {
                    this.commit_rename(window, cx);
                }
            }),
        ];
        Self {
            clipper,
            items: Vec::new(),
            scanned: false,
            filter: Filter::All,
            query,
            rename,
            thumbs: HashMap::new(),
            in_flight: 0,
            stale: Vec::new(),
            scroll: ScrollHandle::new(),
            overlay: Overlay::None,
            cursor: None,
            deleted: None,
            toast: None,
            toast_seq: 0,
            focus: cx.focus_handle(),
            folders: (crate::recorder::sessions_root().join(".sessions"), exports, screenshots),
            shot_key,
            _scan: None,
            _subs: subs,
        }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    /// The save folder or screenshots folder changed.
    pub fn set_folders(&mut self, exports: PathBuf, screenshots: PathBuf) {
        self.folders.1 = exports;
        self.folders.2 = screenshots;
    }

    /// Re-reads the folders in the background (on show, and after anything was saved).
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let (s, e, p) = self.folders.clone();
        let job = cx.background_executor().spawn(async move { scan(&s, &e, &p) });
        self._scan = Some(cx.spawn(async move |this, cx| {
            let items = job.await;
            let _ = this.update(cx, |l, cx| {
                // Thumbnails whose file changed or went away are dropped.
                let keep: HashSet<&PathBuf> = items.iter().map(|i| &i.path).collect();
                let gone: Vec<PathBuf> = l.thumbs.keys().filter(|k| !keep.contains(k)).cloned().collect();
                for k in gone {
                    if let Some(Slot::Ready(t)) = l.thumbs.remove(&k) {
                        l.stale.push(t.image);
                    }
                }
                for it in &items {
                    if let Some(old) = l.items.iter().find(|o| o.path == it.path)
                        && (old.modified != it.modified || old.bytes != it.bytes)
                        && let Some(Slot::Ready(t)) = l.thumbs.remove(&it.path)
                    {
                        l.stale.push(t.image);
                    }
                }
                l.items = items;
                l.scanned = true;
                cx.notify();
            });
        }));
    }

    fn visible_items(&self, cx: &App) -> Vec<Item> {
        let q = self.query.read(cx).value().to_lowercase();
        let parked = self.deleted.as_ref().map(|d| &d.item.path);
        self.items
            .iter()
            .filter(|i| self.filter.admits(i.kind) && Some(&i.path) != parked)
            .filter(|i| q.is_empty() || i.name.to_lowercase().contains(q.trim()))
            .cloned()
            .collect()
    }

    fn aspect(&self, it: &Item) -> f32 {
        match self.thumbs.get(&it.path) {
            Some(Slot::Ready(t)) => t.aspect,
            _ => it.aspect.unwrap_or(16.0 / 10.0),
        }
    }

    fn request_thumb(&mut self, it: &Item, cx: &mut Context<Self>) {
        if self.in_flight >= IN_FLIGHT || self.thumbs.contains_key(&it.path) {
            return;
        }
        self.in_flight += 1;
        self.thumbs.insert(it.path.clone(), Slot::Loading);
        let item = it.clone();
        let job = cx.background_executor().spawn(async move { make_thumb(&item) });
        let path = it.path.clone();
        cx.spawn(async move |this, cx| {
            let t = job.await;
            let _ = this.update(cx, |l, cx| {
                l.in_flight -= 1;
                l.thumbs.insert(path, t.map_or(Slot::Failed, Slot::Ready));
                cx.notify();
            });
        })
        .detach();
    }

    /// Keeps at most `KEEP` decoded thumbnails, releasing those farthest from the view.
    fn evict(&mut self, order: &[PathBuf], first_visible: usize) {
        let ready: Vec<(usize, PathBuf)> = order.iter().enumerate().filter(|(_, p)| matches!(self.thumbs.get(*p), Some(Slot::Ready(_)))).map(|(i, p)| (i, p.clone())).collect();
        if ready.len() <= KEEP {
            return;
        }
        let mut by_far = ready;
        by_far.sort_by_key(|(i, _)| std::cmp::Reverse(i.abs_diff(first_visible)));
        let n = by_far.len() - KEEP;
        for (_, p) in by_far.into_iter().take(n) {
            if let Some(Slot::Ready(t)) = self.thumbs.remove(&p) {
                self.stale.push(t.image);
            }
        }
    }

    // ---------- actions ----------

    fn with_clipper(&self, cx: &mut App, f: impl FnOnce(&mut crate::Clipper, &mut Context<crate::Clipper>) + 'static) {
        if let Some(c) = self.clipper.upgrade() {
            cx.defer(move |cx| c.update(cx, |c, cx| f(c, cx)));
        }
    }

    fn open(&mut self, it: &Item, export: bool, cx: &mut Context<Self>) {
        self.overlay = Overlay::None;
        match it.kind {
            Kind::Video => {
                let dir = it.path.clone();
                self.with_clipper(cx, move |c, cx| c.open_session(dir, export, cx));
            }
            Kind::Screenshot => {
                let p = it.path.clone();
                cx.defer(move |cx| crate::editor::view::open_with(p, export, cx));
            }
            Kind::Export => {
                let _ = std::process::Command::new("open").arg(&it.path).spawn();
            }
        }
    }

    fn copy(&mut self, it: &Item, cx: &mut Context<Self>) {
        self.overlay = Overlay::None;
        if it.kind == Kind::Screenshot {
            let p = it.path.clone();
            let job = cx.background_executor().spawn(async move { crate::editor::baked(&p) });
            cx.spawn(async move |this, cx| {
                let res = job.await;
                let _ = this.update(cx, |l, cx| {
                    let ok = res.and_then(|img| crate::editor::copy_image(&img, cx)).is_ok();
                    l.note(if ok { "Copied" } else { "Couldn't copy the image. Open it and try Copy there." }, cx);
                });
            })
            .detach();
        } else {
            // A file reference, like Finder's Copy, so it pastes into chats and folders.
            let file = it.source();
            let job = cx.background_executor().spawn(async move {
                std::process::Command::new("osascript")
                    .arg("-e")
                    .arg(format!("set the clipboard to (POSIX file \"{}\")", file.display().to_string().replace('"', "\\\"")))
                    .status()
                    .is_ok_and(|s| s.success())
            });
            cx.spawn(async move |this, cx| {
                let ok = job.await;
                let _ = this.update(cx, |l, cx| l.note(if ok { "Copied" } else { "Couldn't copy the file. Use Show in folder and copy it there." }, cx));
            })
            .detach();
        }
    }

    fn note(&mut self, text: &str, cx: &mut Context<Self>) {
        self.toast_seq += 1;
        let seq = self.toast_seq;
        self.toast = Some((Toast::Note(text.to_string()), seq));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(3)).await;
            let _ = this.update(cx, |l, cx| {
                if l.toast.as_ref().is_some_and(|t| t.1 == seq) {
                    l.toast = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn delete(&mut self, it: &Item, cx: &mut Context<Self>) {
        self.overlay = Overlay::None;
        // A previous delete finishes now (its undo window is replaced by this one).
        if let Some(d) = self.deleted.take() {
            finish(&d.undo);
        }
        let undo = match it.kind {
            Kind::Screenshot => shots::delete(&it.path).map(Undo::Shot),
            _ => park(&it.path),
        };
        let undo = match undo {
            Ok(u) => u,
            Err(e) => {
                self.note(&format!("Couldn't delete {}: {e}. Check the folder's permissions.", it.name), cx);
                return;
            }
        };
        self.toast_seq += 1;
        let seq = self.toast_seq;
        self.toast = Some((Toast::Deleted, seq));
        let timer = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(5)).await;
            let _ = this.update(cx, |l, cx| {
                if let Some(d) = l.deleted.take() {
                    finish(&d.undo);
                }
                if l.toast.as_ref().is_some_and(|t| t.1 == seq) {
                    l.toast = None;
                }
                l.refresh(cx);
            });
        });
        self.deleted = Some(Deleted { item: it.clone(), undo, _timer: timer });
        cx.notify();
    }

    fn undo_delete(&mut self, cx: &mut Context<Self>) {
        if let Some(d) = self.deleted.take() {
            if let Err(e) = restore(&d.undo) {
                self.note(&format!("Couldn't restore {}: {e}. It's in the folder's .pending-delete.", d.item.name), cx);
                return;
            }
        }
        self.toast = None;
        self.refresh(cx);
    }

    fn start_rename(&mut self, it: &Item, window: &mut Window, cx: &mut Context<Self>) {
        let stem = match it.kind {
            Kind::Video => it.path.file_name(),
            _ => it.path.file_stem(),
        }
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
        self.rename.update(cx, |s, cx| s.set_value(stem, window, cx));
        let fh = self.rename.read(cx).focus_handle(cx);
        window.focus(&fh, cx);
        self.overlay = Overlay::Rename(it.path.clone());
        cx.notify();
    }

    fn commit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Overlay::Rename(path) = self.overlay.clone() else { return };
        let name = self.rename.read(cx).value().trim().to_string();
        self.overlay = Overlay::None;
        window.focus(&self.focus, cx);
        if name.is_empty() {
            cx.notify();
            return;
        }
        match rename_item(&path, &name) {
            Ok(_) => self.refresh(cx),
            Err(e) => self.note(&e.to_string(), cx),
        }
        cx.notify();
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = ev.keystroke.key.as_str();
        let m = ev.keystroke.modifiers;
        if key == "escape" {
            if self.overlay != Overlay::None {
                self.overlay = Overlay::None;
                window.focus(&self.focus, cx);
                cx.notify();
            }
            return;
        }
        if self.overlay != Overlay::None {
            return;
        }
        let list = self.visible_items(cx);
        if list.is_empty() {
            return;
        }
        let width = f32::from(self.scroll.bounds().size.width).max(400.0);
        let aspects: Vec<f32> = list.iter().map(|i| self.aspect(i)).collect();
        let (_, rects, _) = masonry(&aspects, width, tk::S4);
        let cur = self.cursor.unwrap_or(0).min(list.len() - 1);
        // Arrow keys move to the nearest card in that direction.
        let pick = |dx: f32, dy: f32| -> Option<usize> {
            let (x, y, w, h) = rects[cur];
            let (cx0, cy0) = (x + w / 2.0, y + h / 2.0);
            rects
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != cur)
                .filter_map(|(i, &(x2, y2, w2, h2))| {
                    let (vx, vy) = (x2 + w2 / 2.0 - cx0, y2 + h2 / 2.0 - cy0);
                    let along = vx * dx + vy * dy;
                    (along > 1.0).then(|| (i, along + (vx * dy - vy * dx).abs() * 2.0))
                })
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(i, _)| i)
        };
        let next = match key {
            "left" => pick(-1.0, 0.0),
            "right" => pick(1.0, 0.0),
            "up" => pick(0.0, -1.0),
            "down" => pick(0.0, 1.0),
            "enter" if self.cursor.is_some() => {
                self.open(&list[cur], false, cx);
                return;
            }
            "backspace" | "delete" if m.platform && self.cursor.is_some() => {
                self.delete(&list[cur], cx);
                return;
            }
            "c" if m.platform && self.cursor.is_some() => {
                self.copy(&list[cur], cx);
                return;
            }
            "e" if m.platform && self.cursor.is_some() => {
                self.open(&list[cur], true, cx);
                return;
            }
            _ => return,
        };
        let i = if self.cursor.is_none() { Some(0) } else { next };
        if let Some(i) = i {
            self.cursor = Some(i);
            // Keep it in view.
            let (_, y, _, h) = rects[i];
            let view = f32::from(self.scroll.bounds().size.height);
            let off = -f32::from(self.scroll.offset().y);
            if y < off {
                self.scroll.set_offset(point(px(0.), px(-(y - tk::S4))));
            } else if y + h > off + view {
                self.scroll.set_offset(point(px(0.), px(-(y + h - view + tk::S4))));
            }
            cx.stop_propagation();
            cx.notify();
        }
    }

    // ---------- render pieces ----------

    fn top_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let now = crate::theme::preference();
        let (icon, next) = match now {
            crate::theme::Appearance::System => (icons::sun_moon(), crate::theme::Appearance::Light),
            crate::theme::Appearance::Light => (icons::sun(), crate::theme::Appearance::Dark),
            crate::theme::Appearance::Dark => (icons::moon(), crate::theme::Appearance::System),
        };
        let create_open = self.overlay == Overlay::Create;
        div()
            .h(px(tk::HEADER_H + 8.0))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(tk::S4))
            .pl(px(tk::TRAFFIC_INSET))
            .pr(px(tk::S6))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(tk::S2))
                    .child(img(crate::mascot::image(56)).h(px(26.)).w(px(26. * crate::mascot::aspect())))
                    .child(div().text_size(px(tk::T18)).font_weight(FontWeight::BOLD).child("Clipper")),
            )
            .child(
                div().flex_1().flex().justify_center().child(
                    div().w_full().max_w(px(520.)).child(
                        Input::new(&self.query)
                            .prefix(icons::search().size(px(18.)).text_color(rgb(tk::ink_soft())))
                            .cleanable(true)
                            .rounded_full()
                            .h(px(40.))
                            .bg(rgb(tk::mist()))
                            .border_0(),
                    ),
                ),
            )
            .child(
                ui::icon_button("lib-theme", icon, format!("Appearance: {}", now.label())).on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                    crate::theme::set_preference(next, cx);
                    cx.notify();
                })),
            )
            .child(
                div().relative().child(
                    gpui_kit::component::button::Button::new("create")
                        .cursor_pointer()
                        .danger()
                        .rounded_full()
                        .size(px(44.))
                        .icon(icons::plus().size(px(22.)))
                        .tooltip("Create")
                        .on_click(cx.listener(move |l, _: &ClickEvent, _, cx| {
                            l.overlay = if create_open { Overlay::None } else { Overlay::Create };
                            cx.notify();
                        })),
                ),
            )
    }

    fn filters(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let items = Filter::ALL
            .into_iter()
            .map(|f| {
                let on: ui::OnClick = Box::new(cx.listener(move |l, _: &ClickEvent, _, cx| {
                    l.filter = f;
                    l.cursor = None;
                    l.scroll.set_offset(point(px(0.), px(0.)));
                    cx.notify();
                }));
                (SharedString::from(f.label()), self.filter == f, on)
            })
            .collect();
        div().px(px(tk::S6)).pb(px(tk::S4)).child(ui::pill_tabs("lib-filter", items))
    }

    fn create_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let shot_key = self.shot_key.clone();
        div()
            .absolute()
            .top(px(tk::HEADER_H + 4.0))
            .right(px(tk::S6))
            .occlude()
            .child(
                ui::menu(232.)
                    .child(ui::menu_item("c-record", Some(icons::record()), "Record screen").on_click(cx.listener(|l, _: &ClickEvent, _, cx| {
                        l.overlay = Overlay::None;
                        l.with_clipper(cx, |c, cx| c.show_studio(cx));
                    })))
                    .child(
                        ui::menu_item("c-shot", Some(icons::camera()), "Take screenshot")
                            .child(div().flex_1())
                            .child(div().text_size(px(tk::T12)).text_color(rgb(tk::ink_soft())).child(shot_key))
                            .on_click(cx.listener(|l, _: &ClickEvent, _, cx| {
                                l.overlay = Overlay::None;
                                l.with_clipper(cx, |c, cx| c.take_screenshot(true, cx));
                            })),
                    )
                    .child(ui::menu_item("c-import", Some(icons::import()), "Import").on_click(cx.listener(|l, _: &ClickEvent, _, cx| {
                        l.overlay = Overlay::None;
                        l.with_clipper(cx, |c, cx| c.choose_images(cx));
                    })))
                    .with_animation("create-menu", ui::enter(), |el, t| el.opacity(t)),
            )
    }

    fn empty(&self, searching: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let col = div().size_full().flex().flex_col().items_center().justify_center().gap(px(tk::S3));
        if searching {
            return col
                .child(div().text_size(px(tk::T18)).font_weight(FontWeight::BOLD).child("No matches"))
                .child(ui::hint("Try another name, or pick All above."));
        }
        col.child(img(crate::mascot::image(160)).h(px(72.)).w(px(72. * crate::mascot::aspect())))
            .child(div().text_size(px(tk::T24)).font_weight(FontWeight::BOLD).child("Nothing here yet"))
            .child(ui::hint("Record your screen or take a screenshot to get started."))
            .child(
                div()
                    .pt(px(tk::S2))
                    .flex()
                    .gap(px(tk::S2))
                    .child(ui::coral_button("empty-record").icon(icons::record()).label("Record screen").on_click(cx.listener(|l, _: &ClickEvent, _, cx| {
                        l.with_clipper(cx, |c, cx| c.show_studio(cx));
                    })))
                    .child(ui::secondary_button("empty-shot").icon(icons::camera()).label("Take screenshot").on_click(cx.listener(|l, _: &ClickEvent, _, cx| {
                        l.with_clipper(cx, |c, cx| c.take_screenshot(true, cx));
                    }))),
            )
    }

    fn card(&self, i: usize, it: &Item, (x, y, w, h): (f32, f32, f32, f32), cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let thumb = match self.thumbs.get(&it.path) {
            Some(Slot::Ready(t)) => Some(t),
            _ => None,
        };
        let meta = match (it.kind, thumb.and_then(|t| t.duration)) {
            (_, Some(d)) => {
                let s = d.round() as u64;
                format!("{}:{:02}", s / 60, s % 60)
            }
            _ => human_size(it.bytes),
        };
        let selected = self.cursor == Some(i);
        let menu_open = self.overlay == Overlay::Menu(it.path.clone());
        let group: SharedString = format!("card-{i}").into();
        let id = gpui_kit::ElementId::Name(format!("card:{}", it.path.display()).into());
        let action = |key: &'static str, icon: gpui_kit::component::Icon, tip: &'static str| {
            gpui_kit::component::button::Button::new(gpui_kit::ElementId::Name(format!("{key}:{i}").into()))
                .cursor_pointer()
                .rounded_full()
                .size(px(34.))
                .icon(icon.size(px(18.)))
                .tooltip(tip)
                .custom(
                    ButtonCustomVariant::new(cx)
                        .color(tk::alpha(tk::WHITE, 0.92))
                        .foreground(gpui_kit::rgb(tk::BLACK).into())
                        .hover(tk::alpha(tk::WHITE, 1.0))
                        .active(tk::alpha(tk::WHITE, 0.8)),
                )
        };
        let (a, b, c, d) = (it.clone(), it.clone(), it.clone(), it.clone());
        let edit_tip = match it.kind {
            Kind::Export => "Open",
            _ => "Edit",
        };
        let overlay = div()
            .absolute()
            .inset_0()
            .flex()
            .flex_col()
            .justify_end()
            .opacity(if selected || menu_open { 1.0 } else { 0.0 })
            .group_hover(group.clone(), |s| s.opacity(1.0))
            .child(
                div()
                    .absolute()
                    .top(px(tk::S3))
                    .right(px(tk::S3))
                    .flex()
                    .gap(px(tk::S1 + 2.0))
                    .child(action("edit", if it.kind == Kind::Export { icons::image() } else { icons::edit() }, edit_tip).on_click(cx.listener(move |l, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        l.open(&a, false, cx);
                    })))
                    .child(action("copy", icons::copy(), "Copy  ⌘C").on_click(cx.listener(move |l, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        l.copy(&b, cx);
                    })))
                    .child(
                        action("export", icons::export(), "Export  ⌘E").disabled(it.kind == Kind::Export).on_click(cx.listener(move |l, _: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            l.open(&c, true, cx);
                        })),
                    )
                    .child(action("more", icons::more(), "More").on_click(cx.listener(move |l, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        l.overlay = if l.overlay == Overlay::Menu(d.path.clone()) { Overlay::None } else { Overlay::Menu(d.path.clone()) };
                        cx.notify();
                    }))),
            )
            .child(
                div()
                    .h(px(76.0f32.min(h)))
                    .px(px(tk::S4))
                    .pb(px(tk::S3))
                    .flex()
                    .flex_col()
                    .justify_end()
                    // GPUI clips to a square box: the scrim rounds its own corners.
                    .rounded_b(px(tk::R_STAGE))
                    .bg(gpui_kit::linear_gradient(180., gpui_kit::linear_color_stop(tk::scrim(0.0), 0.), gpui_kit::linear_color_stop(tk::scrim(0.62), 1.)))
                    .text_color(rgb(tk::on_scrim()))
                    .child(div().text_size(px(tk::T13)).font_weight(FontWeight::SEMIBOLD).truncate().child(it.name.clone()))
                    .child(div().text_size(px(tk::T12)).font_features(ui::tnum()).opacity(0.85).child(meta)),
            );
        let open_item = it.clone();
        div()
            .id(id)
            .group(group)
            .absolute()
            .left(px(x))
            .top(px(y))
            .w(px(w))
            .h(px(h))
            .tab_index(0)
            .rounded(px(tk::R_STAGE))
            .overflow_hidden()
            .bg(rgb(tk::mist()))
            .cursor_pointer()
            .hover(move |s| s.top(px(y - tk::LIFT)))
            .focus_visible(|s| s.shadow(ui::focus_ring()))
            .when(selected, |d| d.shadow(ui::focus_ring()))
            .children(thumb.map(|t| img(t.image.clone()).absolute().inset_0().size_full().rounded(px(tk::R_STAGE)).object_fit(ObjectFit::Cover)))
            .when(thumb.is_none() && matches!(self.thumbs.get(&it.path), Some(Slot::Failed)), |d| {
                d.flex().items_center().justify_center().child(if it.is_movie() { icons::film() } else { icons::image() }.size(px(28.)).text_color(rgb(tk::ink_soft())))
            })
            .child(overlay)
            .on_click(cx.listener(move |l, _: &ClickEvent, _, cx| {
                if l.overlay != Overlay::None {
                    l.overlay = Overlay::None;
                    cx.notify();
                    return;
                }
                l.open(&open_item, false, cx);
            }))
            .into_any_element()
    }

    fn card_menu(&self, it: &Item, (x, y, w, _h): (f32, f32, f32, f32), cx: &mut Context<Self>) -> impl IntoElement {
        let (a, b, c) = (it.clone(), it.clone(), it.clone());
        div()
            .absolute()
            .left(px(x + w - 200.0 - tk::S3))
            .top(px(y + tk::S3 + 40.0))
            .occlude()
            .child(
                ui::menu(200.)
                    .child(ui::menu_item("m-rename", Some(icons::edit()), "Rename").on_click(cx.listener(move |l, _: &ClickEvent, window, cx| l.start_rename(&a, window, cx))))
                    .child(ui::menu_item("m-reveal", Some(icons::folder()), "Show in folder").on_click(cx.listener(move |l, _: &ClickEvent, _, cx| {
                        l.overlay = Overlay::None;
                        cx.reveal_path(&b.source());
                        cx.notify();
                    })))
                    .child(ui::menu_item("m-delete", Some(icons::trash()), "Delete").on_click(cx.listener(move |l, _: &ClickEvent, _, cx| l.delete(&c, cx))))
                    .with_animation("card-menu", ui::enter(), |el, t| el.opacity(t)),
            )
    }

    fn rename_dialog(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .child(div().id("rename-scrim").absolute().inset_0().bg(tk::scrim(0.32)).on_click(cx.listener(|l, _: &ClickEvent, window, cx| {
                l.overlay = Overlay::None;
                window.focus(&l.focus, cx);
                cx.notify();
            })))
            .child(
                ui::popover()
                    .occlude()
                    .w(px(400.))
                    .p(px(tk::S6))
                    .flex()
                    .flex_col()
                    .gap(px(tk::S3))
                    .child(div().text_size(px(tk::T18)).font_weight(FontWeight::BOLD).child("Rename"))
                    .child(Input::new(&self.rename))
                    .child(
                        div()
                            .pt(px(tk::S2))
                            .flex()
                            .justify_end()
                            .gap(px(tk::S2))
                            .child(ui::ghost_button("rename-cancel").label("Cancel").on_click(cx.listener(|l, _: &ClickEvent, window, cx| {
                                l.overlay = Overlay::None;
                                window.focus(&l.focus, cx);
                                cx.notify();
                            })))
                            .child(ui::primary_button("rename-ok").label("Rename").on_click(cx.listener(|l, _: &ClickEvent, window, cx| l.commit_rename(window, cx)))),
                    ),
            )
    }
}

/// Renames a library item on disk: a session folder, or a file and its project file.
pub fn rename_item(path: &Path, name: &str) -> anyhow::Result<PathBuf> {
    if name.contains('/') || name.starts_with('.') {
        anyhow::bail!("Names can't start with a dot or contain a slash. Pick another name.");
    }
    let is_dir = path.is_dir();
    let new = match (is_dir, path.extension()) {
        (false, Some(e)) => path.with_file_name(format!("{name}.{}", e.to_string_lossy())),
        _ => path.with_file_name(name),
    };
    if new == path {
        return Ok(new);
    }
    if new.exists() {
        anyhow::bail!("Something called \"{name}\" is already there. Pick another name.");
    }
    std::fs::rename(path, &new)?;
    if !is_dir {
        let old_side = crate::editor::doc::Document::sidecar_path(path);
        if old_side.exists() {
            let _ = std::fs::rename(&old_side, crate::editor::doc::Document::sidecar_path(&new));
        }
    }
    Ok(new)
}

fn human_size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else {
        format!("{:.0} KB", (b / 1e3).max(1.0))
    }
}

impl Render for Library {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for img in self.stale.drain(..) {
            let _ = window.drop_image(img);
        }
        if !self.scanned && self._scan.is_none() {
            self.refresh(cx);
        }
        let list = self.visible_items(cx);
        let bounds = self.scroll.bounds();
        let vp = window.viewport_size();
        let width = {
            let w = f32::from(bounds.size.width);
            if w > 10.0 { w - tk::S6 * 2.0 } else { f32::from(vp.width) - tk::S6 * 2.0 }
        };
        let view_h = {
            let h = f32::from(bounds.size.height);
            if h > 10.0 { h } else { f32::from(vp.height) }
        };
        let aspects: Vec<f32> = list.iter().map(|i| self.aspect(i)).collect();
        let (_, rects, total) = masonry(&aspects, width, tk::S4);
        let top = -f32::from(self.scroll.offset().y);
        let (lo, hi) = (top - view_h * 0.5, top + view_h * 1.5);
        let shown: Vec<usize> = (0..list.len()).filter(|&i| rects[i].1 + rects[i].3 >= lo && rects[i].1 <= hi).collect();
        // Thumbnails: what's on screen first, then a little ahead.
        let on_screen: Vec<usize> = shown.iter().copied().filter(|&i| rects[i].1 + rects[i].3 >= top && rects[i].1 <= top + view_h).collect();
        for &i in on_screen.iter().chain(shown.iter()) {
            let it = list[i].clone();
            self.request_thumb(&it, cx);
        }
        let order: Vec<PathBuf> = list.iter().map(|i| i.path.clone()).collect();
        self.evict(&order, shown.first().copied().unwrap_or(0));

        let searching = !self.query.read(cx).value().is_empty() || self.filter != Filter::All;
        let mut grid = div().relative().w(px(width)).h(px(total + tk::S6));
        for &i in &shown {
            grid = grid.child(self.card(i, &list[i], rects[i], cx));
        }
        let menu = match &self.overlay {
            Overlay::Menu(p) => list.iter().position(|i| &i.path == p).map(|i| self.card_menu(&list[i], rects[i], cx).into_any_element()),
            _ => None,
        };
        let body = if list.is_empty() && self.scanned {
            div().flex_1().min_h_0().child(self.empty(searching, cx)).into_any_element()
        } else {
            div()
                .id("library-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .on_scroll(cx.listener(|_, _, _, cx| cx.notify()))
                .px(px(tk::S6))
                .child(grid.children(menu))
                .into_any_element()
        };
        let toast = self.toast.as_ref().map(|(t, seq)| {
            let pill = match t {
                Toast::Deleted => ui::toast_pill().child("Deleted").child(ui::ghost_button("undo-delete").label("Undo").on_click(cx.listener(|l, _: &ClickEvent, _, cx| l.undo_delete(cx)))),
                Toast::Note(s) => ui::toast_pill().pr(px(tk::S4)).child(s.clone()),
            };
            ui::toast_layer(gpui_kit::ElementId::Name(format!("lib-toast-{seq}").into()), pill)
        });
        div()
            .id("library")
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(rgb(tk::paper()))
            .text_color(rgb(tk::text()))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .child(self.top_bar(cx))
            .child(self.filters(cx))
            .child(body)
            .when(self.overlay == Overlay::Create, |d| {
                d.child(div().id("create-dismiss").absolute().inset_0().on_click(cx.listener(|l, _: &ClickEvent, _, cx| {
                    l.overlay = Overlay::None;
                    cx.notify();
                })))
                .child(self.create_menu(cx))
            })
            .when(matches!(self.overlay, Overlay::Rename(_)), |d| d.child(self.rename_dialog(cx)))
            .children(toast)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masonry_columns_follow_width_and_keep_aspect() {
        let a = vec![16.0 / 9.0, 9.0 / 16.0, 1.0, 16.0 / 9.0];
        let (cols, rects, total) = masonry(&a, 1000.0, 16.0);
        assert_eq!(cols, 4);
        for (r, &asp) in rects.iter().zip(&a) {
            assert!((r.2 / r.3 - asp).abs() < 0.02, "{r:?} vs {asp}");
        }
        assert!(total > 0.0);
        assert_eq!(masonry(&a, 300.0, 16.0).0, 2, "never fewer than 2");
        assert_eq!(masonry(&a, 5000.0, 16.0).0, 6, "never more than 6");
        // Cards go to the shortest column, so columns stay even.
        let many = vec![1.0; 12];
        let (_, r, _) = masonry(&many, 1000.0, 16.0);
        assert_eq!(r[4].1, r[0].1 + r[0].3 + 16.0);
    }

    #[test]
    fn masonry_lays_out_a_thousand_items_fast() {
        let a: Vec<f32> = (0..1000).map(|i| if i % 3 == 0 { 9.0 / 16.0 } else { 16.0 / 9.0 }).collect();
        let t = std::time::Instant::now();
        for _ in 0..100 {
            masonry(&a, 1400.0, 16.0);
        }
        assert!(t.elapsed().as_millis() < 200, "{:?}", t.elapsed());
    }

    #[test]
    fn friendly_names() {
        assert_eq!(display_name(Path::new("/x/20261114-020712"), Kind::Video), "Recording Nov 14, 02:07");
        assert_eq!(display_name(Path::new("/x/Screenshot_20261107-100000-2.png"), Kind::Screenshot), "Screenshot Nov 7, 10:00");
        assert_eq!(display_name(Path::new("/x/demo trip.mp4"), Kind::Export), "demo trip");
    }

    #[test]
    fn scans_three_folders_and_renames() {
        let root = std::env::temp_dir().join(format!("clipper-home-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (s, e, p) = (root.join("s"), root.join("e"), root.join("p"));
        std::fs::create_dir_all(s.join("20261114-020712")).unwrap();
        std::fs::write(s.join("20261114-020712/raw.mp4"), b"x").unwrap();
        std::fs::create_dir_all(s.join("empty")).unwrap();
        std::fs::create_dir_all(&e).unwrap();
        std::fs::write(e.join("clip.gif"), b"x").unwrap();
        std::fs::write(e.join("notes.txt"), b"x").unwrap();
        std::fs::create_dir_all(&p).unwrap();
        image::RgbaImage::new(40, 20).save(p.join("Screenshot_20261114-020000.png")).unwrap();
        image::RgbaImage::new(40, 20).save(p.join("Screenshot_20261114-020000_edited_20261114-020100.png")).unwrap();
        let items = scan(&s, &e, &p);
        let kinds: Vec<Kind> = items.iter().map(|i| i.kind).collect();
        assert_eq!(items.len(), 4, "{items:?}");
        assert_eq!(kinds.iter().filter(|k| **k == Kind::Video).count(), 1);
        assert_eq!(kinds.iter().filter(|k| **k == Kind::Screenshot).count(), 1);
        assert_eq!(kinds.iter().filter(|k| **k == Kind::Export).count(), 2);
        let shot = items.iter().find(|i| i.kind == Kind::Screenshot).unwrap();
        assert_eq!(shot.aspect, Some(2.0));
        // Rename keeps the extension and moves the project file along.
        std::fs::write(crate::editor::doc::Document::sidecar_path(&shot.path), b"{}").unwrap();
        let new = rename_item(&shot.path, "Bug report").unwrap();
        assert_eq!(new.file_name().unwrap(), "Bug report.png");
        assert!(crate::editor::doc::Document::sidecar_path(&new).exists());
        assert!(rename_item(&new, "a/b").is_err());
        // Undoable delete of a session folder.
        let u = park(&s.join("20261114-020712")).unwrap();
        assert!(scan(&s, &e, &p).iter().all(|i| i.kind != Kind::Video));
        restore(&u).unwrap();
        assert!(s.join("20261114-020712/raw.mp4").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
