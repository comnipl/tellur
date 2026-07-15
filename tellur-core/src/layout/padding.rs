//! [`Padding`]: empty space around a child.

use crate::geometry::{Constraints, EdgeInsets, Rect, Transform, Vec2};
use crate::vector::{Group, Node, VectorComponent, VectorGraphic};
use crate::Keyable;

/// Wraps a child with empty space on each side.
#[crate::component(vector)]
#[derive(Keyable)]
pub struct Padding {
    pub insets: EdgeInsets,
    #[builder(into)]
    pub child: Box<dyn VectorComponent>,
}

impl Padding {
    fn inset_size(&self) -> Vec2 {
        Vec2(self.insets.horizontal(), self.insets.vertical())
    }
}

impl VectorComponent for Padding {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        let inset = self.inset_size();
        let child_size = self.child.layout(constraints.shrink(inset));
        Vec2(child_size.0 + inset.0, child_size.1 + inset.1)
    }

    fn render(&self, size: Vec2) -> VectorGraphic {
        let inset = self.inset_size();
        let inner_size = Vec2((size.0 - inset.0).max(0.0), (size.1 - inset.1).max(0.0));
        let inner = self.child.render(inner_size);
        VectorGraphic {
            view_box: Rect {
                origin: Vec2::ZERO,
                size,
            },
            root: Node::Group(Group {
                transform: Transform::translate(self.insets.top_left()),
                opacity: 1.0,
                children: vec![inner.root],
            }),
        }
    }
}

pub(super) mod raster {
    use crate::geometry::{Constraints, EdgeInsets, Rect, Vec2};
    use crate::layer::{composite_children, translate_rect, union_rect};
    use crate::raster::{RasterComponent, RasterImage, RasterResidency, Resolution};
    use crate::render_context::RenderContext;
    use crate::Keyable;

    /// Raster mirror of the vector [`Padding`](super::Padding).
    #[crate::component(raster)]
    #[derive(Keyable)]
    pub struct Padding {
        pub insets: EdgeInsets,
        #[builder(into)]
        pub child: Box<dyn RasterComponent>,
    }

    impl Padding {
        fn inset_size(&self) -> Vec2 {
            Vec2(self.insets.horizontal(), self.insets.vertical())
        }
    }

    impl RasterComponent for Padding {
        fn layout(&self, constraints: Constraints) -> Vec2 {
            let inset = self.inset_size();
            let child_size = self.child.layout(constraints.shrink(inset));
            Vec2(child_size.0 + inset.0, child_size.1 + inset.1)
        }

        fn paint_bounds(&self, size: Vec2) -> Rect {
            let inset = self.inset_size();
            let inner_size = Vec2((size.0 - inset.0).max(0.0), (size.1 - inset.1).max(0.0));
            let child_paint = self.child.paint_bounds(inner_size);
            union_rect(
                Rect {
                    origin: Vec2::ZERO,
                    size,
                },
                translate_rect(child_paint, self.insets.top_left()),
            )
        }

        fn render(
            &self,
            size: Vec2,
            target: Resolution,
            residency: RasterResidency,
            ctx: &mut dyn RenderContext,
        ) -> RasterImage {
            let inset = self.inset_size();
            let inner_size = Vec2((size.0 - inset.0).max(0.0), (size.1 - inset.1).max(0.0));
            let paint_rect = self.paint_bounds(size);
            composite_children(
                paint_rect,
                target,
                &[(self.insets.top_left(), inner_size, self.child.as_ref())],
                residency,
                ctx,
            )
        }
    }
}
