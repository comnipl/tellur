//! The [`Span`] trait: one styled run within a line of
//! [`Text`](crate::text::Text).
//!
//! A line is a sequence of spans laid out left-to-right. A span can shape
//! itself — given the base style it inherits from the enclosing `Text` —
//! into placed vector paths plus the vertical metrics the line needs to
//! position it. Normal `Text` rendering may coalesce adjacent compatible
//! built-in spans first so cross-boundary kerning and formula layout are
//! preserved. [`TextSpan`](crate::text::TextSpan) is the ordinary styled-text
//! span; with the `latex` feature, [`MathSpan`](crate::math::MathSpan) renders
//! a LaTeX formula as another kind of span. Anything implementing `Span` flows
//! into `Text::builder().span(...)`.

use std::any::Any;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crate::dyn_compare::{DynEq, DynHash};
use crate::text::{Font, Weight};
use crate::vector::{Paint, PathCommand};

/// The base style the enclosing [`Text`](crate::text::Text) hands to each
/// span. A span uses these wherever it does not override them.
pub struct SpanContext<'a> {
    /// The base font; a span may shape with a different one.
    pub font: &'a Arc<Font>,
    /// The base size in logical pixels per em.
    pub size: f32,
    /// The base weight.
    pub weight: Weight,
    /// The base fill applied to ink without its own paint.
    pub fill: &'a Paint,
}

/// The geometry and metrics one span contributes to a line.
///
/// `paths` are in span-local, baseline-relative coordinates: x starts at
/// `0` and the text baseline is at `y = 0`, with y increasing downward
/// (so ink above the baseline has negative y). The enclosing `Text`
/// advances the pen by `width` and drops the span onto the line baseline.
pub struct ShapedSpan {
    /// Pen advance — how far the next span begins to the right.
    pub width: f32,
    /// Extent above the baseline (`>= 0`).
    pub ascent: f32,
    /// Extent below the baseline (`>= 0`).
    pub descent: f32,
    /// Filled paths, each paired with its paint.
    pub paths: Vec<(Vec<PathCommand>, Paint)>,
}

/// One run within a line of [`Text`](crate::text::Text).
///
/// Implementors shape themselves into placed paths given the inherited
/// base style. The super-traits let `Box<dyn Span>` live in the cache-key
/// component trees that drive render memoization: [`DynEq`]/[`DynHash`]
/// give it `PartialEq`/`Eq`/`Hash`, and [`SpanClone`] makes it cloneable.
pub trait Span: SpanClone + DynEq + DynHash {
    /// Shape this span against `ctx`, returning baseline-relative paths
    /// and the metrics the line needs to place it.
    fn shape(&self, ctx: &SpanContext<'_>) -> ShapedSpan;
}

// Compile-time guarantee that `Span` is dyn-safe.
const _: Option<&dyn Span> = None;

/// Clone support for `Box<dyn Span>`. Blanket-implemented for every
/// `Span` that is `Clone`; callers never implement it by hand.
pub trait SpanClone {
    fn clone_box(&self) -> Box<dyn Span>;
}

impl<T: Span + Clone + 'static> SpanClone for T {
    fn clone_box(&self) -> Box<dyn Span> {
        Box::new(self.clone())
    }
}

impl Clone for Box<dyn Span> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

// Identity of a `dyn Span` is its concrete type plus that type's own
// equality/hash — the same trait-object machinery `VectorComponent` uses,
// so a `Box<dyn Span>` participates in derived cache keys.
impl PartialEq for dyn Span {
    fn eq(&self, other: &Self) -> bool {
        DynEq::dyn_eq(self, other.as_any())
    }
}

impl Eq for dyn Span {}

impl Hash for dyn Span {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Any::type_id(self.as_any()).hash(state);
        DynHash::dyn_hash(self, state);
    }
}
