//! Placement primitives for positioning components inside a parent layout.
//!
//! [`Placed`] is a `(position, component)` pair stored by layer-like
//! containers (e.g. [`VectorLayer::children`](crate::layer::VectorLayer)).
//! It deliberately lives *outside* the component traits so that a
//! [`VectorComponent`] or [`RasterComponent`] describes only its intrinsic
//! shape, while its placement in a parent is expressed by wrapping it.
//!
//! The [`VectorPlacement`] and [`RasterPlacement`] extension traits give
//! every component fluent methods to produce a `Placed`:
//!
//! ```ignore
//! use tellur_core::placement::VectorPlacement;
//!
//! scene.add(background.at(Vec2::ZERO));
//! scene.add(circle.anchored(Anchor::CENTER).snap_to(target_point));
//! scene.add(dot.anchored(Anchor::CENTER_LEFT).snap_to(stripe_anchor.point(scene_size)));
//! ```
//!
//! The anchor-based methods mirror [`Vec2::anchored`] →
//! [`AnchoredSize::snap_to`](crate::geometry::AnchoredSize::snap_to) so the
//! geometry vocabulary carries over directly to component placement.

use crate::geometry::{Anchor, Constraints, Vec2};
use crate::raster::RasterComponent;
use crate::vector::VectorComponent;

/// A component paired with its top-left position in the parent's
/// coordinate space.
///
/// `C` is typically `dyn VectorComponent` or `dyn RasterComponent`, so the
/// concrete struct stored is `Placed<dyn VectorComponent>` etc. The
/// `?Sized` bound allows the dyn case.
pub struct Placed<C: ?Sized> {
    pub position: Vec2,
    pub child: Box<C>,
}

/// Extension trait that adds placement methods to every [`VectorComponent`].
///
/// Brought into scope alongside `use tellur_core::vector::VectorComponent`,
/// it lets callers write `circle.at(pos)` or
/// `circle.anchored(Anchor::CENTER).snap_to(target)` instead of manually
/// computing the offset and boxing the component.
pub trait VectorPlacement: VectorComponent + Sized + 'static {
    /// Places the component so its local origin `(0, 0)` lands at `position`
    /// in the parent's coordinate space.
    fn at(self, position: Vec2) -> Placed<dyn VectorComponent> {
        Placed {
            position,
            child: Box::new(self),
        }
    }

    /// Begins an anchor-based placement: `anchor` picks a point on the
    /// component's own [`view_box`](VectorComponent::view_box), which a
    /// follow-up `snap_to` then aligns onto a point in the parent.
    fn anchored(self, anchor: Anchor) -> AnchoredVectorComponent<Self> {
        AnchoredVectorComponent {
            component: self,
            anchor,
        }
    }
}

impl<T: VectorComponent + 'static> VectorPlacement for T {}

/// Intermediate produced by [`VectorPlacement::anchored`]. Holds the
/// component and the anchor point on its `view_box` until a snap target is
/// provided.
pub struct AnchoredVectorComponent<C: VectorComponent> {
    component: C,
    anchor: Anchor,
}

impl<C: VectorComponent + 'static> AnchoredVectorComponent<C> {
    /// Places the component so the chosen anchor on its intrinsic layout
    /// size (obtained via `layout(Constraints::UNBOUNDED)`) lands on
    /// `target_point` in the parent's coordinate space.
    pub fn snap_to(self, target_point: Vec2) -> Placed<dyn VectorComponent> {
        let intrinsic = self.component.layout(Constraints::UNBOUNDED);
        let position = intrinsic.anchored(self.anchor).snap_to(target_point);
        Placed {
            position,
            child: Box::new(self.component),
        }
    }
}

/// Extension trait that adds placement methods to every [`RasterComponent`].
///
/// Mirrors [`VectorPlacement`] one-to-one; the produced `Placed` wraps a
/// `dyn RasterComponent` instead.
pub trait RasterPlacement: RasterComponent + Sized + 'static {
    /// Places the component so its local origin `(0, 0)` lands at `position`
    /// in the parent's coordinate space.
    fn at(self, position: Vec2) -> Placed<dyn RasterComponent> {
        Placed {
            position,
            child: Box::new(self),
        }
    }

    /// Begins an anchor-based placement; see
    /// [`VectorPlacement::anchored`] for the same idea on the vector side.
    fn anchored(self, anchor: Anchor) -> AnchoredRasterComponent<Self> {
        AnchoredRasterComponent {
            component: self,
            anchor,
        }
    }
}

impl<T: RasterComponent + 'static> RasterPlacement for T {}

/// Intermediate produced by [`RasterPlacement::anchored`]; counterpart of
/// [`AnchoredVectorComponent`].
pub struct AnchoredRasterComponent<C: RasterComponent> {
    component: C,
    anchor: Anchor,
}

impl<C: RasterComponent + 'static> AnchoredRasterComponent<C> {
    /// Places the component so the chosen anchor on its intrinsic layout
    /// size (obtained via `layout(Constraints::UNBOUNDED)`) lands on
    /// `target_point` in the parent's coordinate space.
    pub fn snap_to(self, target_point: Vec2) -> Placed<dyn RasterComponent> {
        let intrinsic = self.component.layout(Constraints::UNBOUNDED);
        let position = intrinsic.anchored(self.anchor).snap_to(target_point);
        Placed {
            position,
            child: Box::new(self.component),
        }
    }
}
