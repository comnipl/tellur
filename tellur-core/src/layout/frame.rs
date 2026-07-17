//! [`Frame`]: per-axis sizing for one top-left-aligned child.

use crate::geometry::{Constraints, Rect, Transform, Vec2};
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
    let fill = constraints.fill_size();
    let w = match width {
        SizeMode::Fill => fill.0,
        SizeMode::Hug => hug.unwrap().0,
        SizeMode::Fixed(v) => v,
    };
    let h = match height {
        SizeMode::Fill => fill.1,
        SizeMode::Hug => hug.unwrap().1,
        SizeMode::Fixed(v) => v,
    };
    constraints.constrain(Vec2(w, h))
}

/// Sizes the outer box independently on each axis (`Fill` / `Hug` / `Fixed`).
///
/// Both knobs default to `Hug`, and the child always stays at the box's
/// top-left. Wrap the child in
/// [`Positioned`](crate::placement::Positioned) for anchor-based placement:
///
/// - sizing only: `Frame::builder().width(SizeMode::Fill).child(c)`
/// - centering in the available space:
///   `Frame::builder().width(SizeMode::Fill).height(SizeMode::Fill)
///   .child(c.anchored(Anchor::CENTER).snap_to(Anchor::CENTER))`
#[crate::component(vector)]
#[derive(Clone, Keyable)]
pub struct Frame {
    #[builder(default = SizeMode::Hug)]
    pub width: SizeMode,
    #[builder(default = SizeMode::Hug)]
    pub height: SizeMode,
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
        let inner = self.child.render(child_size);
        VectorGraphic {
            view_box: Rect {
                origin: Vec2::ZERO,
                size,
            },
            root: Node::Group(Group {
                transform: Transform::IDENTITY,
                opacity: 1.0,
                children: vec![inner.root],
            }),
        }
    }
}

pub(super) mod raster {
    use super::{resolve_size_mode, SizeMode};
    use crate::geometry::{Constraints, Rect, Vec2};
    use crate::layer::{composite_children, translate_rect, union_rect};
    use crate::raster::{RasterComponent, RasterImage, RasterResidency, Resolution};
    use crate::render_context::RenderContext;
    use crate::Keyable;

    /// Sizes the outer box on each axis (`Fill` / `Hug` / `Fixed`) and keeps
    /// the child at top-left. See the vector [`Frame`](super::Frame).
    #[crate::component(raster)]
    #[derive(Clone, Keyable)]
    pub struct Frame {
        #[builder(default = SizeMode::Hug)]
        pub width: SizeMode,
        #[builder(default = SizeMode::Hug)]
        pub height: SizeMode,
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
            let child_paint = self.child.paint_bounds(child_size);
            union_rect(
                Rect {
                    origin: Vec2::ZERO,
                    size,
                },
                translate_rect(child_paint, Vec2::ZERO),
            )
        }

        fn render(
            &self,
            size: Vec2,
            target: Resolution,
            residency: RasterResidency,
            ctx: &mut dyn RenderContext,
        ) -> RasterImage {
            let child_size = self.child.layout(Constraints::loose(size));
            let paint_rect = self.paint_bounds(size);
            composite_children(
                paint_rect,
                target,
                &[(Vec2::ZERO, child_size, self.child.as_ref())],
                residency,
                ctx,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Anchor;
    use crate::placement::raster::RasterPlacement;
    use crate::raster::{PixelFormat, RasterComponent, RasterImage, RasterResidency, Resolution};
    use crate::render_context::{PassThrough, RenderContext};
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
            child: rect(30.0, 20.0).boxed(),
        };
        let size = frame.layout(Constraints::loose(Vec2(100.0, 100.0)));
        assert_eq!(size, Vec2(30.0, 20.0));
        assert_eq!(root_translation(&frame.render(size)), Vec2::ZERO);
    }

    #[test]
    fn frame_fills_without_moving_its_child() {
        let frame = Frame {
            width: SizeMode::Fill,
            height: SizeMode::Fill,
            child: rect(20.0, 10.0).boxed(),
        };
        let size = frame.layout(Constraints::loose(Vec2(100.0, 50.0)));
        assert_eq!(size, Vec2(100.0, 50.0));
        let graphic = frame.render(size);
        assert_eq!(
            graphic.view_box,
            Rect {
                origin: Vec2::ZERO,
                size
            }
        );
        assert_eq!(root_translation(&graphic), Vec2::ZERO);
    }

    #[derive(Clone, PartialEq, Hash)]
    struct SolidRaster;

    impl RasterComponent for SolidRaster {
        fn layout(&self, constraints: Constraints) -> Vec2 {
            constraints.constrain(Vec2(2.0, 2.0))
        }

        fn render(
            &self,
            _size: Vec2,
            target: Resolution,
            _residency: RasterResidency,
            _ctx: &mut dyn RenderContext,
        ) -> RasterImage {
            RasterImage::cpu(
                target.width,
                target.height,
                PixelFormat::Rgba8,
                [255, 0, 0, 255].repeat((target.width * target.height) as usize),
            )
        }
    }

    #[test]
    fn positioned_anchor_reproduces_centered_frame_pixels() {
        let frame = raster::Frame {
            width: SizeMode::Fixed(4.0),
            height: SizeMode::Fixed(4.0),
            child: SolidRaster
                .anchored(Anchor::CENTER)
                .snap_to(Anchor::CENTER)
                .boxed(),
        };
        let size = frame.layout(Constraints::loose(Vec2(10.0, 10.0)));
        assert_eq!(size, Vec2(4.0, 4.0));

        let image = frame
            .render(
                size,
                Resolution::new(4, 4),
                RasterResidency::Cpu,
                &mut PassThrough,
            )
            .into_cpu()
            .expect("frame renders to CPU");
        for y in 0..4 {
            for x in 0..4 {
                let alpha = image.pixels[((y * 4 + x) * 4 + 3) as usize];
                let expected = if (1..=2).contains(&x) && (1..=2).contains(&y) {
                    255
                } else {
                    0
                };
                assert_eq!(alpha, expected, "alpha mismatch at ({x}, {y})");
            }
        }
    }
}
