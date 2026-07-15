//! Export a short Manim-style write-on animation for inline LaTeX math.
//! Run with the `latex` feature:
//!
//! ```sh
//! cargo run -p tellur-renderer --example math_write_to_mp4 --features latex
//! ```

use std::path::Path;

use tellur_core::builder::VectorBuilderPlacement;
use tellur_core::color::Color;
use tellur_core::effect::VectorBuilderWrite;
use tellur_core::geometry::{Anchor, Vec2};
use tellur_core::layer::VectorLayer;
use tellur_core::math::MathSpan;
use tellur_core::placement::VectorPlacement;
use tellur_core::raster::{RasterComponent, RasterResidency, Resolution};
use tellur_core::render_context::RenderContext;
use tellur_core::shapes::Rectangle;
use tellur_core::text::{Text, SERIF};
use tellur_core::timeline::timeline;
use tellur_core::vector::Paint;
use tellur_renderer::{FfmpegEncoder, Rasterizable};

fn main() {
    let scene_size = Vec2(1280.0, 720.0);
    let tl = timeline(2.0, move |t, target, residency, ctx| {
        frame(t, scene_size, target, residency, ctx)
    });

    let out = Path::new("/tmp/math-write.mp4");
    FfmpegEncoder::new(Resolution::new(1280, 720), 60)
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "18"])
        .encode(&tl, out)
        .expect("encode mp4");

    println!("Wrote {}", out.display());
}

fn frame(
    time: tellur_core::time::TimelineTime,
    scene_size: Vec2,
    target: Resolution,
    residency: RasterResidency,
    ctx: &mut dyn RenderContext,
) -> tellur_core::raster::RasterImage {
    VectorLayer::builder()
        .size(scene_size)
        .child(
            Rectangle::builder()
                .size(scene_size)
                .fill(Paint::Solid(Color::rgb_u8(255, 255, 255)))
                .place_at(Vec2::ZERO),
        )
        .child(
            Text::builder()
                .font(SERIF.clone())
                .size(118.0)
                .fill(Paint::Solid(Color::rgb_u8(18, 24, 32)))
                .span(MathSpan::builder().source(r"e^{i\pi}+1=0"))
                .write_from_with_speed(time, 0.12, 1800.0)
                .anchored(Anchor::CENTER)
                .snap_to(Anchor::CENTER.point(scene_size)),
        )
        .build()
        .rasterize()
        .render(scene_size, target, residency, ctx)
}
