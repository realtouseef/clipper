//! Clipper in the menu bar: a small mascot icon with Capture Full Screen, Capture Region,
//! Record, Open Clipper and Quit, so the app keeps running in the background after its window is closed.
//!
//! The status item is plain AppKit (through the Objective-C runtime). Menu picks set a bit
//! that the app's tick drains with `take_actions`, like the global hotkeys.

use resvg::{tiny_skia, usvg};
use std::ffi::{c_char, c_void};
use std::sync::atomic::{AtomicU32, Ordering};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    /// The whole screen under the pointer, right away (no delay).
    ShotFull,
    /// Drag out a region, then capture it (no delay).
    ShotRegion,
    EditImage,
    Record,
    Open,
    Quit,
}

impl Action {
    const ALL: [Action; 6] = [Action::ShotFull, Action::ShotRegion, Action::EditImage, Action::Record, Action::Open, Action::Quit];
    fn tag(self) -> isize {
        self as isize
    }
}

static PICKED: AtomicU32 = AtomicU32::new(0);

/// Queues an action as if it were picked from the menu (the Dock icon's reopen uses `Open`).
pub fn request(action: Action) {
    PICKED.fetch_or(1 << action.tag(), Ordering::SeqCst);
}

/// Menu picks since the last call.
pub fn take_actions() -> Vec<Action> {
    let bits = PICKED.swap(0, Ordering::SeqCst);
    Action::ALL.into_iter().filter(|a| bits & (1 << a.tag()) != 0).collect()
}

const ICON_SVG: &str = include_str!("../assets/brand/menubar.svg");

/// The menu bar icon: black on transparent, `h` px tall (macOS tints it as a template).
pub fn icon_png(h: u32) -> Vec<u8> {
    let tree = usvg::Tree::from_str(ICON_SVG, &usvg::Options::default()).expect("menu bar icon parses");
    let size = tree.size();
    let k = h as f32 / size.height();
    let w = (size.width() * k).round() as u32;
    let mut pm = tiny_skia::Pixmap::new(w.max(1), h.max(1)).expect("icon size");
    resvg::render(&tree, tiny_skia::Transform::from_scale(k, k), &mut pm.as_mut());
    pm.encode_png().unwrap_or_default()
}

// ---------- Objective-C runtime ----------

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> *mut c_void;
    fn sel_registerName(name: *const c_char) -> *mut c_void;
    fn objc_msgSend();
    fn objc_allocateClassPair(superclass: *mut c_void, name: *const c_char, extra: usize) -> *mut c_void;
    fn objc_registerClassPair(class: *mut c_void);
    fn class_addMethod(class: *mut c_void, sel: *mut c_void, imp: *const c_void, types: *const c_char) -> bool;
}

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}

type Id = *mut c_void;

unsafe fn sel(name: &std::ffi::CStr) -> Id {
    unsafe { sel_registerName(name.as_ptr()) }
}

unsafe fn class(name: &std::ffi::CStr) -> Id {
    unsafe { objc_getClass(name.as_ptr()) }
}

unsafe fn send0(obj: Id, s: &std::ffi::CStr) -> Id {
    let f: extern "C" fn(Id, Id) -> Id = unsafe { std::mem::transmute(objc_msgSend as unsafe extern "C" fn()) };
    f(obj, unsafe { sel(s) })
}

unsafe fn send1(obj: Id, s: &std::ffi::CStr, a: Id) -> Id {
    let f: extern "C" fn(Id, Id, Id) -> Id = unsafe { std::mem::transmute(objc_msgSend as unsafe extern "C" fn()) };
    f(obj, unsafe { sel(s) }, a)
}

unsafe fn send_int(obj: Id, s: &std::ffi::CStr, a: isize) -> Id {
    let f: extern "C" fn(Id, Id, isize) -> Id = unsafe { std::mem::transmute(objc_msgSend as unsafe extern "C" fn()) };
    f(obj, unsafe { sel(s) }, a)
}

unsafe fn nsstring(s: &str) -> Id {
    let c = std::ffi::CString::new(s).unwrap_or_default();
    let f: extern "C" fn(Id, Id, *const c_char) -> Id = unsafe { std::mem::transmute(objc_msgSend as unsafe extern "C" fn()) };
    f(unsafe { class(c"NSString") }, unsafe { sel(c"stringWithUTF8String:") }, c.as_ptr())
}

/// `-act:` on the menu target: records which item was picked (its tag).
extern "C" fn on_act(_this: Id, _cmd: Id, sender: Id) {
    let f: extern "C" fn(Id, Id) -> isize = unsafe { std::mem::transmute(objc_msgSend as unsafe extern "C" fn()) };
    let tag = f(sender, unsafe { sel(c"tag") });
    if (0..32).contains(&tag) {
        PICKED.fetch_or(1 << tag, Ordering::SeqCst);
    }
}

/// Adds the status item and its menu. Call once, on the main thread. `shortcut` is the
/// screenshot hotkey's label, shown in the menu.
pub fn install(shortcut: &str) {
    unsafe {
        // The menu's target: an NSObject subclass with one action method.
        let mut target_class = objc_getClass(c"ClipperMenuTarget".as_ptr());
        if target_class.is_null() {
            target_class = objc_allocateClassPair(class(c"NSObject"), c"ClipperMenuTarget".as_ptr(), 0);
            class_addMethod(target_class, sel(c"act:"), on_act as *const c_void, c"v@:@".as_ptr());
            objc_registerClassPair(target_class);
        }
        let target = send0(send0(target_class, c"alloc"), c"init");

        let bar = send0(class(c"NSStatusBar"), c"systemStatusBar");
        let f: extern "C" fn(Id, Id, f64) -> Id = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let item = f(bar, sel(c"statusItemWithLength:"), -1.0); // NSVariableStatusItemLength
        send0(item, c"retain");

        // Icon: rendered at 2× and drawn at 18 pt tall, as a template image.
        let png = icon_png(36);
        let fd: extern "C" fn(Id, Id, *const u8, usize) -> Id = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let data = fd(class(c"NSData"), sel(c"dataWithBytes:length:"), png.as_ptr(), png.len());
        let image = send1(send0(class(c"NSImage"), c"alloc"), c"initWithData:", data);
        if !image.is_null() {
            #[repr(C)]
            struct NSSize {
                w: f64,
                h: f64,
            }
            let tree = usvg::Tree::from_str(ICON_SVG, &usvg::Options::default()).expect("menu bar icon parses");
            let aspect = (tree.size().width() / tree.size().height()) as f64;
            let fs: extern "C" fn(Id, Id, NSSize) = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            fs(image, sel(c"setSize:"), NSSize { w: 18.0 * aspect, h: 18.0 });
            let fb: extern "C" fn(Id, Id, bool) = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            fb(image, sel(c"setTemplate:"), true);
            let button = send0(item, c"button");
            send1(button, c"setImage:", image);
            send1(button, c"setToolTip:", nsstring("Clipper"));
        }

        let menu = send0(send0(class(c"NSMenu"), c"alloc"), c"init");
        let add = |title: &str, action: Action, key: &str, mods: isize| {
            let fi: extern "C" fn(Id, Id, Id, Id, Id) -> Id = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let mi = fi(menu, sel(c"addItemWithTitle:action:keyEquivalent:"), nsstring(title), sel(c"act:"), nsstring(key));
            send1(mi, c"setTarget:", target);
            send_int(mi, c"setTag:", action.tag());
            if !key.is_empty() {
                send_int(mi, c"setKeyEquivalentModifierMask:", mods);
            }
        };
        let _ = shortcut; // the hotkey follows the Shots tab's mode, so it isn't shown on either
        add("Capture Full Screen", Action::ShotFull, "", 0);
        add("Capture Region…", Action::ShotRegion, "", 0);
        add("Edit Image…", Action::EditImage, "", 0);
        add("Record Screen", Action::Record, "", 0);
        send1(menu, c"addItem:", send0(class(c"NSMenuItem"), c"separatorItem"));
        add("Open Clipper", Action::Open, "", 0);
        send1(menu, c"addItem:", send0(class(c"NSMenuItem"), c"separatorItem"));
        add("Quit Clipper", Action::Quit, "q", 1 << 20);
        send1(item, c"setMenu:", menu);

        // Living in the menu bar with no window, macOS App Nap throttles Clipper's timers, so
        // a menu pick or hotkey waited seconds for the next tick. Opt out for the app's life.
        let info = send0(class(c"NSProcessInfo"), c"processInfo");
        let begin: extern "C" fn(Id, Id, u64, Id) -> Id = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        // NSActivityUserInitiatedAllowingIdleSystemSleep: no App Nap, the Mac may still sleep.
        let token = begin(info, sel(c"beginActivityWithOptions:reason:"), 0x00FF_FFFF & !(1u64 << 20), nsstring("Menu bar, hotkeys and screenshot cards"));
        send0(token, c"retain");
    }
}

/// Dock icon on (a window is open) or off (running from the menu bar only).
pub fn set_dock_icon_visible(visible: bool) {
    unsafe {
        let app = send0(class(c"NSApplication"), c"sharedApplication");
        // NSApplicationActivationPolicyRegular = 0, Accessory = 1.
        send_int(app, c"setActivationPolicy:", if visible { 0 } else { 1 });
    }
}

/// Whether the main window is closed (Clipper only in the menu bar). While it is, nothing
/// that touches the camera or microphone runs; only the screenshot watcher does.
static HIDDEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn window_hidden() -> bool {
    HIDDEN.load(Ordering::SeqCst)
}

/// Hides a window without closing it (the app keeps running).
pub fn hide_window(window: &gpui_kit::Window) {
    HIDDEN.store(true, Ordering::SeqCst);
    if let Some(w) = ns_window(window) {
        unsafe {
            send1(w, c"orderOut:", std::ptr::null_mut());
        }
    }
}

/// Brings a hidden window back to the front.
pub fn show_window(window: &gpui_kit::Window) {
    HIDDEN.store(false, Ordering::SeqCst);
    if let Some(w) = ns_window(window) {
        unsafe {
            send1(w, c"makeKeyAndOrderFront:", std::ptr::null_mut());
        }
    }
}

fn ns_window(window: &gpui_kit::Window) -> Option<Id> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(h) = handle.as_raw() else { return None };
    let w = unsafe { send0(h.ns_view.as_ptr(), c"window") };
    (!w.is_null()).then_some(w)
}

#[cfg(test)]
mod tests {
    #[test]
    fn menu_bar_icon_renders() {
        let png = super::icon_png(36);
        let img = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(img.height(), 36);
        let opaque = img.pixels().filter(|p| p[3] > 200).count();
        let total = (img.width() * img.height()) as usize;
        assert!(opaque > total / 5 && opaque < total * 4 / 5, "a silhouette with cut-outs: {opaque}/{total}");
    }
}
