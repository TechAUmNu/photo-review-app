//! Time the stills pipeline on real files.
//! Usage: cargo run --release --example benchstill -- <dir-with-jpgs> [count]

use std::time::Instant;

use rust_lib_photo_review_app::indexer::pairing::FileKind;
use rust_lib_photo_review_app::preprocess::stills;

fn main() {
    let dir = std::env::args().nth(1).expect("usage: benchstill <dir> [n]");
    let n: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(8);

    let root = std::path::PathBuf::from(&dir);
    let cache = tempfile::tempdir().unwrap();
    let mut files: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map(|x| x.eq_ignore_ascii_case("jpg"))
                .unwrap_or(false)
        })
        .take(n)
        .collect();
    files.sort_by_key(|e| e.path());

    let start = Instant::now();
    for (i, entry) in files.iter().enumerate() {
        let name = entry.file_name().to_string_lossy().into_owned();
        stills::process_still(
            &root,
            cache.path(),
            &name,
            FileKind::Jpeg,
            &format!("bench{i}"),
            Some(1),
            None,
        )
        .unwrap();
    }
    let elapsed = start.elapsed();
    println!(
        "{} photos in {:.2?} = {:.0} ms/photo (single-threaded)",
        files.len(),
        elapsed,
        elapsed.as_millis() as f64 / files.len() as f64
    );
}
