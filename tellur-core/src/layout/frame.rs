//! [`Frame`]: per-axis sizing plus anchored alignment of one child.

use crate::geometry::{Alignment, Constraints, Rect, Transform, Vec2};
use crate::vector::{Group, Node, VectorComponent, VectorGraphic};
use crate::Keyable;

/// How a sizing-container picks its size on one axis, given the parent's
/// constraints and the child's intrinsic size.
#[derive(Debug, Clone, Copy, Keyable)]
pub enum SizeMode {
    /// Take the parent's max constraint on this axis (collapse to `0.0`
    /// if the max is unbounded). Equivalent to CSS `width: 100%` or
    /// SwiftUI's `.frame(maxWidth: .infinity)`.
    Fill,
    /// Hug the child's intrinsic size on this axis. The child is
    /// queried for its own preferred size and the result is used.
    Hug,
    /// Use exactly the given number of logical units on this axis.
    Fixed(f32),
}

pub(super) fn resolve_size_mode<F: FnOnce(Constraints) -> Vec2>(
    width: SizeMode,
    height: SizeMode,
    constraints: Constraints,
    child_layout: F,
) -> Vec2 {
    let needs_hug = matches!(width, SizeMode::Hug) || matches!(height, SizeMode::Hug);
    let hug = needs_hug.then(|| child_layout(constraints));
    let w = match width {
        SizeMode::Fill => finite_axis(constraints.max.0),
        SizeMode::Hug => hug.unwrap().0,
        SizeMode::Fixed(v) => v,
    };
    let h = match height {
        SizeMode::Fill => finite_axis(constraints.max.1),
        SizeMode::Hug => hug.unwrap().1,
        SizeMode::Fixed(v) => v,
    };
    constraints.constrain(Vec2(w, h))
}

fn finite_axis(v: f32) -> f32 {
    if v.is_finite() {
        v
    } else {
        0.0
    }
}

/// Sizes the outer box independently on each axis (`Fill` / `Hug` /
/// `Fixed`) and aligns the child inside it by an [`Alignment`] anchor
/// pair.
///
/// Both knobs default to the transparent choice — `Hug` on both axes and
/// top-left alignment — so each call site only states what it changes:
///
/// - sizing only: `Frame::builder().width(SizeMode::Fill).child(c)`
/// - centering in the available space:
///   `Frame::builder().width(SizeMode::Fill).height(SizeMode::Fill)
///   .align(Anchor::CENTER).child(c)`
/// - asymmetric anchors: `.align(Anchor::CENTER.to(Anchor::new(0.8, 0.5)))`
#[crate::component(vector)]
#[derive(Keyable)]
pub struct Frame {
    #[builder(default = SizeMode::Hug)]
    pub width: SizeMode,
    #[builder(default = SizeMode::Hug)]
    pub height: SizeMode,
    #[builder(into, default = Alignment::TOP_LEFT)]
    pub align: Alignment,
    #[builder(into)]
    pub child: Box<dyn VectorComponent>,
}

impl VectorComponent for Frame {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        resolve_size_mode(self.width, self.height, constraints, |c| {
            self.child.layout(c)
        })
    }

    fn render(&self, size: Vec2) -> VectorGraphic {
        let child_size = self.child.layout(Constraints::loose(size));
        let pos = child_size
            .anchored(self.align.child)
            .snap_to(self.align.at.point(size));
        let inner = self.child.render(child_size);
        VectorGraphic {
            view_box: Rect {
                origin: Vec2::ZERO,
                size,
            },
            root: Node::Group(Group {
                transform: Transform::translate(pos),
                opacity: 1.0,
                children: vec![inner.root],
            }),
        }
    }
}

pub(super) mod raster {
    use super::{resolve_size_mode, SizeMode};
    use crate::geometry::{Alignment, Constraints, Rect, Vec2};
    use crate::layer::{composite_children, translate_rect, union_rect};
    use crate::raster::{RasterComponent, RasterImage, Resolution};
    use crate::render_context::RenderContext;
    use crate::Keyable;

    /// Sizes the outer box on each axis (`Fill` / `Hug` / `Fixed`) and
    /// aligns the child inside it by an [`Alignment`] anchor pair. See the
    /// vector [`Frame`](super::Frame) for the knob defaults and examples.
    #[crate::component(raster)]
    #[derive(Keyable)]
    pub struct Frame {
        #[builder(default = SizeMode::Hug)]
        pub width: SizeMode,
        #[builder(default = SizeMode::Hug)]
        pub height: SizeMode,
        #[builder(into, default = Alignment::TOP_LEFT)]
        pub align: Alignment,
        #[builder(into)]
        pub child: Box<dyn RasterComponent>,
    }

    impl RasterComponent for Frame {
        fn layout(&self, constraints: Constraints) -> Vec2 {
            resolve_size_mode(self.width, self.height, constraints, |c| {
                self.child.layout(c)
            })
        }

        fn paint_bounds(&self, size: Vec2) -> Rect {
            let child_size = self.child.layout(Constraints::loose(size));
            let pos = child_size
                .anchored(self.align.child)
                .snap_to(self.align.at.point(size));
            let child_paint = self.child.paint_bounds(child_size);
            union_rect(
                Rect {
                    origin: Vec2::ZERO,
                    size,
                },
                translate_rect(child_paint, pos),
            )
        }

        fn render(
            &self,
            size: Vec2,
            target: Resolution,
            ctx: &mut dyn RenderContext,
        ) -> RasterImage {
            let child_size = self.child.layout(Constraints::loose(size));
            let pos = child_size
                .anchored(self.align.child)
                .snap_to(self.align.at.point(size));
            let paint_rect = self.paint_bounds(size);
            composite_children(
                paint_rect,
                target,
                &[(pos, child_size, self.child.as_ref())],
                ctx,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Anchor;
    use crate::shapes::Rectangle;
    use crate::vector::VectorComponent;

    fn rect(w: f32, h: f32) -> Rectangle {
        Rectangle {
            size: Vec2(w, h),
            fill: None,
            stroke: None,
        }
    }

    fn root_translation(graphic: &VectorGraphic) -> Vec2 {
        let Node::Group(group) = &graphic.root else {
            panic!("frame should render a translating group");
        };
        Vec2(group.transform.tx, group.transform.ty)
    }

    #[test]
    fn frame_defaults_hug_the_child_at_top_left() {
        let frame = Frame {
            width: SizeMode::Hug,
            height: SizeMode::Hug,
            align: Alignment::TOP_LEFT,
            child: rect(30.0, 20.0).boxed(),
        };
        let size = frame.layout(Constraints::loose(Vec2(100.0, 100.0)));
        assert_eq!(size, Vec2(30.0, 20.0));
        assert_eq!(root_translation(&frame.render(size)), Vec2::ZERO);
    }

    #[test]
    fn frame_centers_child_in_filled_space() {
        let frame = Frame {
            width: SizeMode::Fill,
            height: SizeMode::Fill,
            align: Anchor::CENTER.into(),
            child: rect(20.0, 10.0).boxed(),
        };
        let size = frame.layout(Constraints::loose(Vec2(100.0, 50.0)));
        assert_eq!(size, Vec2(100.0, 50.0));
        assert_eq!(root_translation(&frame.render(size)), Vec2(40.0, 20.0));
    }

    #[test]
    fn frame_asymmetric_alignment_snaps_child_anchor_onto_box_anchor() {
        let frame = Frame {
            width: SizeMode::Fill,
            height: SizeMode::Fixed(60.0),
            align: Anchor::CENTER.to(Anchor::new(1.0, 0.5)),
            child: rect(20.0, 20.0).boxed(),
        };
        let size = frame.layout(Constraints::loose(Vec2(100.0, 100.0)));
        assert_eq!(size, Vec2(100.0, 60.0));
        // Child center (10, 10) snaps onto (100, 30) → top-left at (90, 20).
        assert_eq!(root_translation(&frame.render(size)), Vec2(90.0, 20.0));
    }
}
