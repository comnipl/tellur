//! [`Clip`]: restricts a child's paint to a region.
//!
//! `Clip` is layout-transparent (it reports the child's own size unchanged)
//! but wraps the rendered output in a [`Node::ClipGroup`], so anything the
//! child paints outside the region is cut away rather than merely excluded
//! from `paint_bounds`.

use crate::geometry::{Constraints, Rect, Transform, Vec2};
use crate::vector::{ClipGroup, Node, PathCommand, VectorComponent, VectorGraphic};
use crate::Keyable;

/// The region a [`Clip`] restricts its child to: an axis-aligned rectangle,
/// or an arbitrary path (filled with the nonzero winding rule, the same as
/// any other filled path).
///
/// Construct with [`ClipRegion::rect`] / [`ClipRegion::path`], or pass a bare
/// [`Rect`] to `Clip::builder().region(...)` — the common rectangular case
/// converts automatically.
#[derive(Debug, Clone, Keyable)]
pub enum ClipRegion {
    Rect(Rect),
    Path(Vec<PathCommand>),
}

impl ClipRegion {
    pub fn rect(rect: Rect) -> Self {
        Self::Rect(rect)
    }

    pub fn path(commands: impl Into<Vec<PathCommand>>) -> Self {
        Self::Path(commands.into())
    }

    /// The region's own bounding box, in the same local coordinate space the
    /// clipped child paints in.
    fn bounds(&self) -> Rect {
        match self {
            Self::Rect(rect) => *rect,
            Self::Path(commands) => path_command_bounds(commands).unwrap_or(Rect {
                origin: Vec2::ZERO,
                size: Vec2::ZERO,
            }),
        }
    }

    /// The region expressed as path commands, ready for [`ClipGroup::commands`].
    fn to_commands(&self) -> Vec<PathCommand> {
        match self {
            Self::Rect(rect) => rect_path_commands(*rect),
            Self::Path(commands) => commands.clone(),
        }
    }
}

impl From<Rect> for ClipRegion {
    fn from(rect: Rect) -> Self {
        Self::Rect(rect)
    }
}

/// Clips `child` to [`region`](Clip::region): the union of the region and the
/// child's layout box is not painted outside the region's bounds.
///
/// Transparent to layout — `layout` and the reported size are exactly the
/// child's own — so `Clip` only ever shrinks what is visible, never the
/// space a parent reserves for it.
#[crate::component(vector)]
#[derive(Keyable)]
pub struct Clip {
    #[builder(into)]
    pub region: ClipRegion,
    #[builder(into)]
    pub child: Box<dyn VectorComponent>,
}

impl VectorComponent for Clip {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        self.child.layout(constraints)
    }

    fn paint_bounds(&self, size: Vec2) -> Rect {
        intersect_rect(self.region.bounds(), self.child.paint_bounds(size))
    }

    fn render(&self, size: Vec2) -> VectorGraphic {
        let inner = self.child.render(size);
        VectorGraphic {
            view_box: self.paint_bounds(size),
            root: Node::ClipGroup(ClipGroup {
                commands: self.region.to_commands(),
                transform: Transform::IDENTITY,
                child: Box::new(inner.root),
            }),
        }
    }
}

fn rect_path_commands(rect: Rect) -> Vec<PathCommand> {
    let Rect {
        origin: Vec2(x, y),
        size: Vec2(w, h),
    } = rect;
    vec![
        PathCommand::MoveTo(Vec2(x, y)),
        PathCommand::LineTo(Vec2(x + w, y)),
        PathCommand::LineTo(Vec2(x + w, y + h)),
        PathCommand::LineTo(Vec2(x, y + h)),
        PathCommand::Close,
    ]
}

/// Bounding box of a path's on-curve and control points. For curves this is a
/// conservative superset (a Bezier segment always lies within the convex hull
/// of its control points), which is exactly what an intersection-based
/// `paint_bounds` needs. `None` for an empty command list.
fn path_command_bounds(commands: &[PathCommand]) -> Option<Rect> {
    let mut min = Vec2(f32::INFINITY, f32::INFINITY);
    let mut max = Vec2(f32::NEG_INFINITY, f32::NEG_INFINITY);
    let mut found = false;
    let mut include = |p: Vec2| {
        min = Vec2(min.0.min(p.0), min.1.min(p.1));
        max = Vec2(max.0.max(p.0), max.1.max(p.1));
        found = true;
    };
    for &command in commands {
        match command {
            PathCommand::MoveTo(p) | PathCommand::LineTo(p) => include(p),
            PathCommand::QuadTo { control, to } => {
                include(control);
                include(to);
            }
            PathCommand::CubicTo { c1, c2, to } => {
                include(c1);
                include(c2);
                include(to);
            }
            PathCommand::Close => {}
        }
    }
    found.then_some(Rect {
        origin: min,
        size: Vec2(max.0 - min.0, max.1 - min.1),
    })
}

/// Largest rectangle contained in both `a` and `b`; zero-size (but not
/// necessarily zero-origin) when they do not overlap.
fn intersect_rect(a: Rect, b: Rect) -> Rect {
    let a_end = Vec2(a.origin.0 + a.size.0, a.origin.1 + a.size.1);
    let b_end = Vec2(b.origin.0 + b.size.0, b.origin.1 + b.size.1);
    let origin = Vec2(a.origin.0.max(b.origin.0), a.origin.1.max(b.origin.1));
    let end = Vec2(a_end.0.min(b_end.0), a_end.1.min(b_end.1));
    Rect {
        origin,
        size: Vec2((end.0 - origin.0).max(0.0), (end.1 - origin.1).max(0.0)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Color;
    use crate::placement::VectorPlacement;
    use crate::shapes::Rectangle;
    use crate::vector::{Node, Paint};

    fn rect(w: f32, h: f32) -> Rectangle {
        Rectangle {
            size: Vec2(w, h),
            fill: Paint::Solid(Color::rgb_u8(10, 20, 30)).into(),
            stroke: None,
        }
    }

    #[test]
    fn layout_passes_through_to_the_child_unchanged() {
        let clip = Clip::builder()
            .region(Rect {
                origin: Vec2::ZERO,
                size: Vec2(5.0, 5.0),
            })
            .child(rect(80.0, 40.0))
            .build();
        assert_eq!(clip.layout(Constraints::UNBOUNDED), Vec2(80.0, 40.0));
    }

    #[test]
    fn render_wraps_the_child_in_a_clip_group() {
        let clip = Clip::builder()
            .region(Rect {
                origin: Vec2(2.0, 2.0),
                size: Vec2(6.0, 6.0),
            })
            .child(rect(10.0, 10.0))
            .build();
        let graphic = clip.render(Vec2(10.0, 10.0));
        let Node::ClipGroup(group) = graphic.root else {
            panic!("Clip should render a ClipGroup");
        };
        assert_eq!(
            group.commands,
            vec![
                PathCommand::MoveTo(Vec2(2.0, 2.0)),
                PathCommand::LineTo(Vec2(8.0, 2.0)),
                PathCommand::LineTo(Vec2(8.0, 8.0)),
                PathCommand::LineTo(Vec2(2.0, 8.0)),
                PathCommand::Close,
            ]
        );
        assert!(matches!(*group.child, Node::Path(_)));
    }

    #[test]
    fn paint_bounds_is_the_intersection_of_region_and_child() {
        // The clip rect only partially overlaps the (0,0)..(10,10) child box.
        let clip = Clip::builder()
            .region(Rect {
                origin: Vec2(5.0, 2.0),
                size: Vec2(20.0, 20.0),
            })
            .child(rect(10.0, 10.0))
            .build();
        assert_eq!(
            clip.paint_bounds(Vec2(10.0, 10.0)),
            Rect {
                origin: Vec2(5.0, 2.0),
                size: Vec2(5.0, 8.0),
            }
        );
    }

    #[test]
    fn paint_bounds_is_empty_when_region_and_child_do_not_overlap() {
        let clip = Clip::builder()
            .region(Rect {
                origin: Vec2(100.0, 100.0),
                size: Vec2(5.0, 5.0),
            })
            .child(rect(10.0, 10.0))
            .build();
        let bounds = clip.paint_bounds(Vec2(10.0, 10.0));
        assert_eq!(bounds.size, Vec2::ZERO);
    }

    #[test]
    fn path_region_bounds_come_from_its_control_points() {
        let clip = Clip::builder()
            .region(ClipRegion::path(vec![
                PathCommand::MoveTo(Vec2(1.0, 1.0)),
                PathCommand::LineTo(Vec2(9.0, 1.0)),
                PathCommand::CubicTo {
                    c1: Vec2(9.0, 12.0),
                    c2: Vec2(1.0, 12.0),
                    to: Vec2(1.0, 9.0),
                },
                PathCommand::Close,
            ]))
            .child(rect(20.0, 20.0).place_at(Vec2::ZERO))
            .build();
        assert_eq!(
            clip.paint_bounds(Vec2(20.0, 20.0)),
            Rect {
                origin: Vec2(1.0, 1.0),
                size: Vec2(8.0, 11.0),
            }
        );
    }

    #[test]
    fn placed_child_offset_is_reflected_in_render_size_pass_through() {
        // Sanity check that Clip forwards the exact size a parent gives it —
        // it must not silently reinterpret or clamp the child's dimensions.
        let clip = Clip::builder()
            .region(Rect {
                origin: Vec2::ZERO,
                size: Vec2(1000.0, 1000.0),
            })
            .child(rect(30.0, 15.0))
            .build();
        let child_size = clip.child.layout(Constraints::UNBOUNDED);
        assert_eq!(clip.render(child_size).view_box.size, Vec2(30.0, 15.0));
    }
}
