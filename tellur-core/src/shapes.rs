//! Basic shape components that implement `VectorComponent`.
//!
//! Each shape produces a `VectorGraphic` whose `view_box` is the tight bounding
//! box of its geometry. Stroke that extends beyond the geometric bounds may be
//! clipped by the renderer.

use crate::component::Component;
use crate::geometry::{Rect, Transform, Vec2};
use crate::vector::{Fill, Node, Path, PathCommand, Stroke, VectorComponent, VectorGraphic};

#[derive(Debug, Clone)]
pub struct Rectangle {
    pub rect: Rect,
    pub fill: Option<Fill>,
    pub stroke: Option<Stroke>,
}

impl Component for Rectangle {}

impl VectorComponent for Rectangle {
    fn render(&self) -> VectorGraphic {
        let o = self.rect.origin;
        let s = self.rect.size;
        let commands = vec![
            PathCommand::MoveTo(o),
            PathCommand::LineTo(Vec2(o.0 + s.0, o.1)),
            PathCommand::LineTo(Vec2(o.0 + s.0, o.1 + s.1)),
            PathCommand::LineTo(Vec2(o.0, o.1 + s.1)),
            PathCommand::Close,
        ];
        VectorGraphic {
            view_box: self.rect,
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
    pub center: Vec2,
    pub radius: f32,
    pub fill: Option<Fill>,
    pub stroke: Option<Stroke>,
}

impl Component for Circle {}

impl VectorComponent for Circle {
    fn render(&self) -> VectorGraphic {
        ellipse_to_graphic(
            self.center,
            Vec2(self.radius, self.radius),
            self.fill.clone(),
            self.stroke.clone(),
        )
    }
}

#[derive(Debug, Clone)]
pub struct Ellipse {
    pub center: Vec2,
    pub radii: Vec2,
    pub fill: Option<Fill>,
    pub stroke: Option<Stroke>,
}

impl Component for Ellipse {}

impl VectorComponent for Ellipse {
    fn render(&self) -> VectorGraphic {
        ellipse_to_graphic(self.center, self.radii, self.fill.clone(), self.stroke.clone())
    }
}

// Magic constant for approximating a quarter-circle with a cubic Bezier:
// 4 * (sqrt(2) - 1) / 3. The maximum error is around 0.027% of the radius.
const KAPPA: f32 = 0.5522847498307933;

fn ellipse_to_graphic(
    center: Vec2,
    radii: Vec2,
    fill: Option<Fill>,
    stroke: Option<Stroke>,
) -> VectorGraphic {
    let Vec2(cx, cy) = center;
    let Vec2(rx, ry) = radii;
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

    let view_box = Rect {
        origin: Vec2(cx - rx, cy - ry),
        size: Vec2(rx * 2.0, ry * 2.0),
    };

    VectorGraphic {
        view_box,
        root: Node::Path(Path {
            commands,
            fill,
            stroke,
            transform: Transform::IDENTITY,
        }),
    }
}
