//! Debug helper: dump every EXIF field kamadak-exif finds in a file.
//! Usage: cargo run --example exifdump -- /path/to/photo.jpg

fn main() {
    let path = std::env::args().nth(1).expect("usage: exifdump <file>");
    let file = std::fs::File::open(&path).expect("open");
    let mut reader = std::io::BufReader::new(file);
    let exif = exif::Reader::new()
        .read_from_container(&mut reader)
        .expect("parse");
    for f in exif.fields() {
        println!(
            "{:30} ifd={:?} value={}",
            f.tag.to_string(),
            f.ifd_num,
            f.display_value()
        );
    }

    let summary =
        rust_lib_photo_review_app::indexer::exif::read_exif(std::path::Path::new(&path));
    println!("\nread_exif() -> {summary:?}");
}
