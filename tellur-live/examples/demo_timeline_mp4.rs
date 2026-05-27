//! Encode the shared `demo_scene` timeline to an mp4 file via ffmpeg.
//!
//! The scene logic is the same one served by the `demo_timeline_plugin`
//! cdylib — both pull from `demo_scene/mod.rs` so the live preview and
//! the offline render stay in lock-step.
//!
//! Run with:
//! ```text
//! cargo run --release --example demo_timeline_mp4 -- /tmp/demo.mp4
//! ```
//! The output path argument is optional; it defaults to `./demo_timeline.mp4`.

use std::path::PathBuf;
use std::process::ExitCode;

use tellur_core::raster::Resolution;
use tellur_renderer::FfmpegEncoder;

#[path = "demo_scene/mod.rs"]
mod scene;

const FPS: u32 = 60;

fn main() -> ExitCode {
    let output = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("demo_timeline.mp4"));

    let (width, height) = scene::SCENE_RESOLUTION;
    let timeline = scene::build_timeline();

    println!(
        "Rendering \"{}\" at {}x{}@{}fps → {}",
        scene::TITLE,
        width,
        height,
        FPS,
        output.display(),
    );

    let result = FfmpegEncoder::new(Resolution::new(width, height), FPS)
        // libx264 + yuv420p keeps the file widely playable; crf 18 is
        // visually-lossless-ish and small enough for a demo deliverable.
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "18"])
        .encode(&timeline, &output);

    match result {
        Ok(()) => {
            println!("Wrote {}", output.display());
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("encode failed: {err}");
            ExitCode::FAILURE
        }
    }
}
