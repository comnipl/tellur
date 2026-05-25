//! Render context for memoizing raster components.
//!
//! The trait defines the interface a renderer driver passes through the
//! `RasterComponent::render` call chain. A component asks the context to
//! render a child rather than calling the child's `render` directly; the
//! context is then free to memoize results, share scratch buffers, or
//! apply other cross-cutting policies.
//!
//! [`PassThrough`] is the trivial implementation — it forwards every
//! request straight to the component without caching. It is enough for
//! tests, single-frame previews, and any caller that doesn't want to
//! pay for cache bookkeeping. The renderer crate provides a caching
//! implementation on top of this trait.

use crate::geometry::Vec2;
use crate::raster::{RasterComponent, RasterImage, Resolution};

/// Drives raster component rendering and provides a hook for caching.
///
/// Components forward child `render` calls through
/// [`RenderContext::render`] (or call it as a free function via
/// `ctx.render(&*child, size, target)`) so the context can intercept and
/// reuse previously-produced results.
pub trait RenderContext {
    /// Renders `component` at the given logical `size` into a
    /// `target`-sized pixel buffer, possibly returning a cached result
    /// from a previous identical request.
    fn render(
        &mut self,
        component: &dyn RasterComponent,
        size: Vec2,
        target: Resolution,
    ) -> RasterImage;
}

/// A `RenderContext` that performs no caching. Every call goes straight
/// through to the component's `render` method. Useful for tests and any
/// caller that wants to opt out of memoization.
pub struct PassThrough;

impl RenderContext for PassThrough {
    fn render(
        &mut self,
        component: &dyn RasterComponent,
        size: Vec2,
        target: Resolution,
    ) -> RasterImage {
        component.render(size, target, self)
    }
}
