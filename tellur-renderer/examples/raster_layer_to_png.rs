//! Compose a scene through the raster `Layer`, which rasterizes each child
//! independently and composites them with positional alpha blending. Three
//! overlapping translucent `Blob`s exercise the positional compositing, the
//! per-child sub-resolution rendering, and the source-over alpha math at
//! the overlap regions.

use std::fs::File;

use tellur_core::color::Color;
use tellur_core::component;
use tellur_core::geometry::{Anchor, Vec2};
use tellur_core::layer::Layer;
use tellur_core::placement::RasterPlacement;
use tellur_core::raster::{RasterComponent, RasterResidency, Resolution};
use tellur_core::render_context::PassThrough;
use tellur_core::shapes::{Circle, Rectangle};
use tellur_core::vector::Paint;
use tellur_renderer::RasterizableBuilder;

/// A translucent colored circle. The whole shape is parameterised by hue
/// and radius; saturation, lightness and alpha are baked in.
#[component(vector)]
fn Blob(radius: f32, hue: f32) -> impl VectorComponent {
    Circle::builder()
        .radius(radius)
        .fill(Paint::Solid(Color::hsla(hue, 0.7, 0.55, 0.65)))
        .build()
}

fn main() {
    let scene_size = Vec2(1280.0, 720.0);
    let scene = Layer::builder()
        .size(scene_size)
        .child(
            Rectangle::builder()
                .size(scene_size)
                .fill(Paint::Solid(Color::rgb_u8(245, 240, 230)))
                .rasterize()
                .place_at(Vec2::ZERO),
        )
        .child(
            Blob::builder()
                .radius(200.0)
                .hue(0.0)
                .rasterize()
                .anchored(Anchor::CENTER)
                .snap_to(Anchor::new(0.4, 0.4)),
        )
        .child(
            Blob::builder()
                .radius(200.0)
                .hue(120.0)
                .rasterize()
                .anchored(Anchor::CENTER)
                .snap_to(Anchor::new(0.6, 0.4)),
        )
        .child(
            Blob::builder()
                .radius(200.0)
                .hue(240.0)
                .rasterize()
                .anchored(Anchor::CENTER)
                .snap_to(Anchor::new(0.5, 0.65)),
        )
        .build();

    let image = scene.render(
        scene_size,
        Resolution::new(1280, 720),
        RasterResidency::Cpu,
        &mut PassThrough,
    );

    let path = "/tmp/raster-scene.png";
    let file = File::create(path).expect("create output file");
    image.export_png(file).expect("export PNG");

    println!("Wrote {} ({}x{})", path, image.width(), image.height());
}
