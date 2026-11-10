//! Screenshots: capture (region, full screen, window), the saved-files library, and the
//! thumbnail cards shown after a capture.

pub mod capture;
pub mod cards;
pub mod library;
pub mod picker;
pub mod watch;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Mode {
    #[default]
    Region,
    Full,
    Window,
}

impl Mode {
    pub const ALL: [Mode; 3] = [Mode::Region, Mode::Full, Mode::Window];
    pub fn label(self) -> &'static str {
        match self {
            Mode::Region => "Region",
            Mode::Full => "Full screen",
            Mode::Window => "Window",
        }
    }
}

pub const DELAYS: [u8; 4] = [0, 3, 5, 10];

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Settings {
    pub mode: Mode,
    /// Seconds before the capture (0 = off).
    pub delay: u8,
    pub cursor: bool,
    pub folder: PathBuf,
    /// Show Clipper's card for screenshots taken with macOS's own shortcuts.
    #[serde(default = "yes")]
    pub detect_system: bool,
}

impl Default for Settings {
    fn default() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        Self { mode: Mode::Region, delay: 0, cursor: false, folder: PathBuf::from(home).join("Pictures/Clipper"), detect_system: true }
    }
}

fn yes() -> bool {
    true
}

fn settings_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join("Library/Application Support/Clipper/screenshots.json")
}

impl Settings {
    pub fn load() -> Self {
        std::fs::read_to_string(settings_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let path = settings_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, serde_json::to_string_pretty(self).unwrap_or_default());
    }
}
