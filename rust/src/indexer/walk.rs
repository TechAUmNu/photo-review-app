//! Source-folder walk: find all camera files under a root.

use std::path::Path;

use walkdir::WalkDir;

use super::pairing::{FileKind, ScannedFile};

/// Walk `root` and return every supported camera file as a ScannedFile with
/// a root-relative path. Hidden files/dirs (dot-prefixed) are skipped, as is
/// anything unreadable.
pub fn scan_source(root: &Path) -> Vec<ScannedFile> {
    let mut files = Vec::new();
    let walker = WalkDir::new(root).follow_links(false).into_iter();

    // depth 0 is the root itself — never filter it, even if dot-prefixed.
    for entry in walker.filter_entry(|e| {
        e.depth() == 0
            || !e
                .file_name()
                .to_str()
                .map(|s| s.starts_with('.'))
                .unwrap_or(false)
    }) {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_file() {
            continue;
        }
        let Some(kind) = FileKind::from_path(entry.path()) else {
            continue;
        };
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        files.push(ScannedFile {
            rel_path: rel.to_string_lossy().into_owned(),
            kind,
            size: meta.len(),
            mtime,
        });
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn finds_supported_files_recursively() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("DCIM/100MSDCF");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("DSC00001.ARW"), b"raw").unwrap();
        fs::write(sub.join("DSC00001.JPG"), b"jpg").unwrap();
        fs::write(sub.join("notes.txt"), b"skip").unwrap();
        fs::write(dir.path().join(".hidden.jpg"), b"skip").unwrap();

        let mut files = scan_source(dir.path());
        files.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].rel_path, "DCIM/100MSDCF/DSC00001.ARW");
        assert_eq!(files[0].kind, FileKind::Raw);
        assert_eq!(files[1].kind, FileKind::Jpeg);
    }
}
