//! [`SizedBox`]: an empty fixed-size placeholder.

use crate::geometry::{Constraints, Rect, Transform, Vec2};
use crate::vector::{Group, Node, VectorComponent, VectorGraphic};

/// An empty box of the given size. Useful as a fixed-size spacer between
/// flex children or to reserve a region without any visible content. For
/// a spacer that grows with the leftover space, use
/// [`Flexible::spacer`](super::Flexible::spacer).
#[crate::component(vector)]
#[derive(PartialEq, Hash)]
pub struct SizedBox {
    pub size: Vec2,
}

impl VectorComponent for SizedBox {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        constraints.constrain(self.size)
    }

    fn render(&self, size: Vec2) -> VectorGraphic {
        VectorGraphic {
            view_box: Rect {
                origin: Vec2::ZERO,
                size,
            },
            root: Node::Group(Group {
                transform: Transform::IDENTITY,
                opacity: 1.0,
                children: vec![],
            }),
        }
    }
}

pub(super) mod raster {
    use crate::color::Color;
    use crate::geometry::{Constraints, Vec2};
    use crate::raster::{Background, RasterComponent, RasterImage, RasterResidency, Resolution};
    use crate::render_context::RenderContext;

    /// Raster mirror of the vector [`SizedBox`](super::SizedBox).
    #[crate::component(raster)]
    #[derive(PartialEq, Hash)]
    pub struct SizedBox {
        pub size: Vec2,
    }

    impl RasterComponent for SizedBox {
        fn layout(&self, constraints: Constraints) -> Vec2 {
            constraints.constrain(self.size)
        }

        fn render(
            &self,
            size: Vec2,
            target: Resolution,
            residency: RasterResidency,
            ctx: &mut dyn RenderContext,
        ) -> RasterImage {
            Background::new(Color::rgba_u8(0, 0, 0, 0)).render(size, target, residency, ctx)
        }
    }
}
