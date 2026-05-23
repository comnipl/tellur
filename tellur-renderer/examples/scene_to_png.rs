//! Compose a two-shape scene in a 16:9 layer and write it to PNG.

use std::fs::File;

use tellur_core::color::Color;
use tellur_core::geometry::{Anchor, Vec2};
use tellur_core::layer::VectorLayer;
use tellur_core::raster::{RasterComponent, Resolution};
use tellur_core::shapes::{Circle, Rectangle};
use tellur_core::vector::{Paint, VectorComponent};
use tellur_renderer::Rasterizable;

fn main() {
    let mut scene = VectorLayer::new(Vec2(1280.0, 720.0));

    let background = Rectangle {
        size: scene.size,
        fill: Paint::Solid(Color::rgb_u8(255, 255, 255)).into(),
        stroke: None,
    };
    scene.add(Vec2::ZERO, background);

    let square = Rectangle {
        size: Vec2(240.0, 240.0),
        fill: Paint::Solid(Color::hsl(200.0, 0.7, 0.55)).into(),
        stroke: None,
    };
    scene.add(
        square
            .view_box()
            .anchor(Anchor::TOP_LEFT)
            .snap_to_anchor(scene.size, Anchor::TOP_LEFT),
        square,
    );

    let circle = Circle {
        radius: 120.0,
        fill: Paint::Solid(Color::hsl(20.0, 0.7, 0.55)).into(),
        stroke: None,
    };
    scene.add(
        circle
            .view_box()
            .anchor(Anchor::BOTTOM_RIGHT)
            .snap_to_anchor(scene.size, Anchor::BOTTOM_RIGHT),
        circle,
    );

    let image = scene.rasterize().render(Resolution::new(1280, 720));

    let path = "/tmp/scene.png";
    let file = File::create(path).expect("create output file");
    image.export_png(file).expect("export PNG");

    println!("Wrote {} ({}x{})", path, image.width, image.height);
}
