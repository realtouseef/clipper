//! System-wide hotkeys via Carbon's `RegisterEventHotKey` (works while other apps are
//! focused and needs no Accessibility permission). Configured in
//! `~/Library/Application Support/Clipper/hotkeys.conf`.

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    PauseResume,
    Stop,
    /// Always registered (not only while recording).
    Screenshot,
}

impl Action {
    /// Registered while recording.
    const ALL: [Action; 2] = [Action::PauseResume, Action::Stop];
    const EVERY: [Action; 3] = [Action::PauseResume, Action::Stop, Action::Screenshot];

    fn id(self) -> u32 {
        match self {
            Action::PauseResume => 1,
            Action::Stop => 2,
            Action::Screenshot => 3,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Action::PauseResume => "pause",
            Action::Stop => "stop",
            Action::Screenshot => "screenshot",
        }
    }

    fn default_combo(self) -> &'static str {
        match self {
            Action::PauseResume => "opt+shift+p",
            Action::Stop => "opt+shift+s",
            Action::Screenshot => "ctrl+shift+s",
        }
    }
}

// Carbon modifier masks.
const CMD: u32 = 1 << 8;
const SHIFT: u32 = 1 << 9;
const OPT: u32 = 1 << 11;
const CTRL: u32 = 1 << 12;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Hotkey {
    code: u32,
    mods: u32,
    label: char,
}

impl Hotkey {
    /// Parses e.g. `opt+shift+p`, `cmd+ctrl+f9`.
    pub fn parse(text: &str) -> Option<Self> {
        let mut mods = 0;
        let mut key = None;
        for part in text.split('+').map(|p| p.trim().to_lowercase()) {
            match part.as_str() {
                "cmd" | "command" | "⌘" => mods |= CMD,
                "shift" | "⇧" => mods |= SHIFT,
                "opt" | "option" | "alt" | "⌥" => mods |= OPT,
                "ctrl" | "control" | "⌃" => mods |= CTRL,
                k => key = Some(keycode(k)?),
            }
        }
        let (code, label) = key?;
        // A bare key would swallow normal typing system-wide.
        (mods != 0).then_some(Self { code, mods, label })
    }

    /// Mac-style label, e.g. "⌥⇧P".
    pub fn label(&self) -> String {
        let mut s = String::new();
        for (m, c) in [(CTRL, '⌃'), (OPT, '⌥'), (SHIFT, '⇧'), (CMD, '⌘')] {
            if self.mods & m != 0 {
                s.push(c);
            }
        }
        if self.label == 'F' {
            s.push_str(&format!("F{}", f_number(self.code)));
        } else {
            s.push(self.label);
        }
        s
    }
}

/// ANSI virtual key codes.
fn keycode(k: &str) -> Option<(u32, char)> {
    const LETTERS: [(char, u32); 26] = [
        ('a', 0), ('b', 11), ('c', 8), ('d', 2), ('e', 14), ('f', 3), ('g', 5), ('h', 4),
        ('i', 34), ('j', 38), ('k', 40), ('l', 37), ('m', 46), ('n', 45), ('o', 31), ('p', 35),
        ('q', 12), ('r', 15), ('s', 1), ('t', 17), ('u', 32), ('v', 9), ('w', 13), ('x', 7),
        ('y', 16), ('z', 6),
    ];
    const DIGITS: [u32; 10] = [29, 18, 19, 20, 21, 23, 22, 26, 28, 25];
    if k == "space" {
        return Some((49, '␣'));
    }
    if let Some(n) = k.strip_prefix('f').and_then(|n| n.parse::<usize>().ok()) {
        return F_KEYS.get(n.checked_sub(1)?).map(|&c| (c, 'F'));
    }
    let mut chars = k.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    if let Some(d) = c.to_digit(10) {
        return Some((DIGITS[d as usize], c));
    }
    LETTERS.iter().find(|(l, _)| *l == c).map(|(l, code)| (*code, l.to_ascii_uppercase()))
}

const F_KEYS: [u32; 12] = [122, 120, 99, 118, 96, 97, 98, 100, 101, 109, 103, 111];

fn f_number(code: u32) -> usize {
    F_KEYS.iter().position(|&c| c == code).map(|i| i + 1).unwrap_or(0)
}

#[derive(Clone, Debug)]
pub struct Config {
    pub pause: Hotkey,
    pub stop: Hotkey,
    pub screenshot: Hotkey,
}

impl Config {
    fn get(&self, a: Action) -> Hotkey {
        match a {
            Action::PauseResume => self.pause,
            Action::Stop => self.stop,
            Action::Screenshot => self.screenshot,
        }
    }
}

pub fn config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join("Library/Application Support/Clipper/hotkeys.conf")
}

/// Loads the config, writing a commented default file on first run. Invalid entries fall
/// back to the defaults.
pub fn load() -> Config {
    let path = config_path();
    let text = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        let mut s = String::from(
            "# Clipper global hotkeys (pause and stop work while recording; screenshot always).\n\
             # Modifiers: cmd, shift, opt, ctrl. Keys: a-z, 0-9, f1-f12, space.\n",
        );
        for a in Action::EVERY {
            s.push_str(&format!("{} = {}\n", a.key(), a.default_combo()));
        }
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&path, &s);
        s
    });
    let pick = |a: Action| {
        text.lines()
            .filter_map(|l| l.split_once('='))
            .find(|(k, _)| k.trim() == a.key())
            .and_then(|(_, v)| Hotkey::parse(v))
            .unwrap_or_else(|| Hotkey::parse(a.default_combo()).unwrap())
    };
    Config { pause: pick(Action::PauseResume), stop: pick(Action::Stop), screenshot: pick(Action::Screenshot) }
}

// ---------- Carbon FFI ----------

#[repr(C)]
#[derive(Clone, Copy)]
struct EventHotKeyID {
    signature: u32,
    id: u32,
}

#[repr(C)]
struct EventTypeSpec {
    event_class: u32,
    event_kind: u32,
}

type Handler = extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> i32;

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn GetApplicationEventTarget() -> *mut c_void;
    fn InstallEventHandler(
        target: *mut c_void,
        handler: Handler,
        num_types: u32,
        list: *const EventTypeSpec,
        user_data: *mut c_void,
        out_ref: *mut *mut c_void,
    ) -> i32;
    fn RegisterEventHotKey(
        code: u32,
        mods: u32,
        id: EventHotKeyID,
        target: *mut c_void,
        options: u32,
        out_ref: *mut *mut c_void,
    ) -> i32;
    fn UnregisterEventHotKey(hotkey: *mut c_void) -> i32;
    fn GetEventParameter(
        event: *mut c_void,
        name: u32,
        desired_type: u32,
        actual_type: *mut u32,
        size: usize,
        actual_size: *mut usize,
        data: *mut c_void,
    ) -> i32;
}

const fn fourcc(s: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*s)
}
const SIGNATURE: u32 = fourcc(b"CLPR");

/// Bit per action id, set by the Carbon handler and drained by `take_pressed`.
static PRESSED: AtomicU32 = AtomicU32::new(0);
static INSTALLED: Mutex<bool> = Mutex::new(false);

extern "C" fn on_hotkey(_next: *mut c_void, event: *mut c_void, _user: *mut c_void) -> i32 {
    let mut hk = EventHotKeyID { signature: 0, id: 0 };
    let status = unsafe {
        GetEventParameter(
            event,
            fourcc(b"----"),
            fourcc(b"hkid"),
            std::ptr::null_mut(),
            std::mem::size_of::<EventHotKeyID>(),
            std::ptr::null_mut(),
            &mut hk as *mut _ as *mut c_void,
        )
    };
    if status == 0 && hk.signature == SIGNATURE {
        PRESSED.fetch_or(1 << hk.id, Ordering::SeqCst);
    }
    0
}

/// Registered hotkeys; unregistered on drop. Must live on the main thread.
pub struct Registration {
    refs: Vec<*mut c_void>,
}

/// Registers the recording hotkeys. Returns which ones failed (e.g. taken by another app).
pub fn register(config: &Config) -> (Registration, Vec<String>) {
    register_actions(config, &Action::ALL)
}

/// Registers the screenshot hotkey (kept for the app's lifetime).
pub fn register_screenshot(config: &Config) -> (Registration, Vec<String>) {
    register_actions(config, &[Action::Screenshot])
}

fn register_actions(config: &Config, actions: &[Action]) -> (Registration, Vec<String>) {
    unsafe {
        let target = GetApplicationEventTarget();
        let mut installed = INSTALLED.lock().unwrap();
        if !*installed {
            let spec = EventTypeSpec { event_class: fourcc(b"keyb"), event_kind: 5 };
            InstallEventHandler(target, on_hotkey, 1, &spec, std::ptr::null_mut(), std::ptr::null_mut());
            *installed = true;
        }
        let mut refs = Vec::new();
        let mut failed = Vec::new();
        for &a in actions {
            let hk = config.get(a);
            let mut r = std::ptr::null_mut();
            let id = EventHotKeyID { signature: SIGNATURE, id: a.id() };
            if RegisterEventHotKey(hk.code, hk.mods, id, target, 0, &mut r) == 0 {
                refs.push(r);
            } else {
                failed.push(hk.label());
            }
        }
        (Registration { refs }, failed)
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        for r in self.refs.drain(..) {
            unsafe {
                UnregisterEventHotKey(r);
            }
        }
    }
}

/// Hotkey presses since the last call.
pub fn take_pressed() -> Vec<Action> {
    let bits = PRESSED.swap(0, Ordering::SeqCst);
    Action::EVERY.into_iter().filter(|a| bits & (1 << a.id()) != 0).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_combos() {
        assert_eq!(Hotkey::parse("opt+shift+p").unwrap().label(), "⌥⇧P");
        assert_eq!(Hotkey::parse("cmd + ctrl + F9").unwrap().label(), "⌃⌘F9");
        assert_eq!(Hotkey::parse("ctrl+5").unwrap().code, 23);
        assert!(Hotkey::parse("p").is_none());
        assert!(Hotkey::parse("cmd+pp").is_none());
    }
}
