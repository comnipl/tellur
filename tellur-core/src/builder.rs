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

use crate::geometry::{Anchor, Constraints, Transform, Vec2};
use crate::placement::{raster::Positioned as RasterPositioned, Positioned};
use crate::raster::RasterComponent;
use crate::vector::{Transformed, VectorComponent, VectorTransform};

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

/// Transform wrappers on complete vector builders, mirroring
/// [`VectorTransform`] on built components.
pub trait VectorBuilderTransform: VectorBuilder {
    fn transform(self, transform: Transform) -> Transformed {
        self.build_component().transform(transform)
    }

    fn opacity(self, opacity: f32) -> Transformed {
        self.build_component().opacity(opacity)
    }
}

impl<B: VectorBuilder> VectorBuilderTransform for B {}

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

/// A raster→raster wrapping step: given a boxed child, produce the *concrete*
/// effect component that wraps it.
///
/// The component macro implements this (for `#[component(raster)]` types whose
/// child field is tagged `#[effect]`) on the effect's *builder while its child
/// slot is still empty* — so a caller passes `DropShadow::builder()…` with no
/// `.child()` and no `.build()`, and [`RasterEffect::effect`] fills the child.
/// `Output` is the concrete component (not `Box<dyn RasterComponent>`, which has
/// no `RasterComponent` impl), so `.effect()` results keep chaining and placing.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a ready-to-apply raster effect",
    note = "an effect builder must have every parameter set and its `#[effect]` child slot still empty"
)]
pub trait Effect {
    type Output: RasterComponent + 'static;
    fn apply(self, child: Box<dyn RasterComponent>) -> Self::Output;
}

/// Blanket extension adding `.effect(...)` / `.effect_with(...)` to every built
/// raster component, mirroring [`RasterPlacement`](crate::placement::RasterPlacement).
///
/// Effects apply inside-out: the first `.effect()` is closest to the base and
/// each subsequent one wraps further out, so
/// `base.effect(halo).effect(drop)` builds `drop { child: halo { child: base } }`.
pub trait RasterEffect: RasterComponent + Sized + 'static {
    /// Applies a builder effect (e.g. `DropShadow::builder()…` with its child
    /// slot empty), returning the concrete wrapper so further `.effect()` /
    /// `.place_at()` calls keep resolving.
    fn effect<E: Effect>(self, effect: E) -> E::Output {
        effect.apply(Box::new(self))
    }

    /// Escape hatch: applies an arbitrary closure that receives the boxed child
    /// and returns any concrete raster component — for ad-hoc or multi-wrapper
    /// composition a single-`child` builder effect cannot express.
    fn effect_with<C, F>(self, wrap: F) -> C
    where
        F: FnOnce(Box<dyn RasterComponent>) -> C,
        C: RasterComponent + 'static,
    {
        wrap(Box::new(self))
    }
}

impl<T: RasterComponent + 'static> RasterEffect for T {}
