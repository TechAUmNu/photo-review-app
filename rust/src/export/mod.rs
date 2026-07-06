//! Export: copy keeper photos (pairs travel together) and keep-video MP4s
//! to the output folder. Nearly pure copying — all rendering happened in
//! preprocessing. Idempotent: re-running skips files already exported
//! (verified by content hash), so an interrupted export just resumes.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{bail, Context, Result};

use crate::db::{self, queries};
use crate::indexer::hash::content_hash;

#[derive(Debug, Clone)]
pub struct ExportStep {
    /// "keepers" | "videos"
    pub phase: String,
    pub done: u64,
    pub total: u64,
    pub current: String,
}

#[derive(Debug, Clone, Default)]
pub struct ExportOutcome {
    pub files_copied: u64,
    pub files_skipped: u64,
    pub files_renamed: u64,
    pub videos_copied: u64,
    pub videos_skipped: u64,
    pub failures: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct PlanSummary {
    // (named distinctly from api::library::ExportPlan for FRB codegen)
    pub keeper_photos: u64,
    pub keeper_files: u64,
    pub keeper_bytes: u64,
    pub videos: u64,
    pub video_bytes: u64,
    pub output_path: Option<String>,
}

static CANCELLED: AtomicBool = AtomicBool::new(false);

pub fn cancel() {
    CANCELLED.store(true, Ordering::Relaxed);
}

pub fn plan(source_id: i64) -> Result<PlanSummary> {
    let conn = db::conn()?;
    let source = queries::get_source(&conn, source_id)?;
    let keepers = queries::kept_photo_files(&conn, source_id)?;
    let videos = queries::kept_videos(&conn, source_id)?;

    let mut plan = PlanSummary {
        keeper_photos: keepers.len() as u64,
        output_path: source.output_path,
        ..Default::default()
    };
    for (_, files) in &keepers {
        plan.keeper_files += files.len() as u64;
        plan.keeper_bytes += files.iter().map(|f| f.size as u64).sum::<u64>();
    }
    plan.videos = videos.len() as u64;
    for v in &videos {
        if let Ok(m) = std::fs::metadata(&v.video_cache_path) {
            plan.video_bytes += m.len();
        }
    }
    Ok(plan)
}

pub fn run_export(
    source_id: i64,
    job_id: &str,
    on_progress: impl Fn(ExportStep) + Sync,
) -> Result<ExportOutcome> {
    CANCELLED.store(false, Ordering::Relaxed);

    let (root, output) = {
        let conn = db::conn()?;
        let s = queries::get_source(&conn, source_id)?;
        (
            PathBuf::from(s.root_path),
            PathBuf::from(s.output_path.context("no output folder set")?),
        )
    };
    let keepers_dir = output.join("keepers");
    let videos_dir = output.join("videos");
    std::fs::create_dir_all(&keepers_dir)?;
    std::fs::create_dir_all(&videos_dir)?;

    let mut outcome = ExportOutcome::default();

    // ---- Phase 1: keeper photos ----
    let keepers = {
        let conn = db::conn()?;
        queries::kept_photo_files(&conn, source_id)?
    };
    let total = keepers.len() as u64;
    for (done, (photo_id, files)) in keepers.iter().enumerate() {
        if CANCELLED.load(Ordering::Relaxed) {
            return Ok(outcome);
        }
        let first_name = files
            .first()
            .map(|f| file_name(&f.rel_path))
            .unwrap_or_default();
        on_progress(ExportStep {
            phase: "keepers".into(),
            done: done as u64,
            total,
            current: first_name,
        });

        match export_photo(&root, &keepers_dir, files, *photo_id, job_id) {
            Ok(actions) => {
                for a in actions {
                    match a {
                        CopyAction::Copied => outcome.files_copied += 1,
                        CopyAction::SkippedIdentical => outcome.files_skipped += 1,
                        CopyAction::Renamed => {
                            outcome.files_copied += 1;
                            outcome.files_renamed += 1;
                        }
                    }
                }
            }
            Err(e) => outcome
                .failures
                .push(format!("photo {photo_id}: {e:#}")),
        }
    }

    // ---- Phase 2: keep-video MP4s (copy from cache) ----
    let videos = {
        let conn = db::conn()?;
        queries::kept_videos(&conn, source_id)?
    };
    let vtotal = videos.len() as u64;
    for (done, v) in videos.iter().enumerate() {
        if CANCELLED.load(Ordering::Relaxed) {
            return Ok(outcome);
        }
        let dest_name = format!("{}_burst{}.mp4", format_ts(v.start_ms), v.burst_id);
        on_progress(ExportStep {
            phase: "videos".into(),
            done: done as u64,
            total: vtotal,
            current: dest_name.clone(),
        });
        let src = Path::new(&v.video_cache_path);
        let dest = videos_dir.join(&dest_name);
        match copy_one(src, &dest) {
            Ok(CopyAction::SkippedIdentical) => outcome.videos_skipped += 1,
            Ok(_) => {
                outcome.videos_copied += 1;
                let conn = db::conn()?;
                queries::mark_video_exported(&conn, v.burst_id)?;
                queries::log_export(
                    &conn,
                    job_id,
                    None,
                    None,
                    &dest.to_string_lossy(),
                    "copied",
                )?;
            }
            Err(e) => outcome
                .failures
                .push(format!("burst {}: {e:#}", v.burst_id)),
        }
    }

    Ok(outcome)
}

enum CopyAction {
    Copied,
    SkippedIdentical,
    Renamed,
}

/// Copy all files of one photo. The whole pair shares one collision suffix
/// so DSC01234.ARW / DSC01234.JPG always land together.
fn export_photo(
    root: &Path,
    keepers_dir: &Path,
    files: &[queries::KeptFileRow],
    photo_id: i64,
    job_id: &str,
) -> Result<Vec<CopyAction>> {
    // Find a suffix where every member is either free or identical.
    let suffix = (0..100)
        .find(|&s| {
            files.iter().all(|f| {
                let dest = dest_path(keepers_dir, &f.rel_path, s);
                match std::fs::metadata(&dest) {
                    Err(_) => true, // free
                    Ok(m) => {
                        m.len() == f.size as u64
                            && content_hash(&dest, m.len())
                                .map(|h| h == f.content_hash)
                                .unwrap_or(false)
                    }
                }
            })
        })
        .context("gave up finding a free filename after 100 collisions")?;

    let mut actions = Vec::new();
    for f in files {
        let src = root.join(&f.rel_path);
        let dest = dest_path(keepers_dir, &f.rel_path, suffix);
        let mut action = copy_one(&src, &dest)?;
        if suffix > 0 && !matches!(action, CopyAction::SkippedIdentical) {
            action = CopyAction::Renamed;
        }
        let action_str = match action {
            CopyAction::Copied => "copied",
            CopyAction::SkippedIdentical => "skipped_identical",
            CopyAction::Renamed => "renamed",
        };
        let conn = db::conn()?;
        queries::log_export(
            &conn,
            job_id,
            Some(photo_id),
            Some(f.file_id),
            &dest.to_string_lossy(),
            action_str,
        )?;
        actions.push(action);
    }
    Ok(actions)
}

fn dest_path(dir: &Path, rel_path: &str, suffix: u32) -> PathBuf {
    let name = file_name(rel_path);
    if suffix == 0 {
        return dir.join(name);
    }
    let p = Path::new(&name);
    let stem = p.file_stem().unwrap_or_default().to_string_lossy();
    let ext = p
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    dir.join(format!("{stem}_{}{ext}", suffix + 1))
}

fn file_name(rel_path: &str) -> String {
    Path::new(rel_path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| rel_path.to_string())
}

/// Copy src -> dest unless dest already has identical content.
/// Verifies size after copy; copies via .tmp + rename for crash safety.
fn copy_one(src: &Path, dest: &Path) -> Result<CopyAction> {
    let src_meta = std::fs::metadata(src)
        .with_context(|| format!("source missing: {}", src.display()))?;
    if let Ok(dest_meta) = std::fs::metadata(dest) {
        if dest_meta.len() == src_meta.len() {
            let src_hash = content_hash(src, src_meta.len())?;
            let dest_hash = content_hash(dest, dest_meta.len())?;
            if src_hash == dest_hash {
                return Ok(CopyAction::SkippedIdentical);
            }
        }
        // Same name, different content: caller picks a suffix; reaching here
        // with a conflicting dest is a bug in suffix selection.
        bail!("destination exists with different content: {}", dest.display());
    }
    let tmp = dest.with_extension("part");
    std::fs::copy(src, &tmp)?;
    let copied = std::fs::metadata(&tmp)?.len();
    if copied != src_meta.len() {
        let _ = std::fs::remove_file(&tmp);
        bail!(
            "size mismatch copying {} ({} != {})",
            src.display(),
            copied,
            src_meta.len()
        );
    }
    std::fs::rename(&tmp, dest)?;
    Ok(CopyAction::Copied)
}

/// start_ms (unix millis, camera local time) -> "YYYYMMDD_HHMMSS".
fn format_ts(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}{m:02}{d:02}_{:02}{:02}{:02}",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

/// Howard Hinnant's civil_from_days.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_formatting() {
        // 2026-07-06 10:30:05 UTC = 1783333805
        assert_eq!(format_ts(1_783_333_805_000), "20260706_103005");
        assert_eq!(format_ts(0), "19700101_000000");
    }

    #[test]
    fn dest_path_suffixing() {
        let dir = Path::new("/out/keepers");
        assert_eq!(
            dest_path(dir, "DCIM/DSC1.ARW", 0),
            Path::new("/out/keepers/DSC1.ARW")
        );
        assert_eq!(
            dest_path(dir, "DCIM/DSC1.ARW", 1),
            Path::new("/out/keepers/DSC1_2.ARW")
        );
    }

    #[test]
    fn copy_one_skips_identical_and_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.jpg");
        let dest = dir.path().join("out/a.jpg");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&src, b"hello world").unwrap();

        assert!(matches!(copy_one(&src, &dest).unwrap(), CopyAction::Copied));
        assert!(matches!(
            copy_one(&src, &dest).unwrap(),
            CopyAction::SkippedIdentical
        ));
        // Different content at dest -> error (suffix logic must prevent this).
        std::fs::write(&dest, b"different!!").unwrap();
        assert!(copy_one(&src, &dest).is_err());
    }
}
