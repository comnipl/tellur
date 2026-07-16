//! Basic shape components that implement `VectorComponent`.
//!
//! Each shape declares its intrinsic size through `layout` and produces a
//! `VectorGraphic` whose view box matches its paint bounds. A stroked shape
//! widens that view box by half the stroke width on each side. The shape will
//! adapt if the parent imposes tight constraints — e.g. a `Circle` placed
//! under tight non-square constraints renders as an ellipse.

use std::f32::consts::{FRAC_PI_2, PI, TAU};

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

    fn paint_bounds(&self, size: Vec2) -> Rect {
        stroked_bounds(size, &self.stroke)
    }

    fn render(&self, size: Vec2) -> VectorGraphic {
        let Vec2(w, h) = size;
        let Some((fill, stroke)) = visible_paints(size, &self.fill, &self.stroke) else {
            return empty_graphic(size);
        };
        let commands = vec![
            PathCommand::MoveTo(Vec2(0.0, 0.0)),
            PathCommand::LineTo(Vec2(w, 0.0)),
            PathCommand::LineTo(Vec2(w, h)),
            PathCommand::LineTo(Vec2(0.0, h)),
            PathCommand::Close,
        ];
        VectorGraphic {
            view_box: stroked_bounds(size, &self.stroke),
            root: Node::Path(Path {
                commands,
                fill,
                stroke,
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

    fn paint_bounds(&self, size: Vec2) -> Rect {
        stroked_bounds(size, &self.stroke)
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

    fn paint_bounds(&self, size: Vec2) -> Rect {
        stroked_bounds(size, &self.stroke)
    }

    fn render(&self, size: Vec2) -> VectorGraphic {
        ellipse_to_graphic(
            Vec2(size.0 * 0.5, size.1 * 0.5),
            self.fill.clone(),
            self.stroke.clone(),
        )
    }
}

/// A circular or elliptical arc from `start_angle` to `end_angle` (radians, in
/// this project's y-down coordinate system: `0` points along +x, `-PI/2`
/// points straight up, and the sweep runs clockwise on screen as the angle
/// increases).
///
/// Traced as cubic Bezier segments split at most every 90° — the same
/// technique [`Circle`] and [`Ellipse`] use for a full turn — so it is a
/// genuine curve rather than a polyline approximation, and effects that walk
/// path geometry (like [`Write`](crate::effect::Write)) work on it unchanged.
///
/// With a visible `fill`, the arc's chord is closed with a straight line
/// (like SVG's implicit closepath-on-fill), producing a circular segment
/// rather than a pie slice to the center. With `stroke` only, the path stays
/// open — the two endpoints are not connected.
#[crate::component(vector)]
#[derive(Debug, Clone, Keyable)]
pub struct Arc {
    pub radius: f32,
    pub start_angle: f32,
    pub end_angle: f32,
    #[builder(into)]
    pub fill: Option<Fill>,
    #[builder(into)]
    pub stroke: Option<Stroke>,
}

impl VectorComponent for Arc {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        constraints.constrain(Vec2(self.radius * 2.0, self.radius * 2.0))
    }

    fn paint_bounds(&self, size: Vec2) -> Rect {
        stroked_bounds(size, &self.stroke)
    }

    fn render(&self, size: Vec2) -> VectorGraphic {
        let Some((fill, stroke)) = visible_paints(size, &self.fill, &self.stroke) else {
            return empty_graphic(size);
        };
        let center = Vec2(size.0 * 0.5, size.1 * 0.5);
        let radii = center;
        let mut commands = arc_path_commands(center, radii, self.start_angle, self.end_angle);
        if commands.is_empty() {
            return empty_graphic(size);
        }
        if fill.is_some() {
            commands.push(PathCommand::Close);
        }
        VectorGraphic {
            view_box: stroked_bounds(size, &stroke),
            root: Node::Path(Path {
                commands,
                fill,
                stroke,
                transform: Transform::IDENTITY,
            }),
        }
    }
}

/// Which radius [`RegularPolygon`]'s `radius` parameter measures.
#[derive(Debug, Clone, Copy, Keyable)]
pub enum PolygonRadius {
    /// Distance from the center to each vertex — the circumscribed circle.
    Circumradius(f32),
    /// Distance from the center to the midpoint of each edge — the inscribed
    /// circle (also called the apothem).
    Apothem(f32),
}

impl PolygonRadius {
    pub fn circumradius(radius: f32) -> Self {
        Self::Circumradius(radius)
    }

    pub fn apothem(radius: f32) -> Self {
        Self::Apothem(radius)
    }

    /// Resolves to the circumradius for a polygon of `sides` sides.
    fn resolve(self, sides: usize) -> f32 {
        match self {
            Self::Circumradius(r) => r,
            Self::Apothem(r) => r / (PI / sides as f32).cos(),
        }
    }
}

/// A bare `f32` is a circumradius — the common case (`.radius(100.0)`).
impl From<f32> for PolygonRadius {
    fn from(circumradius: f32) -> Self {
        Self::Circumradius(circumradius)
    }
}

/// A regular polygon (equal sides and angles) inscribed in a circle.
///
/// `radius` accepts either [`PolygonRadius::circumradius`] (distance to each
/// vertex) or [`PolygonRadius::apothem`] (distance to each edge's midpoint) —
/// or a bare `f32`, which is shorthand for a circumradius. `rotation`
/// defaults to `-PI/2`, putting a vertex straight up (a flat-bottomed
/// triangle, a pointy-top hexagon, etc.).
///
/// Panics (in `layout`/`render`) if `sides < 3`.
#[crate::component(vector)]
#[derive(Debug, Clone, Keyable)]
pub struct RegularPolygon {
    pub sides: usize,
    #[builder(into)]
    pub radius: PolygonRadius,
    #[builder(default = -FRAC_PI_2)]
    pub rotation: f32,
    #[builder(into)]
    pub fill: Option<Fill>,
    #[builder(into)]
    pub stroke: Option<Stroke>,
}

impl VectorComponent for RegularPolygon {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        assert_valid_sides(self.sides);
        let diameter = self.radius.resolve(self.sides) * 2.0;
        constraints.constrain(Vec2(diameter, diameter))
    }

    fn paint_bounds(&self, size: Vec2) -> Rect {
        stroked_bounds(size, &self.stroke)
    }

    fn render(&self, size: Vec2) -> VectorGraphic {
        assert_valid_sides(self.sides);
        let Some((fill, stroke)) = visible_paints(size, &self.fill, &self.stroke) else {
            return empty_graphic(size);
        };
        let center = Vec2(size.0 * 0.5, size.1 * 0.5);
        let radii = center;
        let mut commands = Vec::with_capacity(self.sides + 1);
        for i in 0..self.sides {
            let angle = self.rotation + TAU * i as f32 / self.sides as f32;
            let p = Vec2(
                center.0 + radii.0 * angle.cos(),
                center.1 + radii.1 * angle.sin(),
            );
            commands.push(if i == 0 {
                PathCommand::MoveTo(p)
            } else {
                PathCommand::LineTo(p)
            });
        }
        commands.push(PathCommand::Close);
        VectorGraphic {
            view_box: stroked_bounds(size, &stroke),
            root: Node::Path(Path {
                commands,
                fill,
                stroke,
                transform: Transform::IDENTITY,
            }),
        }
    }
}

fn assert_valid_sides(sides: usize) {
    assert!(
        sides >= 3,
        "RegularPolygon requires at least 3 sides, got {sides}"
    );
}

/// Draws an arbitrary set of [`PathCommand`]s over a fixed logical canvas.
///
/// `size` is the `layout` size and the logical canvas used to derive paint
/// bounds. Like the other shapes here, a stroke outsets those bounds by half
/// its width on every side. Commands outside the declared canvas are not
/// included automatically, so size the canvas to fit all command geometry.
///
/// This is the escape hatch for geometry the other shapes cannot express
/// (freeform outlines, letter/glyph-style paths, etc.) without having to hand
/// -write a `VectorComponent` impl.
#[crate::component(vector)]
#[derive(Debug, Clone, Keyable)]
pub struct PathShape {
    pub size: Vec2,
    pub commands: Vec<PathCommand>,
    #[builder(into)]
    pub fill: Option<Fill>,
    #[builder(into)]
    pub stroke: Option<Stroke>,
}

impl VectorComponent for PathShape {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        constraints.constrain(self.size)
    }

    fn paint_bounds(&self, size: Vec2) -> Rect {
        stroked_bounds(size, &self.stroke)
    }

    fn render(&self, size: Vec2) -> VectorGraphic {
        let view_box = self.paint_bounds(size);
        let Some((fill, stroke)) = visible_paints(size, &self.fill, &self.stroke) else {
            return VectorGraphic {
                view_box,
                root: Node::empty(),
            };
        };
        if self.commands.is_empty() {
            return VectorGraphic {
                view_box,
                root: Node::empty(),
            };
        }
        VectorGraphic {
            view_box,
            root: Node::Path(Path {
                commands: self.commands.clone(),
                fill,
                stroke,
                transform: Transform::IDENTITY,
            }),
        }
    }
}

// Magic constant for approximating a quarter-circle with a cubic Bezier:
// 4 * (sqrt(2) - 1) / 3. The maximum error is around 0.027% of the radius.
const KAPPA: f32 = 0.552_284_8;

fn stroked_bounds(size: Vec2, stroke: &Option<Stroke>) -> Rect {
    let outset = stroke
        .as_ref()
        .map(|stroke| (stroke.width * 0.5).max(0.0))
        .unwrap_or(0.0);
    if outset == 0.0 {
        return Rect {
            origin: Vec2::ZERO,
            size,
        };
    }
    Rect {
        origin: Vec2(-outset, -outset),
        size: Vec2(size.0 + outset * 2.0, size.1 + outset * 2.0),
    }
}

/// Drops paints that cannot produce visible ink, and reports `None` when the
/// shape as a whole is invisible (degenerate box, or no visible fill/stroke)
/// so the caller can render an [`empty_graphic`] instead. Shapes cull
/// themselves: callers never need an "is it visible?" guard around a leaf.
#[allow(clippy::type_complexity)]
fn visible_paints(
    size: Vec2,
    fill: &Option<Fill>,
    stroke: &Option<Stroke>,
) -> Option<(Option<Fill>, Option<Stroke>)> {
    if size.0 <= 0.0 || size.1 <= 0.0 {
        return None;
    }
    let fill = fill.clone().filter(Fill::is_visible);
    let stroke = stroke.clone().filter(Stroke::is_visible);
    if fill.is_none() && stroke.is_none() {
        return None;
    }
    Some((fill, stroke))
}

/// The graphic an invisible shape renders as: the layout box with no ink.
fn empty_graphic(size: Vec2) -> VectorGraphic {
    VectorGraphic {
        view_box: Rect {
            origin: Vec2::ZERO,
            size,
        },
        root: Node::empty(),
    }
}

// Builds an ellipse whose tight bounding box is anchored at the local origin
// `(0, 0)` and has size `2 * radii`.
fn ellipse_to_graphic(radii: Vec2, fill: Option<Fill>, stroke: Option<Stroke>) -> VectorGraphic {
    let Vec2(rx, ry) = radii;
    let size = Vec2(rx * 2.0, ry * 2.0);
    let Some((fill, stroke)) = visible_paints(size, &fill, &stroke) else {
        return empty_graphic(size);
    };
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
        view_box: stroked_bounds(size, &stroke),
        root: Node::Path(Path {
            commands,
            fill,
            stroke,
            transform: Transform::IDENTITY,
        }),
    }
}

/// A point on an ellipse centered at `center` with semi-axes `radii`, at
/// `angle` radians (this project's y-down convention: `0` is +x, `-PI/2` is
/// straight up).
fn ellipse_point(center: Vec2, radii: Vec2, angle: f32) -> Vec2 {
    Vec2(
        center.0 + radii.0 * angle.cos(),
        center.1 + radii.1 * angle.sin(),
    )
}

/// Cubic-Bezier control points approximating the elliptical arc from `a0` to
/// `a1` (should be at most 90° apart — [`arc_path_commands`] splits larger
/// sweeps before calling this). `p0` is `ellipse_point(center, radii, a0)`;
/// returns `(c1, c2, p1)`.
///
/// Generalizes the [`KAPPA`] control-point offset used above for a 90° quarter
/// turn to an arbitrary sweep: `k = (4/3) * tan(sweep/4)` is the standard
/// closed-form single-cubic approximation of a circular/elliptical arc.
fn ellipse_arc_segment(center: Vec2, radii: Vec2, a0: f32, a1: f32) -> (Vec2, Vec2, Vec2) {
    let p0 = ellipse_point(center, radii, a0);
    let p1 = ellipse_point(center, radii, a1);
    let k = (4.0 / 3.0) * ((a1 - a0) * 0.25).tan();
    let c1 = Vec2(p0.0 - k * radii.0 * a0.sin(), p0.1 + k * radii.1 * a0.cos());
    let c2 = Vec2(p1.0 + k * radii.0 * a1.sin(), p1.1 - k * radii.1 * a1.cos());
    (c1, c2, p1)
}

/// Path commands for the elliptical arc from `start_angle` to `end_angle`
/// (the sweep may be negative or exceed a full turn), split into at-most-90°
/// cubic Bezier segments. Empty if the sweep is zero, non-finite, or the
/// ellipse is degenerate. Does not close the path — callers append
/// [`PathCommand::Close`] for a filled sector.
fn arc_path_commands(
    center: Vec2,
    radii: Vec2,
    start_angle: f32,
    end_angle: f32,
) -> Vec<PathCommand> {
    let sweep = end_angle - start_angle;
    if sweep == 0.0 || !sweep.is_finite() {
        return Vec::new();
    }
    let segments = (sweep.abs() / FRAC_PI_2).ceil().max(1.0) as usize;
    let step = sweep / segments as f32;
    let mut commands = Vec::with_capacity(segments + 1);
    commands.push(PathCommand::MoveTo(ellipse_point(
        center,
        radii,
        start_angle,
    )));
    let mut angle = start_angle;
    for _ in 0..segments {
        let next_angle = angle + step;
        let (c1, c2, to) = ellipse_arc_segment(center, radii, angle, next_angle);
        commands.push(PathCommand::CubicTo { c1, c2, to });
        angle = next_angle;
    }
    commands
}

#[cfg(test)]
mod builder_tests {
    use super::*;
    use crate::builder::{VectorBuilderPlacement, VectorBuilderTransform};
    use crate::color::Color;
    use crate::geometry::{Anchor, Transform};
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

    #[test]
    fn builder_transform_and_opacity() {
        let transformed = Ellipse::builder()
            .radii(Vec2(5.0, 5.0))
            .transform(Transform::scale(Vec2(2.0, 2.0)))
            .opacity(0.5);
        assert_eq!(transformed.transform, Transform::scale(Vec2(2.0, 2.0)));
        assert_eq!(transformed.opacity, 0.5);
    }

    #[test]
    fn shape_paint_bounds_include_stroke_outset() {
        let rect = Rectangle::builder()
            .size(Vec2(10.0, 20.0))
            .stroke(Stroke {
                paint: paint(),
                width: 4.0,
                dash: None,
            })
            .build();
        assert_eq!(
            rect.paint_bounds(Vec2(10.0, 20.0)),
            Rect {
                origin: Vec2(-2.0, -2.0),
                size: Vec2(14.0, 24.0),
            }
        );
    }

    #[test]
    fn shape_view_box_matches_stroked_paint_bounds() {
        let stroke = Stroke {
            paint: paint(),
            width: 4.0,
            dash: None,
        };
        let expected = Rect {
            origin: Vec2(-2.0, -2.0),
            size: Vec2(14.0, 24.0),
        };

        let rect = Rectangle::builder()
            .size(Vec2(10.0, 20.0))
            .stroke(stroke.clone())
            .build();
        assert_eq!(rect.render(Vec2(10.0, 20.0)).view_box, expected);

        let ellipse = Ellipse::builder()
            .radii(Vec2(5.0, 10.0))
            .stroke(stroke.clone())
            .build();
        assert_eq!(ellipse.render(Vec2(10.0, 20.0)).view_box, expected);

        let circle = Circle::builder().radius(5.0).stroke(stroke).build();
        assert_eq!(
            circle.render(Vec2(10.0, 10.0)).view_box,
            Rect {
                origin: Vec2(-2.0, -2.0),
                size: Vec2(14.0, 14.0),
            }
        );
    }

    #[test]
    fn arc_layout_matches_circle_diameter_square() {
        let arc = Arc::builder()
            .radius(5.0)
            .start_angle(0.0)
            .end_angle(FRAC_PI_2)
            .stroke(Stroke::new(paint(), 1.0))
            .build();
        assert_eq!(arc.layout(Constraints::UNBOUNDED), Vec2(10.0, 10.0));
    }

    #[test]
    fn arc_quarter_turn_is_a_single_cubic_segment() {
        // A 90° sweep needs exactly one Bezier segment, matching the
        // per-quadrant construction `Circle`/`Ellipse` use for a full turn.
        let arc = Arc::builder()
            .radius(5.0)
            .start_angle(0.0)
            .end_angle(FRAC_PI_2)
            .stroke(Stroke::new(paint(), 1.0))
            .build();
        let Node::Path(path) = arc.render(Vec2(10.0, 10.0)).root else {
            panic!("a stroked arc renders as a single path");
        };
        assert_eq!(path.commands.len(), 2, "{:?}", path.commands);
        assert!(matches!(path.commands[0], PathCommand::MoveTo(_)));
        assert!(matches!(path.commands[1], PathCommand::CubicTo { .. }));
        // No fill: the chord must not be closed.
        assert!(!path
            .commands
            .iter()
            .any(|c| matches!(c, PathCommand::Close)));
    }

    #[test]
    fn arc_half_turn_needs_two_segments() {
        let arc = Arc::builder()
            .radius(5.0)
            .start_angle(0.0)
            .end_angle(PI)
            .stroke(Stroke::new(paint(), 1.0))
            .build();
        let Node::Path(path) = arc.render(Vec2(10.0, 10.0)).root else {
            panic!("a stroked arc renders as a single path");
        };
        let cubic_count = path
            .commands
            .iter()
            .filter(|c| matches!(c, PathCommand::CubicTo { .. }))
            .count();
        assert_eq!(cubic_count, 2);
    }

    #[test]
    fn arc_with_fill_closes_the_chord() {
        let arc = Arc::builder()
            .radius(5.0)
            .start_angle(0.0)
            .end_angle(FRAC_PI_2)
            .fill(paint())
            .build();
        let Node::Path(path) = arc.render(Vec2(10.0, 10.0)).root else {
            panic!("a filled arc renders as a single path");
        };
        assert_eq!(path.commands.last(), Some(&PathCommand::Close));
    }

    #[test]
    fn arc_zero_sweep_renders_no_ink() {
        let arc = Arc::builder()
            .radius(5.0)
            .start_angle(0.3)
            .end_angle(0.3)
            .stroke(Stroke::new(paint(), 1.0))
            .build();
        assert_eq!(arc.render(Vec2(10.0, 10.0)).root, Node::empty());
    }

    #[test]
    fn arc_write_on_reveals_a_partial_stroke() {
        use crate::effect::VectorWrite;
        use crate::phase::Phase;

        let arc = Arc::builder()
            .radius(5.0)
            .start_angle(0.0)
            .end_angle(PI)
            .stroke(Stroke::new(paint(), 1.0))
            .build();
        let full = arc.clone().write_on(Phase::ONE).render(Vec2(10.0, 10.0));
        assert_eq!(full, arc.render(Vec2(10.0, 10.0)));

        let half = arc.write_on(Phase::HALF).render(Vec2(10.0, 10.0));
        assert_ne!(half.root, Node::empty());
        assert_ne!(half, full);
    }

    #[test]
    fn regular_polygon_apothem_resolves_to_circumradius() {
        // For a hexagon, circumradius = apothem / cos(PI/6).
        let hexagon = RegularPolygon::builder()
            .sides(6)
            .radius(PolygonRadius::apothem(10.0))
            .build();
        let expected_diameter = 2.0 * 10.0 / (PI / 6.0).cos();
        let size = hexagon.layout(Constraints::UNBOUNDED);
        assert!((size.0 - expected_diameter).abs() < 0.001, "{size:?}");
        assert_eq!(size.0, size.1);
    }

    #[test]
    fn regular_polygon_bare_f32_radius_is_circumradius() {
        let triangle = RegularPolygon::builder().sides(3).radius(10.0).build();
        assert_eq!(triangle.layout(Constraints::UNBOUNDED), Vec2(20.0, 20.0));
    }

    #[test]
    fn regular_polygon_default_rotation_points_a_vertex_up() {
        let square = RegularPolygon::builder()
            .sides(4)
            .radius(10.0)
            .stroke(Stroke::new(paint(), 1.0))
            .build();
        let Node::Path(path) = square.render(Vec2(20.0, 20.0)).root else {
            panic!("a stroked polygon renders as a single path");
        };
        let Some(PathCommand::MoveTo(first)) = path.commands.first() else {
            panic!("polygon path should start with MoveTo");
        };
        // Center (10, 10), radius 10: straight up is (10, 0).
        assert!((first.0 - 10.0).abs() < 0.001, "{first:?}");
        assert!((first.1 - 0.0).abs() < 0.001, "{first:?}");
    }

    #[test]
    fn regular_polygon_closes_its_outline() {
        let pentagon = RegularPolygon::builder()
            .sides(5)
            .radius(10.0)
            .fill(paint())
            .build();
        let Node::Path(path) = pentagon.render(Vec2(20.0, 20.0)).root else {
            panic!("a filled polygon renders as a single path");
        };
        assert_eq!(path.commands.len(), 6); // MoveTo + 4 LineTo + Close
        assert_eq!(path.commands.last(), Some(&PathCommand::Close));
    }

    #[test]
    #[should_panic(expected = "at least 3 sides")]
    fn regular_polygon_rejects_fewer_than_three_sides() {
        let degenerate = RegularPolygon::builder().sides(2).radius(10.0).build();
        degenerate.layout(Constraints::UNBOUNDED);
    }

    #[test]
    fn path_shape_layout_matches_declared_size() {
        let shape = PathShape::builder()
            .size(Vec2(40.0, 30.0))
            .commands(vec![])
            .build();
        assert_eq!(shape.layout(Constraints::UNBOUNDED), Vec2(40.0, 30.0));
    }

    #[test]
    fn path_shape_renders_the_given_commands_untouched() {
        let commands = vec![
            PathCommand::MoveTo(Vec2(0.0, 0.0)),
            PathCommand::LineTo(Vec2(40.0, 0.0)),
            PathCommand::LineTo(Vec2(20.0, 30.0)),
            PathCommand::Close,
        ];
        let shape = PathShape::builder()
            .size(Vec2(40.0, 30.0))
            .commands(commands.clone())
            .fill(paint())
            .build();
        let graphic = shape.render(Vec2(40.0, 30.0));
        assert_eq!(
            graphic.view_box,
            Rect {
                origin: Vec2::ZERO,
                size: Vec2(40.0, 30.0),
            }
        );
        let Node::Path(path) = graphic.root else {
            panic!("PathShape renders as a single path");
        };
        assert_eq!(path.commands, commands);
        assert_eq!(path.fill, Some(Fill { paint: paint() }));
    }

    #[test]
    fn path_shape_paint_bounds_include_stroke_outset_and_match_view_box() {
        let shape = PathShape::builder()
            .size(Vec2(40.0, 30.0))
            .commands(vec![
                PathCommand::MoveTo(Vec2(0.0, 0.0)),
                PathCommand::LineTo(Vec2(40.0, 30.0)),
            ])
            .stroke(Stroke::new(paint(), 2.5))
            .build();
        let size = Vec2(40.0, 30.0);
        let expected = Rect {
            origin: Vec2(-1.25, -1.25),
            size: Vec2(42.5, 32.5),
        };

        assert_eq!(shape.paint_bounds(size), expected);
        assert_eq!(shape.render(size).view_box, expected);
    }

    #[test]
    fn path_shape_with_no_commands_or_paint_renders_no_ink() {
        let empty_commands = PathShape::builder()
            .size(Vec2(10.0, 10.0))
            .commands(vec![
                PathCommand::MoveTo(Vec2::ZERO),
                PathCommand::LineTo(Vec2(10.0, 10.0)),
            ])
            .build();
        assert_eq!(empty_commands.render(Vec2(10.0, 10.0)).root, Node::empty());

        let no_commands = PathShape::builder()
            .size(Vec2(10.0, 10.0))
            .commands(vec![])
            .fill(paint())
            .build();
        assert_eq!(no_commands.render(Vec2(10.0, 10.0)).root, Node::empty());
    }
}
