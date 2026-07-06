//! Burst grouping: pure functions over capture timestamps so they can be
//! unit-tested without touching the filesystem or database.

/// Input to grouping: one logical photo (RAW+JPEG pair or single file).
#[derive(Debug, Clone, PartialEq)]
pub struct GroupablePhoto {
    pub photo_id: i64,
    pub capture_time_ms: i64,
    /// Photos the user manually split/merged are never regrouped.
    pub burst_locked: bool,
}

/// One detected burst: indices into the (sorted) input slice.
#[derive(Debug, Clone, PartialEq)]
pub struct BurstGroup {
    /// photo_ids in capture order, frame_index = position in this vec.
    pub photo_ids: Vec<i64>,
    pub start_ms: i64,
    pub end_ms: i64,
}

impl BurstGroup {
    pub fn frame_count(&self) -> usize {
        self.photo_ids.len()
    }

    /// Estimated capture rate; None for zero-duration bursts.
    /// Snapped to the camera's standard rate ladder when close, so 1x
    /// playback is exactly real time despite ms-rounded timestamps.
    pub fn fps_estimate(&self) -> Option<f64> {
        let n = self.photo_ids.len();
        let dur_ms = self.end_ms - self.start_ms;
        if n < 2 || dur_ms <= 0 {
            return None;
        }
        Some(snap_fps((n as f64 - 1.0) * 1000.0 / dur_ms as f64))
    }
}

/// Snap a measured burst rate to the nearest standard camera rate when
/// within tolerance; short bursts measured over sub-second spans wobble
/// by a few percent (e.g. 62.3 for a 60fps drive).
pub fn snap_fps(estimate: f64) -> f64 {
    const STANDARD: &[f64] = &[
        5.0, 10.0, 15.0, 20.0, 24.0, 25.0, 30.0, 60.0, 120.0, 240.0,
    ];
    for &rate in STANDARD {
        if (estimate - rate).abs() / rate <= 0.12 {
            return rate;
        }
    }
    estimate
}

/// Result of grouping: bursts plus the photos that remain singles.
#[derive(Debug, Default, PartialEq)]
pub struct GroupingResult {
    pub bursts: Vec<BurstGroup>,
    pub single_photo_ids: Vec<i64>,
}

/// Group photos into bursts by adjacent capture-time gap.
///
/// Photos are sorted by `capture_time_ms` internally. A chain of photos whose
/// adjacent gaps are all `<= gap_ms` becomes a burst when it has at least
/// `min_burst_len` frames; shorter chains are singles. Locked photos are
/// excluded from grouping entirely (they keep their manual assignment) and
/// act as chain breakers for their neighbours only if absent — i.e. we simply
/// filter them out before chaining.
pub fn group_bursts(
    photos: &[GroupablePhoto],
    gap_ms: i64,
    min_burst_len: usize,
) -> GroupingResult {
    let mut sorted: Vec<&GroupablePhoto> =
        photos.iter().filter(|p| !p.burst_locked).collect();
    sorted.sort_by_key(|p| (p.capture_time_ms, p.photo_id));

    let mut result = GroupingResult::default();
    let mut chain: Vec<&GroupablePhoto> = Vec::new();

    let flush = |chain: &mut Vec<&GroupablePhoto>, result: &mut GroupingResult| {
        if chain.len() >= min_burst_len {
            result.bursts.push(BurstGroup {
                photo_ids: chain.iter().map(|p| p.photo_id).collect(),
                start_ms: chain.first().unwrap().capture_time_ms,
                end_ms: chain.last().unwrap().capture_time_ms,
            });
        } else {
            result
                .single_photo_ids
                .extend(chain.iter().map(|p| p.photo_id));
        }
        chain.clear();
    };

    for photo in sorted {
        match chain.last() {
            Some(prev) if photo.capture_time_ms - prev.capture_time_ms <= gap_ms => {
                chain.push(photo);
            }
            Some(_) => {
                flush(&mut chain, &mut result);
                chain.push(photo);
            }
            None => chain.push(photo),
        }
    }
    flush(&mut chain, &mut result);

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn photo(id: i64, t: i64) -> GroupablePhoto {
        GroupablePhoto {
            photo_id: id,
            capture_time_ms: t,
            burst_locked: false,
        }
    }

    const GAP: i64 = 250;
    const MIN_LEN: usize = 3;

    #[test]
    fn empty_input() {
        let r = group_bursts(&[], GAP, MIN_LEN);
        assert!(r.bursts.is_empty());
        assert!(r.single_photo_ids.is_empty());
    }

    #[test]
    fn all_singles_when_far_apart() {
        let photos = vec![photo(1, 0), photo(2, 10_000), photo(3, 20_000)];
        let r = group_bursts(&photos, GAP, MIN_LEN);
        assert!(r.bursts.is_empty());
        assert_eq!(r.single_photo_ids, vec![1, 2, 3]);
    }

    #[test]
    fn simple_burst_at_120fps() {
        // 8ms period (ms-rounded 120fps capture); 10 frames. The raw
        // estimate is 125 but snaps to the standard 120 rate.
        let photos: Vec<_> = (0..10).map(|i| photo(i, i * 8)).collect();
        let r = group_bursts(&photos, GAP, MIN_LEN);
        assert_eq!(r.bursts.len(), 1);
        assert_eq!(r.bursts[0].frame_count(), 10);
        assert!(r.single_photo_ids.is_empty());
        assert_eq!(r.bursts[0].fps_estimate().unwrap(), 120.0);
    }

    #[test]
    fn fps_snapping() {
        assert_eq!(snap_fps(62.3), 60.0);
        assert_eq!(snap_fps(53.3), 60.0); // 12% band catches short-burst noise
        assert_eq!(snap_fps(120.8), 120.0);
        assert_eq!(snap_fps(14.2), 15.0);
        // Way off any standard rate: left as measured.
        assert_eq!(snap_fps(43.0), 43.0);
    }

    #[test]
    fn gap_exactly_at_threshold_stays_in_burst() {
        let photos = vec![photo(1, 0), photo(2, 250), photo(3, 500)];
        let r = group_bursts(&photos, GAP, MIN_LEN);
        assert_eq!(r.bursts.len(), 1);
        assert_eq!(r.bursts[0].photo_ids, vec![1, 2, 3]);
    }

    #[test]
    fn gap_just_over_threshold_splits() {
        let photos = vec![photo(1, 0), photo(2, 251), photo(3, 502)];
        let r = group_bursts(&photos, GAP, MIN_LEN);
        assert!(r.bursts.is_empty());
        assert_eq!(r.single_photo_ids, vec![1, 2, 3]);
    }

    #[test]
    fn two_frame_chain_is_singles_with_min_len_3() {
        let photos = vec![photo(1, 0), photo(2, 100)];
        let r = group_bursts(&photos, GAP, MIN_LEN);
        assert!(r.bursts.is_empty());
        assert_eq!(r.single_photo_ids, vec![1, 2]);
    }

    #[test]
    fn two_bursts_separated_by_pause() {
        let mut photos: Vec<_> = (0..5).map(|i| photo(i, i * 10)).collect();
        photos.extend((0..5).map(|i| photo(100 + i, 60_000 + i * 10)));
        let r = group_bursts(&photos, GAP, MIN_LEN);
        assert_eq!(r.bursts.len(), 2);
        assert_eq!(r.bursts[0].photo_ids, vec![0, 1, 2, 3, 4]);
        assert_eq!(r.bursts[1].photo_ids, vec![100, 101, 102, 103, 104]);
    }

    #[test]
    fn unsorted_input_is_sorted_by_time() {
        let photos = vec![photo(3, 20), photo(1, 0), photo(2, 10)];
        let r = group_bursts(&photos, GAP, MIN_LEN);
        assert_eq!(r.bursts.len(), 1);
        assert_eq!(r.bursts[0].photo_ids, vec![1, 2, 3]);
    }

    #[test]
    fn identical_timestamps_tie_break_by_id() {
        // Missing subsec can collapse several frames onto the same second.
        let photos = vec![photo(2, 1000), photo(1, 1000), photo(3, 1000)];
        let r = group_bursts(&photos, GAP, MIN_LEN);
        assert_eq!(r.bursts.len(), 1);
        assert_eq!(r.bursts[0].photo_ids, vec![1, 2, 3]);
    }

    #[test]
    fn locked_photos_are_excluded() {
        let mut photos: Vec<_> = (0..5).map(|i| photo(i, i * 10)).collect();
        photos[2].burst_locked = true;
        let r = group_bursts(&photos, GAP, MIN_LEN);
        assert_eq!(r.bursts.len(), 1);
        assert_eq!(r.bursts[0].photo_ids, vec![0, 1, 3, 4]);
    }

    #[test]
    fn burst_followed_by_single() {
        let photos = vec![
            photo(1, 0),
            photo(2, 10),
            photo(3, 20),
            photo(4, 5_000),
        ];
        let r = group_bursts(&photos, GAP, MIN_LEN);
        assert_eq!(r.bursts.len(), 1);
        assert_eq!(r.bursts[0].photo_ids, vec![1, 2, 3]);
        assert_eq!(r.single_photo_ids, vec![4]);
    }
}
