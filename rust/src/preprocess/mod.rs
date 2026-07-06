//! Upfront preprocessing: extract all stills and render all burst videos
//! into the cache BEFORE review starts. Resumable — every output is keyed
//! by content hash (stills) or burst id (videos) and skipped when present.

pub mod arw;
pub mod stills;
pub mod video;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use anyhow::{Context, Result};
use rayon::prelude::*;

use crate::db::{self, queries};
use crate::indexer::pairing::FileKind;

#[derive(Debug, Clone)]
pub struct PreprocessStep {
    /// "stills" | "videos"
    pub phase: String,
    pub done: u64,
    pub total: u64,
    pub failed: u64,
}

#[derive(Debug, Clone, Default)]
pub struct PreprocessOutcome {
    pub stills_processed: u64,
    pub stills_skipped: u64,
    pub videos_rendered: u64,
    pub videos_skipped: u64,
    pub failures: Vec<String>,
}

static CANCELLED: AtomicBool = AtomicBool::new(false);

pub fn cancel() {
    CANCELLED.store(true, Ordering::Relaxed);
}

fn kind_from_str(kind: &str) -> FileKind {
    match kind {
        "raw" => FileKind::Raw,
        "heif" => FileKind::Heif,
        _ => FileKind::Jpeg,
    }
}

pub fn run_preprocess(
    source_id: i64,
    on_progress: impl Fn(PreprocessStep) + Sync,
) -> Result<PreprocessOutcome> {
    CANCELLED.store(false, Ordering::Relaxed);

    let (root, cache) = {
        let conn = db::conn()?;
        let s = queries::get_source(&conn, source_id)?;
        (
            PathBuf::from(s.root_path),
            PathBuf::from(s.cache_path.context("no cache folder set for source")?),
        )
    };
    std::fs::create_dir_all(cache.join("previews"))?;
    std::fs::create_dir_all(cache.join("thumbs"))?;
    std::fs::create_dir_all(cache.join("videos"))?;

    let mut outcome = PreprocessOutcome::default();

    // Resolve ffmpeg up front: videos always need it, HEIF stills need it
    // on non-macOS. Soft here; hard error when a phase actually requires it.
    let ffmpeg = {
        let conn = db::conn()?;
        let setting = queries::get_setting(&conn, "ffmpeg_path")?;
        video::find_ffmpeg(setting.as_deref()).ok()
    };

    // ---- Phase 1: stills (parallel across all cores) ----
    let photos = {
        let conn = db::conn()?;
        queries::photos_for_preprocess(&conn, source_id)?
    };
    let total = photos.len() as u64;
    let done = AtomicU64::new(0);
    let failed = AtomicU64::new(0);
    let failures: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let sharpness_updates: Mutex<Vec<(i64, f64)>> = Mutex::new(Vec::new());
    let skipped = AtomicU64::new(0);

    photos.par_iter().for_each(|photo| {
        if CANCELLED.load(Ordering::Relaxed) {
            return;
        }
        match stills::process_still(
            &root,
            &cache,
            &photo.rel_path,
            kind_from_str(&photo.kind),
            &photo.content_hash,
            photo.orientation,
            ffmpeg.as_deref(),
        ) {
            Ok(out) => {
                if out.skipped {
                    skipped.fetch_add(1, Ordering::Relaxed);
                }
                if let Some(s) = out.sharpness {
                    sharpness_updates.lock().unwrap().push((photo.photo_id, s));
                }
            }
            Err(e) => {
                failed.fetch_add(1, Ordering::Relaxed);
                failures
                    .lock()
                    .unwrap()
                    .push(format!("{}: {e:#}", photo.rel_path));
            }
        }
        let n = done.fetch_add(1, Ordering::Relaxed) + 1;
        if n % 20 == 0 || n == total {
            on_progress(PreprocessStep {
                phase: "stills".into(),
                done: n,
                total,
                failed: failed.load(Ordering::Relaxed),
            });
        }
    });

    // Persist sharpness in one transaction.
    {
        let mut conn = db::conn()?;
        let tx = conn.transaction()?;
        for (photo_id, s) in sharpness_updates.into_inner().unwrap() {
            queries::set_sharpness(&tx, photo_id, s)?;
        }
        tx.commit()?;
    }
    outcome.stills_skipped = skipped.load(Ordering::Relaxed);
    outcome.stills_processed =
        total - outcome.stills_skipped - failed.load(Ordering::Relaxed);

    if CANCELLED.load(Ordering::Relaxed) {
        outcome.failures = failures.into_inner().unwrap();
        return Ok(outcome);
    }

    // ---- Phase 2: videos (limited parallelism; ffmpeg is multithreaded) ----
    let ffmpeg = ffmpeg.context(
        "ffmpeg not found: install it or set its path in settings (needed to render burst videos)",
    )?;
    let bursts = {
        let conn = db::conn()?;
        queries::burst_ids_for_video(&conn, source_id)?
    };
    // Frames per burst, read up front to keep DB access single-threaded.
    let burst_jobs: Vec<(i64, Option<f64>, Vec<String>)> = {
        let conn = db::conn()?;
        bursts
            .iter()
            .map(|(id, fps)| {
                Ok((*id, *fps, queries::burst_preview_hashes(&conn, *id)?))
            })
            .collect::<Result<Vec<_>>>()?
    };

    let vtotal = burst_jobs.len() as u64;
    let vdone = AtomicU64::new(0);
    let vskipped = AtomicU64::new(0);
    let vfailed = AtomicU64::new(0);
    let video_updates: Mutex<Vec<(i64, String)>> = Mutex::new(Vec::new());

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads((num_cpus() / 2).max(1))
        .build()
        .context("building video thread pool")?;

    pool.install(|| {
        burst_jobs.par_iter().for_each(|(burst_id, fps, hashes)| {
            if CANCELLED.load(Ordering::Relaxed) {
                return;
            }
            let out_path = cache.join("videos").join(format!("{burst_id}.mp4"));
            if out_path.exists() {
                vskipped.fetch_add(1, Ordering::Relaxed);
                video_updates
                    .lock()
                    .unwrap()
                    .push((*burst_id, out_path.to_string_lossy().into_owned()));
            } else {
                let frame_paths: Vec<PathBuf> = hashes
                    .iter()
                    .map(|h| stills::still_paths(&cache, h).preview)
                    .filter(|p| p.exists())
                    .collect();
                let fps = fps.unwrap_or(30.0);
                let result = video::render_video(&video::VideoJob {
                    ffmpeg: &ffmpeg,
                    frame_paths: &frame_paths,
                    fps,
                    out_path: &out_path,
                });
                match result {
                    Ok(()) => {
                        video_updates
                            .lock()
                            .unwrap()
                            .push((*burst_id, out_path.to_string_lossy().into_owned()));
                    }
                    Err(e) => {
                        vfailed.fetch_add(1, Ordering::Relaxed);
                        failures
                            .lock()
                            .unwrap()
                            .push(format!("burst {burst_id}: {e:#}"));
                    }
                }
            }
            let n = vdone.fetch_add(1, Ordering::Relaxed) + 1;
            on_progress(PreprocessStep {
                phase: "videos".into(),
                done: n,
                total: vtotal,
                failed: vfailed.load(Ordering::Relaxed),
            });
        });
    });

    {
        let mut conn = db::conn()?;
        let tx = conn.transaction()?;
        for (burst_id, path) in video_updates.into_inner().unwrap() {
            queries::set_video_cache(&tx, burst_id, &path)?;
        }
        tx.commit()?;
    }

    outcome.videos_skipped = vskipped.load(Ordering::Relaxed);
    outcome.videos_rendered =
        vtotal - outcome.videos_skipped - vfailed.load(Ordering::Relaxed);
    outcome.failures = failures.into_inner().unwrap();
    Ok(outcome)
}

/// Cache completeness check, used by the UI to gate review.
#[derive(Debug, Clone, Default)]
pub struct CacheReport {
    pub stills_total: u64,
    pub stills_cached: u64,
    pub videos_total: u64,
    pub videos_cached: u64,
}

pub fn cache_status(source_id: i64) -> Result<CacheReport> {
    let conn = db::conn()?;
    let s = queries::get_source(&conn, source_id)?;
    let Some(cache) = s.cache_path else {
        return Ok(CacheReport::default());
    };
    let cache = Path::new(&cache);

    let photos = queries::photos_for_preprocess(&conn, source_id)?;
    let bursts = queries::burst_ids_for_video(&conn, source_id)?;

    let mut status = CacheReport {
        stills_total: photos.len() as u64,
        videos_total: bursts.len() as u64,
        ..Default::default()
    };
    for p in &photos {
        let paths = stills::still_paths(cache, &p.content_hash);
        if paths.preview.exists() && paths.thumb.exists() {
            status.stills_cached += 1;
        }
    }
    for (burst_id, _) in &bursts {
        if cache.join("videos").join(format!("{burst_id}.mp4")).exists() {
            status.videos_cached += 1;
        }
    }
    Ok(status)
}

fn num_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, RgbImage};

    /// Whole-pipeline test: index a fake card of real JPEGs, preprocess,
    /// verify stills + burst MP4 + sharpness + resumability.
    /// Uses the global DB (file-backed in a tempdir), so this is the only
    /// test allowed to call db::init().
    #[test]
    fn preprocess_end_to_end() {
        if video::find_ffmpeg(None).is_err() {
            eprintln!("ffmpeg not available; skipping");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        db::init(&dir.path().join("library.db")).unwrap();

        // Fake card: 5 JPEGs with identical mtimes -> mtime fallback groups
        // them into one burst.
        let card = dir.path().join("card/DCIM/100MSDCF");
        std::fs::create_dir_all(&card).unwrap();
        for i in 0..5u32 {
            let img = DynamicImage::ImageRgb8(RgbImage::from_fn(800, 600, |x, y| {
                image::Rgb([((x + i * 60) % 256) as u8, (y % 256) as u8, 120])
            }));
            img.save(card.join(format!("DSC0000{i}.JPG"))).unwrap();
        }
        let now = std::time::SystemTime::now();
        for i in 0..5 {
            let f = std::fs::File::options()
                .write(true)
                .open(card.join(format!("DSC0000{i}.JPG")))
                .unwrap();
            f.set_modified(now).unwrap();
        }

        let root = dir.path().join("card");
        let cache = dir.path().join("cache");
        let source_id = {
            let conn = db::conn().unwrap();
            let s = queries::upsert_source(&conn, root.to_str().unwrap()).unwrap();
            queries::set_cache_path(&conn, s.id, cache.to_str().unwrap()).unwrap();
            s.id
        };
        crate::indexer::run_index(source_id, |_| {}).unwrap();

        {
            let conn = db::conn().unwrap();
            let bursts = queries::list_bursts(&conn, source_id, None, 0, 10).unwrap();
            assert_eq!(bursts.len(), 1, "expected one burst from equal mtimes");
            assert_eq!(bursts[0].frame_count, 5);
        }

        let outcome = run_preprocess(source_id, |_| {}).unwrap();
        assert_eq!(outcome.stills_processed, 5);
        assert_eq!(outcome.videos_rendered, 1);
        assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);

        let status = cache_status(source_id).unwrap();
        assert_eq!(status.stills_cached, status.stills_total);
        assert_eq!(status.videos_cached, 1);

        // Sharpness persisted.
        {
            let conn = db::conn().unwrap();
            let frames = queries::burst_frames(
                &conn,
                queries::list_bursts(&conn, source_id, None, 0, 10).unwrap()[0].id,
            )
            .unwrap();
            assert!(frames.iter().all(|f| f.sharpness.is_some()));
        }

        // Re-run: everything cached, nothing re-rendered.
        let again = run_preprocess(source_id, |_| {}).unwrap();
        assert_eq!(again.stills_skipped, 5);
        assert_eq!(again.videos_skipped, 1);
        assert_eq!(again.videos_rendered, 0);
    }
}
