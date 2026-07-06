//! Burst → MP4 rendering by piping cached preview JPEGs into ffmpeg.
//! Encoded once at final export quality; review playback and "keep video"
//! export both use the same file.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

const FFMPEG_EXE: &str = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };

/// Locate ffmpeg: explicit setting > bundled binary next to the app >
/// PATH > common install locations. Cross-platform (macOS/Windows/Linux).
pub fn find_ffmpeg(setting: Option<&str>) -> Result<PathBuf> {
    if let Some(p) = setting.filter(|s| !s.trim().is_empty()) {
        let p = PathBuf::from(p.trim());
        if p.is_file() {
            return Ok(p);
        }
        bail!("configured ffmpeg path does not exist: {}", p.display());
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let bundled = dir.join(FFMPEG_EXE);
            if bundled.is_file() {
                return Ok(bundled);
            }
        }
    }
    // PATH search, no external `which`/`where` needed.
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(FFMPEG_EXE);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    // GUI apps often miss package-manager PATH entries.
    let candidates: &[PathBuf] = &[
        #[cfg(target_os = "macos")]
        PathBuf::from("/opt/homebrew/bin/ffmpeg"),
        #[cfg(target_os = "macos")]
        PathBuf::from("/usr/local/bin/ffmpeg"),
        #[cfg(target_os = "linux")]
        PathBuf::from("/usr/bin/ffmpeg"),
        #[cfg(windows)]
        PathBuf::from(r"C:\ffmpeg\bin\ffmpeg.exe"),
        #[cfg(windows)]
        PathBuf::from(r"C:\ProgramData\chocolatey\bin\ffmpeg.exe"),
    ];
    for c in candidates {
        if c.is_file() {
            return Ok(c.clone());
        }
    }
    #[cfg(windows)]
    if let Some(home) = std::env::var_os("USERPROFILE") {
        let scoop = PathBuf::from(home).join(r"scoop\shims\ffmpeg.exe");
        if scoop.is_file() {
            return Ok(scoop);
        }
    }
    bail!(
        "ffmpeg not found: install it (macOS: brew install ffmpeg; \
         Windows: winget install ffmpeg) or set its path in settings"
    )
}

/// One frame fed to the encoder.
#[derive(Debug, Clone)]
pub enum FrameInput {
    /// A JPEG file piped as-is (original or cached preview).
    File(PathBuf),
    /// A Sony ARW whose embedded JPEG preview is extracted while piping.
    ArwEmbedded(PathBuf),
}

pub struct VideoJob<'a> {
    pub ffmpeg: &'a Path,
    pub frames: &'a [FrameInput],
    pub fps: f64,
    pub out_path: &'a Path,
    /// Cap on the long edge; None = keep native resolution.
    pub long_edge: Option<u32>,
}

/// Render a CFR MP4. Frames are piped through stdin (image2pipe), so no
/// temp files or filename escaping. H.264 up to 4096px; HEVC above that
/// (H.264 levels top out around 4K — full-res A9III frames are 6000px).
pub fn render_video(job: &VideoJob) -> Result<()> {
    if job.frames.is_empty() {
        bail!("burst has no frames");
    }
    if let Some(parent) = job.out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Clamp to sane bounds; A9III bursts are ~5-120 fps.
    let fps = job.fps.clamp(1.0, 240.0);
    let tmp = job.out_path.with_extension("mp4.tmp");

    let scale = match job.long_edge {
        // Fit within the box without ever upscaling; keep dims even.
        Some(le) => format!(
            "scale=min({le}\\,iw):min({le}\\,ih):force_original_aspect_ratio=decrease:force_divisible_by=2"
        ),
        None => "scale=ceil(iw/2)*2:ceil(ih/2)*2".to_string(),
    };
    let use_hevc = job.long_edge.map(|le| le > 4096).unwrap_or(true);
    let codec: &[&str] = if use_hevc {
        if cfg!(target_os = "macos") {
            // Hardware HEVC: fast enough for full-res 120fps bursts.
            &["-c:v", "hevc_videotoolbox", "-q:v", "60", "-tag:v", "hvc1"]
        } else {
            &["-c:v", "libx265", "-preset", "medium", "-crf", "20", "-tag:v", "hvc1"]
        }
    } else {
        &["-c:v", "libx264", "-preset", "medium", "-crf", "18"]
    };

    let mut child = Command::new(job.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "image2pipe"])
        .args(["-framerate", &format!("{fps:.3}")])
        .args(["-i", "-"])
        .args(["-vf", &scale])
        .args(codec)
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
        for frame in job.frames {
            let bytes = match frame {
                FrameInput::File(path) => std::fs::read(path)
                    .with_context(|| format!("reading frame {}", path.display()))?,
                FrameInput::ArwEmbedded(path) => super::arw::extract_largest_jpeg(path)?,
            };
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

/// Retime a finished MP4 so it plays at `rate` (0.25 = quarter speed)
/// WITHOUT re-encoding: input timestamps are scaled with -itsscale and the
/// streams are copied bit-exact. A 120fps burst exported at 0.25x becomes a
/// smooth 30fps slow-motion file.
pub fn remux_with_rate(ffmpeg: &Path, src: &Path, dest: &Path, rate: f64) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = dest.with_extension("mp4.tmp");
    let out = Command::new(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-itsscale", &format!("{:.6}", 1.0 / rate.clamp(0.01, 4.0))])
        .arg("-i")
        .arg(src)
        .args(["-c", "copy", "-movflags", "+faststart", "-f", "mp4"])
        .arg(&tmp)
        .output()
        .with_context(|| format!("spawning {}", ffmpeg.display()))?;
    if !out.status.success() {
        let _ = std::fs::remove_file(&tmp);
        bail!(
            "ffmpeg remux failed for {}: {}",
            dest.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    std::fs::rename(&tmp, dest)?;
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
            frames.push(FrameInput::File(p));
        }
        let out = dir.path().join("videos/burst_1.mp4");
        render_video(&VideoJob {
            ffmpeg: &ffmpeg,
            frames: &frames,
            fps: 24.0,
            out_path: &out,
            long_edge: Some(2048),
        })
        .unwrap();
        let meta = std::fs::metadata(&out).unwrap();
        assert!(meta.len() > 1000, "mp4 too small: {} bytes", meta.len());

        // Retimed export: 0.25x should be bit-copied but 4x the duration.
        let slow = dir.path().join("videos/burst_1_slow.mp4");
        remux_with_rate(&ffmpeg, &out, &slow, 0.25).unwrap();
        assert!(slow.exists());
    }
}
