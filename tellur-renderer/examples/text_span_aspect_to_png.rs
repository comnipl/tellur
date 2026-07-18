//! Render text spans with independent X/Y glyph scale, then write PNG output.

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
    let scene_size = Vec2(1600.0, 900.0);
    let background = Paint::Solid(Color::rgb_u8(248, 248, 244));
    let ink = Paint::Solid(Color::rgb_u8(28, 32, 38));
    let wide = Paint::Solid(Color::rgb_u8(31, 120, 183));
    let tall = Paint::Solid(Color::rgb_u8(197, 72, 64));

    let scene = VectorLayer::builder()
        .size(scene_size)
        .child(
            Rectangle::builder()
                .size(scene_size)
                .fill(background)
                .place_at(Vec2::ZERO),
        )
        .child(
            Text::builder()
                .font(SANS_SERIF.clone())
                .size(112.0)
                .fill(ink.clone())
                .span("normal  ")
                .span(
                    TextSpan::builder()
                        .text("wide")
                        .fill(wide.clone())
                        .scale_x(1.55)
                        .scale_y(0.86),
                )
                .span("  ")
                .span(
                    TextSpan::builder()
                        .text("tall")
                        .fill(tall.clone())
                        .scale_x(0.72)
                        .scale_y(1.42),
                )
                .anchored(Anchor::CENTER)
                .snap_to(Anchor::CENTER),
        )
        .child(
            Text::builder()
                .font(SANS_SERIF.clone())
                .size(42.0)
                .fill(Paint::Solid(Color::rgb_u8(88, 94, 104)))
                .span("TextSpan scale_x / scale_y")
                .anchored(Anchor::CENTER)
                .snap_to(Vec2(scene_size.0 * 0.5, scene_size.1 * 0.66)),
        )
        .build();

    let image = scene.rasterize().render(
        scene_size,
        Resolution::new(1600, 900),
        RasterResidency::Cpu,
        &mut PassThrough,
    );

    let out = "/tmp/text_span_aspect.png";
    let file = File::create(out).expect("create output file");
    image.export_png(file).expect("export PNG");
    println!("Wrote {} ({}x{})", out, image.width(), image.height());
}
