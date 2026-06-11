//! Placement: wrapping a component in a [`Positioned`] so it paints at an
//! offset in its parent's coordinate space.
//!
//! [`Positioned`] is the component-system replacement for the old `Placed`
//! `(position, child)` pair. Placement is now expressed as a real component
//! that a parent stores as a plain `Box<dyn _Component>`, so it composes with
//! [`Fragment`](crate::fragment::Fragment), the layout containers, and
//! everything else uniformly — there is no separate "placed" world.
//!
//! The fluent entry points mirror the geometry vocabulary:
//!
//! ```ignore
//! use tellur_core::placement::VectorPlacement;
//!
//! background.place_at(Vec2::ZERO);                       // top-left at a point
//! circle.anchored(Anchor::CENTER).snap_to(target_point); // an anchor onto a point
//! ```
//!
//! The anchor-based path mirrors [`Vec2::anchored`] →
//! [`AnchoredSize::snap_to`](crate::geometry::AnchoredSize::snap_to), so the
//! geometry vocabulary carries straight over to component placement.

use crate::geometry::{Anchor, Constraints, Rect, Transform, Vec2};
use crate::vector::{Group, Node, VectorComponent, VectorGraphic};
use crate::Keyable;

/// A [`VectorComponent`] shifted by `offset` in its parent's coordinate space.
///
/// Renders its child inside a translating [`Group`], so the produced node tree
/// is identical to what a parent container used to emit for a placed child.
/// `offset` is computed eagerly by the [`VectorPlacement`] fluent methods (and
/// their builder-side mirror), so a `Positioned` is fully determined at
/// construction — there is no re-anchoring at render time.
#[derive(Keyable)]
pub struct Positioned {
    pub offset: Vec2,
    pub child: Box<dyn VectorComponent>,
}

impl Positioned {
    /// Wraps `child` so its local origin is shifted to `offset`.
    pub fn new(offset: Vec2, child: impl Into<Box<dyn VectorComponent>>) -> Self {
        Self {
            offset,
            child: child.into(),
        }
    }
}

impl VectorComponent for Positioned {
    fn layout(&self, _constraints: Constraints) -> Vec2 {
        // A placed child is author-positioned: the canvas does not impose a
        // size on it, so measure it unbounded — the same measurement
        // `anchored().snap_to()` and `Fragment` use. Passing the parent's
        // constraints through would let a loose canvas clamp the child
        // (squashing an oversized circle into an ellipse) while the snap
        // offset was computed against the unclamped size.
        self.child.layout(Constraints::UNBOUNDED)
    }

    fn paint_bounds(&self, size: Vec2) -> Rect {
        let bounds = self.child.paint_bounds(size);
        Rect {
            origin: bounds.origin + self.offset,
            size: bounds.size,
        }
    }

    fn render(&self, size: Vec2) -> VectorGraphic {
        let inner = self.child.render(size);
        VectorGraphic {
            view_box: Rect {
                origin: inner.view_box.origin + self.offset,
                size: inner.view_box.size,
            },
            root: Node::Group(Group {
                transform: Transform::translate(self.offset),
                opacity: 1.0,
                children: vec![inner.root],
            }),
        }
    }
}

impl From<Positioned> for Box<dyn VectorComponent> {
    fn from(positioned: Positioned) -> Self {
        Box::new(positioned)
    }
}

/// Extension trait that adds placement methods to every [`VectorComponent`].
///
/// Brought into scope alongside `use tellur_core::vector::VectorComponent`, it
/// lets callers write `circle.place_at(pos)` or
/// `circle.anchored(Anchor::CENTER).snap_to(target)` to obtain a
/// [`Positioned`] component.
pub trait VectorPlacement: VectorComponent + Sized + 'static {
    /// Places the component so its local origin `(0, 0)` lands at `position`.
    fn place_at(self, position: Vec2) -> Positioned {
        Positioned {
            offset: position,
            child: Box::new(self),
        }
    }

    /// Begins an anchor-based placement: `anchor` picks a point on the
    /// component's intrinsic box, which a follow-up `snap_to` aligns onto a
    /// point in the parent.
    fn anchored(self, anchor: Anchor) -> AnchoredVectorComponent<Self> {
        AnchoredVectorComponent {
            component: self,
            anchor,
        }
    }
}

impl<T: VectorComponent + 'static> VectorPlacement for T {}

/// Intermediate produced by [`VectorPlacement::anchored`]. Holds the component
/// and the chosen anchor until a snap target is provided.
pub struct AnchoredVectorComponent<C: VectorComponent> {
    component: C,
    anchor: Anchor,
}

impl<C: VectorComponent + 'static> AnchoredVectorComponent<C> {
    /// Places the component so the chosen anchor on its intrinsic layout size
    /// (obtained via `layout(Constraints::UNBOUNDED)`) lands on `target_point`.
    pub fn snap_to(self, target_point: Vec2) -> Positioned {
        let intrinsic = self.component.layout(Constraints::UNBOUNDED);
        let offset = intrinsic.anchored(self.anchor).snap_to(target_point);
        Positioned {
            offset,
            child: Box::new(self.component),
        }
    }
}

/// Raster counterparts of the vector placement types. Same shape and
/// semantics, operating on `Box<dyn RasterComponent>`.
///
/// A raster [`Positioned`](raster::Positioned) carries its offset purely in its
/// `paint_bounds` origin; `render` delegates to the child untouched, because
/// the parent's `composite_children` pass applies the offset at compositing
/// time (`position + paint_bounds.origin - paint_rect.origin`).
pub mod raster {
    use crate::geometry::{Anchor, Constraints, Rect, Vec2};
    use crate::raster::{RasterComponent, RasterImage, Resolution};
    use crate::render_context::{CachePolicy, RenderContext};
    use crate::Keyable;

    /// A [`RasterComponent`] shifted by `offset` in its parent's coordinate
    /// space. See the [module docs](self) for how the offset is applied.
    #[derive(Keyable)]
    pub struct Positioned {
        pub offset: Vec2,
        pub child: Box<dyn RasterComponent>,
    }

    impl Positioned {
        pub fn new(offset: Vec2, child: impl Into<Box<dyn RasterComponent>>) -> Self {
            Self {
                offset,
                child: child.into(),
            }
        }
    }

    impl RasterComponent for Positioned {
        fn layout(&self, _constraints: Constraints) -> Vec2 {
            // Same rule as the vector `Positioned`: a placed child is
            // author-positioned, so it is measured unbounded rather than
            // clamped by the canvas it happens to sit in.
            self.child.layout(Constraints::UNBOUNDED)
        }

        fn paint_bounds(&self, size: Vec2) -> Rect {
            let bounds = self.child.paint_bounds(size);
            Rect {
                origin: bounds.origin + self.offset,
                size: bounds.size,
            }
        }

        fn cache_policy(&self) -> CachePolicy {
            // A `Positioned` produces no image of its own — it delegates
            // straight to its child. Caching it would only duplicate the
            // child's entry (and double-count its bytes), so stay transparent
            // and let the child own the cache slot.
            CachePolicy::Transparent
        }

        fn render(
            &self,
            size: Vec2,
            target: Resolution,
            ctx: &mut dyn RenderContext,
        ) -> RasterImage {
            // The offset rides in `paint_bounds`; the parent composites it.
            // Route the child through the context (rather than a direct
            // `child.render`) so the child owns the cache entry and its render
            // time is attributed to the child, not folded into this wrapper.
            ctx.render(self.child.as_ref(), size, target)
        }
    }

    impl From<Positioned> for Box<dyn RasterComponent> {
        fn from(positioned: Positioned) -> Self {
            Box::new(positioned)
        }
    }

    /// Raster mirror of [`VectorPlacement`](super::VectorPlacement).
    pub trait RasterPlacement: RasterComponent + Sized + 'static {
        fn place_at(self, position: Vec2) -> Positioned {
            Positioned {
                offset: position,
                child: Box::new(self),
            }
        }

        fn anchored(self, anchor: Anchor) -> AnchoredRasterComponent<Self> {
            AnchoredRasterComponent {
                component: self,
                anchor,
            }
        }
    }

    impl<T: RasterComponent + 'static> RasterPlacement for T {}

    /// Intermediate produced by [`RasterPlacement::anchored`].
    pub struct AnchoredRasterComponent<C: RasterComponent> {
        component: C,
        anchor: Anchor,
    }

    impl<C: RasterComponent + 'static> AnchoredRasterComponent<C> {
        pub fn snap_to(self, target_point: Vec2) -> Positioned {
            let intrinsic = self.component.layout(Constraints::UNBOUNDED);
            let offset = intrinsic.anchored(self.anchor).snap_to(target_point);
            Positioned {
                offset,
                child: Box::new(self.component),
            }
        }
    }
}

// Re-export the raster placement trait at the module root so existing
// `use tellur_core::placement::RasterPlacement;` paths keep resolving.
pub use raster::RasterPlacement;

#[cfg(test)]
mod tests {
    use super::raster::RasterPlacement;
    use super::*;
    use crate::color::Color;
    use crate::raster::{PixelFormat, RasterComponent, RasterImage, Resolution};
    use crate::render_context::RenderContext;
    use crate::shapes::Circle;
    use crate::vector::Paint;

    fn big_circle() -> Circle {
        // Diameter 1440 — taller than a 1080-high canvas.
        Circle::builder()
            .radius(720.0)
            .fill(Paint::Solid(Color::rgb_u8(0, 0, 0)))
            .build()
    }

    #[test]
    fn placed_child_is_measured_unbounded() {
        // The canvas hands loose constraints smaller than the circle; the
        // placed child still reports its intrinsic size instead of getting
        // squashed into an ellipse.
        let placed = big_circle().place_at(Vec2::ZERO);
        let size = placed.layout(Constraints::loose(Vec2(1920.0, 1080.0)));
        assert_eq!(size, Vec2(1440.0, 1440.0));
    }

    #[test]
    fn snap_offset_and_layout_agree_for_oversized_children() {
        // `anchored()` computes the offset against the unbounded measurement;
        // `layout` must report the same size or the child renders off-center.
        let placed = big_circle()
            .anchored(Anchor::CENTER)
            .snap_to(Vec2(960.0, 540.0));
        assert_eq!(placed.offset, Vec2(240.0, -180.0));
        let size = placed.layout(Constraints::loose(Vec2(1920.0, 1080.0)));
        assert_eq!(size, Vec2(1440.0, 1440.0));
    }

    #[derive(PartialEq, Hash)]
    struct FixedRaster;

    impl RasterComponent for FixedRaster {
        fn layout(&self, constraints: Constraints) -> Vec2 {
            constraints.constrain(Vec2(2000.0, 2000.0))
        }

        fn render(&self, _s: Vec2, t: Resolution, _ctx: &mut dyn RenderContext) -> RasterImage {
            let bytes = (t.width as usize) * (t.height as usize) * 4;
            RasterImage::cpu(t.width, t.height, PixelFormat::Rgba8, vec![0; bytes])
        }
    }

    #[test]
    fn raster_placed_child_is_measured_unbounded() {
        let placed = FixedRaster.place_at(Vec2::ZERO);
        let size = placed.layout(Constraints::loose(Vec2(100.0, 100.0)));
        assert_eq!(size, Vec2(2000.0, 2000.0));
    }
}
