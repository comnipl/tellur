//! Vector outline extraction for filled vector shapes.
//!
//! [`Outlined`] converts the visible vector ink of its child into filled path
//! geometry that represents only the requested outline band. Unlike renderer
//! strokes, the output remains ordinary vector paths and can be further
//! transformed or rasterized through the existing pipeline.

use clipper2::{difference, inflate, intersect, simplify, union, EndType, FillRule, JoinType};
use kurbo::{BezPath, PathEl, Point};

use crate::builder::VectorBuilder;
use crate::geometry::{Constraints, Rect, Transform, Vec2};
use crate::vector::{Fill, Node, Paint, Path, PathCommand, Stroke, VectorComponent, VectorGraphic};
use crate::Keyable;

const DEFAULT_TOLERANCE: f32 = 0.2;
const DEFAULT_MITER_LIMIT: f32 = 4.0;
const MAX_CURVE_STEPS: usize = 96;

type ClipperPaths = clipper2::Paths<clipper2::Milli>;

/// Which side of the original silhouette the outline band occupies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OutlineSide {
    /// The whole band is outside the child silhouette.
    Outset,
    /// The whole band is inside the child silhouette.
    Inset,
    /// The band is centered on the child silhouette boundary.
    Center,
}

impl Default for OutlineSide {
    fn default() -> Self {
        Self::Outset
    }
}

/// Corner treatment used when offsetting the silhouette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OutlineJoin {
    Round,
    Square,
    Bevel,
    Miter,
}

impl Default for OutlineJoin {
    fn default() -> Self {
        Self::Round
    }
}

/// A vector component that renders only an outline band around its child.
#[crate::component(vector)]
#[derive(Keyable)]
pub struct Outlined {
    /// Band width in logical units.
    pub width: f32,
    /// Paint applied to the generated outline paths.
    #[builder(into)]
    pub paint: Paint,
    /// Whether the band is outside, inside, or centered on the silhouette.
    #[builder(default)]
    pub side: OutlineSide,
    /// Join style for offset corners.
    #[builder(default)]
    pub join: OutlineJoin,
    /// Miter limit used for [`OutlineJoin::Miter`].
    #[builder(default = DEFAULT_MITER_LIMIT)]
    pub miter_limit: f32,
    /// Flattening/simplification tolerance in logical units.
    #[builder(default = DEFAULT_TOLERANCE)]
    pub tolerance: f32,
    #[builder(into)]
    pub child: Box<dyn VectorComponent>,
}

impl Outlined {
    pub fn new<C: VectorComponent + 'static>(
        width: f32,
        paint: impl Into<Paint>,
        child: C,
    ) -> Self {
        Self::from_box(width, paint, Box::new(child))
    }

    pub fn from_box(width: f32, paint: impl Into<Paint>, child: Box<dyn VectorComponent>) -> Self {
        Self {
            width,
            paint: paint.into(),
            side: OutlineSide::default(),
            join: OutlineJoin::default(),
            miter_limit: DEFAULT_MITER_LIMIT,
            tolerance: DEFAULT_TOLERANCE,
            child,
        }
    }

    pub fn side(mut self, side: OutlineSide) -> Self {
        self.side = side;
        self
    }

    pub fn join(mut self, join: OutlineJoin) -> Self {
        self.join = join;
        self
    }

    pub fn miter_limit(mut self, miter_limit: f32) -> Self {
        self.miter_limit = miter_limit;
        self
    }

    pub fn tolerance(mut self, tolerance: f32) -> Self {
        self.tolerance = tolerance;
        self
    }
}

impl VectorComponent for Outlined {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        self.child.layout(constraints)
    }

    fn paint_bounds(&self, size: Vec2) -> Rect {
        let inner = self.child.render(size);
        let paths = self.outline_paths(&inner.root);
        outline_view_box(size, &inner.view_box, paths.as_ref())
    }

    fn render(&self, size: Vec2) -> VectorGraphic {
        let inner = self.child.render(size);
        let paths = self.outline_paths(&inner.root);
        let view_box = outline_view_box(size, &inner.view_box, paths.as_ref());
        let Some(paths) = paths else {
            return VectorGraphic {
                view_box,
                root: Node::empty(),
            };
        };

        VectorGraphic {
            view_box,
            root: paths_to_node(paths, self.paint.clone()),
        }
    }
}

/// Extension trait adding outline extraction to built vector components.
pub trait VectorOutline: VectorComponent + Sized + 'static {
    fn outlined(self, width: f32, paint: impl Into<Paint>) -> Outlined {
        Outlined::new(width, paint, self)
    }
}

impl<T: VectorComponent + 'static> VectorOutline for T {}

/// Builder-side outline extraction, so complete builders do not need `.build()`.
pub trait VectorBuilderOutline: VectorBuilder {
    fn outlined(self, width: f32, paint: impl Into<Paint>) -> Outlined {
        Outlined::new(width, paint, self.build_component())
    }
}

impl<B: VectorBuilder> VectorBuilderOutline for B {}

impl Outlined {
    fn outline_paths(&self, root: &Node) -> Option<ClipperPaths> {
        if self.width <= 0.0 || !self.paint.is_visible() {
            return None;
        }

        let tolerance = clean_tolerance(self.tolerance);
        let raw = collect_node_paths(root, Transform::IDENTITY, tolerance)?;
        let silhouette = union(raw, ClipperPaths::default(), FillRule::NonZero).ok()?;
        if silhouette.is_empty() {
            return None;
        }

        let join = self.join.into();
        let miter_limit = self.miter_limit.max(1.0) as f64;
        let width = self.width as f64;
        let result = match self.side {
            OutlineSide::Outset => {
                let expanded =
                    offset_paths(silhouette.clone(), width, join, miter_limit, tolerance);
                difference(expanded, silhouette, FillRule::NonZero).ok()?
            }
            OutlineSide::Inset => {
                let inset = offset_paths(silhouette.clone(), -width, join, miter_limit, tolerance);
                difference(silhouette, inset, FillRule::NonZero).ok()?
            }
            OutlineSide::Center => {
                let expanded = offset_paths(
                    silhouette.clone(),
                    width * 0.5,
                    join,
                    miter_limit,
                    tolerance,
                );
                let inset = offset_paths(silhouette, -width * 0.5, join, miter_limit, tolerance);
                difference(expanded, inset, FillRule::NonZero).ok()?
            }
        };

        let result = simplify(result, tolerance as f64, false);
        (!result.is_empty()).then_some(result)
    }
}

impl From<OutlineJoin> for JoinType {
    fn from(join: OutlineJoin) -> Self {
        match join {
            OutlineJoin::Round => JoinType::Round,
            OutlineJoin::Square => JoinType::Square,
            OutlineJoin::Bevel => JoinType::Bevel,
            OutlineJoin::Miter => JoinType::Miter,
        }
    }
}

fn clean_tolerance(tolerance: f32) -> f32 {
    tolerance.clamp(0.01, 10.0)
}

fn offset_paths(
    paths: ClipperPaths,
    delta: f64,
    join: JoinType,
    miter_limit: f64,
    tolerance: f32,
) -> ClipperPaths {
    let paths = inflate(paths, delta, join, EndType::Polygon, miter_limit);
    simplify(paths, tolerance as f64, false)
}

fn collect_node_paths(node: &Node, transform: Transform, tolerance: f32) -> Option<ClipperPaths> {
    match node {
        Node::Group(group) => collect_group_paths(
            &group.children,
            transform.concat(group.transform),
            group.opacity,
            tolerance,
        ),
        Node::SingleGroup(group) => {
            if group.opacity <= 0.0 || group.opacity.is_nan() {
                None
            } else {
                collect_node_paths(&group.child, transform.concat(group.transform), tolerance)
            }
        }
        Node::ClipGroup(group) => {
            let child = collect_node_paths(&group.child, transform, tolerance)?;
            let clip = path_fill_to_paths(
                &group.commands,
                transform.concat(group.transform),
                tolerance,
            )?;
            intersect(child, clip, FillRule::NonZero).ok()
        }
        Node::Path(path) => path_to_visible_paths(path, transform, tolerance),
    }
}

fn collect_group_paths(
    children: &[Node],
    transform: Transform,
    opacity: f32,
    tolerance: f32,
) -> Option<ClipperPaths> {
    if opacity <= 0.0 || opacity.is_nan() {
        return None;
    }
    let mut paths = ClipperPaths::default();
    for child in children {
        if let Some(child_paths) = collect_node_paths(child, transform, tolerance) {
            paths.push(child_paths);
        }
    }
    paths.contains_points().then_some(paths)
}

fn path_to_visible_paths(path: &Path, parent: Transform, tolerance: f32) -> Option<ClipperPaths> {
    let transform = parent.concat(path.transform);
    let mut paths = ClipperPaths::default();

    if path.fill.as_ref().is_some_and(|fill| fill.is_visible()) {
        if let Some(fill_paths) = path_fill_to_paths(&path.commands, transform, tolerance) {
            paths.push(fill_paths);
        }
    }

    if let Some(stroke) = path.stroke.as_ref().filter(|stroke| stroke.is_visible()) {
        if let Some(stroke_paths) =
            path_stroke_to_paths(&path.commands, stroke, transform, tolerance)
        {
            paths.push(stroke_paths);
        }
    }

    paths.contains_points().then_some(paths)
}

fn path_fill_to_paths(
    commands: &[PathCommand],
    transform: Transform,
    tolerance: f32,
) -> Option<ClipperPaths> {
    let contours = flatten_commands(commands, transform, tolerance, true);
    contours_to_paths(contours)
}

fn path_stroke_to_paths(
    commands: &[PathCommand],
    stroke: &Stroke,
    transform: Transform,
    tolerance: f32,
) -> Option<ClipperPaths> {
    let bez = commands_to_bez_path(commands, transform);
    if bez.elements().is_empty() {
        return None;
    }
    let style = kurbo::Stroke::new((stroke.width * max_scale(transform)).max(0.0) as f64)
        .with_join(kurbo::Join::Round)
        .with_caps(kurbo::Cap::Round);
    let stroked = kurbo::stroke(
        bez.elements().iter().copied(),
        &style,
        &kurbo::StrokeOpts::default(),
        tolerance as f64,
    );
    let contours = flatten_bez_path(&stroked, tolerance);
    contours_to_paths(contours)
}

fn contours_to_paths(contours: Vec<Vec<Vec2>>) -> Option<ClipperPaths> {
    let paths: Vec<Vec<(f64, f64)>> = contours
        .into_iter()
        .filter_map(clean_contour)
        .map(|contour| {
            contour
                .into_iter()
                .map(|p| (p.0 as f64, p.1 as f64))
                .collect()
        })
        .collect();
    (!paths.is_empty()).then(|| paths.into())
}

fn clean_contour(mut contour: Vec<Vec2>) -> Option<Vec<Vec2>> {
    dedupe_consecutive(&mut contour);
    if contour.len() >= 2 && nearly_same(*contour.first()?, *contour.last()?) {
        contour.pop();
    }
    dedupe_consecutive(&mut contour);
    if contour.len() < 3 || polygon_area(&contour).abs() <= 0.0001 {
        return None;
    }
    Some(contour)
}

fn dedupe_consecutive(points: &mut Vec<Vec2>) {
    let mut deduped = Vec::with_capacity(points.len());
    for &point in points.iter() {
        if deduped.last().is_none_or(|last| !nearly_same(*last, point)) {
            deduped.push(point);
        }
    }
    *points = deduped;
}

fn flatten_commands(
    commands: &[PathCommand],
    transform: Transform,
    tolerance: f32,
    closed_only: bool,
) -> Vec<Vec<Vec2>> {
    let mut contours = Vec::new();
    let mut current_contour = Vec::new();
    let mut current = None;
    let mut start = None;

    for &command in commands {
        match command {
            PathCommand::MoveTo(p) => {
                finish_contour(&mut contours, &mut current_contour, closed_only, false);
                let p = transform.transform_point(p);
                current_contour.push(p);
                current = Some(p);
                start = Some(p);
            }
            PathCommand::LineTo(to) => {
                let to = transform.transform_point(to);
                if current.is_some() {
                    current_contour.push(to);
                }
                current = Some(to);
            }
            PathCommand::QuadTo { control, to } => {
                let Some(from) = current else {
                    continue;
                };
                let control = transform.transform_point(control);
                let to = transform.transform_point(to);
                let steps = curve_steps(from, control, to, None, tolerance);
                for i in 1..=steps {
                    let t = i as f32 / steps as f32;
                    current_contour.push(quad_point(from, control, to, t));
                }
                current = Some(to);
            }
            PathCommand::CubicTo { c1, c2, to } => {
                let Some(from) = current else {
                    continue;
                };
                let c1 = transform.transform_point(c1);
                let c2 = transform.transform_point(c2);
                let to = transform.transform_point(to);
                let steps = curve_steps(from, c1, to, Some(c2), tolerance);
                for i in 1..=steps {
                    let t = i as f32 / steps as f32;
                    current_contour.push(cubic_point(from, c1, c2, to, t));
                }
                current = Some(to);
            }
            PathCommand::Close => {
                if let Some(start) = start {
                    current_contour.push(start);
                }
                finish_contour(&mut contours, &mut current_contour, closed_only, true);
                current = None;
                start = None;
            }
        }
    }
    finish_contour(&mut contours, &mut current_contour, closed_only, false);

    contours
}

fn finish_contour(
    contours: &mut Vec<Vec<Vec2>>,
    current_contour: &mut Vec<Vec2>,
    closed_only: bool,
    was_closed: bool,
) {
    if !current_contour.is_empty() && (!closed_only || was_closed) {
        contours.push(std::mem::take(current_contour));
    } else {
        current_contour.clear();
    }
}

fn commands_to_bez_path(commands: &[PathCommand], transform: Transform) -> BezPath {
    let mut path = BezPath::new();
    for command in commands {
        match *command {
            PathCommand::MoveTo(p) => path.move_to(to_point(transform.transform_point(p))),
            PathCommand::LineTo(p) => path.line_to(to_point(transform.transform_point(p))),
            PathCommand::QuadTo { control, to } => path.quad_to(
                to_point(transform.transform_point(control)),
                to_point(transform.transform_point(to)),
            ),
            PathCommand::CubicTo { c1, c2, to } => path.curve_to(
                to_point(transform.transform_point(c1)),
                to_point(transform.transform_point(c2)),
                to_point(transform.transform_point(to)),
            ),
            PathCommand::Close => path.close_path(),
        }
    }
    path
}

fn flatten_bez_path(path: &BezPath, tolerance: f32) -> Vec<Vec<Vec2>> {
    let commands: Vec<PathCommand> = path
        .elements()
        .iter()
        .filter_map(|el| match *el {
            PathEl::MoveTo(p) => Some(PathCommand::MoveTo(from_point(p))),
            PathEl::LineTo(p) => Some(PathCommand::LineTo(from_point(p))),
            PathEl::QuadTo(p1, p2) => Some(PathCommand::QuadTo {
                control: from_point(p1),
                to: from_point(p2),
            }),
            PathEl::CurveTo(p1, p2, p3) => Some(PathCommand::CubicTo {
                c1: from_point(p1),
                c2: from_point(p2),
                to: from_point(p3),
            }),
            PathEl::ClosePath => Some(PathCommand::Close),
        })
        .collect();
    flatten_commands(&commands, Transform::IDENTITY, tolerance, true)
}

fn paths_to_node(paths: ClipperPaths, paint: Paint) -> Node {
    let commands = paths_to_commands(paths);
    if commands.is_empty() {
        return Node::empty();
    }

    Node::Path(Path {
        commands,
        fill: Some(Fill { paint }),
        stroke: None,
        transform: Transform::IDENTITY,
    })
}

fn paths_to_commands(paths: ClipperPaths) -> Vec<PathCommand> {
    let raw: Vec<Vec<(f64, f64)>> = paths.into();
    let mut commands = Vec::new();
    for path in raw {
        let mut iter = path.into_iter();
        let Some((x, y)) = iter.next() else {
            continue;
        };
        commands.push(PathCommand::MoveTo(Vec2(x as f32, y as f32)));
        for (x, y) in iter {
            commands.push(PathCommand::LineTo(Vec2(x as f32, y as f32)));
        }
        commands.push(PathCommand::Close);
    }
    commands
}

fn outline_view_box(size: Vec2, inner_view_box: &Rect, paths: Option<&ClipperPaths>) -> Rect {
    let layout = Rect {
        origin: Vec2::ZERO,
        size,
    };
    let mut bounds = union_rect(layout, *inner_view_box);
    if let Some(paths) = paths {
        if let Some(path_bounds) = paths_bounds(paths) {
            bounds = union_rect(bounds, path_bounds);
        }
    }
    bounds
}

fn paths_bounds(paths: &ClipperPaths) -> Option<Rect> {
    let raw: Vec<Vec<(f64, f64)>> = paths.clone().into();
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    let mut found = false;
    for path in raw {
        for (x, y) in path {
            let x = x as f32;
            let y = y as f32;
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
            found = true;
        }
    }
    found.then_some(Rect {
        origin: Vec2(min_x, min_y),
        size: Vec2(max_x - min_x, max_y - min_y),
    })
}

fn union_rect(a: Rect, b: Rect) -> Rect {
    let a_end = Vec2(a.origin.0 + a.size.0, a.origin.1 + a.size.1);
    let b_end = Vec2(b.origin.0 + b.size.0, b.origin.1 + b.size.1);
    let origin = Vec2(a.origin.0.min(b.origin.0), a.origin.1.min(b.origin.1));
    let end = Vec2(a_end.0.max(b_end.0), a_end.1.max(b_end.1));
    Rect {
        origin,
        size: Vec2(end.0 - origin.0, end.1 - origin.1),
    }
}

fn curve_steps(p0: Vec2, c1: Vec2, p1: Vec2, c2: Option<Vec2>, tolerance: f32) -> usize {
    let control_length = match c2 {
        Some(c2) => distance(p0, c1) + distance(c1, c2) + distance(c2, p1),
        None => distance(p0, c1) + distance(c1, p1),
    };
    ((control_length / tolerance.max(0.01)).ceil() as usize).clamp(4, MAX_CURVE_STEPS)
}

fn quad_point(p0: Vec2, c: Vec2, p1: Vec2, t: f32) -> Vec2 {
    let p01 = lerp(p0, c, t);
    let p12 = lerp(c, p1, t);
    lerp(p01, p12, t)
}

fn cubic_point(p0: Vec2, c1: Vec2, c2: Vec2, p1: Vec2, t: f32) -> Vec2 {
    let p01 = lerp(p0, c1, t);
    let p12 = lerp(c1, c2, t);
    let p23 = lerp(c2, p1, t);
    let p012 = lerp(p01, p12, t);
    let p123 = lerp(p12, p23, t);
    lerp(p012, p123, t)
}

fn lerp(a: Vec2, b: Vec2, t: f32) -> Vec2 {
    Vec2(a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

fn distance(a: Vec2, b: Vec2) -> f32 {
    ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt()
}

fn nearly_same(a: Vec2, b: Vec2) -> bool {
    (a.0 - b.0).abs() <= 0.0001 && (a.1 - b.1).abs() <= 0.0001
}

fn polygon_area(points: &[Vec2]) -> f32 {
    let mut area = 0.0;
    for i in 0..points.len() {
        let a = points[i];
        let b = points[(i + 1) % points.len()];
        area += a.0 * b.1 - b.0 * a.1;
    }
    area * 0.5
}

fn to_point(p: Vec2) -> Point {
    Point::new(p.0 as f64, p.1 as f64)
}

fn from_point(p: Point) -> Vec2 {
    Vec2(p.x as f32, p.y as f32)
}

fn max_scale(transform: Transform) -> f32 {
    let aa = transform.a * transform.a + transform.b * transform.b;
    let cc = transform.c * transform.c + transform.d * transform.d;
    let ac = transform.a * transform.c + transform.b * transform.d;
    let trace = aa + cc;
    let det = aa * cc - ac * ac;
    let discriminant = (trace * trace - 4.0 * det).max(0.0);
    let lambda = (trace + discriminant.sqrt()) * 0.5;
    if lambda.is_finite() {
        lambda.sqrt()
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Color;
    use crate::shapes::Rectangle;

    fn red() -> Paint {
        Paint::solid(Color::rgb_u8(255, 0, 0))
    }

    fn blue() -> Paint {
        Paint::solid(Color::rgb_u8(0, 0, 255))
    }

    #[test]
    fn outset_outline_expands_paint_bounds() {
        let outlined = Rectangle {
            size: Vec2(10.0, 6.0),
            fill: Some(red().into()),
            stroke: None,
        }
        .outlined(2.0, blue());

        let bounds = outlined.paint_bounds(Vec2(10.0, 6.0));
        assert!(bounds.origin.0 <= -1.9, "{bounds:?}");
        assert!(bounds.origin.1 <= -1.99, "{bounds:?}");
        assert!(bounds.size.0 >= 13.9, "{bounds:?}");
        assert!(bounds.size.1 >= 9.8, "{bounds:?}");
    }

    #[test]
    fn inset_outline_keeps_layout_bounds() {
        let outlined = Rectangle {
            size: Vec2(10.0, 6.0),
            fill: Some(red().into()),
            stroke: None,
        }
        .outlined(2.0, blue())
        .side(OutlineSide::Inset);

        assert_eq!(
            outlined.paint_bounds(Vec2(10.0, 6.0)),
            Rect {
                origin: Vec2::ZERO,
                size: Vec2(10.0, 6.0),
            }
        );
    }

    #[test]
    fn outlined_outputs_only_the_outline_paint() {
        let outlined = Rectangle {
            size: Vec2(10.0, 6.0),
            fill: Some(red().into()),
            stroke: None,
        }
        .outlined(2.0, blue());

        let graphic = outlined.render(Vec2(10.0, 6.0));
        let Node::Path(path) = graphic.root else {
            panic!("outlined rectangle should produce one compound path");
        };
        assert_eq!(path.stroke, None);
        assert_eq!(path.fill, Some(Fill { paint: blue() }));
        assert!(path
            .commands
            .iter()
            .any(|cmd| matches!(cmd, PathCommand::Close)));
        assert!(
            path.commands
                .iter()
                .filter(|cmd| matches!(cmd, PathCommand::MoveTo(_)))
                .count()
                >= 2,
            "outline should contain at least an outer contour and an inner cutout"
        );
    }

    #[test]
    fn centered_outline_uses_both_sides() {
        let outlined = Rectangle {
            size: Vec2(10.0, 6.0),
            fill: Some(red().into()),
            stroke: None,
        }
        .outlined(2.0, blue())
        .side(OutlineSide::Center);

        let bounds = outlined.paint_bounds(Vec2(10.0, 6.0));
        assert!(bounds.origin.0 <= -0.9, "{bounds:?}");
        assert!(bounds.origin.1 <= -0.99, "{bounds:?}");
        assert!(bounds.size.0 >= 11.9, "{bounds:?}");
        assert!(bounds.size.1 >= 7.9, "{bounds:?}");
    }
}
