//! Compose a two-shape scene in a 16:9 layer and write it to PNG.

use std::fs::File;

use tellur_core::color::Color;
use tellur_core::geometry::{Anchor, Vec2};
use tellur_core::layer::VectorLayer;
use tellur_core::placement::VectorPlacement;
use tellur_core::raster::{RasterComponent, Resolution};
use tellur_core::render_context::PassThrough;
use tellur_core::shapes::{Circle, Rectangle};
use tellur_core::vector::Paint;
use tellur_renderer::Rasterizable;

fn main() {
    let scene_size = Vec2(1280.0, 720.0);
    let scene = VectorLayer {
        size: Some(scene_size),
        children: vec![
            Rectangle {
                size: scene_size,
                fill: Paint::Solid(Color::rgb_u8(255, 255, 255)).into(),
                stroke: None,
            }
            .at(Vec2::ZERO),
            Rectangle {
                size: Vec2(240.0, 240.0),
                fill: Paint::Solid(Color::hsl(200.0, 0.7, 0.55)).into(),
                stroke: None,
            }
            .anchored(Anchor::TOP_LEFT)
            .snap_to(Anchor::TOP_LEFT.point(scene_size)),
            Circle {
                radius: 120.0,
                fill: Paint::Solid(Color::hsl(20.0, 0.7, 0.55)).into(),
                stroke: None,
            }
            .anchored(Anchor::BOTTOM_RIGHT)
            .snap_to(Anchor::BOTTOM_RIGHT.point(scene_size)),
        ],
    };

    let image = scene
        .rasterize()
        .render(scene_size, Resolution::new(1280, 720), &mut PassThrough);

    let path = "/tmp/scene.png";
    let file = File::create(path).expect("create output file");
    image.export_png(file).expect("export PNG");

    println!("Wrote {} ({}x{})", path, image.width(), image.height());
}
