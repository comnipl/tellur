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

use std::any::Any;

use crate::geometry::Vec2;
use crate::raster::{CpuRasterImage, RasterComponent, RasterImage, Resolution};

/// How aggressively a render context should try to keep work on the GPU.
///
/// This is a policy signal, not a guarantee. Components should ask the context
/// for GPU hooks only when this prefers GPU work, and every hook is optional so
/// CPU fallback remains the default behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum GpuPreference {
    #[default]
    Disabled,
    PreferGpu,
}

impl GpuPreference {
    pub const fn prefers_gpu(self) -> bool {
        matches!(self, Self::PreferGpu)
    }
}

/// Drives raster component rendering and provides a hook for caching.
///
/// Components forward child `render` calls through
/// [`RenderContext::render`] (or call it as a free function via
/// `ctx.render(&*child, size, target)`) so the context can intercept and
/// reuse previously-produced results.
pub trait RenderContext {
    /// Exposes the concrete context for backend-specific rendering paths.
    ///
    /// Components should still branch on [`RenderContext::prefers_gpu`] first;
    /// downcasting is only for the implementation detail of talking to a
    /// concrete GPU backend when one is present.
    fn as_any_mut(&mut self) -> &mut dyn Any;

    /// Whether components should try optional GPU paths before falling back to
    /// CPU rendering.
    fn gpu_preference(&self) -> GpuPreference {
        GpuPreference::Disabled
    }

    fn prefers_gpu(&self) -> bool {
        self.gpu_preference().prefers_gpu()
    }

    /// Renders `component` at the given logical `size` into a
    /// `target`-sized pixel buffer, possibly returning a cached result
    /// from a previous identical request.
    fn render(
        &mut self,
        component: &dyn RasterComponent,
        size: Vec2,
        target: Resolution,
    ) -> RasterImage;

    /// Reads a rendered image back into CPU memory.
    ///
    /// GPU contexts that return `RasterImage::Gpu` must override this. The
    /// default handles the CPU fallback path and treats an unresolved GPU image
    /// as a backend bug.
    fn readback(&mut self, image: RasterImage) -> CpuRasterImage {
        match image {
            RasterImage::Cpu(image) => image,
            RasterImage::Gpu(surface) => {
                panic!(
                    "render context returned a GPU image for backend '{}' but did not implement readback",
                    surface.backend()
                )
            }
        }
    }
}

/// A `RenderContext` that performs no caching. Every call goes straight
/// through to the component's `render` method. Useful for tests and any
/// caller that wants to opt out of memoization.
pub struct PassThrough;

impl RenderContext for PassThrough {
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn render(
        &mut self,
        component: &dyn RasterComponent,
        size: Vec2,
        target: Resolution,
    ) -> RasterImage {
        component.render(size, target, self)
    }
}
