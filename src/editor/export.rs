//! Encoding the rendered document. Exports carry pixels only: no EXIF, no text chunks, no
//! embedded thumbnail (the encoders here write none), so nothing of the original survives
//! under a redaction.

use anyhow::{Result, anyhow};
use image::{ImageEncoder, RgbaImage};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Format {
    #[default]
    Png,
    Jpg,
    Webp,
}

impl Format {
    pub const ALL: [Format; 3] = [Format::Png, Format::Jpg, Format::Webp];
    pub fn ext(self) -> &'static str {
        match self {
            Format::Png => "png",
            Format::Jpg => "jpg",
            Format::Webp => "webp",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Format::Png => "PNG",
            Format::Jpg => "JPG",
            Format::Webp => "WebP",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
pub struct Settings {
    pub format: Format,
    /// JPG / WebP quality, 1..=100.
    pub quality: u8,
    /// 1 or 2.
    pub scale: u8,
}

impl Default for Settings {
    fn default() -> Self {
        Self { format: Format::Png, quality: 90, scale: 1 }
    }
}

pub fn encode(img: &RgbaImage, format: Format, quality: u8) -> Result<Vec<u8>> {
    let (w, h) = img.dimensions();
    let mut out = Vec::new();
    match format {
        Format::Png => {
            image::codecs::png::PngEncoder::new(&mut out).write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgba8)?;
        }
        Format::Jpg => {
            // No alpha in JPG: flatten onto white.
            let rgb: Vec<u8> = img
                .pixels()
                .flat_map(|p| {
                    let a = p[3] as u16;
                    [0, 1, 2].map(|c| ((p[c] as u16 * a + 255 * (255 - a)) / 255) as u8)
                })
                .collect();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality.clamp(1, 100))
                .write_image(&rgb, w, h, image::ExtendedColorType::Rgb8)?;
        }
        Format::Webp => unsafe {
            let mut ptr: *mut u8 = std::ptr::null_mut();
            let len = libwebp_sys::WebPEncodeRGBA(img.as_raw().as_ptr(), w as i32, h as i32, (w * 4) as i32, quality.clamp(1, 100) as f32, &mut ptr);
            if len == 0 || ptr.is_null() {
                return Err(anyhow!("WebP encoding failed"));
            }
            out.extend_from_slice(std::slice::from_raw_parts(ptr, len));
            libwebp_sys::WebPFree(ptr as *mut std::ffi::c_void);
        },
    }
    Ok(out)
}

/// `<name>_edited_<timestamp>.<ext>` in `dir`, made unique with `-2`, `-3`… if taken.
pub fn output_path(dir: &Path, name: &str, stamp: &str, format: Format) -> PathBuf {
    let base = format!("{name}_edited_{stamp}");
    let mut path = dir.join(format!("{base}.{}", format.ext()));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{base}-{n}.{}", format.ext()));
        n += 1;
    }
    path
}

#[cfg(test)]
mod tests {
    use super::super::doc::*;
    use super::super::render::{Cache, render};
    use super::*;

    /// PNG chunk types in a file.
    fn png_chunks(data: &[u8]) -> Vec<String> {
        let mut out = Vec::new();
        let mut i = 8;
        while i + 8 <= data.len() {
            let len = u32::from_be_bytes(data[i..i + 4].try_into().unwrap()) as usize;
            out.push(String::from_utf8_lossy(&data[i + 4..i + 8]).to_string());
            i += 12 + len;
        }
        out
    }

    #[test]
    fn exported_files_hold_no_original_pixels_under_redactions() {
        // Text-like original.
        let mut base = RgbaImage::from_pixel(900, 400, image::Rgba([255, 255, 255, 255]));
        let t = super::super::text::rasterize(&"account 4242 4242 4242 4242\n".repeat(8), "", 30.0, 400, [0, 0, 0, 255], Align::Left).unwrap();
        image::imageops::overlay(&mut base, &t.image, 8, 8);
        let area = r(8.0, 8.0, 600.0, 250.0);
        for effect in [Effect::Blur(2.0), Effect::Pixelate(2.0), Effect::Solid([0, 0, 0, 255])] {
            let mut d = Document::new("a.png", (900, 400));
            d.add(Kind::Redact(Redact { area: Area::Rect(area), effect }));
            let out = render(&d, &base, 1.0, &Cache::default());
            for format in Format::ALL {
                let bytes = encode(&out, format, 90).unwrap();
                let back = image::load_from_memory(&bytes).unwrap().to_rgba8();
                assert_eq!(back.dimensions(), base.dimensions());
                // Compare the redacted area with the original: it must differ beyond a
                // threshold wherever the original had text.
                let inner = area.inflate(-6.0);
                let (mut ink, mut close) = (0, 0);
                let mut diff = 0.0;
                let mut n = 0.0;
                for y in inner.y as u32..inner.bottom() as u32 {
                    for x in inner.x as u32..inner.right() as u32 {
                        let (o, e) = (base.get_pixel(x, y), back.get_pixel(x, y));
                        let dv = (0..3).map(|k| (o[k] as f32 - e[k] as f32).abs()).sum::<f32>() / 3.0;
                        diff += dv;
                        n += 1.0;
                        if o[0] < 60 {
                            ink += 1;
                            if dv < 8.0 && !matches!(effect, Effect::Solid(_)) {
                                close += 1;
                            }
                        }
                    }
                }
                assert!(diff / n > 20.0, "{effect:?} {format:?}: mean diff {}", diff / n);
                assert!(close * 50 < ink, "{effect:?} {format:?}: {close} of {ink} ink pixels kept");
            }
        }
    }

    #[test]
    fn exports_carry_no_metadata() {
        let img = RgbaImage::from_pixel(32, 32, image::Rgba([1, 2, 3, 255]));
        let png = encode(&img, Format::Png, 90).unwrap();
        assert!(png_chunks(&png).iter().all(|c| ["IHDR", "IDAT", "IEND"].contains(&c.as_str())), "{:?}", png_chunks(&png));
        let jpg = encode(&img, Format::Jpg, 80).unwrap();
        // No APP1 (EXIF/XMP) segment.
        assert!(!jpg.windows(2).any(|w| w == [0xff, 0xe1]));
        let webp = encode(&img, Format::Webp, 80).unwrap();
        assert_eq!(&webp[0..4], b"RIFF");
        for chunk in [b"EXIF", b"XMP ", b"ICCP"] {
            assert!(!webp.windows(4).any(|w| w == chunk));
        }
    }

    #[test]
    fn quality_and_names() {
        let mut img = RgbaImage::new(200, 200);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = image::Rgba([(x * 7 % 255) as u8, (y * 13 % 255) as u8, ((x ^ y) % 255) as u8, 255]);
        }
        let lo = encode(&img, Format::Jpg, 20).unwrap().len();
        let hi = encode(&img, Format::Jpg, 95).unwrap().len();
        assert!(lo < hi);
        let lo = encode(&img, Format::Webp, 20).unwrap().len();
        let hi = encode(&img, Format::Webp, 95).unwrap().len();
        assert!(lo < hi);
        let dir = std::env::temp_dir().join(format!("clipper-names-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = output_path(&dir, "shot", "20261107-101010", Format::Png);
        assert_eq!(a.file_name().unwrap(), "shot_edited_20261107-101010.png");
        std::fs::write(&a, b"x").unwrap();
        assert_eq!(output_path(&dir, "shot", "20261107-101010", Format::Png).file_name().unwrap(), "shot_edited_20261107-101010-2.png");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
