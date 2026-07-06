//! All SQL lives here, as plain functions over a &Connection (transaction
//! management belongs to the caller for multi-step operations).

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

use crate::indexer::exif::ExifSummary;
use crate::indexer::grouping::{GroupablePhoto, GroupingResult};
use crate::indexer::pairing::LogicalPhoto;

// ---------- sources ----------

#[derive(Debug, Clone)]
pub struct SourceRow {
    pub id: i64,
    pub root_path: String,
    pub output_path: Option<String>,
    pub cache_path: Option<String>,
    pub last_indexed_at: Option<i64>,
}

pub fn upsert_source(conn: &Connection, root_path: &str) -> Result<SourceRow> {
    conn.execute(
        "INSERT INTO sources (root_path, created_at) VALUES (?1, unixepoch())
         ON CONFLICT(root_path) DO NOTHING",
        params![root_path],
    )?;
    get_source_by_root(conn, root_path)
}

fn get_source_by_root(conn: &Connection, root_path: &str) -> Result<SourceRow> {
    Ok(conn.query_row(
        "SELECT id, root_path, output_path, cache_path, last_indexed_at
         FROM sources WHERE root_path = ?1",
        params![root_path],
        source_from_row,
    )?)
}

pub fn get_source(conn: &Connection, source_id: i64) -> Result<SourceRow> {
    Ok(conn.query_row(
        "SELECT id, root_path, output_path, cache_path, last_indexed_at
         FROM sources WHERE id = ?1",
        params![source_id],
        source_from_row,
    )?)
}

pub fn list_sources(conn: &Connection) -> Result<Vec<SourceRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, root_path, output_path, cache_path, last_indexed_at
         FROM sources ORDER BY COALESCE(last_indexed_at, created_at) DESC",
    )?;
    let rows = stmt
        .query_map([], source_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn source_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SourceRow> {
    Ok(SourceRow {
        id: row.get(0)?,
        root_path: row.get(1)?,
        output_path: row.get(2)?,
        cache_path: row.get(3)?,
        last_indexed_at: row.get(4)?,
    })
}

pub fn set_output_path(conn: &Connection, source_id: i64, path: &str) -> Result<()> {
    conn.execute(
        "UPDATE sources SET output_path = ?2 WHERE id = ?1",
        params![source_id, path],
    )?;
    Ok(())
}

pub fn set_cache_path(conn: &Connection, source_id: i64, path: &str) -> Result<()> {
    conn.execute(
        "UPDATE sources SET cache_path = ?2 WHERE id = ?1",
        params![source_id, path],
    )?;
    Ok(())
}

pub fn mark_indexed(conn: &Connection, source_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE sources SET last_indexed_at = unixepoch() WHERE id = ?1",
        params![source_id],
    )?;
    Ok(())
}

// ---------- indexing upserts ----------

/// A fully-analysed logical photo ready for the DB.
pub struct PhotoRecord {
    pub photo: LogicalPhoto,
    pub exif: ExifSummary,
    /// content_hash per file, same order as photo.files.
    pub hashes: Vec<String>,
}

/// Insert or refresh one photo and its files. Existing rows keep their
/// decision fields (keep, burst_id, burst_locked); metadata is refreshed.
/// Returns the photo id.
pub fn upsert_photo(conn: &Connection, source_id: i64, rec: &PhotoRecord) -> Result<i64> {
    let (capture_ms, low_precision) = match rec.exif.capture {
        Some(c) => (c.time_ms, c.low_precision),
        // No EXIF at all: fall back to file mtime (seconds) — low precision.
        None => (
            rec.photo.files.first().map(|f| f.mtime * 1000).unwrap_or(0),
            true,
        ),
    };

    conn.execute(
        "INSERT INTO photos (source_id, dir, stem, capture_time_ms, low_precision,
                             width, height, orientation)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(source_id, dir, stem) DO UPDATE SET
           capture_time_ms = excluded.capture_time_ms,
           low_precision   = excluded.low_precision,
           width           = excluded.width,
           height          = excluded.height,
           orientation     = excluded.orientation",
        params![
            source_id,
            rec.photo.dir,
            rec.photo.stem,
            capture_ms,
            low_precision as i64,
            rec.exif.width,
            rec.exif.height,
            rec.exif.orientation,
        ],
    )?;
    let photo_id: i64 = conn.query_row(
        "SELECT id FROM photos WHERE source_id = ?1 AND dir = ?2 AND stem = ?3",
        params![source_id, rec.photo.dir, rec.photo.stem],
        |r| r.get(0),
    )?;

    // Files: replace wholesale (cheap, few rows per photo).
    conn.execute("DELETE FROM files WHERE photo_id = ?1", params![photo_id])?;
    for (file, hash) in rec.photo.files.iter().zip(&rec.hashes) {
        conn.execute(
            "INSERT INTO files (photo_id, rel_path, kind, size, mtime, content_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                photo_id,
                file.rel_path,
                file.kind.as_str(),
                file.size,
                file.mtime,
                hash
            ],
        )?;
    }
    Ok(photo_id)
}

/// Look up a prior hash so unchanged files skip re-hashing on re-index.
pub fn find_existing_hash(
    conn: &Connection,
    source_id: i64,
    rel_path: &str,
    size: u64,
    mtime: i64,
) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT f.content_hash FROM files f
             JOIN photos p ON p.id = f.photo_id
             WHERE p.source_id = ?1 AND f.rel_path = ?2 AND f.size = ?3 AND f.mtime = ?4",
            params![source_id, rel_path, size, mtime],
            |r| r.get(0),
        )
        .optional()?)
}

/// Remove photos whose files no longer exist in the latest scan.
/// `seen` is the set of (dir, stem) keys present on disk.
pub fn delete_vanished_photos(
    conn: &Connection,
    source_id: i64,
    seen: &std::collections::HashSet<(String, String)>,
) -> Result<usize> {
    let mut stmt =
        conn.prepare("SELECT id, dir, stem FROM photos WHERE source_id = ?1")?;
    let existing = stmt
        .query_map(params![source_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut deleted = 0;
    for (id, dir, stem) in existing {
        if !seen.contains(&(dir, stem)) {
            conn.execute("DELETE FROM photos WHERE id = ?1", params![id])?;
            deleted += 1;
        }
    }
    if deleted > 0 {
        // Drop bursts that lost all their frames.
        conn.execute(
            "DELETE FROM bursts WHERE source_id = ?1
             AND id NOT IN (SELECT DISTINCT burst_id FROM photos WHERE burst_id IS NOT NULL)",
            params![source_id],
        )?;
    }
    Ok(deleted)
}

// ---------- grouping ----------

/// Photos not yet assigned to any burst and not manually locked.
/// Grouping is incremental: photos already in bursts are never disturbed,
/// which preserves burst decisions (status/keep_video) across re-indexes.
pub fn ungrouped_photos(conn: &Connection, source_id: i64) -> Result<Vec<GroupablePhoto>> {
    let mut stmt = conn.prepare(
        "SELECT id, capture_time_ms, burst_locked FROM photos
         WHERE source_id = ?1 AND burst_id IS NULL",
    )?;
    let rows = stmt
        .query_map(params![source_id], |r| {
            Ok(GroupablePhoto {
                photo_id: r.get(0)?,
                capture_time_ms: r.get(1)?,
                burst_locked: r.get::<_, i64>(2)? != 0,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Write a GroupingResult: create burst rows and assign photos.
pub fn apply_grouping(
    conn: &Connection,
    source_id: i64,
    result: &GroupingResult,
) -> Result<()> {
    for burst in &result.bursts {
        conn.execute(
            "INSERT INTO bursts (source_id, start_ms, end_ms, frame_count, fps_estimate)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                source_id,
                burst.start_ms,
                burst.end_ms,
                burst.frame_count() as i64,
                burst.fps_estimate(),
            ],
        )?;
        let burst_id = conn.last_insert_rowid();
        for (frame_index, photo_id) in burst.photo_ids.iter().enumerate() {
            conn.execute(
                "UPDATE photos SET burst_id = ?1, frame_index = ?2 WHERE id = ?3",
                params![burst_id, frame_index as i64, photo_id],
            )?;
        }
    }
    Ok(())
}

// ---------- browse ----------

#[derive(Debug, Clone)]
pub struct BurstSummaryRow {
    pub id: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub frame_count: i64,
    pub fps_estimate: Option<f64>,
    pub status: String,
    pub keep_video: bool,
    /// Playback rate the exported video should have (1.0 = real time).
    pub export_rate: f64,
    pub kept_count: i64,
    pub hero_photo_id: i64,
    /// Root-relative display file of the middle frame (jpeg > heif > raw).
    pub hero_rel_path: Option<String>,
    pub hero_kind: Option<String>,
    pub hero_hash: Option<String>,
    pub video_cache_path: Option<String>,
}

pub fn list_bursts(
    conn: &Connection,
    source_id: i64,
    status_filter: Option<&str>,
    offset: i64,
    limit: i64,
) -> Result<Vec<BurstSummaryRow>> {
    let mut stmt = conn.prepare(
        "SELECT b.id, b.start_ms, b.end_ms, b.frame_count, b.fps_estimate,
                b.status, b.keep_video, b.export_rate, b.video_cache_path,
                (SELECT COUNT(*) FROM photos k WHERE k.burst_id = b.id AND k.keep = 1),
                h.id,
                (SELECT f.rel_path FROM files f WHERE f.photo_id = h.id
                 ORDER BY CASE f.kind WHEN 'jpeg' THEN 0 WHEN 'heif' THEN 1 ELSE 2 END
                 LIMIT 1),
                (SELECT f.kind FROM files f WHERE f.photo_id = h.id
                 ORDER BY CASE f.kind WHEN 'jpeg' THEN 0 WHEN 'heif' THEN 1 ELSE 2 END
                 LIMIT 1),
                (SELECT f.content_hash FROM files f WHERE f.photo_id = h.id
                 ORDER BY CASE f.kind WHEN 'jpeg' THEN 0 WHEN 'heif' THEN 1 ELSE 2 END
                 LIMIT 1)
         FROM bursts b
         LEFT JOIN photos h ON h.id =
           (SELECT p.id FROM photos p WHERE p.burst_id = b.id
            AND p.frame_index >= (b.frame_count / 2)
            ORDER BY p.frame_index LIMIT 1)
         WHERE b.source_id = ?1 AND (?2 IS NULL OR b.status = ?2)
         ORDER BY b.start_ms
         LIMIT ?3 OFFSET ?4",
    )?;
    let rows = stmt
        .query_map(params![source_id, status_filter, limit, offset], |r| {
            Ok(BurstSummaryRow {
                id: r.get(0)?,
                start_ms: r.get(1)?,
                end_ms: r.get(2)?,
                frame_count: r.get(3)?,
                fps_estimate: r.get(4)?,
                status: r.get(5)?,
                keep_video: r.get::<_, i64>(6)? != 0,
                export_rate: r.get(7)?,
                video_cache_path: r.get(8)?,
                kept_count: r.get(9)?,
                hero_photo_id: r.get::<_, Option<i64>>(10)?.unwrap_or(0),
                hero_rel_path: r.get(11)?,
                hero_kind: r.get(12)?,
                hero_hash: r.get(13)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

#[derive(Debug, Clone)]
pub struct PhotoRow {
    pub id: i64,
    pub dir: String,
    pub stem: String,
    pub capture_time_ms: i64,
    pub frame_index: Option<i64>,
    pub keep: bool,
    pub sharpness: Option<f64>,
    /// Root-relative path of the best display file (jpeg > heif > raw).
    pub preview_rel_path: String,
    pub preview_kind: String,
    /// content_hash of that file — the preview-cache key.
    pub preview_hash: String,
}

fn photo_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<PhotoRow> {
    Ok(PhotoRow {
        id: r.get(0)?,
        dir: r.get(1)?,
        stem: r.get(2)?,
        capture_time_ms: r.get(3)?,
        frame_index: r.get(4)?,
        keep: r.get::<_, i64>(5)? != 0,
        sharpness: r.get(6)?,
        preview_rel_path: r.get(7)?,
        preview_kind: r.get(8)?,
        preview_hash: r.get(9)?,
    })
}

/// Best display file per photo: jpeg beats heif beats raw.
const PREVIEW_FILE_SELECT: &str = "
  (SELECT f.rel_path FROM files f WHERE f.photo_id = p.id
   ORDER BY CASE f.kind WHEN 'jpeg' THEN 0 WHEN 'heif' THEN 1 ELSE 2 END
   LIMIT 1),
  (SELECT f.kind FROM files f WHERE f.photo_id = p.id
   ORDER BY CASE f.kind WHEN 'jpeg' THEN 0 WHEN 'heif' THEN 1 ELSE 2 END
   LIMIT 1),
  (SELECT f.content_hash FROM files f WHERE f.photo_id = p.id
   ORDER BY CASE f.kind WHEN 'jpeg' THEN 0 WHEN 'heif' THEN 1 ELSE 2 END
   LIMIT 1)";

pub fn list_singles(
    conn: &Connection,
    source_id: i64,
    offset: i64,
    limit: i64,
) -> Result<Vec<PhotoRow>> {
    let sql = format!(
        "SELECT p.id, p.dir, p.stem, p.capture_time_ms, p.frame_index, p.keep,
                p.sharpness, {PREVIEW_FILE_SELECT}
         FROM photos p
         WHERE p.source_id = ?1 AND p.burst_id IS NULL
         ORDER BY p.capture_time_ms
         LIMIT ?2 OFFSET ?3"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![source_id, limit, offset], photo_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn burst_frames(conn: &Connection, burst_id: i64) -> Result<Vec<PhotoRow>> {
    let sql = format!(
        "SELECT p.id, p.dir, p.stem, p.capture_time_ms, p.frame_index, p.keep,
                p.sharpness, {PREVIEW_FILE_SELECT}
         FROM photos p
         WHERE p.burst_id = ?1
         ORDER BY p.frame_index"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![burst_id], photo_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

// ---------- preprocessing ----------

/// One photo's preview-source file, for still extraction.
#[derive(Debug, Clone)]
pub struct PreprocessPhotoRow {
    pub photo_id: i64,
    pub rel_path: String,
    pub kind: String,
    pub content_hash: String,
    pub orientation: Option<u16>,
}

pub fn photos_for_preprocess(
    conn: &Connection,
    source_id: i64,
) -> Result<Vec<PreprocessPhotoRow>> {
    let mut stmt = conn.prepare(
        "SELECT p.id, f.rel_path, f.kind, f.content_hash, p.orientation
         FROM photos p
         JOIN files f ON f.id =
           (SELECT f2.id FROM files f2 WHERE f2.photo_id = p.id
            ORDER BY CASE f2.kind WHEN 'jpeg' THEN 0 WHEN 'heif' THEN 1 ELSE 2 END
            LIMIT 1)
         WHERE p.source_id = ?1
         ORDER BY p.capture_time_ms",
    )?;
    let rows = stmt
        .query_map(params![source_id], |r| {
            Ok(PreprocessPhotoRow {
                photo_id: r.get(0)?,
                rel_path: r.get(1)?,
                kind: r.get(2)?,
                content_hash: r.get(3)?,
                orientation: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn set_sharpness(conn: &Connection, photo_id: i64, sharpness: f64) -> Result<()> {
    conn.execute(
        "UPDATE photos SET sharpness = ?2 WHERE id = ?1",
        params![photo_id, sharpness],
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct BurstVideoJob {
    pub burst_id: i64,
    pub fps_estimate: Option<f64>,
    /// Previously rendered video, if any (may predate a cache move).
    pub video_cache_path: Option<String>,
}

pub fn burst_ids_for_video(conn: &Connection, source_id: i64) -> Result<Vec<BurstVideoJob>> {
    let mut stmt = conn.prepare(
        "SELECT id, fps_estimate, video_cache_path FROM bursts
         WHERE source_id = ?1 ORDER BY start_ms",
    )?;
    let rows = stmt
        .query_map(params![source_id], |r| {
            Ok(BurstVideoJob {
                burst_id: r.get(0)?,
                fps_estimate: r.get(1)?,
                video_cache_path: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn burst_preview_hashes(conn: &Connection, burst_id: i64) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT (SELECT f.content_hash FROM files f WHERE f.photo_id = p.id
                 ORDER BY CASE f.kind WHEN 'jpeg' THEN 0 WHEN 'heif' THEN 1 ELSE 2 END
                 LIMIT 1)
         FROM photos p WHERE p.burst_id = ?1 ORDER BY p.frame_index",
    )?;
    let rows = stmt
        .query_map(params![burst_id], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Content hash of one photo's preview-source file.
pub fn preview_hash_for_photo(conn: &Connection, photo_id: i64) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT f.content_hash FROM files f WHERE f.photo_id = ?1
             ORDER BY CASE f.kind WHEN 'jpeg' THEN 0 WHEN 'heif' THEN 1 ELSE 2 END
             LIMIT 1",
            params![photo_id],
            |r| r.get(0),
        )
        .optional()?)
}

pub fn set_video_cache(conn: &Connection, burst_id: i64, path: &str) -> Result<()> {
    conn.execute(
        "UPDATE bursts SET video_cache_path = ?2, preprocessed_at = unixepoch()
         WHERE id = ?1",
        params![burst_id, path],
    )?;
    Ok(())
}

pub fn get_setting(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            params![key],
            |r| r.get(0),
        )
        .optional()?)
}

pub fn set_setting(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

// ---------- export ----------

#[derive(Debug, Clone)]
pub struct KeptFileRow {
    pub file_id: i64,
    pub rel_path: String,
    pub size: i64,
    pub content_hash: String,
}

/// All files of every kept photo, grouped per photo (pairs travel together).
pub fn kept_photo_files(
    conn: &Connection,
    source_id: i64,
) -> Result<Vec<(i64, Vec<KeptFileRow>)>> {
    let mut stmt = conn.prepare(
        "SELECT p.id, f.id, f.rel_path, f.size, f.content_hash
         FROM photos p JOIN files f ON f.photo_id = p.id
         WHERE p.source_id = ?1 AND p.keep = 1
         ORDER BY p.capture_time_ms, p.id, f.kind",
    )?;
    let mut result: Vec<(i64, Vec<KeptFileRow>)> = Vec::new();
    let rows = stmt.query_map(params![source_id], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            KeptFileRow {
                file_id: r.get(1)?,
                rel_path: r.get(2)?,
                size: r.get(3)?,
                content_hash: r.get(4)?,
            },
        ))
    })?;
    for row in rows {
        let (photo_id, file) = row?;
        match result.last_mut() {
            Some((id, files)) if *id == photo_id => files.push(file),
            _ => result.push((photo_id, vec![file])),
        }
    }
    Ok(result)
}

#[derive(Debug, Clone)]
pub struct KeptVideoRow {
    pub burst_id: i64,
    pub start_ms: i64,
    pub video_cache_path: String,
    pub export_rate: f64,
}

pub fn kept_videos(conn: &Connection, source_id: i64) -> Result<Vec<KeptVideoRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, start_ms, video_cache_path, export_rate FROM bursts
         WHERE source_id = ?1 AND keep_video = 1 AND video_cache_path IS NOT NULL
         ORDER BY start_ms",
    )?;
    let rows = stmt
        .query_map(params![source_id], |r| {
            Ok(KeptVideoRow {
                burst_id: r.get(0)?,
                start_ms: r.get(1)?,
                video_cache_path: r.get(2)?,
                export_rate: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn mark_video_exported(conn: &Connection, burst_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE bursts SET video_exported_at = unixepoch() WHERE id = ?1",
        params![burst_id],
    )?;
    Ok(())
}

pub fn log_export(
    conn: &Connection,
    job_id: &str,
    photo_id: Option<i64>,
    file_id: Option<i64>,
    dest_path: &str,
    action: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO export_log (job_id, photo_id, file_id, dest_path, action, completed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, unixepoch())",
        params![job_id, photo_id, file_id, dest_path, action],
    )?;
    Ok(())
}

// ---------- decisions ----------

pub fn set_frame_keep(conn: &Connection, photo_ids: &[i64], keep: bool) -> Result<()> {
    for id in photo_ids {
        conn.execute(
            "UPDATE photos SET keep = ?2 WHERE id = ?1",
            params![id, keep as i64],
        )?;
    }
    Ok(())
}

pub fn set_burst_status(conn: &Connection, burst_id: i64, status: &str) -> Result<()> {
    conn.execute(
        "UPDATE bursts SET status = ?2 WHERE id = ?1",
        params![burst_id, status],
    )?;
    Ok(())
}

pub fn set_keep_video(conn: &Connection, burst_id: i64, keep: bool) -> Result<()> {
    conn.execute(
        "UPDATE bursts SET keep_video = ?2 WHERE id = ?1",
        params![burst_id, keep as i64],
    )?;
    Ok(())
}

pub fn set_export_rate(conn: &Connection, burst_id: i64, rate: f64) -> Result<()> {
    conn.execute(
        "UPDATE bursts SET export_rate = ?2 WHERE id = ?1",
        params![burst_id, rate.clamp(0.01, 4.0)],
    )?;
    Ok(())
}

/// Frame inputs for video rendering: preview-source file per frame.
#[derive(Debug, Clone)]
pub struct BurstFrameSource {
    pub content_hash: String,
    pub rel_path: String,
    pub kind: String,
}

pub fn burst_frame_sources(conn: &Connection, burst_id: i64) -> Result<Vec<BurstFrameSource>> {
    let mut stmt = conn.prepare(
        "SELECT f.content_hash, f.rel_path, f.kind
         FROM photos p
         JOIN files f ON f.id =
           (SELECT f2.id FROM files f2 WHERE f2.photo_id = p.id
            ORDER BY CASE f2.kind WHEN 'jpeg' THEN 0 WHEN 'heif' THEN 1 ELSE 2 END
            LIMIT 1)
         WHERE p.burst_id = ?1 ORDER BY p.frame_index",
    )?;
    let rows = stmt
        .query_map(params![burst_id], |r| {
            Ok(BurstFrameSource {
                content_hash: r.get(0)?,
                rel_path: r.get(1)?,
                kind: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

#[derive(Debug, Clone, Default)]
pub struct StatsRow {
    pub total_bursts: i64,
    pub decided_bursts: i64,
    pub total_singles: i64,
    pub kept_photos: i64,
    pub kept_videos: i64,
}

pub fn progress_stats(conn: &Connection, source_id: i64) -> Result<StatsRow> {
    let (total_bursts, decided_bursts, kept_videos) = conn.query_row(
        "SELECT COUNT(*),
                SUM(CASE WHEN status != 'undecided' THEN 1 ELSE 0 END),
                SUM(keep_video)
         FROM bursts WHERE source_id = ?1",
        params![source_id],
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                r.get::<_, Option<i64>>(2)?.unwrap_or(0),
            ))
        },
    )?;
    let total_singles: i64 = conn.query_row(
        "SELECT COUNT(*) FROM photos WHERE source_id = ?1 AND burst_id IS NULL",
        params![source_id],
        |r| r.get(0),
    )?;
    let kept_photos: i64 = conn.query_row(
        "SELECT COUNT(*) FROM photos WHERE source_id = ?1 AND keep = 1",
        params![source_id],
        |r| r.get(0),
    )?;
    Ok(StatsRow {
        total_bursts,
        decided_bursts,
        total_singles,
        kept_photos,
        kept_videos,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_in_memory;
    use crate::indexer::exif::{CaptureTime, ExifSummary};
    use crate::indexer::grouping::group_bursts;
    use crate::indexer::pairing::{FileKind, LogicalPhoto, ScannedFile};

    fn record(dir: &str, stem: &str, time_ms: i64, with_raw: bool) -> PhotoRecord {
        let mut files = vec![ScannedFile {
            rel_path: format!("{dir}/{stem}.JPG"),
            kind: FileKind::Jpeg,
            size: 1000,
            mtime: 1,
        }];
        let mut hashes = vec![format!("jpg-{stem}")];
        if with_raw {
            files.insert(
                0,
                ScannedFile {
                    rel_path: format!("{dir}/{stem}.ARW"),
                    kind: FileKind::Raw,
                    size: 5000,
                    mtime: 1,
                },
            );
            hashes.insert(0, format!("raw-{stem}"));
        }
        PhotoRecord {
            photo: LogicalPhoto {
                dir: dir.to_string(),
                stem: stem.to_string(),
                files,
            },
            exif: ExifSummary {
                capture: Some(CaptureTime {
                    time_ms,
                    low_precision: false,
                }),
                width: Some(6000),
                height: Some(4000),
                orientation: Some(1),
            },
            hashes,
        }
    }

    fn index_photos(conn: &Connection, source_id: i64, recs: &[PhotoRecord]) {
        for rec in recs {
            upsert_photo(conn, source_id, rec).unwrap();
        }
        let ungrouped = ungrouped_photos(conn, source_id).unwrap();
        let grouping = group_bursts(&ungrouped, 250, 3);
        apply_grouping(conn, source_id, &grouping).unwrap();
    }

    #[test]
    fn end_to_end_index_group_browse() {
        let conn = init_in_memory();
        let source = upsert_source(&conn, "/Volumes/CARD").unwrap();

        // A 5-frame burst plus one lone photo.
        let mut recs: Vec<_> = (0..5)
            .map(|i| record("d", &format!("dsc0000{i}"), 1000 + i * 10, true))
            .collect();
        recs.push(record("d", "dsc09999", 99_000, false));
        index_photos(&conn, source.id, &recs);

        let bursts = list_bursts(&conn, source.id, None, 0, 10).unwrap();
        assert_eq!(bursts.len(), 1);
        assert_eq!(bursts[0].frame_count, 5);
        assert_eq!(bursts[0].status, "undecided");
        assert!(bursts[0].hero_photo_id > 0);

        let singles = list_singles(&conn, source.id, 0, 10).unwrap();
        assert_eq!(singles.len(), 1);
        assert_eq!(singles[0].stem, "dsc09999");

        let frames = burst_frames(&conn, bursts[0].id).unwrap();
        assert_eq!(frames.len(), 5);
        assert_eq!(frames[0].frame_index, Some(0));
        // Pair photo previews from the JPEG.
        assert_eq!(frames[0].preview_kind, "jpeg");
        assert!(frames[0].preview_rel_path.ends_with(".JPG"));
    }

    #[test]
    fn reindex_preserves_decisions() {
        let conn = init_in_memory();
        let source = upsert_source(&conn, "/Volumes/CARD").unwrap();
        let recs: Vec<_> = (0..4)
            .map(|i| record("d", &format!("f{i}"), 1000 + i * 10, false))
            .collect();
        index_photos(&conn, source.id, &recs);

        let bursts = list_bursts(&conn, source.id, None, 0, 10).unwrap();
        let burst_id = bursts[0].id;
        let frames = burst_frames(&conn, burst_id).unwrap();
        set_frame_keep(&conn, &[frames[1].id], true).unwrap();
        set_burst_status(&conn, burst_id, "done").unwrap();
        set_keep_video(&conn, burst_id, true).unwrap();

        // Re-index the same files (e.g. card re-mounted).
        index_photos(&conn, source.id, &recs);

        let bursts = list_bursts(&conn, source.id, None, 0, 10).unwrap();
        assert_eq!(bursts.len(), 1, "no duplicate bursts after re-index");
        assert_eq!(bursts[0].id, burst_id);
        assert_eq!(bursts[0].status, "done");
        assert!(bursts[0].keep_video);
        assert_eq!(bursts[0].kept_count, 1);
    }

    #[test]
    fn vanished_photos_are_deleted_with_their_bursts() {
        let conn = init_in_memory();
        let source = upsert_source(&conn, "/Volumes/CARD").unwrap();
        let recs: Vec<_> = (0..3)
            .map(|i| record("d", &format!("f{i}"), 1000 + i * 10, false))
            .collect();
        index_photos(&conn, source.id, &recs);
        assert_eq!(list_bursts(&conn, source.id, None, 0, 10).unwrap().len(), 1);

        let seen = std::collections::HashSet::new(); // everything gone
        let deleted = delete_vanished_photos(&conn, source.id, &seen).unwrap();
        assert_eq!(deleted, 3);
        assert!(list_bursts(&conn, source.id, None, 0, 10).unwrap().is_empty());
    }

    #[test]
    fn progress_stats_counts() {
        let conn = init_in_memory();
        let source = upsert_source(&conn, "/Volumes/CARD").unwrap();
        let recs: Vec<_> = (0..3)
            .map(|i| record("d", &format!("f{i}"), 1000 + i * 10, false))
            .collect();
        index_photos(&conn, source.id, &recs);
        let bursts = list_bursts(&conn, source.id, None, 0, 10).unwrap();
        set_burst_status(&conn, bursts[0].id, "rejected").unwrap();

        let stats = progress_stats(&conn, source.id).unwrap();
        assert_eq!(stats.total_bursts, 1);
        assert_eq!(stats.decided_bursts, 1);
        assert_eq!(stats.total_singles, 0);
    }
}
