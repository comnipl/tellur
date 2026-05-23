//! Compose a scene through the raster `Layer`, which rasterizes each child
//! independently and composites them with positional alpha blending. Three
//! overlapping translucent `Blob`s exercise the positional compositing, the
//! per-child sub-resolution rendering, and the source-over alpha math at
//! the overlap regions.

use std::fs::File;

use tellur_core::color::Color;
use tellur_core::geometry::{Anchor, Vec2};
use tellur_core::layer::Layer;
use tellur_core::raster::{RasterComponent, Resolution};
use tellur_core::shapes::{Circle, Rectangle};
use tellur_core::vector::{Paint, VectorComponent, VectorGraphic};
use tellur_renderer::Rasterizable;

/// A translucent colored circle. The whole shape is parameterised by hue
/// and radius; saturation, lightness and alpha are baked in.
struct Blob {
    radius: f32,
    hue: f32,
}

impl VectorComponent for Blob {
    fn view_box(&self) -> Vec2 {
        Vec2(self.radius * 2.0, self.radius * 2.0)
    }

    fn render(&self) -> VectorGraphic {
        Circle {
            radius: self.radius,
            fill: Paint::Solid(Color::hsla(self.hue, 0.7, 0.55, 0.65)).into(),
            stroke: None,
        }
        .render()
    }
}

fn main() {
    let mut scene = Layer::new(Vec2(1280.0, 720.0));

    let background = Rectangle {
        size: scene.size,
        fill: Paint::Solid(Color::rgb_u8(245, 240, 230)).into(),
        stroke: None,
    }
    .rasterize();
    scene.add(Vec2::ZERO, background);

    let red = Blob { radius: 200.0, hue: 0.0 }.rasterize();
    scene.add(
        red.view_box()
            .anchor(Anchor::CENTER)
            .snap_to(scene.size, Anchor::new(0.4, 0.4)),
        red,
    );

    let green = Blob { radius: 200.0, hue: 120.0 }.rasterize();
    scene.add(
        green
            .view_box()
            .anchor(Anchor::CENTER)
            .snap_to(scene.size, Anchor::new(0.6, 0.4)),
        green,
    );

    let blue = Blob { radius: 200.0, hue: 240.0 }.rasterize();
    scene.add(
        blue.view_box()
            .anchor(Anchor::CENTER)
            .snap_to(scene.size, Anchor::new(0.5, 0.65)),
        blue,
    );

    let image = scene.render(Resolution::new(1280, 720));

    let path = "/tmp/raster-scene.png";
    let file = File::create(path).expect("create output file");
    image.export_png(file).expect("export PNG");

    println!("Wrote {} ({}x{})", path, image.width, image.height);
}
