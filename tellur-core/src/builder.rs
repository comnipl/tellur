//! Builder-side ergonomics for the component API.
//!
//! Every component type derives a `bon` builder. To let a *complete* builder
//! flow into a parent's child slot (and be placed) without an explicit
//! `.build()`, each component generates:
//!
//! - `From<TBuilder<S: IsComplete>> for Box<dyn _Component>` and
//!   `From<T> for Box<dyn _Component>`, so child setters typed
//!   `impl Into<Box<dyn _Component>>` accept either a built value or a
//!   complete builder; and
//! - an impl of [`VectorBuilder`] / [`RasterBuilder`] on the complete builder,
//!   which the blanket placement extensions below hang off of.
//!
//! The marker trait carries an associated `Output` (the built component type)
//! and a `build_component` shim, so placement helpers that need the concrete
//! type — and the renderer's `.rasterize()` — can be written once as blankets.
//!
//! `Output` is bounded `PartialEq + Hash + 'static` (not just `*Component`):
//! the component traits are `DynEq + DynHash`, which do *not* imply
//! `PartialEq + Hash`, yet the render cache and `Rasterize<V>` require them.
//! Every real component satisfies the bound, so it costs nothing.

use std::hash::Hash;

use crate::geometry::{Anchor, Constraints, Vec2};
use crate::placement::{raster::Positioned as RasterPositioned, Positioned};
use crate::raster::RasterComponent;
use crate::vector::VectorComponent;

/// Implemented (by the component macro) for a *complete* builder of a
/// [`VectorComponent`]. The blanket [`VectorBuilderPlacement`] hangs off it.
pub trait VectorBuilder: Sized {
    type Output: VectorComponent + PartialEq + Hash + 'static;
    /// Finishes the builder. This is the `.build()` the caller never writes.
    fn build_component(self) -> Self::Output;
}

/// Raster counterpart of [`VectorBuilder`].
pub trait RasterBuilder: Sized {
    type Output: RasterComponent + PartialEq + Hash + 'static;
    fn build_component(self) -> Self::Output;
}

/// Placement on vector builders, mirroring
/// [`VectorPlacement`](crate::placement::VectorPlacement) on built components.
/// Blanket-implemented for every [`VectorBuilder`], so
/// `Foo::builder()…​.place_at(pos)` works with no `.build()`.
pub trait VectorBuilderPlacement: VectorBuilder {
    /// Places the built component's local origin at `position`.
    fn place_at(self, position: Vec2) -> Positioned {
        Positioned {
            offset: position,
            child: Box::new(self.build_component()),
        }
    }

    /// Begins an anchor placement; finish with
    /// [`snap_to`](AnchoredVectorBuilder::snap_to).
    fn anchored(self, anchor: Anchor) -> AnchoredVectorBuilder<Self::Output> {
        AnchoredVectorBuilder {
            component: self.build_component(),
            anchor,
        }
    }
}

impl<B: VectorBuilder> VectorBuilderPlacement for B {}

/// Raster counterpart of [`VectorBuilderPlacement`].
pub trait RasterBuilderPlacement: RasterBuilder {
    fn place_at(self, position: Vec2) -> RasterPositioned {
        RasterPositioned {
            offset: position,
            child: Box::new(self.build_component()),
        }
    }

    fn anchored(self, anchor: Anchor) -> AnchoredRasterBuilder<Self::Output> {
        AnchoredRasterBuilder {
            component: self.build_component(),
            anchor,
        }
    }
}

impl<B: RasterBuilder> RasterBuilderPlacement for B {}

/// Intermediate produced by [`VectorBuilderPlacement::anchored`]; mirrors
/// [`AnchoredVectorComponent`](crate::placement::AnchoredVectorComponent) but
/// holds an already-built component.
pub struct AnchoredVectorBuilder<C: VectorComponent> {
    component: C,
    anchor: Anchor,
}

impl<C: VectorComponent + 'static> AnchoredVectorBuilder<C> {
    /// Places the component so the chosen anchor on its intrinsic layout size
    /// lands on `target_point`.
    pub fn snap_to(self, target_point: Vec2) -> Positioned {
        let intrinsic = self.component.layout(Constraints::UNBOUNDED);
        let offset = intrinsic.anchored(self.anchor).snap_to(target_point);
        Positioned {
            offset,
            child: Box::new(self.component),
        }
    }
}

/// Raster counterpart of [`AnchoredVectorBuilder`].
pub struct AnchoredRasterBuilder<C: RasterComponent> {
    component: C,
    anchor: Anchor,
}

impl<C: RasterComponent + 'static> AnchoredRasterBuilder<C> {
    pub fn snap_to(self, target_point: Vec2) -> RasterPositioned {
        let intrinsic = self.component.layout(Constraints::UNBOUNDED);
        let offset = intrinsic.anchored(self.anchor).snap_to(target_point);
        RasterPositioned {
            offset,
            child: Box::new(self.component),
        }
    }
}
