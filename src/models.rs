//! Speech-to-text model files: the catalog, where they live, and downloads with resume and
//! SHA256 verification.
//!
//! Models are never bundled. They are fetched only when the user presses Download (the only
//! network use in the app) and stored in `~/Library/Application Support/Clipper/models`.
//! A download writes `<file>.part`, resumes it with an HTTP Range request after an
//! interruption or Cancel, and only becomes `<file>` once its SHA256 matches.

use anyhow::{Context as _, Result, anyhow};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;

#[cfg(test)]
const BASE: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/";

/// One downloadable model file.
#[derive(Debug, PartialEq, Eq)]
pub struct Model {
    pub id: &'static str,
    pub name: &'static str,
    pub file: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub size: u64,
    /// Approximate memory while transcribing.
    pub ram_mb: u32,
    pub english_only: bool,
}

macro_rules! model {
    ($id:literal, $name:literal, $file:literal, $sha:literal, $size:literal, $ram:literal, $en:literal) => {
        Model {
            id: $id,
            name: $name,
            file: $file,
            url: concat!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/", $file),
            sha256: $sha,
            size: $size,
            ram_mb: $ram,
            english_only: $en,
        }
    };
}

/// Speech models (whisper.cpp ggml, q5_1 quantized), smallest first.
pub const MODELS: [Model; 6] = [
    model!("tiny.en", "Tiny (English)", "ggml-tiny.en-q5_1.bin", "c77c5766f1cef09b6b7d47f21b546cbddd4157886b3b5d6d4f709e91e66c7c2b", 32166155, 120, true),
    model!("tiny", "Tiny (multilingual)", "ggml-tiny-q5_1.bin", "818710568da3ca15689e31a743197b520007872ff9576237bda97bd1b469c3d7", 32152673, 120, false),
    model!("base.en", "Base (English)", "ggml-base.en-q5_1.bin", "4baf70dd0d7c4247ba2b81fafd9c01005ac77c2f9ef064e00dcf195d0e2fdd2f", 59721011, 210, true),
    model!("base", "Base (multilingual)", "ggml-base-q5_1.bin", "422f1ae452ade6f30a004d7e5c6a43195e4433bc370bf23fac9cc591f01a8898", 59707625, 210, false),
    model!("small.en", "Small (English)", "ggml-small.en-q5_1.bin", "bfdff4894dcb76bbf647d56263ea2a96645423f1669176f4844a1bf8e478ad30", 190098681, 520, true),
    model!("small", "Small (multilingual)", "ggml-small-q5_1.bin", "ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb", 190085487, 520, false),
];

/// Silero voice activity detection for whisper.cpp; fetched along with any speech model.
pub const VAD: Model = Model {
    id: "silero-vad",
    name: "Silero VAD",
    file: "ggml-silero-v5.1.2.bin",
    url: "https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v5.1.2.bin",
    sha256: "29940d98d42b91fbd05ce489f3ecf7c72f0a42f027e4875919a28fb4c04ea2cf",
    size: 885098,
    ram_mb: 10,
    english_only: false,
};

/// The recommended model: small, fast and accurate enough for English narration.
pub const RECOMMENDED: &str = "base.en";

pub fn by_id(id: &str) -> Option<&'static Model> {
    MODELS.iter().find(|m| m.id == id)
}

/// `~/Library/Application Support/Clipper/models`.
pub fn models_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join("Library/Application Support/Clipper/models")
}

impl Model {
    pub fn path(&self) -> PathBuf {
        models_dir().join(self.file)
    }

    fn part_path(&self) -> PathBuf {
        models_dir().join(format!("{}.part", self.file))
    }

    /// Downloaded and verified (verified files are the only ones renamed into place).
    pub fn installed(&self) -> bool {
        std::fs::metadata(self.path()).is_ok_and(|m| m.len() == self.size)
    }

    /// Bytes of an interrupted download that a new Download resumes from.
    pub fn partial(&self) -> u64 {
        std::fs::metadata(self.part_path()).map(|m| m.len()).unwrap_or(0)
    }

    pub fn delete(&self) {
        let _ = std::fs::remove_file(self.path());
        let _ = std::fs::remove_file(self.part_path());
    }

    pub fn size_label(&self) -> String {
        format!("{} MB", (self.size as f64 / 1e6).round())
    }
}

/// A running download (a speech model plus the VAD model if it's missing).
pub struct Download {
    pub model: &'static Model,
    /// Bytes done and total, over every file in this download.
    pub done: Arc<AtomicU64>,
    pub total: u64,
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<Result<()>>>,
}

impl Download {
    pub fn start(model: &'static Model) -> Self {
        let files: Vec<&'static Model> = [&VAD, model].into_iter().filter(|m| !m.installed()).collect();
        let total = files.iter().map(|m| m.size).sum();
        let done = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (d, c) = (done.clone(), cancel.clone());
        let handle = std::thread::spawn(move || {
            std::fs::create_dir_all(models_dir())?;
            let client = client()?;
            let mut base = 0;
            for m in files {
                fetch(&client, m.url, &m.path(), m.size, m.sha256, &|n| d.store(base + n, Ordering::Relaxed), &c)?;
                base += m.size;
            }
            Ok(())
        });
        Self { model, done, total, cancel, handle: Some(handle) }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// 0..=1.
    pub fn fraction(&self) -> f32 {
        (self.done.load(Ordering::Relaxed) as f64 / self.total.max(1) as f64) as f32
    }

    /// `Some(result)` once the download thread has ended.
    pub fn poll(&mut self) -> Option<Result<()>> {
        if self.handle.as_ref().is_some_and(|h| h.is_finished()) {
            let h = self.handle.take()?;
            return Some(h.join().unwrap_or_else(|_| Err(anyhow!("download crashed"))));
        }
        None
    }
}

impl Drop for Download {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .user_agent("Clipper")
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(None)
        .build()
        .context("Could not start the downloader")
}

pub const CANCELLED: &str = "Download cancelled";

/// Downloads `url` to `dest` (via `dest.part`, resuming it if present), checks the size and
/// SHA256, and only then moves it into place. On a checksum mismatch the partial file is
/// deleted (a retry starts clean). On cancel or a dropped connection it's kept for resume.
fn fetch(
    client: &reqwest::blocking::Client,
    url: &str,
    dest: &Path,
    size: u64,
    sha256: &str,
    progress: &dyn Fn(u64),
    cancel: &AtomicBool,
) -> Result<()> {
    let part = dest.with_file_name(format!("{}.part", dest.file_name().unwrap().to_string_lossy()));
    let mut have = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    if have > size {
        let _ = std::fs::remove_file(&part);
        have = 0;
    }
    if have < size {
        let mut req = client.get(url);
        if have > 0 {
            req = req.header(reqwest::header::RANGE, format!("bytes={have}-"));
        }
        let mut resp = req.send().with_context(|| format!("Could not reach {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(anyhow!("Download failed: HTTP {status}"));
        }
        // 206: the server continues where we left off; 200: it sends the whole file.
        let resume = status == reqwest::StatusCode::PARTIAL_CONTENT && have > 0;
        if !resume {
            have = 0;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(resume)
            .truncate(!resume)
            .open(&part)
            .with_context(|| format!("Could not write {}", part.display()))?;
        let mut buf = vec![0u8; 256 * 1024];
        progress(have);
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(anyhow!(CANCELLED));
            }
            let n = match resp.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => return Err(anyhow!("Download interrupted: {e}")),
            };
            file.write_all(&buf[..n])?;
            have += n as u64;
            progress(have);
        }
        file.flush()?;
    }
    if have != size {
        return Err(anyhow!("Download interrupted ({have} of {size} bytes); press Download to resume"));
    }
    let actual = sha256_file(&part)?;
    if !actual.eq_ignore_ascii_case(sha256) {
        let _ = std::fs::remove_file(&part);
        return Err(anyhow!("Checksum mismatch: the download was corrupted and has been removed. Try again."));
    }
    std::fs::rename(&part, dest)?;
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;

    /// A one-file HTTP server with Range support. `cut` drops the connection after that many
    /// body bytes on the first request (an interrupted download).
    fn serve(body: Vec<u8>, cut: Option<usize>) -> (String, Arc<AtomicU64>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/model.bin", listener.local_addr().unwrap());
        let ranged = Arc::new(AtomicU64::new(0));
        let r = ranged.clone();
        std::thread::spawn(move || {
            let mut first = true;
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut from = 0usize;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    let lower = line.to_ascii_lowercase();
                    if let Some(v) = lower.strip_prefix("range: bytes=") {
                        from = v.trim().trim_end_matches('-').parse().unwrap_or(0);
                        r.fetch_add(1, Ordering::Relaxed);
                    }
                }
                let rest = &body[from..];
                let head = if from > 0 {
                    format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {from}-{}/{}\r\nConnection: close\r\n\r\n",
                        rest.len(),
                        body.len() - 1,
                        body.len()
                    )
                } else {
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", rest.len())
                };
                let _ = stream.write_all(head.as_bytes());
                let send = match (first, cut) {
                    (true, Some(c)) => &rest[..c.min(rest.len())],
                    _ => rest,
                };
                let _ = stream.write_all(send);
                let _ = stream.flush();
                first = false;
                // Dropping the stream closes the connection (mid-body when cut).
            }
        });
        (url, ranged)
    }

    fn body() -> (Vec<u8>, String) {
        let body: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let sha = Sha256::digest(&body).iter().map(|b| format!("{b:02x}")).collect();
        (body, sha)
    }

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("clipper-models-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("model.bin")
    }

    #[test]
    fn downloads_and_resumes_after_interruption() {
        let (body, sha) = body();
        let (url, ranged) = serve(body.clone(), Some(120_000));
        let dest = tmp("resume");
        let c = client().unwrap();
        let no = AtomicBool::new(false);
        // First attempt is cut off: error, partial kept, nothing in place.
        let err = fetch(&c, &url, &dest, body.len() as u64, &sha, &|_| {}, &no).unwrap_err();
        assert!(err.to_string().contains("interrupted"), "{err}");
        assert!(!dest.exists());
        let part = dest.with_file_name("model.bin.part");
        assert_eq!(std::fs::metadata(&part).unwrap().len(), 120_000);
        // Second attempt resumes with a Range request and completes.
        let last = AtomicU64::new(0);
        fetch(&c, &url, &dest, body.len() as u64, &sha, &|n| last.store(n, Ordering::Relaxed), &no).unwrap();
        assert_eq!(ranged.load(Ordering::Relaxed), 1, "no Range request on resume");
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        assert_eq!(last.load(Ordering::Relaxed), body.len() as u64);
        assert!(!part.exists());
    }

    #[test]
    fn bad_checksum_fails_safely() {
        let (body, _) = body();
        let (url, _) = serve(body.clone(), None);
        let dest = tmp("badsum");
        let wrong = "0".repeat(64);
        let err = fetch(&client().unwrap(), &url, &dest, body.len() as u64, &wrong, &|_| {}, &AtomicBool::new(false))
            .unwrap_err();
        assert!(err.to_string().contains("Checksum"), "{err}");
        // Nothing usable left behind: no model, no partial to resume from.
        assert!(!dest.exists());
        assert!(!dest.with_file_name("model.bin.part").exists());
    }

    #[test]
    fn cancel_keeps_partial_for_resume() {
        let (body, sha) = body();
        let (url, _) = serve(body.clone(), None);
        let dest = tmp("cancel");
        let cancel = AtomicBool::new(false);
        let err = fetch(
            &client().unwrap(),
            &url,
            &dest,
            body.len() as u64,
            &sha,
            &|n| {
                if n > 0 {
                    cancel.store(true, Ordering::Relaxed);
                }
            },
            &cancel,
        )
        .unwrap_err();
        assert_eq!(err.to_string(), CANCELLED);
        assert!(!dest.exists());
    }

    #[test]
    fn catalog() {
        assert!(by_id(RECOMMENDED).is_some_and(|m| m.english_only));
        for m in MODELS.iter().chain([&VAD]) {
            assert_eq!(m.sha256.len(), 64, "{}", m.id);
            assert!(m.url.ends_with(m.file));
        }
        assert!(MODELS[0].url.starts_with(BASE));
    }
}
