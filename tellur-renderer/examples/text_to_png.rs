//! Render "Hello world!" with the middle span in red, centered on a
//! 1920x1080 white canvas, then write the result to PNG. Uses the
//! system default sans-serif font resolved via fontconfig.

use std::fs::File;

use tellur_core::builder::VectorBuilderPlacement;
use tellur_core::color::Color;
use tellur_core::geometry::{Anchor, Vec2};
use tellur_core::layer::VectorLayer;
use tellur_core::raster::{RasterComponent, RasterResidency, Resolution};
use tellur_core::render_context::PassThrough;
use tellur_core::shapes::Rectangle;
use tellur_core::text::{Text, TextSpan, SANS_SERIF};
use tellur_core::vector::Paint;
use tellur_renderer::Rasterizable;

fn main() {
    let scene_size = Vec2(1920.0, 1080.0);
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
                .font(SANS_SERIF.clone())
                .size(96.0)
                .fill(Paint::Solid(Color::rgb_u8(30, 30, 30)))
                .span("Hello ")
                .span(
                    TextSpan::builder()
                        .text("world")
                        .fill(Paint::Solid(Color::rgb_u8(220, 60, 60))),
                )
                .span("!")
                .anchored(Anchor::CENTER)
                .snap_to(Anchor::CENTER.point(scene_size)),
        )
        .build();

    let image = scene.rasterize().render(
        scene_size,
        Resolution::new(1920, 1080),
        RasterResidency::Cpu,
        &mut PassThrough,
    );

    let out = "/tmp/text.png";
    let file = File::create(out).expect("create output file");
    image.export_png(file).expect("export PNG");
    println!("Wrote {} ({}x{})", out, image.width(), image.height());
}
