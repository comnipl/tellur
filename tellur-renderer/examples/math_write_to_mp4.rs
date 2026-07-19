//! Export a short Manim-style write-on animation for inline LaTeX math.
//! Run with the `latex` feature:
//!
//! ```sh
//! cargo run -p tellur-renderer --example math_write_to_mp4 --features latex
//! ```

use std::path::Path;

use tellur_core::builder::VectorBuilderPlacement;
use tellur_core::color::Color;
use tellur_core::component;
use tellur_core::effect::VectorBuilderWrite;
use tellur_core::geometry::{Anchor, Vec2};
use tellur_core::layer::VectorLayer;
use tellur_core::math::MathSpan;
use tellur_core::placement::VectorPlacement;
use tellur_core::raster::Resolution;
use tellur_core::shapes::Rectangle;
use tellur_core::text::{Text, SERIF};
use tellur_core::timeline_component::{resolve_with_canvas, Clock, Timed};
use tellur_core::vector::Paint;
use tellur_renderer::{AudioExport, FfmpegEncoder, Rasterizable};

const SCENE_SIZE: Vec2 = Vec2(1280.0, 720.0);

#[component(timeline)]
fn MathWrite(#[clock] clock: Clock) -> impl TimelineComponent {
    let time = clock.local();
    VectorLayer::builder()
        .size(SCENE_SIZE)
        .child(
            Rectangle::builder()
                .size(SCENE_SIZE)
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
                .snap_to(Anchor::CENTER),
        )
        .build()
        .rasterize()
}

fn main() {
    let resolved = resolve_with_canvas(MathWrite::builder().build().at(0.0..2.0), SCENE_SIZE)
        .expect("math-write timeline resolves");

    let out = Path::new("/tmp/math-write.mp4");
    FfmpegEncoder::new(Resolution::new(1280, 720), 60)
        .audio(AudioExport::Omit)
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "18"])
        .encode(&resolved, out)
        .expect("encode mp4");

    println!("Wrote {}", out.display());
}
