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
//! The output path argument is optional; it defaults to `/tmp/demo_timeline.mp4`.

use std::path::PathBuf;
use std::process::ExitCode;

use tellur_core::raster::{RasterImage, Resolution};
use tellur_core::render_context::RenderContext;
use tellur_core::time::TimelineTime;
use tellur_core::timeline::Timeline;
use tellur_core::timeline_component::{resolve_with_canvas, ResolvedTimeline};
use tellur_renderer::FfmpegEncoder;

#[path = "demo_scene/mod.rs"]
mod scene;

const FPS: u32 = 60;

/// Adapts a resolved [`TimelineComponent`] tree to the legacy [`Timeline`] trait
/// the [`FfmpegEncoder::encode`] video path drives.
///
/// The old encode path (no audio second input) keeps the mp4 byte-identical to
/// the pre-timeline scene; the A/V `encode_timeline` path would always mux a
/// (here silent) audio stream, which this scene has none of. A `None` frame —
/// the timeline contributing nothing at `t` — is emitted as a transparent
/// fill, matching `ResolvedTimeline`'s own export semantics.
struct ResolvedAdapter {
    resolved: ResolvedTimeline,
}

impl Timeline for ResolvedAdapter {
    fn duration(&self) -> f32 {
        self.resolved.duration()
    }

    fn build(&self, t: TimelineTime, target: Resolution, ctx: &mut dyn RenderContext) -> RasterImage {
        self.resolved
            .frame(t, target, ctx)
            .unwrap_or_else(|| transparent(target))
    }
}

/// A fully transparent RGBA frame at `target` resolution.
fn transparent(target: Resolution) -> RasterImage {
    let bytes = (target.width as usize) * (target.height as usize) * 4;
    RasterImage::cpu(
        target.width,
        target.height,
        tellur_core::raster::PixelFormat::Rgba8,
        vec![0u8; bytes],
    )
}

fn main() -> ExitCode {
    let output = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/demo_timeline.mp4"));

    let (width, height) = scene::SCENE_RESOLUTION;

    // Resolve the scene once against its authoring canvas (matching the live
    // plugin's `canvas = (1920, 1080)`), then drive the resolved tree through
    // the byte-identical video-only encode path.
    let resolved = match resolve_with_canvas(scene::build_timeline(), scene::SCENE_CANVAS) {
        Ok(resolved) => resolved,
        Err(err) => {
            eprintln!("resolve failed: {err}");
            return ExitCode::FAILURE;
        }
    };
    let timeline = ResolvedAdapter { resolved };

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
