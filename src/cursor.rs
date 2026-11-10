//! Cursor tracking via CoreGraphics: polls position + left-button state at ~120 Hz.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use crate::clock::{self, Nanos};
use std::time::Duration;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventCreate(source: *const std::ffi::c_void) -> *mut std::ffi::c_void;
    fn CGEventGetLocation(event: *mut std::ffi::c_void) -> CGPoint;
    fn CGEventSourceButtonState(state_id: i32, button: u32) -> bool;
    fn CGDisplayBounds(display: u32) -> CGRect;
    fn CGGetActiveDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
    fn CGDisplayCopyDisplayMode(display: u32) -> *mut std::ffi::c_void;
    fn CGDisplayModeGetPixelWidth(mode: *mut std::ffi::c_void) -> usize;
    fn CGDisplayModeRelease(mode: *mut std::ffi::c_void);
    fn CGMainDisplayID() -> u32;
}

/// Every active display (main first), with its global bounds in points and scale factor.
pub fn displays() -> Vec<crate::region::Display> {
    let mut ids = [0u32; 16];
    let mut count = 0u32;
    unsafe {
        if CGGetActiveDisplayList(16, ids.as_mut_ptr(), &mut count) != 0 {
            return Vec::new();
        }
        let main = CGMainDisplayID();
        let mut list: Vec<crate::region::Display> = ids[..count as usize]
            .iter()
            .map(|&id| {
                let b = CGDisplayBounds(id);
                let mode = CGDisplayCopyDisplayMode(id);
                let px = if mode.is_null() { 0 } else { CGDisplayModeGetPixelWidth(mode) };
                if !mode.is_null() {
                    CGDisplayModeRelease(mode);
                }
                let scale = if px > 0 && b.size.width > 0.0 { px as f64 / b.size.width } else { 1.0 };
                crate::region::Display { id, origin: (b.origin.x, b.origin.y), size: (b.size.width, b.size.height), scale }
            })
            .collect();
        list.sort_by_key(|d| d.id != main);
        list
    }
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const std::ffi::c_void);
}

/// One cursor sample. `t` is seconds since tracking started; x/y are in display points.
#[derive(Clone, Copy, Debug)]
pub struct Sample {
    pub t: f64,
    pub x: f64,
    pub y: f64,
    pub down: bool,
}

pub(crate) fn cursor_location() -> (f64, f64) {
    unsafe {
        let ev = CGEventCreate(std::ptr::null());
        if ev.is_null() {
            return (0.0, 0.0);
        }
        let p = CGEventGetLocation(ev);
        CFRelease(ev);
        (p.x, p.y)
    }
}

/// Global bounds (x, y, w, h) in points of the n-th active display. avfoundation's
/// "Capture screen N" devices follow the same order as `CGGetActiveDisplayList`.
pub fn display_bounds(n: usize) -> Option<(f64, f64, f64, f64)> {
    let mut ids = [0u32; 16];
    let mut count = 0u32;
    unsafe {
        if CGGetActiveDisplayList(16, ids.as_mut_ptr(), &mut count) != 0 || n >= count as usize {
            return None;
        }
        let r = CGDisplayBounds(ids[n]);
        Some((r.origin.x, r.origin.y, r.size.width, r.size.height))
    }
}

pub struct Tracker {
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    samples: Arc<Mutex<Vec<(Nanos, Sample)>>>,
    handle: Option<JoinHandle<()>>,
}

impl Tracker {
    pub fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let paused = Arc::new(AtomicBool::new(false));
        let samples = Arc::new(Mutex::new(Vec::with_capacity(8192)));
        let (s, p, v) = (stop.clone(), paused.clone(), samples.clone());
        let handle = std::thread::spawn(move || {
            while !s.load(Ordering::Relaxed) {
                if p.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(8));
                    continue;
                }
                let (x, y) = cursor_location();
                // 0 = kCGEventSourceStateCombinedSessionState; buttons 0 = left, 1 = right.
                // Either counts as a click (both trigger the click highlight).
                let down = unsafe { CGEventSourceButtonState(0, 0) || CGEventSourceButtonState(0, 1) };
                v.lock().unwrap().push((clock::now(), Sample { t: 0.0, x, y, down }));
                std::thread::sleep(Duration::from_millis(8));
            }
        });
        Self { stop, paused, samples, handle: Some(handle) }
    }

    /// While paused no samples are logged.
    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
    }

    /// Stops tracking and returns the raw samples stamped on the shared host clock
    /// (`t` is unset).
    pub fn finish(mut self) -> Vec<(Nanos, Sample)> {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        std::mem::take(&mut *self.samples.lock().unwrap())
    }
}

impl Drop for Tracker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

pub fn write_csv(path: &std::path::Path, samples: &[Sample]) -> std::io::Result<()> {
    use std::fmt::Write as _;
    let mut s = String::from("t,x,y,down\n");
    for p in samples {
        let _ = writeln!(s, "{:.4},{:.1},{:.1},{}", p.t, p.x, p.y, p.down as u8);
    }
    std::fs::write(path, s)
}

