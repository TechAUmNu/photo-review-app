//! Isolated render test: originals -> render_video at several settings.
//! Usage: cargo run --example renderiso -- <jpg> <jpg> <jpg> <outdir>

use rust_lib_photo_review_app::preprocess::video::{
    find_ffmpeg, render_video, FrameInput, VideoJob,
};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (files, outdir) = args.split_at(args.len() - 1);
    let outdir = std::path::PathBuf::from(&outdir[0]);
    let frames: Vec<FrameInput> = files
        .iter()
        .map(|f| FrameInput::File(std::path::PathBuf::from(f)))
        .collect();
    let ffmpeg = find_ffmpeg(None)?;

    for (label, long_edge) in [("le2048", Some(2048)), ("le3840", Some(3840)), ("native", None)]
    {
        let out = outdir.join(format!("iso_{label}.mp4"));
        eprintln!("rendering {label}...");
        render_video(&VideoJob {
            ffmpeg: &ffmpeg,
            frames: &frames,
            fps: 30.0,
            out_path: &out,
            long_edge,
        })?;
        eprintln!("done {}", out.display());
    }
    Ok(())
}
