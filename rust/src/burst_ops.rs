//! Manual burst surgery: split, merge, regroup. These only mutate the DB
//! and delete stale cached MP4s; re-rendering happens by re-running the
//! (resumable) preprocess pass, which skips everything still cached.

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection};

use crate::db::{self, queries};
use crate::indexer::grouping;

/// Split a burst before `at_frame_index`; frames [at..] become a new burst.
/// All affected photos are locked against auto-regrouping.
/// Returns (original_burst_id, new_burst_id).
pub fn split_burst(burst_id: i64, at_frame_index: i64) -> Result<(i64, i64)> {
    let mut conn = db::conn()?;
    split_burst_on(&mut conn, burst_id, at_frame_index)
}

pub fn split_burst_on(
    conn: &mut Connection,
    burst_id: i64,
    at_frame_index: i64,
) -> Result<(i64, i64)> {
    let tx = conn.transaction()?;

    let frames = queries::burst_frames(&tx, burst_id)?;
    if at_frame_index <= 0 || at_frame_index as usize >= frames.len() {
        bail!("split point must be inside the burst (1..{})", frames.len() - 1);
    }
    let source_id: i64 = tx.query_row(
        "SELECT source_id FROM bursts WHERE id = ?1",
        params![burst_id],
        |r| r.get(0),
    )?;

    let (head, tail) = frames.split_at(at_frame_index as usize);

    // New burst for the tail.
    tx.execute(
        "INSERT INTO bursts (source_id, start_ms, end_ms, frame_count)
         VALUES (?1, 0, 0, 0)",
        params![source_id],
    )?;
    let new_id = tx.last_insert_rowid();
    for (i, f) in tail.iter().enumerate() {
        tx.execute(
            "UPDATE photos SET burst_id = ?1, frame_index = ?2, burst_locked = 1
             WHERE id = ?3",
            params![new_id, i as i64, f.id],
        )?;
    }
    for f in head {
        tx.execute(
            "UPDATE photos SET burst_locked = 1 WHERE id = ?1",
            params![f.id],
        )?;
    }
    recompute_burst(&tx, burst_id)?;
    recompute_burst(&tx, new_id)?;
    clear_video_cache(&tx, burst_id)?;
    tx.commit()?;
    Ok((burst_id, new_id))
}

/// Merge bursts into the earliest one; frames re-ordered by capture time.
/// Returns the surviving burst id.
pub fn merge_bursts(burst_ids: Vec<i64>) -> Result<i64> {
    let mut conn = db::conn()?;
    merge_bursts_on(&mut conn, burst_ids)
}

pub fn merge_bursts_on(conn: &mut Connection, burst_ids: Vec<i64>) -> Result<i64> {
    if burst_ids.len() < 2 {
        bail!("need at least two bursts to merge");
    }
    let tx = conn.transaction()?;

    // Survivor = burst with earliest start.
    let mut infos: Vec<(i64, i64)> = Vec::new(); // (id, start_ms)
    for id in &burst_ids {
        let start: i64 = tx
            .query_row(
                "SELECT start_ms FROM bursts WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .with_context(|| format!("burst {id} not found"))?;
        infos.push((*id, start));
    }
    infos.sort_by_key(|(_, s)| *s);
    let survivor = infos[0].0;

    // All frames of all bursts, ordered by capture time.
    let mut all: Vec<(i64, i64)> = Vec::new(); // (photo_id, capture_time_ms)
    for (id, _) in &infos {
        for f in queries::burst_frames(&tx, *id)? {
            all.push((f.id, f.capture_time_ms));
        }
    }
    all.sort_by_key(|(id, t)| (*t, *id));

    for (i, (photo_id, _)) in all.iter().enumerate() {
        tx.execute(
            "UPDATE photos SET burst_id = ?1, frame_index = ?2, burst_locked = 1
             WHERE id = ?3",
            params![survivor, i as i64, photo_id],
        )?;
    }
    for (id, _) in infos.iter().skip(1) {
        clear_video_cache(&tx, *id)?;
        tx.execute("DELETE FROM bursts WHERE id = ?1", params![id])?;
    }
    recompute_burst(&tx, survivor)?;
    clear_video_cache(&tx, survivor)?;
    tx.commit()?;
    Ok(survivor)
}

/// Re-run automatic grouping with a new gap for everything not locked.
/// Bursts containing any locked photo are left untouched.
pub fn regroup(source_id: i64, gap_ms: i64, min_burst_len: i64) -> Result<u64> {
    let mut conn = db::conn()?;
    regroup_on(&mut conn, source_id, gap_ms, min_burst_len)
}

pub fn regroup_on(
    conn: &mut Connection,
    source_id: i64,
    gap_ms: i64,
    min_burst_len: i64,
) -> Result<u64> {
    let tx = conn.transaction()?;

    // Bursts safe to dissolve: none of their photos are locked.
    let mut stmt = tx.prepare(
        "SELECT b.id FROM bursts b WHERE b.source_id = ?1 AND NOT EXISTS
           (SELECT 1 FROM photos p WHERE p.burst_id = b.id AND p.burst_locked = 1)",
    )?;
    let dissolvable: Vec<i64> = stmt
        .query_map(params![source_id], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);

    for id in &dissolvable {
        clear_video_cache(&tx, *id)?;
        tx.execute(
            "UPDATE photos SET burst_id = NULL, frame_index = NULL WHERE burst_id = ?1",
            params![id],
        )?;
        tx.execute("DELETE FROM bursts WHERE id = ?1", params![id])?;
    }

    let ungrouped = queries::ungrouped_photos(&tx, source_id)?;
    let result = grouping::group_bursts(&ungrouped, gap_ms, min_burst_len.max(2) as usize);
    let burst_count = result.bursts.len() as u64;
    queries::apply_grouping(&tx, source_id, &result)?;
    queries::set_setting(&tx, "gap_ms", &gap_ms.to_string())?;
    tx.commit()?;
    Ok(burst_count)
}

/// Recompute start/end/count/fps from current frames.
fn recompute_burst(conn: &Connection, burst_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE bursts SET
           start_ms = (SELECT MIN(capture_time_ms) FROM photos WHERE burst_id = ?1),
           end_ms = (SELECT MAX(capture_time_ms) FROM photos WHERE burst_id = ?1),
           frame_count = (SELECT COUNT(*) FROM photos WHERE burst_id = ?1)
         WHERE id = ?1",
        params![burst_id],
    )?;
    conn.execute(
        "UPDATE bursts SET fps_estimate =
           CASE WHEN frame_count >= 2 AND end_ms > start_ms
                THEN (frame_count - 1) * 1000.0 / (end_ms - start_ms)
                ELSE NULL END
         WHERE id = ?1",
        params![burst_id],
    )?;
    Ok(())
}

/// Delete the stale cached MP4 (if any) and clear the DB pointer so the
/// next preprocess run re-renders this burst.
fn clear_video_cache(conn: &Connection, burst_id: i64) -> Result<()> {
    let path: Option<String> = conn
        .query_row(
            "SELECT video_cache_path FROM bursts WHERE id = ?1",
            params![burst_id],
            |r| r.get(0),
        )
        .unwrap_or(None);
    if let Some(p) = path {
        let _ = std::fs::remove_file(p);
    }
    conn.execute(
        "UPDATE bursts SET video_cache_path = NULL, preprocessed_at = NULL
         WHERE id = ?1",
        params![burst_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_in_memory;
    use crate::db::queries::{
        apply_grouping, burst_frames, list_bursts, ungrouped_photos, upsert_photo,
        upsert_source, PhotoRecord,
    };
    use crate::indexer::exif::{CaptureTime, ExifSummary};
    use crate::indexer::pairing::{FileKind, LogicalPhoto, ScannedFile};

    fn seed_burst(conn: &Connection, source_id: i64, n: i64, t0: i64) {
        for i in 0..n {
            let stem = format!("f{t0}_{i}");
            upsert_photo(
                conn,
                source_id,
                &PhotoRecord {
                    photo: LogicalPhoto {
                        dir: "d".into(),
                        stem: stem.clone(),
                        files: vec![ScannedFile {
                            rel_path: format!("d/{stem}.JPG"),
                            kind: FileKind::Jpeg,
                            size: 100,
                            mtime: 1,
                        }],
                    },
                    exif: ExifSummary {
                        capture: Some(CaptureTime {
                            time_ms: t0 + i * 10,
                            low_precision: false,
                        }),
                        ..Default::default()
                    },
                    hashes: vec![format!("h-{stem}")],
                },
            )
            .unwrap();
        }
        let ungrouped = ungrouped_photos(conn, source_id).unwrap();
        let g = grouping::group_bursts(&ungrouped, 250, 3);
        apply_grouping(conn, source_id, &g).unwrap();
    }

    #[test]
    fn split_then_merge_roundtrip() {
        let mut conn = init_in_memory();
        let s = upsert_source(&conn, "/card").unwrap();
        seed_burst(&conn, s.id, 6, 1000);

        let bursts = list_bursts(&conn, s.id, None, 0, 10).unwrap();
        assert_eq!(bursts.len(), 1);
        let original = bursts[0].id;

        let (a, b) = split_burst_on(&mut conn, original, 4).unwrap();
        assert_eq!(a, original);
        let bursts = list_bursts(&conn, s.id, None, 0, 10).unwrap();
        assert_eq!(bursts.len(), 2);
        let fa = burst_frames(&conn, a).unwrap();
        let fb = burst_frames(&conn, b).unwrap();
        assert_eq!(fa.len(), 4);
        assert_eq!(fb.len(), 2);
        assert_eq!(fb[0].frame_index, Some(0), "tail reindexed from 0");

        // Locked bursts survive a regroup.
        regroup_on(&mut conn, s.id, 100, 3).unwrap();
        assert_eq!(list_bursts(&conn, s.id, None, 0, 10).unwrap().len(), 2);

        // Merge back together.
        let survivor = merge_bursts_on(&mut conn, vec![a, b]).unwrap();
        assert_eq!(survivor, a, "earliest burst survives");
        let bursts = list_bursts(&conn, s.id, None, 0, 10).unwrap();
        assert_eq!(bursts.len(), 1);
        assert_eq!(bursts[0].frame_count, 6);
        let frames = burst_frames(&conn, survivor).unwrap();
        let order: Vec<i64> = frames.iter().map(|f| f.capture_time_ms).collect();
        let mut sorted = order.clone();
        sorted.sort();
        assert_eq!(order, sorted, "frames in capture order after merge");
    }

    #[test]
    fn split_rejects_out_of_range() {
        let mut conn = init_in_memory();
        let s = upsert_source(&conn, "/card").unwrap();
        seed_burst(&conn, s.id, 4, 1000);
        let burst = list_bursts(&conn, s.id, None, 0, 10).unwrap()[0].id;
        assert!(split_burst_on(&mut conn, burst, 0).is_err());
        assert!(split_burst_on(&mut conn, burst, 4).is_err());
    }

    #[test]
    fn regroup_dissolves_unlocked_only() {
        let mut conn = init_in_memory();
        let s = upsert_source(&conn, "/card").unwrap();
        // Two bursts 60s apart, frames 10ms apart.
        seed_burst(&conn, s.id, 4, 1000);
        seed_burst(&conn, s.id, 4, 61_000);
        assert_eq!(list_bursts(&conn, s.id, None, 0, 10).unwrap().len(), 2);

        // Tighter gap than 10ms -> everything becomes singles.
        let n = regroup_on(&mut conn, s.id, 5, 3).unwrap();
        assert_eq!(n, 0);
        assert!(list_bursts(&conn, s.id, None, 0, 10).unwrap().is_empty());

        // Back to a workable gap.
        let n = regroup_on(&mut conn, s.id, 250, 3).unwrap();
        assert_eq!(n, 2);
    }
}
