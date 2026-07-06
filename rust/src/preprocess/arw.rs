//! Extract the embedded JPEG preview from a Sony ARW.
//!
//! Strategy: scan for JFIF SOI/EOI marker pairs and return the largest
//! well-formed JPEG span. This is model-agnostic and robust (exiftool-style
//! TIFF/MakerNote walking can come later as an optimisation); ARW files are
//! only read this way when the user shot RAW without a JPEG/HEIF sidecar.

use std::path::Path;

use anyhow::{bail, Context, Result};

/// Minimum bytes for a span to count as a real preview (not the 160px thumb).
const MIN_PREVIEW_BYTES: usize = 100 * 1024;

pub fn extract_largest_jpeg(path: &Path) -> Result<Vec<u8>> {
    let data = std::fs::read(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let mut best: Option<(usize, usize)> = None; // (start, len)

    let mut i = 0;
    while i + 3 < data.len() {
        // SOI followed by a marker byte (FFD8 FF..)
        if data[i] == 0xFF && data[i + 1] == 0xD8 && data[i + 2] == 0xFF {
            if let Some(end) = find_eoi(&data, i + 2) {
                let len = end - i;
                if len >= MIN_PREVIEW_BYTES
                    && best.map(|(_, l)| len > l).unwrap_or(true)
                {
                    best = Some((i, len));
                }
                // Continue searching *after* this JPEG.
                i = end;
                continue;
            }
        }
        i += 1;
    }

    match best {
        Some((start, len)) => Ok(data[start..start + len].to_vec()),
        None => bail!(
            "no embedded JPEG preview >= {} KiB found in {}",
            MIN_PREVIEW_BYTES / 1024,
            path.display()
        ),
    }
}

/// Walk JPEG segments from just after SOI to locate the matching EOI.
/// Returns the index one past the EOI marker.
fn find_eoi(data: &[u8], mut i: usize) -> Option<usize> {
    // i points at the first marker after SOI.
    loop {
        if i + 1 >= data.len() || data[i] != 0xFF {
            return None;
        }
        let marker = data[i + 1];
        match marker {
            0xD9 => return Some(i + 2), // EOI (bare, before scan data)
            0xDA => {
                // Start of scan: entropy-coded data; scan for EOI.
                let mut j = i + 2;
                while j + 1 < data.len() {
                    if data[j] == 0xFF && data[j + 1] == 0xD9 {
                        return Some(j + 2);
                    }
                    j += 1;
                }
                return None;
            }
            // Standalone markers without length.
            0x01 | 0xD0..=0xD7 => i += 2,
            _ => {
                if i + 3 >= data.len() {
                    return None;
                }
                let seg_len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
                if seg_len < 2 {
                    return None;
                }
                i += 2 + seg_len;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Build a minimal fake JPEG of a given payload size.
    fn fake_jpeg(payload: usize) -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8, 0xFF, 0xDA, 0x00, 0x04, 0x00, 0x00];
        v.extend(std::iter::repeat(0xAB).take(payload));
        v.extend([0xFF, 0xD9]);
        v
    }

    #[test]
    fn picks_largest_span_and_skips_small_thumb() {
        let small = fake_jpeg(10 * 1024); // 10 KiB "thumbnail"
        let large = fake_jpeg(300 * 1024); // 300 KiB "preview"

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fake.arw");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"II*\x00SOMETIFFHEADERPADDING").unwrap();
        f.write_all(&small).unwrap();
        f.write_all(b"PADDING").unwrap();
        f.write_all(&large).unwrap();
        f.write_all(b"TRAILER").unwrap();
        drop(f);

        let jpeg = extract_largest_jpeg(&path).unwrap();
        assert_eq!(jpeg.len(), large.len());
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
        assert_eq!(&jpeg[jpeg.len() - 2..], &[0xFF, 0xD9]);
    }

    #[test]
    fn errors_when_no_preview() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.arw");
        std::fs::write(&path, b"II*\x00 no jpeg here").unwrap();
        assert!(extract_largest_jpeg(&path).is_err());
    }
}
