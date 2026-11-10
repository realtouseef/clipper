//! Compiles the ScreenCaptureKit helpers next to the build and tells the app where they are:
//! the screen recorder (`helpers/capture.swift`, `CLIPPER_CAPTURE_HELPER`) and the
//! screenshot taker (`helpers/shot.swift`, `CLIPPER_SHOT_HELPER`).

use std::path::PathBuf;
use std::process::Command;

fn main() {
    compile("helpers/capture.swift", "clipper-capture", "CLIPPER_CAPTURE_HELPER");
    compile("helpers/shot.swift", "clipper-shot", "CLIPPER_SHOT_HELPER");
}

fn compile(src: &str, name: &str, env: &str) {
    println!("cargo:rerun-if-changed={src}");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join(name);
    let arch = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86_64") => "x86_64",
        _ => "arm64",
    };
    let status = Command::new("swiftc")
        .args(["-O", "-target", &format!("{arch}-apple-macos15.0"), src, "-o"])
        .arg(&out)
        .status()
        .expect("swiftc not found (install the Xcode command line tools)");
    assert!(status.success(), "failed to compile {src}");
    println!("cargo:rustc-env={env}={}", out.display());
}
