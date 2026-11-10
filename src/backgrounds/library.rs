//! The user's background library in the app data directory: imported images (original,
//! 400 px thumbnail, 1440 px proxy, orientation tag), favorites, saved gradients, recents
//! and a watched folder. Previews use the proxy, exports the original.

use super::Gradient;
use super::fit::Orientation;
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub const THUMB: u32 = 400;
pub const PROXY: u32 = 1440;
const EXTS: [&str; 4] = ["png", "jpg", "jpeg", "webp"];

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct ImageEntry {
    pub id: String,
    pub name: String,
    /// File name inside `originals/`.
    pub file: String,
    pub orientation: Orientation,
    #[serde(default)]
    pub tags: Vec<String>,
    pub width: u32,
    pub height: u32,
    pub added: u64,
    /// Where it came from (watched folder bookkeeping, relinking).
    #[serde(default)]
    pub source: Option<PathBuf>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct SavedGradient {
    pub id: String,
    pub name: String,
    pub gradient: Gradient,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
struct Data {
    images: Vec<ImageEntry>,
    favorites: Vec<String>,
    gradients: Vec<SavedGradient>,
    recent: Vec<String>,
    watched: Option<PathBuf>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Filter {
    All,
    Landscape,
    Portrait,
    Square,
    Favorites,
}

impl Filter {
    pub fn label(self) -> &'static str {
        match self {
            Filter::All => "All",
            Filter::Landscape => "Landscape",
            Filter::Portrait => "Portrait",
            Filter::Square => "Square",
            Filter::Favorites => "Favorites",
        }
    }
}

pub struct Library {
    dir: PathBuf,
    data: Data,
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn default_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join("Library/Application Support/Clipper/backgrounds")
}

/// A unique id for a new image (time plus a counter, so parallel imports never collide).
fn new_id() -> String {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    format!("{:x}{:04x}", now(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed) & 0xffff)
}

/// Decodes `path` and writes its copy, thumbnail and proxy into the library folder `dir`.
/// The slow part of an import; needs no lock on the library.
fn prepare(dir: &Path, path: &Path) -> Result<ImageEntry> {
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_lowercase).unwrap_or_default();
    if !EXTS.contains(&ext.as_str()) {
        return Err(anyhow!("Backgrounds can be PNG, JPG or WebP images."));
    }
    let img = image::open(path).map_err(|e| anyhow!("Could not read {}: {e}", path.display()))?;
    let id = new_id();
    let entry = ImageEntry {
        id: id.clone(),
        name: path.file_stem().and_then(|s| s.to_str()).unwrap_or("Image").to_string(),
        file: format!("{id}.{ext}"),
        orientation: Orientation::of(img.width(), img.height()),
        tags: Vec::new(),
        width: img.width(),
        height: img.height(),
        added: now(),
        source: Some(path.to_path_buf()),
    };
    write_files(dir, &entry, path, &img)?;
    Ok(entry)
}

/// The library's copy of an image, its thumbnail and its proxy.
fn write_files(dir: &Path, entry: &ImageEntry, src: &Path, img: &image::DynamicImage) -> Result<()> {
    for sub in ["originals", "thumbs", "proxies"] {
        std::fs::create_dir_all(dir.join(sub))?;
    }
    std::fs::copy(src, dir.join("originals").join(&entry.file))?;
    let rgb = img.to_rgb8();
    let save = |max: u32, out: PathBuf| -> Result<()> {
        let small = image::DynamicImage::ImageRgb8(rgb.clone()).thumbnail(max, max).to_rgb8();
        let mut f = std::fs::File::create(out)?;
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut f, 88).encode_image(&small)?;
        Ok(())
    };
    save(THUMB, dir.join("thumbs").join(format!("{}.jpg", entry.id)))?;
    save(PROXY, dir.join("proxies").join(format!("{}.jpg", entry.id)))?;
    Ok(())
}

static SCANNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A watched-folder scan is running.
pub fn scanning() -> bool {
    SCANNING.load(std::sync::atomic::Ordering::SeqCst)
}

/// Imports new images from the app library's watched folder. Blocking (run it off the UI
/// thread); the library is locked only briefly around each insert, never while decoding,
/// so the picker keeps drawing and fills in as images arrive. Returns how many were added;
/// does nothing if a scan is already running.
pub fn scan_watched_global() -> usize {
    use std::sync::atomic::Ordering;
    if SCANNING.swap(true, Ordering::SeqCst) {
        return 0;
    }
    let (dir, todo) = {
        let Ok(lib) = global().lock() else {
            SCANNING.store(false, Ordering::SeqCst);
            return 0;
        };
        (lib.dir.clone(), lib.new_in_watched())
    };
    let mut added = 0;
    for p in todo {
        if let Ok(entry) = prepare(&dir, &p) {
            if let Ok(mut lib) = global().lock() {
                if !lib.data.images.iter().any(|e| e.source.as_deref() == Some(p.as_path())) {
                    lib.data.images.insert(0, entry);
                    lib.save();
                    added += 1;
                }
            }
        }
    }
    SCANNING.store(false, Ordering::SeqCst);
    added
}

/// The app's library (loaded on first use).
pub fn global() -> &'static Mutex<Library> {
    static LIB: OnceLock<Mutex<Library>> = OnceLock::new();
    LIB.get_or_init(|| Mutex::new(Library::open(default_dir())))
}

impl Library {
    pub fn open(dir: PathBuf) -> Self {
        let data = std::fs::read_to_string(dir.join("library.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        Self { dir, data }
    }

    fn save(&self) {
        let _ = std::fs::create_dir_all(&self.dir);
        let _ = std::fs::write(self.dir.join("library.json"), serde_json::to_string_pretty(&self.data).unwrap_or_default());
    }

    pub fn images(&self) -> &[ImageEntry] {
        &self.data.images
    }

    pub fn get(&self, id: &str) -> Option<&ImageEntry> {
        self.data.images.iter().find(|e| e.id == id)
    }

    pub fn original(&self, id: &str) -> Option<PathBuf> {
        self.get(id).map(|e| self.dir.join("originals").join(&e.file))
    }

    pub fn proxy(&self, id: &str) -> PathBuf {
        self.dir.join("proxies").join(format!("{id}.jpg"))
    }

    pub fn thumb(&self, id: &str) -> PathBuf {
        self.dir.join("thumbs").join(format!("{id}.jpg"))
    }

    /// The original file is gone (shows a placeholder until relinked).
    pub fn missing(&self, id: &str) -> bool {
        self.original(id).is_none_or(|p| !p.exists())
    }

    /// Copies an image in: orientation tag, thumbnail and proxy.
    pub fn import(&mut self, path: &Path) -> Result<ImageEntry> {
        let entry = prepare(&self.dir, path)?;
        self.data.images.insert(0, entry.clone());
        self.save();
        Ok(entry)
    }

    /// Points a missing image at a new file (same id, so backgrounds using it keep working).
    pub fn relink(&mut self, id: &str, path: &Path) -> Result<()> {
        let img = image::open(path).map_err(|e| anyhow!("Could not read {}: {e}", path.display()))?;
        let ext = path.extension().and_then(|e| e.to_str()).map(str::to_lowercase).unwrap_or_default();
        let mut entry = self.get(id).cloned().ok_or_else(|| anyhow!("No such image"))?;
        entry.file = format!("{id}.{ext}");
        entry.width = img.width();
        entry.height = img.height();
        entry.orientation = Orientation::of(img.width(), img.height());
        entry.source = Some(path.to_path_buf());
        write_files(&self.dir, &entry, path, &img)?;
        if let Some(e) = self.data.images.iter_mut().find(|e| e.id == id) {
            *e = entry;
        }
        self.save();
        Ok(())
    }

    pub fn delete(&mut self, id: &str) {
        if let Some(e) = self.get(id).cloned() {
            let _ = std::fs::remove_file(self.dir.join("originals").join(&e.file));
            let _ = std::fs::remove_file(self.thumb(id));
            let _ = std::fs::remove_file(self.proxy(id));
        }
        self.data.images.retain(|e| e.id != id);
        self.data.favorites.retain(|f| f != id);
        self.data.recent.retain(|f| f != id);
        self.save();
    }

    pub fn rename(&mut self, id: &str, name: &str) {
        if let Some(e) = self.data.images.iter_mut().find(|e| e.id == id) {
            e.name = name.trim().to_string();
        }
        self.save();
    }

    pub fn set_tags(&mut self, id: &str, tags: Vec<String>) {
        if let Some(e) = self.data.images.iter_mut().find(|e| e.id == id) {
            e.tags = tags.into_iter().map(|t| t.trim().to_lowercase()).filter(|t| !t.is_empty()).collect();
        }
        self.save();
    }

    /// Favorites hold preset ids, saved-gradient ids and image ids alike.
    pub fn is_favorite(&self, id: &str) -> bool {
        self.data.favorites.iter().any(|f| f == id)
    }

    pub fn toggle_favorite(&mut self, id: &str) -> bool {
        let on = !self.is_favorite(id);
        if on {
            self.data.favorites.push(id.to_string());
        } else {
            self.data.favorites.retain(|f| f != id);
        }
        self.save();
        on
    }

    /// Marks an image as just used (most recent first, at most 8).
    pub fn touch(&mut self, id: &str) {
        self.data.recent.retain(|r| r != id);
        self.data.recent.insert(0, id.to_string());
        self.data.recent.truncate(8);
        self.save();
    }

    pub fn recent(&self) -> Vec<&ImageEntry> {
        self.data.recent.iter().filter_map(|id| self.get(id)).collect()
    }

    /// Images for the grid: filtered, and with `portrait_first` (output 9:16) portrait images
    /// first, or landscape first for 16:9.
    pub fn ordered(&self, filter: Filter, portrait_first: bool) -> Vec<&ImageEntry> {
        let mut list: Vec<&ImageEntry> = self
            .data
            .images
            .iter()
            .filter(|e| match filter {
                Filter::All => true,
                Filter::Landscape => e.orientation == Orientation::Landscape,
                Filter::Portrait => e.orientation == Orientation::Portrait,
                Filter::Square => e.orientation == Orientation::Square,
                Filter::Favorites => self.is_favorite(&e.id),
            })
            .collect();
        let rank = |o: Orientation| match (o, portrait_first) {
            (Orientation::Portrait, true) | (Orientation::Landscape, false) => 0,
            (Orientation::Square, _) => 1,
            _ => 2,
        };
        list.sort_by_key(|e| rank(e.orientation));
        list
    }

    pub fn saved_gradients(&self) -> &[SavedGradient] {
        &self.data.gradients
    }

    pub fn save_gradient(&mut self, name: &str, gradient: Gradient) -> String {
        let id = format!("mine:{:x}{}", now(), self.data.gradients.len());
        self.data.gradients.insert(0, SavedGradient { id: id.clone(), name: name.to_string(), gradient });
        self.save();
        id
    }

    pub fn watched(&self) -> Option<&Path> {
        self.data.watched.as_deref()
    }

    pub fn set_watched(&mut self, dir: Option<PathBuf>) {
        self.data.watched = dir;
        self.save();
    }

    /// Images in the watched folder that aren't in the library yet.
    fn new_in_watched(&self) -> Vec<PathBuf> {
        let Some(dir) = self.data.watched.clone() else { return Vec::new() };
        let known: std::collections::HashSet<PathBuf> = self.data.images.iter().filter_map(|e| e.source.clone()).collect();
        let mut out: Vec<PathBuf> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|x| x.to_str()).is_some_and(|x| EXTS.contains(&x.to_lowercase().as_str())))
            .filter(|p| !known.contains(p))
            .collect();
        out.sort();
        out
    }

    /// Imports images in the watched folder that aren't in the library yet (this library,
    /// blocking; the app uses `scan_watched_global`).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn scan_watched(&mut self) -> Vec<String> {
        self.new_in_watched().into_iter().filter_map(|p| self.import(&p).ok().map(|e| e.id)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(dir: &Path, name: &str, w: u32, h: u32) -> PathBuf {
        let p = dir.join(name);
        image::RgbImage::from_pixel(w, h, image::Rgb([200, 100, 50])).save(&p).unwrap();
        p
    }

    #[test]
    fn import_tags_orientation_and_orders_by_ratio() {
        let root = std::env::temp_dir().join(format!("clipper-bglib-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let mut lib = Library::open(root.join("lib"));
        let wide = lib.import(&png(&src, "wide.png", 1600, 900)).unwrap();
        let tall = lib.import(&png(&src, "tall.png", 900, 1600)).unwrap();
        let sq = lib.import(&png(&src, "sq.png", 1000, 1000)).unwrap();
        assert_eq!(wide.orientation, Orientation::Landscape);
        assert_eq!(tall.orientation, Orientation::Portrait);
        assert_eq!(sq.orientation, Orientation::Square);
        assert!(lib.thumb(&wide.id).exists() && lib.proxy(&wide.id).exists());
        let (tw, th) = image::image_dimensions(lib.thumb(&wide.id)).unwrap();
        assert_eq!(tw.max(th), THUMB);
        // 9:16 output: portrait first; 16:9: landscape first.
        assert_eq!(lib.ordered(Filter::All, true)[0].id, tall.id);
        assert_eq!(lib.ordered(Filter::All, false)[0].id, wide.id);
        assert_eq!(lib.ordered(Filter::Square, true).len(), 1);
        // Favorites, rename, tags, recents.
        assert!(lib.toggle_favorite(&sq.id));
        assert_eq!(lib.ordered(Filter::Favorites, true).len(), 1);
        lib.rename(&sq.id, " Desk ");
        lib.set_tags(&sq.id, vec!["Office ".into(), "".into()]);
        lib.touch(&wide.id);
        // Persisted.
        let lib2 = Library::open(root.join("lib"));
        assert_eq!(lib2.get(&sq.id).unwrap().name, "Desk");
        assert_eq!(lib2.get(&sq.id).unwrap().tags, vec!["office".to_string()]);
        assert_eq!(lib2.recent()[0].id, wide.id);
        assert!(lib2.is_favorite(&sq.id));
        // Missing originals can be relinked; delete removes everything.
        std::fs::remove_file(lib.original(&tall.id).unwrap()).unwrap();
        assert!(lib.missing(&tall.id));
        lib.relink(&tall.id, &png(&src, "tall2.png", 900, 1600)).unwrap();
        assert!(!lib.missing(&tall.id));
        lib.delete(&wide.id);
        assert!(lib.get(&wide.id).is_none() && !lib.thumb(&wide.id).exists());
        // Watched folder: new files come in once.
        let watch = root.join("watch");
        std::fs::create_dir_all(&watch).unwrap();
        png(&watch, "a.jpg", 300, 200);
        lib.set_watched(Some(watch.clone()));
        assert_eq!(lib.scan_watched().len(), 1);
        assert_eq!(lib.scan_watched().len(), 0);
        assert!(lib.import(&root.join("nope.gif")).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
