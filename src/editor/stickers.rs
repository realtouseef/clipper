//! Stickers: the built-in SVG pack (original artwork, `assets/stickers`) and the user's
//! imported PNG/SVG/WebP files ("My Stickers", kept in Application Support).

use super::doc::StickerSrc;
use anyhow::{Result, anyhow};
use image::RgbaImage;
use resvg::{tiny_skia, usvg};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

pub struct Builtin {
    pub name: &'static str,
    pub category: &'static str,
    pub svg: &'static str,
}

macro_rules! s {
    ($name:literal, $cat:literal) => {
        Builtin { name: $name, category: $cat, svg: include_str!(concat!("../../assets/stickers/", $name, ".svg")) }
    };
}

pub const CATEGORIES: [&str; 5] = ["Arrows", "Badges", "Bubbles", "Shapes", "Reactions"];

pub const BUILTIN: [Builtin; 21] = [
    s!("arrow-bold", "Arrows"),
    s!("arrow-curved", "Arrows"),
    s!("arrow-loop", "Arrows"),
    s!("badge-new", "Badges"),
    s!("badge-tip", "Badges"),
    s!("badge-check", "Badges"),
    s!("badge-cross", "Badges"),
    s!("badge-warning", "Badges"),
    s!("badge-star", "Badges"),
    s!("bubble-round", "Bubbles"),
    s!("bubble-square", "Bubbles"),
    s!("bubble-thought", "Bubbles"),
    s!("bubble-shout", "Bubbles"),
    s!("shape-star", "Shapes"),
    s!("shape-heart", "Shapes"),
    s!("shape-circle", "Shapes"),
    s!("shape-burst", "Shapes"),
    s!("react-thumbs-up", "Reactions"),
    s!("react-smile", "Reactions"),
    s!("react-wow", "Reactions"),
    s!("react-fire", "Reactions"),
];

fn svg_options() -> &'static usvg::Options<'static> {
    static OPT: OnceLock<usvg::Options<'static>> = OnceLock::new();
    OPT.get_or_init(|| {
        let mut db = usvg::fontdb::Database::new();
        db.load_font_data(include_bytes!("../../assets/fonts/Poppins-Bold.ttf").to_vec());
        usvg::Options { fontdb: Arc::new(db), ..Default::default() }
    })
}

/// Natural aspect ratio (w/h) of a sticker, for placing it undistorted.
pub fn aspect(src: &StickerSrc) -> f32 {
    match load_tree(src) {
        Some(t) => t.size().width() / t.size().height().max(1.0),
        None => match src {
            StickerSrc::File(p) => image::image_dimensions(p).map(|(w, h)| w as f32 / h.max(1) as f32).unwrap_or(1.0),
            StickerSrc::Builtin(_) => 1.0,
        },
    }
}

fn load_tree(src: &StickerSrc) -> Option<usvg::Tree> {
    match src {
        StickerSrc::Builtin(name) => {
            let b = BUILTIN.iter().find(|b| b.name == name)?;
            usvg::Tree::from_str(b.svg, svg_options()).ok()
        }
        StickerSrc::File(p) if is_svg(p) => usvg::Tree::from_data(&std::fs::read(p).ok()?, svg_options()).ok(),
        StickerSrc::File(_) => None,
    }
}

fn is_svg(p: &Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("svg"))
}

/// The sticker drawn at exactly `w`×`h` pixels (straight alpha).
pub fn render(src: &StickerSrc, w: u32, h: u32) -> Option<RgbaImage> {
    let (w, h) = (w.max(1), h.max(1));
    if let Some(tree) = load_tree(src) {
        let mut pm = tiny_skia::Pixmap::new(w, h)?;
        let size = tree.size();
        let t = tiny_skia::Transform::from_scale(w as f32 / size.width(), h as f32 / size.height());
        resvg::render(&tree, t, &mut pm.as_mut());
        return Some(super::render::pixmap_to_image(&pm));
    }
    let StickerSrc::File(p) = src else { return None };
    let img = image::open(p).ok()?.to_rgba8();
    Some(image::imageops::resize(&img, w, h, image::imageops::FilterType::Triangle))
}

/// Where imported stickers are kept.
pub fn my_stickers_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join("Library/Application Support/Clipper/stickers")
}

/// The user's imported stickers, oldest first.
pub fn my_stickers() -> Vec<PathBuf> {
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(my_stickers_dir())
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()).is_some_and(|e| ["png", "svg", "webp"].contains(&e.to_lowercase().as_str())))
        .map(|p| (std::fs::metadata(&p).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH), p))
        .collect();
    files.sort();
    files.into_iter().map(|(_, p)| p).collect()
}

/// Copies a PNG, SVG or WebP into My Stickers (after checking it loads). Returns the copy.
pub fn import(path: &Path) -> Result<PathBuf> {
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_lowercase).unwrap_or_default();
    if !["png", "svg", "webp"].contains(&ext.as_str()) {
        return Err(anyhow!("Stickers can be PNG, SVG or WebP files."));
    }
    let probe = StickerSrc::File(path.to_path_buf());
    if render(&probe, 32, 32).is_none() {
        return Err(anyhow!("Could not read {}.", path.display()));
    }
    let dir = my_stickers_dir();
    std::fs::create_dir_all(&dir)?;
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("sticker");
    let mut dest = dir.join(format!("{stem}.{ext}"));
    let mut n = 2;
    while dest.exists() {
        dest = dir.join(format!("{stem}-{n}.{ext}"));
        n += 1;
    }
    std::fs::copy(path, &dest)?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_renders() {
        for b in &BUILTIN {
            let src = StickerSrc::Builtin(b.name.into());
            let a = aspect(&src);
            let img = render(&src, (96.0 * a) as u32, 96).unwrap_or_else(|| panic!("{} renders", b.name));
            let painted = img.pixels().filter(|p| p[3] > 128).count();
            assert!(painted > 400, "{} painted {painted}", b.name);
            assert!(CATEGORIES.contains(&b.category));
        }
        // Badge text is drawn with the bundled font.
        let img = render(&StickerSrc::Builtin("badge-new".into()), 220, 100).unwrap();
        let dark = img.pixels().filter(|p| p[3] > 200 && p[0] < 40 && p[1] < 40).count();
        assert!(dark > 1500, "badge text and outline: {dark}");
    }
}
