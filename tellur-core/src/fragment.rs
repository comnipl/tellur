//! [`Fragment`]: a transparent grouping of components.
//!
//! A `Fragment` overlays its children without imposing any transform of its
//! own — it is the component-system form of "several siblings", and an *empty*
//! `Fragment` is the form of "nothing". It pairs with
//! [`Positioned`](crate::placement::Positioned): a `Fragment` groups, a
//! `Positioned` offsets, and between them a component can be nothing, one,
//! many, or placed — all inside the component system, mirroring React's
//! element / fragment / `null`.
//!
//! `Fragment` never shifts its children's coordinates: its root group uses the
//! identity transform, so each child paints wherever its own
//! `Positioned`/leaf coordinates put it. The only coordinate it computes is its
//! reported `paint_bounds` (the bounding box of the children), used for
//! auto-fit and raster sub-resolution sizing.

use crate::geometry::{Constraints, Rect, Vec2};
use crate::layer::{render_vector_children, vector_children_bounds};
use crate::vector::{VectorComponent, VectorGraphic};

/// A transparent group of [`VectorComponent`] children. An empty `Fragment`
/// renders nothing (the "null" form).
#[crate::component(vector)]
#[derive(PartialEq, Hash)]
pub struct Fragment {
    #[children(each = child)]
    pub children: Vec<Box<dyn VectorComponent>>,
}

impl Fragment {
    /// A fragment with no children — renders nothing.
    pub fn empty() -> Self {
        Self {
            children: Vec::new(),
        }
    }

    /// A fragment wrapping exactly one child.
    pub fn single(child: impl Into<Box<dyn VectorComponent>>) -> Self {
        Self {
            children: vec![child.into()],
        }
    }
}

impl<T: Into<Box<dyn VectorComponent>>> FromIterator<T> for Fragment {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Self {
            children: iter.into_iter().map(Into::into).collect(),
        }
    }
}

impl VectorComponent for Fragment {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        constraints.constrain(vector_children_bounds(&self.children).size)
    }

    fn paint_bounds(&self, _size: Vec2) -> Rect {
        vector_children_bounds(&self.children)
    }

    fn render(&self, size: Vec2) -> VectorGraphic {
        let origin = vector_children_bounds(&self.children).origin;
        render_vector_children(
            &self.children,
            Rect { origin, size },
            Constraints::loose(size),
        )
    }
}

/// Raster counterpart of [`Fragment`]. Same transparent-grouping semantics,
/// operating on `Box<dyn RasterComponent>`.
pub mod raster {
    use crate::geometry::{Constraints, Rect, Vec2};
    use crate::layer::{composite_children, raster_children_bounds};
    use crate::raster::{RasterComponent, RasterImage, RasterResidency, Resolution};
    use crate::render_context::RenderContext;

    /// A transparent group of [`RasterComponent`] children. Empty = nothing.
    #[crate::component(raster)]
    #[derive(PartialEq, Hash)]
    pub struct Fragment {
        #[children(each = child)]
        pub children: Vec<Box<dyn RasterComponent>>,
    }

    impl Fragment {
        pub fn empty() -> Self {
            Self {
                children: Vec::new(),
            }
        }

        pub fn single(child: impl Into<Box<dyn RasterComponent>>) -> Self {
            Self {
                children: vec![child.into()],
            }
        }
    }

    impl<T: Into<Box<dyn RasterComponent>>> FromIterator<T> for Fragment {
        fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
            Self {
                children: iter.into_iter().map(Into::into).collect(),
            }
        }
    }

    impl RasterComponent for Fragment {
        fn layout(&self, constraints: Constraints) -> Vec2 {
            constraints.constrain(raster_children_bounds(&self.children).size)
        }

        fn paint_bounds(&self, _size: Vec2) -> Rect {
            raster_children_bounds(&self.children)
        }

        fn render(
            &self,
            size: Vec2,
            target: Resolution,
            residency: RasterResidency,
            ctx: &mut dyn RenderContext,
        ) -> RasterImage {
            let paint_rect = raster_children_bounds(&self.children);
            let child_constraints = Constraints::loose(size);
            let placed: Vec<(Vec2, Vec2, &dyn RasterComponent)> = self
                .children
                .iter()
                .map(|child| {
                    let child_size = child.layout(child_constraints);
                    (Vec2::ZERO, child_size, child.as_ref())
                })
                .collect();
            composite_children(paint_rect, target, &placed, residency, ctx)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Color;
    use crate::placement::VectorPlacement;
    use crate::shapes::Rectangle;
    use crate::vector::Paint;

    fn rect(w: f32, h: f32) -> Rectangle {
        Rectangle {
            size: Vec2(w, h),
            fill: Paint::Solid(Color::rgb_u8(0, 0, 0)).into(),
            stroke: None,
        }
    }

    #[test]
    fn fragment_fits_single_child_size() {
        let fragment = Fragment {
            children: vec![rect(80.0, 40.0).place_at(Vec2(10.0, 20.0)).into()],
        };
        assert_eq!(fragment.layout(Constraints::UNBOUNDED), Vec2(80.0, 40.0));
    }

    #[test]
    fn fragment_unions_disjoint_children() {
        let fragment = Fragment {
            children: vec![
                rect(50.0, 50.0).place_at(Vec2(0.0, 0.0)).into(),
                rect(50.0, 50.0).place_at(Vec2(100.0, 100.0)).into(),
            ],
        };
        // bounding (0,0)..(150,150) → size (150, 150)
        assert_eq!(fragment.layout(Constraints::UNBOUNDED), Vec2(150.0, 150.0));
    }

    #[test]
    fn fragment_handles_negative_positions() {
        let fragment = Fragment {
            children: vec![
                rect(100.0, 100.0).place_at(Vec2(-30.0, 0.0)).into(),
                rect(100.0, 100.0).place_at(Vec2(50.0, 20.0)).into(),
            ],
        };
        // bounding (-30,0)..(150,120) → size (180, 120)
        assert_eq!(fragment.layout(Constraints::UNBOUNDED), Vec2(180.0, 120.0));
    }

    #[test]
    fn fragment_empty_is_zero() {
        let fragment = Fragment::empty();
        assert_eq!(fragment.layout(Constraints::UNBOUNDED), Vec2::ZERO);
    }
}
