//! Basic shape components that implement `VectorComponent`.
//!
//! Each shape declares its intrinsic size through `layout` and produces
//! a `VectorGraphic` covering the layout-chosen size in `render`. The
//! shape will adapt if the parent imposes tight constraints — e.g. a
//! `Circle` placed under tight non-square constraints renders as an
//! ellipse.

use crate::geometry::{Constraints, Rect, Transform, Vec2};
use crate::vector::{Fill, Node, Path, PathCommand, Stroke, VectorComponent, VectorGraphic};
use crate::Keyable;

#[crate::component(vector)]
#[derive(Debug, Clone, PartialEq, Hash)]
pub struct Rectangle {
    pub size: Vec2,
    #[builder(into)]
    pub fill: Option<Fill>,
    #[builder(into)]
    pub stroke: Option<Stroke>,
}

impl VectorComponent for Rectangle {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        constraints.constrain(self.size)
    }

    fn render(&self, size: Vec2) -> VectorGraphic {
        let Vec2(w, h) = size;
        let commands = vec![
            PathCommand::MoveTo(Vec2(0.0, 0.0)),
            PathCommand::LineTo(Vec2(w, 0.0)),
            PathCommand::LineTo(Vec2(w, h)),
            PathCommand::LineTo(Vec2(0.0, h)),
            PathCommand::Close,
        ];
        VectorGraphic {
            view_box: Rect {
                origin: Vec2::ZERO,
                size,
            },
            root: Node::Path(Path {
                commands,
                fill: self.fill.clone(),
                stroke: self.stroke.clone(),
                transform: Transform::IDENTITY,
            }),
        }
    }
}

#[crate::component(vector)]
#[derive(Debug, Clone, Keyable)]
pub struct Circle {
    pub radius: f32,
    #[builder(into)]
    pub fill: Option<Fill>,
    #[builder(into)]
    pub stroke: Option<Stroke>,
}

impl VectorComponent for Circle {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        constraints.constrain(Vec2(self.radius * 2.0, self.radius * 2.0))
    }

    fn render(&self, size: Vec2) -> VectorGraphic {
        ellipse_to_graphic(
            Vec2(size.0 * 0.5, size.1 * 0.5),
            self.fill.clone(),
            self.stroke.clone(),
        )
    }
}

#[crate::component(vector)]
#[derive(Debug, Clone, PartialEq, Hash)]
pub struct Ellipse {
    pub radii: Vec2,
    #[builder(into)]
    pub fill: Option<Fill>,
    #[builder(into)]
    pub stroke: Option<Stroke>,
}

impl VectorComponent for Ellipse {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        constraints.constrain(Vec2(self.radii.0 * 2.0, self.radii.1 * 2.0))
    }

    fn render(&self, size: Vec2) -> VectorGraphic {
        ellipse_to_graphic(
            Vec2(size.0 * 0.5, size.1 * 0.5),
            self.fill.clone(),
            self.stroke.clone(),
        )
    }
}

// Magic constant for approximating a quarter-circle with a cubic Bezier:
// 4 * (sqrt(2) - 1) / 3. The maximum error is around 0.027% of the radius.
const KAPPA: f32 = 0.552_284_8;

// Builds an ellipse whose tight bounding box is anchored at the local origin
// `(0, 0)` and has size `2 * radii`.
fn ellipse_to_graphic(radii: Vec2, fill: Option<Fill>, stroke: Option<Stroke>) -> VectorGraphic {
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
        view_box: Rect {
            origin: Vec2::ZERO,
            size: Vec2(rx * 2.0, ry * 2.0),
        },
        root: Node::Path(Path {
            commands,
            fill,
            stroke,
            transform: Transform::IDENTITY,
        }),
    }
}

#[cfg(test)]
mod builder_tests {
    use super::*;
    use crate::builder::VectorBuilderPlacement;
    use crate::color::Color;
    use crate::geometry::Anchor;
    use crate::vector::Paint;

    fn paint() -> Paint {
        Paint::Solid(Color::rgb_u8(1, 2, 3))
    }

    #[test]
    fn complete_builder_converts_into_self_and_box() {
        // bon derive(Into): a complete builder converts into the struct itself
        // (so `impl Into<Ellipse>` args accept a builder, no `.build()`).
        let e: Ellipse = Ellipse::builder()
            .radii(Vec2(3.0, 3.0))
            .fill(paint())
            .into();
        assert_eq!(e.radii, Vec2(3.0, 3.0));
        assert!(e.fill.is_some());

        // ours: complete builder -> Box<dyn VectorComponent>, no `.build()`.
        let _boxed: Box<dyn VectorComponent> = Ellipse::builder().radii(Vec2(3.0, 3.0)).into();
        // ours: a built value -> Box<dyn VectorComponent>.
        let _boxed2: Box<dyn VectorComponent> = Ellipse {
            radii: Vec2(1.0, 1.0),
            fill: None,
            stroke: None,
        }
        .into();
    }

    #[test]
    fn builder_place_at_and_anchored_snap() {
        // place_at on a builder, no `.build()`.
        let p = Ellipse::builder()
            .radii(Vec2(5.0, 5.0))
            .place_at(Vec2(2.0, 3.0));
        assert_eq!(p.offset, Vec2(2.0, 3.0));

        // anchored().snap_to() on a builder: CENTER of a 10x10 box snapped to
        // (10,10) lands its origin at (5,5).
        let p2 = Ellipse::builder()
            .radii(Vec2(5.0, 5.0))
            .anchored(Anchor::CENTER)
            .snap_to(Vec2(10.0, 10.0));
        assert_eq!(p2.offset, Vec2(5.0, 5.0));
    }
}
