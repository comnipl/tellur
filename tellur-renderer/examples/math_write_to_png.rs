//! Render several snapshots of a Manim-style write-on reveal for inline LaTeX
//! math. Run with the `latex` feature:
//!
//! ```sh
//! cargo run -p tellur-renderer --example math_write_to_png --features latex
//! ```

use std::fs::File;

use tellur_core::builder::VectorBuilderPlacement;
use tellur_core::color::Color;
use tellur_core::effect::VectorBuilderWrite;
use tellur_core::geometry::{Anchor, Vec2};
use tellur_core::layer::VectorLayer;
use tellur_core::math::MathSpan;
use tellur_core::placement::VectorPlacement;
use tellur_core::raster::{RasterComponent, Resolution};
use tellur_core::render_context::PassThrough;
use tellur_core::shapes::Rectangle;
use tellur_core::text::{Text, SERIF};
use tellur_core::time::TimelineTime;
use tellur_core::vector::Paint;
use tellur_renderer::Rasterizable;

fn main() {
    let scene_size = Vec2(2100.0, 520.0);
    let baseline_y = 240.0;
    let samples = [0.0, 0.65, 1.15, 1.35, 1.75];

    let mut scene = VectorLayer::builder().size(scene_size).child(
        Rectangle::builder()
            .size(scene_size)
            .fill(Paint::Solid(Color::rgb_u8(255, 255, 255)))
            .place_at(Vec2::ZERO),
    );

    for (i, progress) in samples.into_iter().enumerate() {
        let x = 220.0 + i as f32 * 415.0;
        scene = scene
            .child(
                Text::builder()
                    .font(SERIF.clone())
                    .size(72.0)
                    .fill(Paint::Solid(Color::rgb_u8(22, 28, 36)))
                    .span(MathSpan::builder().source(r"e^{i\pi}+1=0"))
                    .write_from_with_speed(TimelineTime::new(progress), 0.0, 1100.0)
                    .anchored(Anchor::CENTER)
                    .snap_to(Vec2(x, baseline_y)),
            )
            .child(
                Text::builder()
                    .font(SERIF.clone())
                    .size(26.0)
                    .fill(Paint::Solid(Color::rgb_u8(92, 100, 112)))
                    .span(format!("{progress:.2}"))
                    .anchored(Anchor::CENTER)
                    .snap_to(Vec2(x, baseline_y + 112.0)),
            );
    }

    let image = scene.build().rasterize().render(
        scene_size,
        Resolution::new(scene_size.0 as u32, scene_size.1 as u32),
        &mut PassThrough,
    );

    let out = "/tmp/math-write.png";
    let file = File::create(out).expect("create output file");
    image.export_png(file).expect("export PNG");
    println!("Wrote {} ({}x{})", out, image.width(), image.height());
}
