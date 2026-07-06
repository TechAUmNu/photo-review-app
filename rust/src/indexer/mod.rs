//! Indexing pipeline: walk the source, read EXIF, hash files, pair
//! RAW+sidecars, group bursts, persist. CPU-bound stages run on rayon.

pub mod exif;
pub mod grouping;
pub mod hash;
pub mod pairing;
pub mod walk;

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::Result;
use rayon::prelude::*;

use crate::db::{self, queries};
use exif::ExifSummary;
use pairing::LogicalPhoto;

#[derive(Debug, Clone)]
pub struct PipelineProgress {
    /// "walk" | "analyze" | "group"
    pub phase: String,
    pub done: u64,
    pub total: u64,
}

#[derive(Debug, Clone)]
pub struct IndexOutcome {
    pub photos: u64,
    pub bursts: u64,
    pub singles: u64,
}

/// Defaults; adjustable via settings later.
pub const DEFAULT_GAP_MS: i64 = 250;
pub const DEFAULT_MIN_BURST_LEN: usize = 3;

/// Run a full index of `source_id`. `on_progress` is called from worker
/// threads (throttled by the caller if needed).
pub fn run_index(
    source_id: i64,
    on_progress: impl Fn(PipelineProgress) + Sync,
) -> Result<IndexOutcome> {
    let root = {
        let conn = db::conn()?;
        queries::get_source(&conn, source_id)?.root_path
    };
    let root = Path::new(&root);

    on_progress(PipelineProgress {
        phase: "walk".into(),
        done: 0,
        total: 0,
    });
    let files = walk::scan_source(root);
    let photos = pairing::pair_files(files);
    let total = photos.len() as u64;

    // Analyze in parallel: EXIF from the preview-source file, hash all files.
    // DB reads for hash-skipping happen up front (single connection).
    let prior_hashes: Vec<Vec<Option<String>>> = {
        let conn = db::conn()?;
        photos
            .iter()
            .map(|p| {
                p.files
                    .iter()
                    .map(|f| {
                        queries::find_existing_hash(
                            &conn, source_id, &f.rel_path, f.size, f.mtime,
                        )
                        .ok()
                        .flatten()
                    })
                    .collect()
            })
            .collect()
    };

    let done = AtomicUsize::new(0);
    let records: Vec<queries::PhotoRecord> = photos
        .into_par_iter()
        .zip(prior_hashes)
        .map(|(photo, priors)| {
            let rec = analyze_photo(root, photo, priors);
            let n = done.fetch_add(1, Ordering::Relaxed) as u64 + 1;
            if n % 50 == 0 || n == total {
                on_progress(PipelineProgress {
                    phase: "analyze".into(),
                    done: n,
                    total,
                });
            }
            rec
        })
        .collect();

    on_progress(PipelineProgress {
        phase: "group".into(),
        done: 0,
        total,
    });

    {
        let mut conn = db::conn()?;
        let tx = conn.transaction()?;
        let seen: HashSet<(String, String)> = records
            .iter()
            .map(|r| (r.photo.dir.clone(), r.photo.stem.clone()))
            .collect();
        for rec in &records {
            queries::upsert_photo(&tx, source_id, rec)?;
        }
        queries::delete_vanished_photos(&tx, source_id, &seen)?;

        // Incremental grouping: only photos not already in a burst.
        let ungrouped = queries::ungrouped_photos(&tx, source_id)?;
        let grouping = grouping::group_bursts(&ungrouped, DEFAULT_GAP_MS, DEFAULT_MIN_BURST_LEN);
        queries::apply_grouping(&tx, source_id, &grouping)?;
        queries::mark_indexed(&tx, source_id)?;
        tx.commit()?;
    } // guard dropped before re-locking below

    let conn = db::conn()?;
    let stats = queries::progress_stats(&conn, source_id)?;
    Ok(IndexOutcome {
        photos: records.len() as u64,
        bursts: stats.total_bursts as u64,
        singles: stats.total_singles as u64,
    })
}

fn analyze_photo(
    root: &Path,
    photo: LogicalPhoto,
    prior_hashes: Vec<Option<String>>,
) -> queries::PhotoRecord {
    // EXIF from the fastest useful file (sidecar JPEG/HEIF preferred).
    let preview = photo.preview_source();
    let exif_summary: ExifSummary = exif::read_exif(&root.join(&preview.rel_path));

    let hashes = photo
        .files
        .iter()
        .zip(prior_hashes)
        .map(|(f, prior)| match prior {
            Some(h) => h,
            None => hash::content_hash(&root.join(&f.rel_path), f.size)
                .unwrap_or_else(|_| format!("unreadable-{}", f.rel_path)),
        })
        .collect();

    queries::PhotoRecord {
        photo,
        exif: exif_summary,
        hashes,
    }
}
