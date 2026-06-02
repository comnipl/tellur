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
    fn layout(&self, constraints: Constraints) -> Vec2 {
        self.child.layout(constraints)
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
        fn layout(&self, constraints: Constraints) -> Vec2 {
            self.child.layout(constraints)
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
