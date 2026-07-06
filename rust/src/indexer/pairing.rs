//! RAW+JPEG/HEIF pairing: files sharing a directory and stem are one logical
//! photo (e.g. DSC01234.ARW + DSC01234.JPG). Pure and unit-tested.

use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Raw,
    Jpeg,
    Heif,
}

impl FileKind {
    pub fn from_path(path: &Path) -> Option<FileKind> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "arw" => Some(FileKind::Raw),
            "jpg" | "jpeg" => Some(FileKind::Jpeg),
            "hif" | "heif" | "heic" => Some(FileKind::Heif),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            FileKind::Raw => "raw",
            FileKind::Jpeg => "jpeg",
            FileKind::Heif => "heif",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScannedFile {
    /// Path relative to the source root.
    pub rel_path: String,
    pub kind: FileKind,
    pub size: u64,
    pub mtime: i64,
}

/// One logical photo: a RAW+sidecar pair or a lone file.
#[derive(Debug, Clone, PartialEq)]
pub struct LogicalPhoto {
    pub dir: String,
    pub stem: String,
    pub files: Vec<ScannedFile>,
}

impl LogicalPhoto {
    /// The best file to read previews from: sidecar JPEG > HEIF > RAW.
    pub fn preview_source(&self) -> &ScannedFile {
        self.files
            .iter()
            .min_by_key(|f| match f.kind {
                FileKind::Jpeg => 0,
                FileKind::Heif => 1,
                FileKind::Raw => 2,
            })
            .expect("LogicalPhoto always has at least one file")
    }
}

/// Group scanned files into logical photos by (directory, lowercase stem).
/// BTreeMap keeps output deterministic (sorted by dir then stem).
pub fn pair_files(files: Vec<ScannedFile>) -> Vec<LogicalPhoto> {
    let mut by_key: BTreeMap<(String, String), Vec<ScannedFile>> = BTreeMap::new();
    for file in files {
        let path = Path::new(&file.rel_path);
        let dir = path
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        by_key.entry((dir, stem)).or_default().push(file);
    }

    by_key
        .into_iter()
        .map(|((dir, stem), mut files)| {
            // Deterministic order within a photo: raw first, then jpeg, heif.
            files.sort_by_key(|f| match f.kind {
                FileKind::Raw => 0,
                FileKind::Jpeg => 1,
                FileKind::Heif => 2,
            });
            LogicalPhoto { dir, stem, files }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(rel_path: &str) -> ScannedFile {
        ScannedFile {
            rel_path: rel_path.to_string(),
            kind: FileKind::from_path(Path::new(rel_path)).unwrap(),
            size: 1000,
            mtime: 0,
        }
    }

    #[test]
    fn raw_jpeg_pair_becomes_one_photo() {
        let photos = pair_files(vec![
            file("DCIM/100MSDCF/DSC01234.ARW"),
            file("DCIM/100MSDCF/DSC01234.JPG"),
        ]);
        assert_eq!(photos.len(), 1);
        assert_eq!(photos[0].files.len(), 2);
        assert_eq!(photos[0].stem, "dsc01234");
        assert_eq!(photos[0].preview_source().kind, FileKind::Jpeg);
    }

    #[test]
    fn same_stem_different_dirs_are_separate() {
        let photos = pair_files(vec![
            file("DCIM/100MSDCF/DSC01234.JPG"),
            file("DCIM/101MSDCF/DSC01234.JPG"),
        ]);
        assert_eq!(photos.len(), 2);
    }

    #[test]
    fn jpeg_only_is_single_file_photo() {
        let photos = pair_files(vec![file("DCIM/100MSDCF/DSC00001.JPG")]);
        assert_eq!(photos.len(), 1);
        assert_eq!(photos[0].files.len(), 1);
        assert_eq!(photos[0].preview_source().kind, FileKind::Jpeg);
    }

    #[test]
    fn raw_only_previews_from_raw() {
        let photos = pair_files(vec![file("DCIM/100MSDCF/DSC00001.ARW")]);
        assert_eq!(photos[0].preview_source().kind, FileKind::Raw);
    }

    #[test]
    fn raw_heif_pair_previews_from_heif() {
        let photos = pair_files(vec![
            file("DCIM/100MSDCF/DSC00001.ARW"),
            file("DCIM/100MSDCF/DSC00001.HIF"),
        ]);
        assert_eq!(photos.len(), 1);
        assert_eq!(photos[0].preview_source().kind, FileKind::Heif);
    }

    #[test]
    fn case_insensitive_stem_matching() {
        let photos = pair_files(vec![
            file("DCIM/100MSDCF/dsc01234.arw"),
            file("DCIM/100MSDCF/DSC01234.JPG"),
        ]);
        assert_eq!(photos.len(), 1);
    }

    #[test]
    fn kind_detection() {
        assert_eq!(
            FileKind::from_path(Path::new("a/b.HEIC")),
            Some(FileKind::Heif)
        );
        assert_eq!(FileKind::from_path(Path::new("a/b.png")), None);
        assert_eq!(FileKind::from_path(Path::new("a/noext")), None);
    }
}
