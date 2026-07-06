//! Content identity hashing: xxh3 of the first 64 KiB plus the file size.
//! Stable across card re-mounts and path changes; cheap enough for 40k files.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use xxhash_rust::xxh3::Xxh3;

const HEAD_LEN: usize = 64 * 1024;

pub fn content_hash(path: &Path, size: u64) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let mut buf = vec![0u8; HEAD_LEN];
    let mut read = 0;
    while read < HEAD_LEN {
        let n = file.read(&mut buf[read..])?;
        if n == 0 {
            break;
        }
        read += n;
    }

    let mut hasher = Xxh3::new();
    hasher.update(&buf[..read]);
    hasher.update(&size.to_le_bytes());
    Ok(format!("{:016x}", hasher.digest()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn stable_across_paths_and_sensitive_to_content() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.jpg");
        let b = dir.path().join("b.jpg");
        let c = dir.path().join("c.jpg");
        fs::write(&a, b"same content").unwrap();
        fs::write(&b, b"same content").unwrap();
        fs::write(&c, b"different!!!").unwrap();

        let ha = content_hash(&a, 12).unwrap();
        let hb = content_hash(&b, 12).unwrap();
        let hc = content_hash(&c, 12).unwrap();
        assert_eq!(ha, hb);
        assert_ne!(ha, hc);
        // Same head but different size must differ.
        let ha2 = content_hash(&a, 999).unwrap();
        assert_ne!(ha, ha2);
    }
}
