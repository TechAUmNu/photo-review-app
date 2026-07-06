//! Still extraction: produce the 2048px preview and 320px thumb for one
//! photo, plus its sharpness score. Every output is keyed by the content
//! hash of the photo's preview-source file, making the pass resumable.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use fast_image_resize::images::{Image as FirImage, ImageRef};
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
use image::codecs::jpeg::JpegEncoder;
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
/// `ffmpeg` is only needed to decode HEIF on non-macOS platforms.
pub fn process_still(
    source_root: &Path,
    cache_dir: &Path,
    rel_path: &str,
    kind: FileKind,
    hash: &str,
    orientation: Option<u16>,
    ffmpeg: Option<&Path>,
) -> Result<StillOutcome> {
    let paths = still_paths(cache_dir, hash);
    if paths.preview.exists() && paths.thumb.exists() {
        return Ok(StillOutcome {
            sharpness: None,
            skipped: true,
        });
    }

    let src = source_root.join(rel_path);
    let full = load_as_preview(&src, kind, &paths.preview, ffmpeg)?.into_rgb8();

    // Resize FIRST (SIMD), then rotate the small result — rotating the
    // full 33MP frame costs more than the resize itself.
    let preview_small = resize_long_edge(&full, PREVIEW_LONG_EDGE)?;
    drop(full);
    let preview_small =
        apply_orientation(DynamicImage::ImageRgb8(preview_small), orientation.unwrap_or(1))
            .into_rgb8();
    write_jpeg(&paths.preview, &preview_small, PREVIEW_QUALITY)?;

    let thumb = resize_long_edge(&preview_small, THUMB_LONG_EDGE)?;
    write_jpeg(&paths.thumb, &thumb, THUMB_QUALITY)?;

    let sharpness =
        variance_of_laplacian(&DynamicImage::ImageRgb8(thumb).to_luma8());
    Ok(StillOutcome {
        sharpness: Some(sharpness),
        skipped: false,
    })
}

/// Load the source into a DynamicImage, whatever its container.
fn load_as_preview(
    src: &Path,
    kind: FileKind,
    preview_out: &Path,
    ffmpeg: Option<&Path>,
) -> Result<DynamicImage> {
    match kind {
        FileKind::Jpeg => {
            image::open(src).with_context(|| format!("decoding {}", src.display()))
        }
        FileKind::Raw => {
            let jpeg = arw::extract_largest_jpeg(src)?;
            image::load_from_memory(&jpeg)
                .with_context(|| format!("decoding embedded preview of {}", src.display()))
        }
        FileKind::Heif => decode_heif(src, preview_out, ffmpeg),
    }
}

/// HEIF -> JPEG written straight into the preview cache slot, then loaded
/// back for thumb + sharpness. macOS uses sips (always present, hardware
/// path); other platforms use ffmpeg (needs HEIF-enabled build, ffmpeg 6+).
fn decode_heif(src: &Path, preview_out: &Path, ffmpeg: Option<&Path>) -> Result<DynamicImage> {
    if let Some(parent) = preview_out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if cfg!(target_os = "macos") {
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
    } else {
        let ffmpeg = ffmpeg.context(
            "HEIF file found but ffmpeg is unavailable (required for HEIF on this platform)",
        )?;
        let scale = format!(
            "scale='min({0},iw)':'min({0},ih)':force_original_aspect_ratio=decrease",
            PREVIEW_LONG_EDGE
        );
        let status = Command::new(ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .arg("-i")
            .arg(src)
            .args(["-frames:v", "1", "-vf", &scale, "-q:v", "3"])
            .arg(preview_out)
            .output()
            .context("running ffmpeg for HEIF decode")?;
        if !status.status.success() {
            bail!(
                "ffmpeg failed decoding HEIF {}: {}",
                src.display(),
                String::from_utf8_lossy(&status.stderr)
            );
        }
    }
    image::open(preview_out)
        .with_context(|| format!("decoding converted HEIF for {}", src.display()))
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

/// SIMD downscale (fast_image_resize) — the hot loop of the stills pass.
fn resize_long_edge(rgb: &RgbImage, long_edge: u32) -> Result<RgbImage> {
    let (w, h) = rgb.dimensions();
    if w.max(h) <= long_edge {
        return Ok(rgb.clone());
    }
    let (nw, nh) = if w >= h {
        (long_edge, (h as u64 * long_edge as u64 / w as u64).max(1) as u32)
    } else {
        ((w as u64 * long_edge as u64 / h as u64).max(1) as u32, long_edge)
    };
    let src = ImageRef::new(w, h, rgb.as_raw(), PixelType::U8x3)
        .context("wrapping source for resize")?;
    let mut dst = FirImage::new(nw, nh, PixelType::U8x3);
    Resizer::new()
        .resize(
            &src,
            &mut dst,
            // Bilinear ≈ the Triangle filter previously used; plenty for
            // preview downscaling and much faster than Lanczos.
            &ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Bilinear)),
        )
        .context("resizing")?;
    RgbImage::from_raw(nw, nh, dst.into_vec()).context("rebuilding resized image")
}

fn write_jpeg(path: &Path, img: &RgbImage, quality: u8) -> Result<()> {
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
        JpegEncoder::new_with_quality(&mut out, quality).encode(
            img.as_raw(),
            img.width(),
            img.height(),
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
        let img = RgbImage::from_fn(4000, 3000, |x, _| {
            image::Rgb([(x % 256) as u8, 100, 50])
        });
        write_jpeg(&root.join("DSC1.JPG"), &img, 90).unwrap();

        let out = process_still(
            &root, &cache, "DSC1.JPG", FileKind::Jpeg, "abc123", Some(1), None,
        )
        .unwrap();
        assert!(!out.skipped);
        assert!(out.sharpness.is_some());

        let paths = still_paths(&cache, "abc123");
        let preview = image::open(&paths.preview).unwrap();
        assert_eq!(preview.width(), 2048); // long edge resized
        assert!(paths.thumb.exists());

        // Second run: cached -> skipped.
        let again = process_still(
            &root, &cache, "DSC1.JPG", FileKind::Jpeg, "abc123", Some(1), None,
        )
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
