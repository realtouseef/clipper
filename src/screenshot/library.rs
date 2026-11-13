//! The screenshots folder: timestamped originals (never overwritten by edits), the recent
//! list, and undoable deletes (files wait in a hidden folder for the undo window, then go to
//! the Trash).

use anyhow::Result;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const PENDING: &str = ".pending-delete";

/// A new, unused `Screenshot_<timestamp>.png` path in `folder` (created if needed).
pub fn new_path(folder: &Path, stamp: &str) -> Result<PathBuf> {
    std::fs::create_dir_all(folder)?;
    let mut path = folder.join(format!("Screenshot_{stamp}.png"));
    let mut n = 2;
    while path.exists() {
        path = folder.join(format!("Screenshot_{stamp}-{n}.png"));
        n += 1;
    }
    Ok(path)
}

/// Is this an original screenshot or imported image (not an export)?
pub fn is_original(p: &Path) -> bool {
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
    (name.starts_with("Screenshot_") || name.starts_with("Image_") || name.starts_with("Merge_")) && name.ends_with(".png") && !name.contains("_edited_")
}

/// Brings an image of the user's into the library to edit: a PNG copy named
/// `Image_<timestamp>.png` in `folder` (the file they picked is never touched). Reads PNG,
/// JPEG and WebP directly, and anything else macOS can open (HEIC, TIFF, GIF, BMP…)
/// through `sips`.
pub fn import(folder: &Path, src: &Path, stamp: &str) -> Result<PathBuf> {
    let img = match image::open(src) {
        Ok(img) => img,
        Err(_) => {
            let tmp = std::env::temp_dir().join(format!("clipper-import-{}-{stamp}.png", std::process::id()));
            let ok = std::process::Command::new("sips")
                .args(["-s", "format", "png"])
                .arg(src)
                .arg("--out")
                .arg(&tmp)
                .output()
                .is_ok_and(|o| o.status.success());
            let img = if ok { image::open(&tmp).ok() } else { None };
            let _ = std::fs::remove_file(&tmp);
            img.ok_or_else(|| anyhow::anyhow!("Can't open {} as an image", src.file_name().and_then(|n| n.to_str()).unwrap_or("that file")))?
        }
    };
    std::fs::create_dir_all(folder)?;
    let mut path = folder.join(format!("Image_{stamp}.png"));
    let mut n = 2;
    while path.exists() {
        path = folder.join(format!("Image_{stamp}-{n}.png"));
        n += 1;
    }
    img.to_rgba8().save(&path)?;
    Ok(path)
}

/// Newest originals first, at most `n`.
pub fn recent(folder: &Path, n: usize) -> Vec<PathBuf> {
    let mut files: Vec<(SystemTime, PathBuf)> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| is_original(p))
        .map(|p| (std::fs::metadata(&p).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH), p))
        .collect();
    files.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    files.into_iter().take(n).map(|(_, p)| p).collect()
}

/// A delete that can still be undone: the original (and its project file) moved aside.
#[derive(Clone, Debug, PartialEq)]
pub struct Pending {
    pub original: PathBuf,
    moved: Vec<(PathBuf, PathBuf)>,
}

/// Moves a screenshot and its project file out of sight until `finish` or `undo`.
pub fn delete(path: &Path) -> Result<Pending> {
    let folder = path.parent().unwrap_or(Path::new("."));
    let hidden = folder.join(PENDING);
    std::fs::create_dir_all(&hidden)?;
    let mut moved = Vec::new();
    for src in [path.to_path_buf(), crate::editor::doc::Document::sidecar_path(path)] {
        if src.exists() {
            let dest = hidden.join(src.file_name().unwrap());
            std::fs::rename(&src, &dest)?;
            moved.push((src, dest));
        }
    }
    Ok(Pending { original: path.to_path_buf(), moved })
}

/// Puts the files back.
pub fn undo(p: &Pending) -> Result<()> {
    for (orig, hidden) in &p.moved {
        std::fs::rename(hidden, orig)?;
    }
    Ok(())
}

/// The undo window passed: send the files to the Trash (deleted outright if that fails).
pub fn finish(p: &Pending) {
    for (_, hidden) in &p.moved {
        to_trash(hidden);
    }
}

fn to_trash(path: &Path) {
    #[cfg(target_os = "macos")]
    let ok = {
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        let mut ctx = trash::TrashContext::default();
        ctx.set_delete_method(DeleteMethod::NsFileManager);
        ctx.delete(path).is_ok()
    };
    #[cfg(not(target_os = "macos"))]
    let ok = trash::delete(path).is_ok();
    if !ok {
        let _ = std::fs::remove_file(path);
    }
}

/// Deletes left waiting when the app quit during an undo window go to the Trash now.
pub fn flush_pending(folder: &Path) {
    for e in std::fs::read_dir(folder.join(PENDING)).into_iter().flatten().flatten() {
        to_trash(&e.path());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::GenericImageView;

    /// Imported images become PNG copies in the library (listed as recent), and the user's
    /// own file is left exactly as it was. Formats the image crate can't read go through sips.
    #[test]
    fn imports_a_copy_to_edit() {
        let dir = std::env::temp_dir().join(format!("clipper-import-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let lib = dir.join("lib");
        std::fs::create_dir_all(&dir).unwrap();
        let jpg = dir.join("holiday photo.jpg");
        image::RgbImage::from_pixel(40, 30, image::Rgb([200, 30, 30])).save(&jpg).unwrap();
        let before = std::fs::read(&jpg).unwrap();
        let a = import(&lib, &jpg, "20261108-120000").unwrap();
        let b = import(&lib, &jpg, "20261108-120000").unwrap();
        assert_eq!(a.file_name().unwrap(), "Image_20261108-120000.png");
        assert_eq!(b.file_name().unwrap(), "Image_20261108-120000-2.png");
        assert_eq!(image::open(&a).unwrap().dimensions(), (40, 30));
        assert_eq!(std::fs::read(&jpg).unwrap(), before, "the picked file changed");
        assert_eq!(recent(&lib, 10).len(), 2);
        // TIFF via sips (macOS).
        let tiff = dir.join("scan.tiff");
        assert!(std::process::Command::new("sips").args(["-s", "format", "tiff"]).arg(&jpg).arg("--out").arg(&tiff).output().unwrap().status.success());
        let c = import(&lib, &tiff, "20261108-120001").unwrap();
        assert_eq!(image::open(&c).unwrap().dimensions(), (40, 30));
        // Not an image.
        let txt = dir.join("notes.txt");
        std::fs::write(&txt, "hello").unwrap();
        assert!(import(&lib, &txt, "20261108-120002").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_recent_and_undoable_delete() {
        let dir = std::env::temp_dir().join(format!("clipper-lib-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let a = new_path(&dir, "20261107-100000").unwrap();
        std::fs::write(&a, b"a").unwrap();
        let b = new_path(&dir, "20261107-100000").unwrap();
        assert_eq!(b.file_name().unwrap(), "Screenshot_20261107-100000-2.png");
        std::fs::write(&b, b"b").unwrap();
        std::fs::write(dir.join("Screenshot_20261107-100000_edited_20261107-100100.png"), b"e").unwrap();
        std::fs::write(crate::editor::doc::Document::sidecar_path(&a), b"{}").unwrap();
        let r = recent(&dir, 10);
        assert_eq!(r.len(), 2, "exports aren't listed: {r:?}");

        let p = delete(&a).unwrap();
        assert!(!a.exists() && !crate::editor::doc::Document::sidecar_path(&a).exists());
        assert_eq!(recent(&dir, 10), vec![b.clone()]);
        undo(&p).unwrap();
        assert!(a.exists() && crate::editor::doc::Document::sidecar_path(&a).exists());

        let p = delete(&a).unwrap();
        finish(&p);
        assert!(!a.exists());
        assert!(std::fs::read_dir(dir.join(PENDING)).unwrap().next().is_none(), "moved to the Trash");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
