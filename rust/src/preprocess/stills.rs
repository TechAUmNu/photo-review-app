//! Still extraction: produce the 2048px preview and 320px thumb for one
//! photo, plus its sharpness score. Every output is keyed by the content
//! hash of the photo's preview-source file, making the pass resumable.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use image::codecs::jpeg::JpegEncoder;
use image::imageops;
use image::{DynamicImage, GrayImage, RgbImage};

use super::arw;
use crate::indexer::pairing::FileKind;

pub const PREVIEW_LONG_EDGE: u32 = 2048;
pub const THUMB_LONG_EDGE: u32 = 320;
pub const PREVIEW_QUALITY: u8 = 82;
pub const THUMB_QUALITY: u8 = 70;

pub struct StillPaths {
    pub preview: PathBuf,
    pub thumb: PathBuf,
}

pub fn still_paths(cache_dir: &Path, hash: &str) -> StillPaths {
    StillPaths {
        preview: cache_dir.join("previews").join(format!("{hash}.jpg")),
        thumb: cache_dir.join("thumbs").join(format!("{hash}.jpg")),
    }
}

pub struct StillOutcome {
    pub sharpness: Option<f64>,
    pub skipped: bool,
}

/// Generate preview + thumb for one photo if not already cached.
/// Returns the sharpness score (computed even on skip only if recompute
/// is needed — cached runs return None and keep the DB value).
pub fn process_still(
    source_root: &Path,
    cache_dir: &Path,
    rel_path: &str,
    kind: FileKind,
    hash: &str,
    orientation: Option<u16>,
) -> Result<StillOutcome> {
    let paths = still_paths(cache_dir, hash);
    if paths.preview.exists() && paths.thumb.exists() {
        return Ok(StillOutcome {
            sharpness: None,
            skipped: true,
        });
    }

    let src = source_root.join(rel_path);
    let preview_img = load_as_preview(&src, kind, &paths.preview)?;

    // Apply EXIF orientation so cached files are display-ready.
    let preview_img = apply_orientation(preview_img, orientation.unwrap_or(1));

    let preview_small = resize_long_edge(&preview_img, PREVIEW_LONG_EDGE);
    write_jpeg(&paths.preview, &preview_small, PREVIEW_QUALITY)?;

    let thumb = resize_long_edge(&preview_small, THUMB_LONG_EDGE);
    write_jpeg(&paths.thumb, &thumb, THUMB_QUALITY)?;

    let sharpness = variance_of_laplacian(&thumb.to_luma8());
    Ok(StillOutcome {
        sharpness: Some(sharpness),
        skipped: false,
    })
}

/// Load the source into a DynamicImage, whatever its container.
fn load_as_preview(src: &Path, kind: FileKind, preview_out: &Path) -> Result<DynamicImage> {
    match kind {
        FileKind::Jpeg => {
            image::open(src).with_context(|| format!("decoding {}", src.display()))
        }
        FileKind::Raw => {
            let jpeg = arw::extract_largest_jpeg(src)?;
            image::load_from_memory(&jpeg)
                .with_context(|| format!("decoding embedded preview of {}", src.display()))
        }
        FileKind::Heif => {
            // macOS: sips converts HEIF -> JPEG (hardware-adjacent path).
            // Convert at full preview size directly into the cache location,
            // then load it back for thumb + sharpness.
            if let Some(parent) = preview_out.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let status = Command::new("sips")
                .arg("-s")
                .arg("format")
                .arg("jpeg")
                .arg("-s")
                .arg("formatOptions")
                .arg(PREVIEW_QUALITY.to_string())
                .arg("-Z")
                .arg(PREVIEW_LONG_EDGE.to_string())
                .arg(src)
                .arg("--out")
                .arg(preview_out)
                .output()
                .context("running sips")?;
            if !status.status.success() {
                bail!(
                    "sips failed on {}: {}",
                    src.display(),
                    String::from_utf8_lossy(&status.stderr)
                );
            }
            image::open(preview_out)
                .with_context(|| format!("decoding sips output for {}", src.display()))
        }
    }
}

fn apply_orientation(img: DynamicImage, orientation: u16) -> DynamicImage {
    match orientation {
        3 => img.rotate180(),
        6 => img.rotate90(),
        8 => img.rotate270(),
        2 => img.fliph(),
        4 => img.flipv(),
        5 => img.rotate90().fliph(),
        7 => img.rotate270().fliph(),
        _ => img,
    }
}

fn resize_long_edge(img: &DynamicImage, long_edge: u32) -> DynamicImage {
    let (w, h) = (img.width(), img.height());
    if w.max(h) <= long_edge {
        return img.clone();
    }
    let (nw, nh) = if w >= h {
        (long_edge, (h as u64 * long_edge as u64 / w as u64).max(1) as u32)
    } else {
        ((w as u64 * long_edge as u64 / h as u64).max(1) as u32, long_edge)
    };
    // Triangle filter: good quality/speed balance for photo downscaling.
    img.resize_exact(nw, nh, imageops::FilterType::Triangle)
}

fn write_jpeg(path: &Path, img: &DynamicImage, quality: u8) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Write via a per-thread temp name then rename, so a killed run never
    // leaves a truncated file, and duplicate photos (same content hash on
    // several files -> same cache path) racing in parallel can't clobber
    // each other's temp file.
    let tmp = path.with_extension(format!("jpg.tmp{:?}", std::thread::current().id()));
    {
        let mut out = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
        let rgb: RgbImage = img.to_rgb8();
        JpegEncoder::new_with_quality(&mut out, quality).encode(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            image::ExtendedColorType::Rgb8,
        )?;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        // Lost a race against an identical-content writer: that's fine.
        if !path.exists() {
            return Err(e.into());
        }
    }
    Ok(())
}

/// Variance of the 4-neighbour Laplacian: the standard cheap focus measure.
/// Higher = sharper. Comparable only between frames of similar content,
/// which is exactly the within-burst use case.
pub fn variance_of_laplacian(gray: &GrayImage) -> f64 {
    let (w, h) = gray.dimensions();
    if w < 3 || h < 3 {
        return 0.0;
    }
    let px = |x: u32, y: u32| gray.get_pixel(x, y)[0] as f64;
    let mut sum = 0.0;
    let mut sum_sq = 0.0;
    let n = ((w - 2) * (h - 2)) as f64;
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let lap = px(x - 1, y) + px(x + 1, y) + px(x, y - 1) + px(x, y + 1)
                - 4.0 * px(x, y);
            sum += lap;
            sum_sq += lap * lap;
        }
    }
    let mean = sum / n;
    sum_sq / n - mean * mean
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sharpness_ranks_sharp_above_blurred() {
        // Checkerboard = sharp edges; flat gray = no edges.
        let sharp = GrayImage::from_fn(64, 64, |x, y| {
            image::Luma([if (x + y) % 2 == 0 { 0 } else { 255 }])
        });
        let flat = GrayImage::from_fn(64, 64, |_, _| image::Luma([128]));
        assert!(variance_of_laplacian(&sharp) > variance_of_laplacian(&flat));
    }

    #[test]
    fn jpeg_roundtrip_and_resume_skip() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("card");
        let cache = dir.path().join("cache");
        std::fs::create_dir_all(&root).unwrap();

        // A real (tiny) JPEG source.
        let img = DynamicImage::ImageRgb8(RgbImage::from_fn(4000, 3000, |x, _| {
            image::Rgb([(x % 256) as u8, 100, 50])
        }));
        write_jpeg(&root.join("DSC1.JPG"), &img, 90).unwrap();

        let out = process_still(&root, &cache, "DSC1.JPG", FileKind::Jpeg, "abc123", Some(1))
            .unwrap();
        assert!(!out.skipped);
        assert!(out.sharpness.is_some());

        let paths = still_paths(&cache, "abc123");
        let preview = image::open(&paths.preview).unwrap();
        assert_eq!(preview.width(), 2048); // long edge resized
        assert!(paths.thumb.exists());

        // Second run: cached -> skipped.
        let again = process_still(&root, &cache, "DSC1.JPG", FileKind::Jpeg, "abc123", Some(1))
            .unwrap();
        assert!(again.skipped);
    }

    #[test]
    fn orientation_rotates_dimensions() {
        let img = DynamicImage::ImageRgb8(RgbImage::new(400, 200));
        let rotated = apply_orientation(img, 6);
        assert_eq!((rotated.width(), rotated.height()), (200, 400));
    }
}
