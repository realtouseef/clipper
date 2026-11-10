//! Spots screenshots taken with macOS's own shortcuts (⌘⇧3 / ⌘⇧4 / ⌘⇧5) as soon as they're
//! saved, so Clipper can show its card for them. Watches the folder macOS saves to (its
//! "location" setting, the Desktop by default).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// What macOS screenshot files start with (its "name" setting, else the defaults; older
/// macOS versions used "Screen Shot").
fn prefixes() -> Vec<String> {
    let mut p = vec!["Screenshot".to_string(), "Screen Shot".to_string()];
    if let Some(name) = read_default("name").filter(|n| !n.is_empty()) {
        p.insert(0, name);
    }
    p
}

fn read_default(key: &str) -> Option<String> {
    let out = Command::new("defaults").args(["read", "com.apple.screencapture", key]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Where macOS saves screenshots.
pub fn system_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    match read_default("location") {
        Some(loc) if !loc.is_empty() => {
            let p = loc.strip_prefix("~/").map(|rest| PathBuf::from(&home).join(rest)).unwrap_or_else(|| PathBuf::from(&loc));
            if p.is_dir() { p } else { PathBuf::from(home).join("Desktop") }
        }
        _ => PathBuf::from(home).join("Desktop"),
    }
}

/// A macOS screenshot file (not a hidden temp file, not one of Clipper's own `Screenshot_…`).
pub fn is_system_screenshot(name: &str, prefixes: &[String]) -> bool {
    let lower = name.to_lowercase();
    let image = [".png", ".jpg", ".jpeg", ".heic", ".tiff"].iter().any(|e| lower.ends_with(e));
    image && !name.starts_with('.') && !name.starts_with("Screenshot_") && prefixes.iter().any(|p| name.starts_with(p.as_str()))
}

pub struct Watcher {
    dir: PathBuf,
    prefixes: Vec<String>,
    seen: HashSet<PathBuf>,
}

impl Watcher {
    /// Starts watching; screenshots already there don't count.
    pub fn new() -> Self {
        Self::in_dir(system_dir(), prefixes())
    }

    pub fn in_dir(dir: PathBuf, prefixes: Vec<String>) -> Self {
        let seen = list(&dir, &prefixes).into_iter().collect();
        Self { dir, prefixes, seen }
    }

    /// Screenshots saved since the last call (macOS writes a hidden temp file and renames it,
    /// so a visible file is complete).
    pub fn poll(&mut self) -> Vec<PathBuf> {
        let mut new: Vec<PathBuf> = list(&self.dir, &self.prefixes).into_iter().filter(|p| !self.seen.contains(p)).collect();
        new.sort();
        self.seen.extend(new.iter().cloned());
        new
    }
}

fn list(dir: &Path, prefixes: &[String]) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_str().is_some_and(|n| is_system_screenshot(n, prefixes)))
        .map(|e| e.path())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spots_new_macos_screenshots_only() {
        let p = vec!["Screenshot".to_string(), "Screen Shot".to_string()];
        assert!(is_system_screenshot("Screenshot 2026-11-08 at 12.46.51\u{202f}AM.png", &p));
        assert!(is_system_screenshot("Screen Shot 2020-01-01 at 10.00.00.jpg", &p));
        assert!(!is_system_screenshot(".Screenshot 2026-11-08 at 12.46.51 AM.png", &p), "temp file");
        assert!(!is_system_screenshot("Screenshot_20261108-104455.png", &p), "Clipper's own");
        assert!(!is_system_screenshot("Screen Recording 2026-11-08.mov", &p));
        assert!(!is_system_screenshot("holiday.png", &p));

        let dir = std::env::temp_dir().join(format!("clipper-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Screenshot old.png"), b"x").unwrap();
        let mut w = Watcher::in_dir(dir.clone(), p);
        assert!(w.poll().is_empty(), "existing files don't count");
        std::fs::write(dir.join(".Screenshot new.png"), b"x").unwrap();
        assert!(w.poll().is_empty(), "still being written");
        std::fs::rename(dir.join(".Screenshot new.png"), dir.join("Screenshot new.png")).unwrap();
        assert_eq!(w.poll(), vec![dir.join("Screenshot new.png")]);
        assert!(w.poll().is_empty(), "reported once");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
