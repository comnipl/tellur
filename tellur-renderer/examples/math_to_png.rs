//! Render a line that mixes ordinary text with an inline LaTeX math span
//! ("This is the line y = 2/3 x^2.") centered on a white canvas, then
//! write it to PNG. Run with the `latex` feature:
//!
//! ```sh
//! cargo run -p tellur-renderer --example math_to_png --features latex
//! ```

use std::fs::File;

use tellur_core::builder::VectorBuilderPlacement;
use tellur_core::color::Color;
use tellur_core::geometry::{Anchor, Vec2};
use tellur_core::layer::VectorLayer;
use tellur_core::math::MathSpan;
use tellur_core::raster::{RasterComponent, RasterResidency, Resolution};
use tellur_core::render_context::PassThrough;
use tellur_core::shapes::Rectangle;
use tellur_core::text::{Text, SERIF};
use tellur_core::vector::Paint;
use tellur_renderer::Rasterizable;

fn main() {
    let scene_size = Vec2(1400.0, 360.0);
    let scene = VectorLayer::builder()
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
                .size(90.0)
                .fill(Paint::Solid(Color::rgb_u8(20, 20, 20)))
                .span("This is the line ")
                // An inline LaTeX formula — same `.span(...)` as text.
                .span(MathSpan::builder().source(r"y = \frac{2}{3} x^2"))
                .span(".")
                .anchored(Anchor::CENTER)
                .snap_to(Anchor::CENTER.point(scene_size)),
        )
        .build();

    let image = scene.rasterize().render(
        scene_size,
        Resolution::new(scene_size.0 as u32, scene_size.1 as u32),
        RasterResidency::Cpu,
        &mut PassThrough,
    );

    let out = "/tmp/math.png";
    let file = File::create(out).expect("create output file");
    image.export_png(file).expect("export PNG");
    println!("Wrote {} ({}x{})", out, image.width(), image.height());
}
