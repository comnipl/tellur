//! Rasterize a circle and write it to a PNG file.

use std::fs::File;

use tellur_core::color::Color;
use tellur_core::geometry::Vec2;
use tellur_core::raster::RasterComponent;
use tellur_core::shapes::Circle;
use tellur_core::vector::{Fill, Paint};
use tellur_renderer::Rasterize;

fn main() {
    let circle = Circle {
        center: Vec2 { x: 128.0, y: 128.0 },
        radius: 100.0,
        fill: Some(Fill {
            paint: Paint::Solid(Color {
                r: 0.9,
                g: 0.3,
                b: 0.4,
                a: 1.0,
            }),
            opacity: 1.0,
        }),
        stroke: None,
    };

    let rasterize = Rasterize {
        vector: circle,
        width: 256,
        height: 256,
    };

    let image = rasterize.render();

    let path = "/tmp/circle.png";
    let file = File::create(path).expect("create output file");
    image.export_png(file).expect("export PNG");

    println!("Wrote {} ({}x{})", path, image.width, image.height);
}
