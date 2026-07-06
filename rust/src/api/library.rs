//! FRB-exposed API for library management: sources, indexing, browsing,
//! and review decisions. DTOs here are mirrored into Dart by codegen.

use anyhow::Result;
use flutter_rust_bridge::frb;

use crate::db::{self, queries};
use crate::indexer;

// ---------- init ----------

/// Open the library database. Call once at startup, before anything else.
pub fn init_db(db_path: String) -> Result<()> {
    db::init(std::path::Path::new(&db_path))
}

// ---------- DTOs ----------

#[derive(Debug, Clone)]
pub struct SourceInfo {
    pub id: i64,
    pub root_path: String,
    pub output_path: Option<String>,
    pub cache_path: Option<String>,
    pub last_indexed_at: Option<i64>,
}

impl From<queries::SourceRow> for SourceInfo {
    fn from(r: queries::SourceRow) -> Self {
        SourceInfo {
            id: r.id,
            root_path: r.root_path,
            output_path: r.output_path,
            cache_path: r.cache_path,
            last_indexed_at: r.last_indexed_at,
        }
    }
}

#[derive(Debug, Clone)]
pub struct IndexProgress {
    pub phase: String,
    pub done: u64,
    pub total: u64,
    pub finished: bool,
    /// Set on the final event.
    pub photos: u64,
    pub bursts: u64,
    pub singles: u64,
}

#[derive(Debug, Clone)]
pub struct BurstSummary {
    pub id: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub frame_count: i64,
    pub fps_estimate: Option<f64>,
    /// "undecided" | "done" | "rejected"
    pub status: String,
    pub keep_video: bool,
    /// Playback rate for the exported video (1.0 = real time).
    pub export_rate: f64,
    pub kept_count: i64,
    pub hero_photo_id: i64,
    /// Absolute path of a representative frame's display file, if usable.
    pub hero_display_path: Option<String>,
    pub hero_display_kind: Option<String>,
    /// Cached 320px thumb of the hero frame (preferred for display).
    pub hero_thumb_path: Option<String>,
    pub video_cache_path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PhotoSummary {
    pub id: i64,
    pub capture_time_ms: i64,
    pub frame_index: Option<i64>,
    pub keep: bool,
    pub sharpness: Option<f64>,
    /// Absolute path of the best original file for direct display
    /// (JPEG > HEIF > RAW). Fallback when the cache is incomplete.
    pub display_path: String,
    /// "raw" | "jpeg" | "heif" — raw means Flutter can't Image.file it.
    pub display_kind: String,
    /// Cached 320px thumb (preferred for grids/filmstrips).
    pub thumb_path: Option<String>,
    /// Cached 2048px preview (paused-frame overlay, zoom).
    pub preview_path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProgressStats {
    pub total_bursts: i64,
    pub decided_bursts: i64,
    pub total_singles: i64,
    pub kept_photos: i64,
    pub kept_videos: i64,
}

// ---------- sources ----------

pub fn select_source(root_path: String) -> Result<SourceInfo> {
    let conn = db::conn()?;
    Ok(queries::upsert_source(&conn, &root_path)?.into())
}

pub fn list_sources() -> Result<Vec<SourceInfo>> {
    let conn = db::conn()?;
    Ok(queries::list_sources(&conn)?
        .into_iter()
        .map(Into::into)
        .collect())
}

pub fn set_output_folder(source_id: i64, path: String) -> Result<()> {
    let conn = db::conn()?;
    queries::set_output_path(&conn, source_id, &path)
}

pub fn set_cache_folder(source_id: i64, path: String) -> Result<()> {
    let conn = db::conn()?;
    queries::set_cache_path(&conn, source_id, &path)
}

// ---------- indexing ----------

/// Index (or re-index) a source. Streams progress; the final event has
/// `finished = true` and the outcome counts. Runs on a dedicated thread.
pub fn start_index(
    source_id: i64,
    sink: crate::frb_generated::StreamSink<IndexProgress>,
) -> Result<()> {
    std::thread::spawn(move || {
        let result = indexer::run_index(source_id, |p| {
            let _ = sink.add(IndexProgress {
                phase: p.phase,
                done: p.done,
                total: p.total,
                finished: false,
                photos: 0,
                bursts: 0,
                singles: 0,
            });
        });
        match result {
            Ok(outcome) => {
                let _ = sink.add(IndexProgress {
                    phase: "done".into(),
                    done: outcome.photos,
                    total: outcome.photos,
                    finished: true,
                    photos: outcome.photos,
                    bursts: outcome.bursts,
                    singles: outcome.singles,
                });
            }
            Err(e) => {
                let _ = sink.add_error(anyhow::anyhow!("indexing failed: {e:#}"));
            }
        }
    });
    Ok(())
}

// ---------- browsing ----------

/// Return an existing cache file path for a hash, or None.
fn cached_path(cache: Option<&str>, sub: &str, hash: &str) -> Option<String> {
    let dir = cache?;
    let p = std::path::Path::new(dir).join(sub).join(format!("{hash}.jpg"));
    p.exists().then(|| p.to_string_lossy().into_owned())
}

fn to_photo_summary(root: &str, cache: Option<&str>, p: queries::PhotoRow) -> PhotoSummary {
    let display_path = std::path::Path::new(root)
        .join(&p.preview_rel_path)
        .to_string_lossy()
        .into_owned();
    PhotoSummary {
        id: p.id,
        capture_time_ms: p.capture_time_ms,
        frame_index: p.frame_index,
        keep: p.keep,
        sharpness: p.sharpness,
        display_path,
        display_kind: p.preview_kind,
        thumb_path: cached_path(cache, "thumbs", &p.preview_hash),
        preview_path: cached_path(cache, "previews", &p.preview_hash),
    }
}

pub fn list_bursts(
    source_id: i64,
    status_filter: Option<String>,
    offset: i64,
    limit: i64,
) -> Result<Vec<BurstSummary>> {
    let conn = db::conn()?;
    let source = queries::get_source(&conn, source_id)?;
    let root = source.root_path;
    let cache = source.cache_path;
    Ok(
        queries::list_bursts(&conn, source_id, status_filter.as_deref(), offset, limit)?
            .into_iter()
            .map(|b| BurstSummary {
                id: b.id,
                start_ms: b.start_ms,
                end_ms: b.end_ms,
                frame_count: b.frame_count,
                fps_estimate: b.fps_estimate,
                status: b.status,
                keep_video: b.keep_video,
                export_rate: b.export_rate,
                kept_count: b.kept_count,
                hero_photo_id: b.hero_photo_id,
                hero_display_path: b.hero_rel_path.as_ref().map(|rel| {
                    std::path::Path::new(&root)
                        .join(rel)
                        .to_string_lossy()
                        .into_owned()
                }),
                hero_display_kind: b.hero_kind,
                hero_thumb_path: b
                    .hero_hash
                    .as_ref()
                    .and_then(|h| cached_path(cache.as_deref(), "thumbs", h)),
                video_cache_path: b.video_cache_path,
            })
            .collect(),
    )
}

pub fn list_singles(source_id: i64, offset: i64, limit: i64) -> Result<Vec<PhotoSummary>> {
    let conn = db::conn()?;
    let source = queries::get_source(&conn, source_id)?;
    Ok(queries::list_singles(&conn, source_id, offset, limit)?
        .into_iter()
        .map(|p| to_photo_summary(&source.root_path, source.cache_path.as_deref(), p))
        .collect())
}

pub fn get_burst_frames(source_id: i64, burst_id: i64) -> Result<Vec<PhotoSummary>> {
    let conn = db::conn()?;
    let source = queries::get_source(&conn, source_id)?;
    Ok(queries::burst_frames(&conn, burst_id)?
        .into_iter()
        .map(|p| to_photo_summary(&source.root_path, source.cache_path.as_deref(), p))
        .collect())
}

// ---------- preprocessing ----------

#[derive(Debug, Clone)]
pub struct PreprocessProgress {
    /// "stills" | "videos" | "done"
    pub phase: String,
    pub done: u64,
    pub total: u64,
    pub failed: u64,
    pub finished: bool,
    /// Populated on the final event.
    pub stills_processed: u64,
    pub stills_skipped: u64,
    pub videos_rendered: u64,
    pub videos_skipped: u64,
    pub failures: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CacheStatus {
    pub stills_total: u64,
    pub stills_cached: u64,
    pub videos_total: u64,
    pub videos_cached: u64,
}

/// Run the full upfront preprocessing pass (stills + burst videos).
/// Resumable: cached outputs are skipped, so re-running after a crash or
/// cancel picks up where it left off.
pub fn start_preprocess(
    source_id: i64,
    sink: crate::frb_generated::StreamSink<PreprocessProgress>,
) -> Result<()> {
    std::thread::spawn(move || {
        let result = crate::preprocess::run_preprocess(source_id, |step| {
            let _ = sink.add(PreprocessProgress {
                phase: step.phase,
                done: step.done,
                total: step.total,
                failed: step.failed,
                finished: false,
                stills_processed: 0,
                stills_skipped: 0,
                videos_rendered: 0,
                videos_skipped: 0,
                failures: vec![],
            });
        });
        match result {
            Ok(outcome) => {
                let _ = sink.add(PreprocessProgress {
                    phase: "done".into(),
                    done: 0,
                    total: 0,
                    failed: outcome.failures.len() as u64,
                    finished: true,
                    stills_processed: outcome.stills_processed,
                    stills_skipped: outcome.stills_skipped,
                    videos_rendered: outcome.videos_rendered,
                    videos_skipped: outcome.videos_skipped,
                    failures: outcome.failures,
                });
            }
            Err(e) => {
                let _ = sink.add_error(anyhow::anyhow!("preprocess failed: {e:#}"));
            }
        }
    });
    Ok(())
}

pub fn cancel_preprocess() {
    crate::preprocess::cancel();
}

pub fn get_cache_status(source_id: i64) -> Result<CacheStatus> {
    let s = crate::preprocess::cache_status(source_id)?;
    Ok(CacheStatus {
        stills_total: s.stills_total,
        stills_cached: s.stills_cached,
        videos_total: s.videos_total,
        videos_cached: s.videos_cached,
    })
}

/// Absolute paths into the preview cache for a photo, or None if missing.
pub fn get_cached_still(source_id: i64, photo_id: i64) -> Result<Option<String>> {
    let conn = db::conn()?;
    let s = queries::get_source(&conn, source_id)?;
    let Some(cache) = s.cache_path else {
        return Ok(None);
    };
    let hash = queries::preview_hash_for_photo(&conn, photo_id)?;
    Ok(hash.and_then(|h| {
        let p = crate::preprocess::stills::still_paths(std::path::Path::new(&cache), &h)
            .preview;
        p.exists().then(|| p.to_string_lossy().into_owned())
    }))
}

// ---------- burst surgery ----------

#[derive(Debug, Clone)]
pub struct SplitResult {
    pub first_burst_id: i64,
    pub second_burst_id: i64,
}

/// Split a burst before `at_frame_index` (frames [at..] become a new burst).
/// The affected bursts lose their cached MP4; re-run preprocessing to
/// re-render them (it skips everything else).
pub fn split_burst(burst_id: i64, at_frame_index: i64) -> Result<SplitResult> {
    let (a, b) = crate::burst_ops::split_burst(burst_id, at_frame_index)?;
    Ok(SplitResult {
        first_burst_id: a,
        second_burst_id: b,
    })
}

/// Merge bursts into the earliest; returns the surviving burst id.
pub fn merge_bursts(burst_ids: Vec<i64>) -> Result<i64> {
    crate::burst_ops::merge_bursts(burst_ids)
}

/// Re-run automatic grouping with a new gap. Manually split/merged bursts
/// are locked and untouched. Returns the number of new bursts formed.
pub fn regroup(source_id: i64, gap_ms: i64, min_burst_len: i64) -> Result<u64> {
    crate::burst_ops::regroup(source_id, gap_ms, min_burst_len)
}

pub fn get_app_setting(key: String) -> Result<Option<String>> {
    let conn = db::conn()?;
    queries::get_setting(&conn, &key)
}

pub fn set_app_setting(key: String, value: String) -> Result<()> {
    let conn = db::conn()?;
    queries::set_setting(&conn, &key, &value)
}

// ---------- decisions ----------

pub fn set_frame_keep(photo_ids: Vec<i64>, keep: bool) -> Result<()> {
    let conn = db::conn()?;
    queries::set_frame_keep(&conn, &photo_ids, keep)
}

/// status: "undecided" | "done" | "rejected"
pub fn set_burst_status(burst_id: i64, status: String) -> Result<()> {
    anyhow::ensure!(
        ["undecided", "done", "rejected"].contains(&status.as_str()),
        "invalid burst status: {status}"
    );
    let conn = db::conn()?;
    queries::set_burst_status(&conn, burst_id, &status)
}

pub fn set_keep_video(burst_id: i64, keep: bool) -> Result<()> {
    let conn = db::conn()?;
    queries::set_keep_video(&conn, burst_id, keep)
}

/// Playback rate the exported video should have (0.25 = quarter speed).
pub fn set_export_rate(burst_id: i64, rate: f64) -> Result<()> {
    let conn = db::conn()?;
    queries::set_export_rate(&conn, burst_id, rate)
}

pub fn get_progress_stats(source_id: i64) -> Result<ProgressStats> {
    let conn = db::conn()?;
    let s = queries::progress_stats(&conn, source_id)?;
    Ok(ProgressStats {
        total_bursts: s.total_bursts,
        decided_bursts: s.decided_bursts,
        total_singles: s.total_singles,
        kept_photos: s.kept_photos,
        kept_videos: s.kept_videos,
    })
}

// ---------- export ----------

#[derive(Debug, Clone)]
pub struct ExportPlan {
    pub keeper_photos: u64,
    pub keeper_files: u64,
    pub keeper_bytes: u64,
    pub videos: u64,
    pub video_bytes: u64,
    pub output_path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ExportProgress {
    /// "keepers" | "videos" | "done"
    pub phase: String,
    pub done: u64,
    pub total: u64,
    pub current: String,
    pub finished: bool,
    pub files_copied: u64,
    pub files_skipped: u64,
    pub files_renamed: u64,
    pub videos_copied: u64,
    pub videos_skipped: u64,
    pub failures: Vec<String>,
}

pub fn plan_export(source_id: i64) -> Result<ExportPlan> {
    let p = crate::export::plan(source_id)?;
    Ok(ExportPlan {
        keeper_photos: p.keeper_photos,
        keeper_files: p.keeper_files,
        keeper_bytes: p.keeper_bytes,
        videos: p.videos,
        video_bytes: p.video_bytes,
        output_path: p.output_path,
    })
}

pub fn start_export(
    source_id: i64,
    sink: crate::frb_generated::StreamSink<ExportProgress>,
) -> Result<()> {
    std::thread::spawn(move || {
        let job_id = format!(
            "job-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        );
        let result = crate::export::run_export(source_id, &job_id, |step| {
            let _ = sink.add(ExportProgress {
                phase: step.phase,
                done: step.done,
                total: step.total,
                current: step.current,
                finished: false,
                files_copied: 0,
                files_skipped: 0,
                files_renamed: 0,
                videos_copied: 0,
                videos_skipped: 0,
                failures: vec![],
            });
        });
        match result {
            Ok(o) => {
                let _ = sink.add(ExportProgress {
                    phase: "done".into(),
                    done: 0,
                    total: 0,
                    current: String::new(),
                    finished: true,
                    files_copied: o.files_copied,
                    files_skipped: o.files_skipped,
                    files_renamed: o.files_renamed,
                    videos_copied: o.videos_copied,
                    videos_skipped: o.videos_skipped,
                    failures: o.failures,
                });
            }
            Err(e) => {
                let _ = sink.add_error(anyhow::anyhow!("export failed: {e:#}"));
            }
        }
    });
    Ok(())
}

pub fn cancel_export() {
    crate::export::cancel();
}

#[frb(init)]
pub fn init_library() {
    flutter_rust_bridge::setup_default_user_utils();
}
