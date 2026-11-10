//! Screenshot editor: document model, history, renderer and the editor window.

pub mod canvas;
pub mod doc;
pub mod export;
pub mod merge;
pub mod panel;
pub mod render;
pub mod spline;
pub mod stickers;
pub mod text;
pub mod view;

use anyhow::{Result, anyhow};
use doc::Document;
use gpui_kit::{App, ClipboardItem, Image, ImageFormat};
use image::RgbaImage;
use std::path::Path;

/// The shareable image for a screenshot: its edits rendered (redactions baked in) when it
/// has a project file, else the original.
pub fn baked(original: &Path) -> Result<RgbaImage> {
    let base = image::open(original).map_err(|e| anyhow!("Could not open {}: {e}", original.display()))?.to_rgba8();
    match load_doc(original) {
        Some(doc) => Ok(render::render(&doc, &base, 1.0, &render::Cache::default())),
        None => Ok(base),
    }
}

/// The project file next to an original, if there is one.
pub fn load_doc(original: &Path) -> Option<Document> {
    let s = std::fs::read_to_string(Document::sidecar_path(original)).ok()?;
    Document::from_json(&s).ok()
}

/// Puts the image on the clipboard as PNG.
pub fn copy_image(img: &RgbaImage, cx: &mut App) -> Result<()> {
    let bytes = export::encode(img, export::Format::Png, 100)?;
    cx.write_to_clipboard(ClipboardItem::new_image(&Image::from_bytes(ImageFormat::Png, bytes)));
    Ok(())
}

/// Lucide icons (ISC, `assets/icons/editor/LICENSE-LUCIDE`).
pub mod icons {
    use gpui_kit::component::Icon;
    macro_rules! icons {
        ($($name:ident => $file:literal),* $(,)?) => {
            $(pub fn $name() -> Icon { Icon::default().data(include_bytes!(concat!("../../assets/icons/editor/", $file, ".svg"))) })*
        };
    }
    icons! {
        select => "mouse-pointer-2", arrow => "move-up-right", line => "minus", rect => "square",
        round_rect => "square-round-corner", ellipse => "circle", text => "type", highlighter => "highlighter",
        step => "list-ordered", pen => "pen-line", sticker => "sticker", crop => "crop", redact => "eye-off",
        background => "wallpaper", undo => "undo-2", redo => "redo-2", copy => "copy",
        save => "save", export => "download", zoom_in => "zoom-in", zoom_out => "zoom-out",
        rotate_left => "rotate-ccw", rotate_right => "rotate-cw", flip_h => "flip-horizontal-2",
        flip_v => "flip-vertical-2", lock => "lock", unlock => "lock-open", trash => "trash",
        front => "bring-to-front", back => "send-to-back", duplicate => "copy-plus", edit => "pencil",
        camera => "camera", emoji => "smile", layout => "layout-template", captions => "captions",
        settings => "settings-2", monitor => "monitor", region => "square-dashed", zoom => "zoom-in", folder => "folder-open",
        forward => "layers-arrow-up", backward => "layers-arrow-down", image => "image",
        merge => "images", callout => "scan-search", add_image => "image-plus",
    }
}
