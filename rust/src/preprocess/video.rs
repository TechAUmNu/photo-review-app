//! Burst → MP4 rendering by piping cached preview JPEGs into ffmpeg.
//! Encoded once at final export quality; review playback and "keep video"
//! export both use the same file.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

/// Locate ffmpeg: explicit setting > bundled binary next to the app > PATH.
pub fn find_ffmpeg(setting: Option<&str>) -> Result<PathBuf> {
    if let Some(p) = setting {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Ok(p);
        }
        bail!("configured ffmpeg path does not exist: {}", p.display());
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let bundled = dir.join("ffmpeg");
            if bundled.is_file() {
                return Ok(bundled);
            }
        }
    }
    which_ffmpeg().context(
        "ffmpeg not found: install it (brew install ffmpeg) or set its path in settings",
    )
}

fn which_ffmpeg() -> Result<PathBuf> {
    let out = Command::new("/usr/bin/which").arg("ffmpeg").output()?;
    if !out.status.success() {
        // Common Homebrew locations aren't always on the GUI app PATH.
        for candidate in ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg"] {
            if Path::new(candidate).is_file() {
                return Ok(PathBuf::from(candidate));
            }
        }
        bail!("ffmpeg not on PATH");
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    ))
}

pub struct VideoJob<'a> {
    pub ffmpeg: &'a Path,
    /// Ordered cached preview JPEGs, one per frame.
    pub frame_paths: &'a [PathBuf],
    pub fps: f64,
    pub out_path: &'a Path,
}

/// Render a CFR MP4 from the frame stills. Frames are piped through stdin
/// (image2pipe), so no temp files or filename escaping.
pub fn render_video(job: &VideoJob) -> Result<()> {
    if job.frame_paths.is_empty() {
        bail!("burst has no frames");
    }
    if let Some(parent) = job.out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Clamp to sane bounds; A9III bursts are ~5-120 fps.
    let fps = job.fps.clamp(1.0, 240.0);
    let tmp = job.out_path.with_extension("mp4.tmp");

    let mut child = Command::new(job.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "image2pipe"])
        .args(["-framerate", &format!("{fps:.3}")])
        .args(["-i", "-"])
        // x264 needs even dimensions; round up so tiny/odd sizes stay valid.
        .args(["-vf", "scale=ceil(iw/2)*2:ceil(ih/2)*2"])
        .args(["-c:v", "libx264", "-preset", "medium", "-crf", "18"])
        .args(["-pix_fmt", "yuv420p", "-movflags", "+faststart"])
        .args(["-r", &format!("{fps:.3}")]) // force CFR at source rate
        .args(["-f", "mp4"]) // .tmp suffix defeats extension inference
        .arg(&tmp)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning {}", job.ffmpeg.display()))?;

    {
        let stdin = child.stdin.as_mut().context("ffmpeg stdin unavailable")?;
        for frame in job.frame_paths {
            let bytes = std::fs::read(frame)
                .with_context(|| format!("reading frame {}", frame.display()))?;
            stdin.write_all(&bytes)?;
        }
    } // drop closes stdin -> ffmpeg finishes

    let output = child.wait_with_output()?;
    if !output.status.success() {
        let _ = std::fs::remove_file(&tmp);
        bail!(
            "ffmpeg failed for {}: {}",
            job.out_path.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    std::fs::rename(&tmp, job.out_path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, RgbImage};

    #[test]
    fn renders_mp4_from_frames() {
        let Ok(ffmpeg) = find_ffmpeg(None) else {
            eprintln!("ffmpeg not available; skipping");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let mut frames = Vec::new();
        for i in 0..12u32 {
            let img = DynamicImage::ImageRgb8(RgbImage::from_fn(640, 360, |x, _| {
                image::Rgb([((x + i * 40) % 256) as u8, 80, 160])
            }));
            let p = dir.path().join(format!("f{i:03}.jpg"));
            img.save(&p).unwrap();
            frames.push(p);
        }
        let out = dir.path().join("videos/burst_1.mp4");
        render_video(&VideoJob {
            ffmpeg: &ffmpeg,
            frame_paths: &frames,
            fps: 24.0,
            out_path: &out,
        })
        .unwrap();
        let meta = std::fs::metadata(&out).unwrap();
        assert!(meta.len() > 1000, "mp4 too small: {} bytes", meta.len());
    }
}
