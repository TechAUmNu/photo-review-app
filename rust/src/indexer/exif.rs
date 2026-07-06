//! Capture-time extraction. A9III writes DateTimeOriginal + SubSecTimeOriginal
//! (millisecond precision) — both are needed to order 120fps frames.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use exif::{In, Tag};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CaptureTime {
    /// Milliseconds since epoch, treating camera local time as UTC.
    /// Absolute correctness doesn't matter for grouping — only deltas do.
    pub time_ms: i64,
    /// True when SubSecTimeOriginal was missing (grouping precision suffers).
    pub low_precision: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ExifSummary {
    pub capture: Option<CaptureTime>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub orientation: Option<u16>,
}

pub fn read_exif(path: &Path) -> ExifSummary {
    let Ok(file) = File::open(path) else {
        return ExifSummary::default();
    };
    let mut reader = BufReader::new(file);
    let Ok(exif) = exif::Reader::new().read_from_container(&mut reader) else {
        return ExifSummary::default();
    };

    let field_str = |tag: Tag| -> Option<String> {
        exif.get_field(tag, In::PRIMARY)
            .map(|f| f.display_value().to_string().trim_matches('"').to_string())
    };
    let field_uint = |tag: Tag| -> Option<u32> {
        exif.get_field(tag, In::PRIMARY)
            .and_then(|f| f.value.get_uint(0))
    };

    let datetime = field_str(Tag::DateTimeOriginal).or_else(|| field_str(Tag::DateTime));
    let subsec = field_str(Tag::SubSecTimeOriginal).or_else(|| field_str(Tag::SubSecTime));

    let capture = datetime.and_then(|dt| parse_capture_time(&dt, subsec.as_deref()));

    ExifSummary {
        capture,
        width: field_uint(Tag::PixelXDimension).or_else(|| field_uint(Tag::ImageWidth)),
        height: field_uint(Tag::PixelYDimension).or_else(|| field_uint(Tag::ImageLength)),
        orientation: field_uint(Tag::Orientation).map(|v| v as u16),
    }
}

/// Parse EXIF "YYYY:MM:DD HH:MM:SS" plus optional subsecond digits.
/// SubSecTime is a *fraction* of a second: "5"=500ms, "57"=570ms, "573"=573ms.
pub fn parse_capture_time(datetime: &str, subsec: Option<&str>) -> Option<CaptureTime> {
    let dt = exif::DateTime::from_ascii(datetime.trim().as_bytes()).ok()?;
    let days = days_from_civil(dt.year as i64, dt.month as i64, dt.day as i64);
    let secs = days * 86_400 + dt.hour as i64 * 3600 + dt.minute as i64 * 60 + dt.second as i64;

    let subsec = subsec.map(str::trim).filter(|s| !s.is_empty());
    let (sub_ms, low_precision) = match subsec.and_then(parse_subsec_ms) {
        Some(ms) => (ms, false),
        None => (0, true),
    };

    Some(CaptureTime {
        time_ms: secs * 1000 + sub_ms,
        low_precision,
    })
}

fn parse_subsec_ms(s: &str) -> Option<i64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // Interpret as fractional digits, truncated/padded to milliseconds.
    let frac: String = format!("{:0<3}", &s[..s.len().min(3)]);
    frac.parse().ok()
}

/// Howard Hinnant's days_from_civil: civil date -> days since 1970-01-01.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_datetime_with_millis() {
        let t = parse_capture_time("2026:07:06 10:30:00", Some("573")).unwrap();
        assert!(!t.low_precision);
        assert_eq!(t.time_ms % 1000, 573);
    }

    #[test]
    fn short_subsec_is_fraction_not_millis() {
        // "5" means .5s = 500ms, not 5ms.
        let t = parse_capture_time("2026:07:06 10:30:00", Some("5")).unwrap();
        assert_eq!(t.time_ms % 1000, 500);
        let t2 = parse_capture_time("2026:07:06 10:30:00", Some("57")).unwrap();
        assert_eq!(t2.time_ms % 1000, 570);
    }

    #[test]
    fn long_subsec_truncates_to_millis() {
        let t = parse_capture_time("2026:07:06 10:30:00", Some("573912")).unwrap();
        assert_eq!(t.time_ms % 1000, 573);
    }

    #[test]
    fn missing_subsec_flags_low_precision() {
        let t = parse_capture_time("2026:07:06 10:30:00", None).unwrap();
        assert!(t.low_precision);
        assert_eq!(t.time_ms % 1000, 0);
    }

    #[test]
    fn subsec_ordering_across_second_boundary() {
        let a = parse_capture_time("2026:07:06 10:30:00", Some("995")).unwrap();
        let b = parse_capture_time("2026:07:06 10:30:01", Some("003")).unwrap();
        assert_eq!(b.time_ms - a.time_ms, 8);
    }

    #[test]
    fn epoch_sanity() {
        let t = parse_capture_time("1970:01:01 00:00:00", Some("000")).unwrap();
        assert_eq!(t.time_ms, 0);
        // 2026-07-06 is after 2026-01-01 (1767225600s).
        let t2 = parse_capture_time("2026:07:06 00:00:00", None).unwrap();
        assert!(t2.time_ms > 1_767_225_600_000);
    }

    #[test]
    fn garbage_returns_none() {
        assert!(parse_capture_time("not a date", None).is_none());
        assert!(parse_capture_time("", Some("5")).is_none());
    }
}
