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

use crate::color::Color;
use crate::geometry::Vec2;
use crate::raster::{CpuRasterImage, RasterComponent, RasterImage, Resolution};
use crate::vector::VectorGraphic;

/// How aggressively a render context should try to keep work on the GPU.
///
/// This is a policy signal, not a guarantee. Components should ask the context
/// for GPU hooks only when this prefers GPU work, and every hook is optional so
/// CPU fallback remains the default behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum GpuPreference {
    Disabled,
    #[default]
    Auto,
    PreferGpu,
}

impl GpuPreference {
    pub const fn prefers_gpu(self) -> bool {
        matches!(self, Self::Auto | Self::PreferGpu)
    }
}

/// Whether a render context should give a component its own cache slot.
///
/// Most components are [`Memoize`](CachePolicy::Memoize): their rendered
/// image is keyed and reused. A [`Transparent`](CachePolicy::Transparent)
/// component produces no image of its own — it delegates straight to a child
/// through [`RenderContext::render`], so caching it would only duplicate the
/// child's entry (and double-count its bytes). This is orthogonal to timing:
/// a transparent call is still timed, it just does not allocate a cache slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CachePolicy {
    #[default]
    Memoize,
    Transparent,
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

    /// Returns a device backend for GPU-capable components.
    ///
    /// The backend exposes generic raster primitives rather than per-component
    /// hooks; `Layer`, `DropShadow`, `Outline`, and future elements decide
    /// inside their own `render` implementation whether those primitives apply.
    fn gpu_backend(&mut self) -> Option<&mut dyn GpuRasterBackend> {
        None
    }

    /// Whether temporal effects should sample and average across their
    /// shutter window this frame.
    ///
    /// A policy signal like [`gpu_preference`](Self::gpu_preference): a
    /// preview host can switch it off to trade motion blur for cheaper
    /// frames, and a temporal effect must then degrade to its unblurred
    /// child render. Defaults to `true` so offline exports stay exact.
    fn motion_blur_enabled(&self) -> bool {
        true
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
            image @ RasterImage::Gpu(_) => {
                let backend = match &image {
                    RasterImage::Gpu(surface) => surface.backend(),
                    RasterImage::Cpu(_) => unreachable!(),
                };
                if let Some(gpu) = self.gpu_backend() {
                    if let Some(image) = gpu.readback(image) {
                        return image;
                    }
                }
                panic!(
                    "render context returned a GPU image for backend '{backend}' but did not implement readback",
                )
            }
        }
    }
}

pub struct CompositeInput<'a> {
    pub image: &'a RasterImage,
    pub offset_x: i32,
    pub offset_y: i32,
}

pub struct DropShadowInput<'a> {
    pub child: &'a RasterImage,
    pub target: Resolution,
    pub child_offset_x: i32,
    pub child_offset_y: i32,
    pub shadow_offset_x: i32,
    pub shadow_offset_y: i32,
    pub blur_radius: u32,
    pub color: Color,
}

pub struct OutlineInput<'a> {
    pub child: &'a RasterImage,
    pub target: Resolution,
    pub child_offset_x: i32,
    pub child_offset_y: i32,
    pub outline_offset_x: i32,
    pub outline_offset_y: i32,
    pub radius_x: u32,
    pub radius_y: u32,
    pub color: Color,
}

pub trait GpuRasterBackend {
    fn composite(
        &mut self,
        target: Resolution,
        inputs: &[CompositeInput<'_>],
    ) -> Option<RasterImage>;

    fn drop_shadow(&mut self, input: DropShadowInput<'_>) -> Option<RasterImage>;

    fn outline(&mut self, input: OutlineInput<'_>) -> Option<RasterImage>;

    fn rasterize(&mut self, graphic: &VectorGraphic, target: Resolution) -> Option<RasterImage>;

    /// Produces a target-sized image filled with a single solid color.
    /// Lets solid-color leaves (backgrounds, transparent spacers) start
    /// life on the GPU instead of being CPU-filled and uploaded.
    fn solid_fill(&mut self, target: Resolution, color: Color) -> Option<RasterImage>;

    /// Averages `frames` over `total` shutter samples into one
    /// `target`-sized image (a motion-blur accumulate).
    ///
    /// Every frame must already be `target`-sized; samples beyond
    /// `frames.len()` count as fully transparent, so a child that
    /// contributed no frame for part of the shutter fades out instead of
    /// brightening. The average is computed in premultiplied integer
    /// space and must match the CPU fallback byte-for-byte.
    fn temporal_average(
        &mut self,
        target: Resolution,
        frames: &[&RasterImage],
        total: u32,
    ) -> Option<RasterImage>;

    fn readback(&mut self, image: RasterImage) -> Option<CpuRasterImage>;
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
