//! Trait-object equality and hashing for component trees.
//!
//! [`RasterComponent`](crate::raster::RasterComponent) and
//! [`VectorComponent`](crate::vector::VectorComponent) need to participate in
//! `PartialEq` and `Hash` so that render results can be memoized in a
//! `RenderContext` cache keyed by component identity. The trait-object form
//! `dyn Component` cannot derive these directly — `PartialEq::eq` is not
//! dyn-safe and `Hash::hash` is parameterised over `H: Hasher`.
//!
//! [`DynEq`] and [`DynHash`] are the standard workaround: they expose the
//! type-erased shape of `==` and `hash` via [`Any`] downcast and a
//! `&mut dyn Hasher` argument, and have blanket impls for any `T: PartialEq +
//! Hash + 'static`. A component trait adds them as super-traits, and a
//! manual `impl PartialEq for dyn Component` / `impl Hash for dyn Component`
//! delegates through them. With that wired up, `Box<dyn Component>`,
//! `Vec<Box<dyn Component>>`, etc. pick up `PartialEq + Hash` automatically
//! through the standard library blanket impls, so `#[derive]` on containers
//! that hold trait objects just works.
//!
//! [`hash_f32`] / [`hash_f32_slice`] are the canonical hashers for `f32`
//! fields and slices in cache-key types. They use `to_bits()` so `-0.0`,
//! `+0.0`, and `NaN` keep distinct hashes (matching the way `PartialEq` on
//! `f32` already treats them distinctly when sourced from bit patterns).

use std::any::Any;
use std::hash::{Hash, Hasher};

/// Object-safe equality. Implemented for all `T: PartialEq + 'static`.
///
/// Used as a super-trait on `RasterComponent` / `VectorComponent` so a
/// `dyn Component` can compare itself with another `dyn Component` by
/// downcasting to its concrete type. Two trait objects of different
/// concrete types compare as not-equal.
///
/// The `dyn_eq` argument is `&dyn Any` (not `&dyn DynEq`) so callers can
/// hand it `other.as_any()` directly — this sidesteps trait-object
/// upcasting between component traits and `DynEq`.
pub trait DynEq: Any {
    fn as_any(&self) -> &dyn Any;
    fn dyn_eq(&self, other: &dyn Any) -> bool;
    /// Concrete type name behind this trait object, suitable for
    /// diagnostics (e.g. `tellur_renderer::shadow::DropShadow`). Backed
    /// by [`std::any::type_name`] specialized to the impl's `Self`, so
    /// callers can introspect a `&dyn Component` without a downcast.
    fn type_name(&self) -> &'static str;
}

impl<T: PartialEq + Any> DynEq for T {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn dyn_eq(&self, other: &dyn Any) -> bool {
        match other.downcast_ref::<T>() {
            Some(o) => self == o,
            None => false,
        }
    }

    fn type_name(&self) -> &'static str {
        std::any::type_name::<T>()
    }
}

/// Object-safe hashing. Implemented for all `T: Hash + ?Sized`.
///
/// `Hash::hash` is generic over the hasher, so `dyn Component` cannot
/// implement it directly. `DynHash` erases the hasher type behind
/// `&mut dyn Hasher`, which works because the standard library has
/// `impl<H: Hasher + ?Sized> Hasher for &mut H` — i.e. `&mut dyn Hasher`
/// is itself a `Hasher`, so calling `Hash::hash(&self, &mut state)` from
/// inside the impl is legal.
pub trait DynHash {
    fn dyn_hash(&self, state: &mut dyn Hasher);
}

impl<T: Hash + ?Sized> DynHash for T {
    fn dyn_hash(&self, mut state: &mut dyn Hasher) {
        self.hash(&mut state);
    }
}

/// Hashes an `f32` by its bit pattern.
///
/// `f32` does not implement `Hash` because its `PartialEq` is not
/// reflexive (`NaN != NaN`), so the standard library refuses to derive a
/// hash that would violate `a == b => hash(a) == hash(b)`. For cache-key
/// use we want a total hash and treat equal bit patterns as equal — this
/// helper makes that intent explicit and consistent across types.
#[inline]
pub fn hash_f32<H: Hasher>(v: f32, state: &mut H) {
    v.to_bits().hash(state);
}

/// Hashes an `&[f32]` by hashing each element's bit pattern in order.
#[inline]
pub fn hash_f32_slice<H: Hasher>(vs: &[f32], state: &mut H) {
    vs.len().hash(state);
    for v in vs {
        hash_f32(*v, state);
    }
}
