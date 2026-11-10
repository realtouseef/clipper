//! Clipper's mascot (a clapperboard with a face), used as the app logo: the header, the Dock
//! icon, empty states and Credits. Drawn from `assets/brand/mascot.svg`.

use gpui_kit::{App, RenderImage};
use image::RgbaImage;
use resvg::{tiny_skia, usvg};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

pub const SVG: &str = include_str!("../assets/brand/mascot.svg");

/// The mascot `h` pixels tall (width follows its aspect), transparent around it.
pub fn render(h: u32) -> RgbaImage {
    let tree = usvg::Tree::from_str(SVG, &usvg::Options::default()).expect("mascot SVG parses");
    let size = tree.size();
    let k = h as f32 / size.height();
    let w = (size.width() * k).round().max(1.0) as u32;
    let mut pm = tiny_skia::Pixmap::new(w, h.max(1)).expect("mascot size");
    resvg::render(&tree, tiny_skia::Transform::from_scale(k, k), &mut pm.as_mut());
    crate::editor::render::pixmap_to_image(&pm)
}

/// The mascot as a GPU image, `h` physical pixels tall (cached per size).
pub fn image(h: u32) -> Arc<RenderImage> {
    static CACHE: OnceLock<Mutex<HashMap<u32, Arc<RenderImage>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap().entry(h).or_insert_with(|| crate::canvas_preview::to_render_image(render(h))).clone()
}

/// Width / height of the mascot.
pub fn aspect() -> f32 {
    let tree = usvg::Tree::from_str(SVG, &usvg::Options::default()).expect("mascot SVG parses");
    tree.size().width() / tree.size().height()
}

/// The app icon: the mascot on a lime rounded square, `size` pixels.
pub fn icon(size: u32) -> RgbaImage {
    let mut pm = tiny_skia::Pixmap::new(size, size).expect("icon size");
    // macOS icon grid: the tile is ~80% of the canvas with a ~22% corner radius.
    let inset = size as f32 * 0.1;
    let side = size as f32 - 2.0 * inset;
    if let Some(path) = crate::editor::render::round_rect_path(crate::editor::doc::r(inset, inset, side, side), side * 0.225) {
        let mut paint = tiny_skia::Paint::default();
        paint.set_color_rgba8(0xa4, 0xf4, 0x7c, 255); // the app's lime accent
        paint.anti_alias = true;
        pm.fill_path(&path, &paint, tiny_skia::FillRule::Winding, tiny_skia::Transform::identity(), None);
    }
    let mut out = crate::editor::render::pixmap_to_image(&pm);
    let m = render((side * 0.78) as u32);
    let (x, y) = ((size - m.width()) / 2, (size - m.height()) / 2);
    image::imageops::overlay(&mut out, &m, x as i64, y as i64);
    out
}

/// Shows the mascot icon in the Dock (the app runs unbundled, so there's no icon file).
pub fn set_dock_icon(_: &App) {
    use std::ffi::{c_char, c_void};
    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> *mut c_void;
        fn sel_registerName(name: *const c_char) -> *mut c_void;
        fn objc_msgSend();
    }
    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {}
    let Ok(png) = crate::editor::export::encode(&icon(512), crate::editor::export::Format::Png, 100) else { return };
    unsafe {
        let send0: extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let send_data: extern "C" fn(*mut c_void, *mut c_void, *const u8, usize) -> *mut c_void =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let send1: extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> *mut c_void = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let data = send_data(objc_getClass(c"NSData".as_ptr()), sel_registerName(c"dataWithBytes:length:".as_ptr()), png.as_ptr(), png.len());
        let alloc = send0(objc_getClass(c"NSImage".as_ptr()), sel_registerName(c"alloc".as_ptr()));
        let img = send1(alloc, sel_registerName(c"initWithData:".as_ptr()), data);
        if img.is_null() {
            return;
        }
        let app = send0(objc_getClass(c"NSApplication".as_ptr()), sel_registerName(c"sharedApplication".as_ptr()));
        send1(app, sel_registerName(c"setApplicationIconImage:".as_ptr()), img);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mascot_and_icon_render() {
        let m = render(200);
        assert_eq!(m.height(), 200);
        assert!((m.width() as f32 / 200.0 - aspect()).abs() < 0.02);
        // Transparent around it, white eyes, yellow stripes, red blush.
        assert_eq!(m.get_pixel(0, 199)[3], 0);
        let count = |f: &dyn Fn(&image::Rgba<u8>) -> bool| m.pixels().filter(|p| p[3] > 200 && f(p)).count();
        assert!(count(&|p| p[0] > 240 && p[1] > 240 && p[2] > 240) > 300, "eyes and smile");
        assert!(count(&|p| p[0] > 220 && p[1] > 170 && p[2] < 110) > 300, "clapper stripes");
        assert!(count(&|p| p[0] > 200 && p[1] < 110 && p[2] < 130) > 50, "blush");
        let icon = icon(256);
        assert_eq!(icon.dimensions(), (256, 256));
        assert_eq!(icon.get_pixel(2, 2)[3], 0, "transparent outside the tile");
        assert_eq!(icon.get_pixel(40, 128)[3], 255, "the tile");
        if let Ok(dir) = std::env::var("CLIPPER_MASCOT_OUT") {
            icon.save(format!("{dir}/icon.png")).unwrap();
            m.save(format!("{dir}/mascot.png")).unwrap();
        }
    }
}
