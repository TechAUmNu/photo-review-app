//! Reproduce the exact preprocess video path for one burst and report
//! what the code decides (setting, frame inputs, output resolution).
//! Usage: cargo run --example rendercheck -- <db_path> <burst_id> <out.mp4>

use rust_lib_photo_review_app::db::{self, queries};
use rust_lib_photo_review_app::preprocess::{self, video};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let db_path = args.next().expect("db path");
    let burst_id: i64 = args.next().expect("burst id").parse()?;
    let out = std::path::PathBuf::from(args.next().expect("out path"));

    db::init(std::path::Path::new(&db_path))?;
    let conn = db::conn()?;

    let long_edge = preprocess::video_long_edge_setting();
    println!("video_long_edge setting = {long_edge}");

    let sources = queries::burst_frame_sources(&conn, burst_id)?;
    println!("frames = {}", sources.len());
    let job: Vec<_> = queries::burst_ids_for_video(&conn, 2)?
        .into_iter()
        .filter(|j| j.burst_id == burst_id)
        .collect();
    let (root, cache) = {
        let s = queries::get_source(&conn, 2)?;
        (
            std::path::PathBuf::from(s.root_path),
            std::path::PathBuf::from(s.cache_path.unwrap()),
        )
    };
    drop(conn);

    let fps = preprocess::encode_fps(job.first().and_then(|j| j.fps_estimate));
    println!("fps = {fps}");

    // Mirror preprocess::frame_input decisions.
    let mut file_count = 0;
    let mut arw_count = 0;
    let mut preview_count = 0;
    let frames: Vec<video::FrameInput> = sources
        .iter()
        .filter_map(|s| {
            let preview =
                preprocess::stills::still_paths(&cache, &s.content_hash).preview;
            if long_edge != 0 && long_edge <= preprocess::stills::PREVIEW_LONG_EDGE {
                preview_count += 1;
                return preview.exists().then_some(video::FrameInput::File(preview));
            }
            match s.kind.as_str() {
                "jpeg" => {
                    file_count += 1;
                    Some(video::FrameInput::File(root.join(&s.rel_path)))
                }
                "raw" => {
                    arw_count += 1;
                    Some(video::FrameInput::ArwEmbedded(root.join(&s.rel_path)))
                }
                _ => {
                    preview_count += 1;
                    preview.exists().then_some(video::FrameInput::File(preview))
                }
            }
        })
        .collect();
    println!(
        "inputs: {} original-jpeg, {} arw-embedded, {} cached-preview",
        file_count, arw_count, preview_count
    );
    if let Some(video::FrameInput::File(p)) = frames.first() {
        println!("first input: {}", p.display());
    }

    let ffmpeg = video::find_ffmpeg(None)?;
    video::render_video(&video::VideoJob {
        ffmpeg: &ffmpeg,
        frames: &frames[..frames.len().min(10)],
        fps,
        out_path: &out,
        long_edge: (long_edge > 0).then_some(long_edge),
    })?;
    println!("rendered {}", out.display());
    Ok(())
}
