//! Basic shape components that implement `VectorComponent`.
//!
//! Each shape produces a `VectorGraphic` whose `view_box` is the shape's
//! tight bounding box, with its top-left corner anchored at the local
//! origin `(0, 0)`. Positioning within a parent coordinate space is the
//! parent's responsibility (e.g. via `VectorLayer::add`).

use crate::geometry::{Transform, Vec2};
use crate::vector::{Fill, Node, Path, PathCommand, Stroke, VectorComponent, VectorGraphic};

#[derive(Debug, Clone)]
pub struct Rectangle {
    pub size: Vec2,
    pub fill: Option<Fill>,
    pub stroke: Option<Stroke>,
}

impl VectorComponent for Rectangle {
    fn view_box(&self) -> Vec2 {
        self.size
    }

    fn render(&self) -> VectorGraphic {
        let Vec2(w, h) = self.size;
        let commands = vec![
            PathCommand::MoveTo(Vec2(0.0, 0.0)),
            PathCommand::LineTo(Vec2(w, 0.0)),
            PathCommand::LineTo(Vec2(w, h)),
            PathCommand::LineTo(Vec2(0.0, h)),
            PathCommand::Close,
        ];
        VectorGraphic {
            view_box: self.size,
            root: Node::Path(Path {
                commands,
                fill: self.fill.clone(),
                stroke: self.stroke.clone(),
                transform: Transform::IDENTITY,
            }),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Circle {
    pub radius: f32,
    pub fill: Option<Fill>,
    pub stroke: Option<Stroke>,
}

impl VectorComponent for Circle {
    fn view_box(&self) -> Vec2 {
        Vec2(self.radius * 2.0, self.radius * 2.0)
    }

    fn render(&self) -> VectorGraphic {
        ellipse_to_graphic(
            Vec2(self.radius, self.radius),
            self.fill.clone(),
            self.stroke.clone(),
        )
    }
}

#[derive(Debug, Clone)]
pub struct Ellipse {
    pub radii: Vec2,
    pub fill: Option<Fill>,
    pub stroke: Option<Stroke>,
}

impl VectorComponent for Ellipse {
    fn view_box(&self) -> Vec2 {
        Vec2(self.radii.0 * 2.0, self.radii.1 * 2.0)
    }

    fn render(&self) -> VectorGraphic {
        ellipse_to_graphic(self.radii, self.fill.clone(), self.stroke.clone())
    }
}

// Magic constant for approximating a quarter-circle with a cubic Bezier:
// 4 * (sqrt(2) - 1) / 3. The maximum error is around 0.027% of the radius.
const KAPPA: f32 = 0.552_284_8;

// Builds an ellipse whose tight bounding box is anchored at the local origin
// `(0, 0)` and has size `2 * radii`.
fn ellipse_to_graphic(
    radii: Vec2,
    fill: Option<Fill>,
    stroke: Option<Stroke>,
) -> VectorGraphic {
    let Vec2(rx, ry) = radii;
    let cx = rx;
    let cy = ry;
    let ox = rx * KAPPA;
    let oy = ry * KAPPA;

    let commands = vec![
        PathCommand::MoveTo(Vec2(cx + rx, cy)),
        PathCommand::CubicTo {
            c1: Vec2(cx + rx, cy + oy),
            c2: Vec2(cx + ox, cy + ry),
            to: Vec2(cx, cy + ry),
        },
        PathCommand::CubicTo {
            c1: Vec2(cx - ox, cy + ry),
            c2: Vec2(cx - rx, cy + oy),
            to: Vec2(cx - rx, cy),
        },
        PathCommand::CubicTo {
            c1: Vec2(cx - rx, cy - oy),
            c2: Vec2(cx - ox, cy - ry),
            to: Vec2(cx, cy - ry),
        },
        PathCommand::CubicTo {
            c1: Vec2(cx + ox, cy - ry),
            c2: Vec2(cx + rx, cy - oy),
            to: Vec2(cx + rx, cy),
        },
        PathCommand::Close,
    ];

    VectorGraphic {
        view_box: Vec2(rx * 2.0, ry * 2.0),
        root: Node::Path(Path {
            commands,
            fill,
            stroke,
            transform: Transform::IDENTITY,
        }),
    }
}
