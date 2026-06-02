use std::any::Any;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::sync::Arc;

use bytes::Bytes;
use thiserror::Error;

use crate::dyn_compare::{DynEq, DynHash};
use crate::geometry::{Constraints, Rect, Vec2};
use crate::render_context::{CachePolicy, RenderContext};

#[derive(Debug, Clone)]
pub enum RasterImage {
    Cpu(CpuRasterImage),
    Gpu(GpuSurface),
}

impl RasterImage {
    pub fn cpu(width: u32, height: u32, format: PixelFormat, pixels: impl Into<Bytes>) -> Self {
        Self::Cpu(CpuRasterImage {
            width,
            height,
            format,
            pixels: pixels.into(),
        })
    }

    pub fn width(&self) -> u32 {
        match self {
            Self::Cpu(image) => image.width,
            Self::Gpu(surface) => surface.width,
        }
    }

    pub fn height(&self) -> u32 {
        match self {
            Self::Cpu(image) => image.height,
            Self::Gpu(surface) => surface.height,
        }
    }

    pub fn format(&self) -> PixelFormat {
        match self {
            Self::Cpu(image) => image.format,
            Self::Gpu(surface) => surface.format,
        }
    }

    pub fn as_cpu(&self) -> Option<&CpuRasterImage> {
        match self {
            Self::Cpu(image) => Some(image),
            Self::Gpu(_) => None,
        }
    }

    pub fn into_cpu(self) -> Result<CpuRasterImage, Self> {
        match self {
            Self::Cpu(image) => Ok(image),
            Self::Gpu(_) => Err(self),
        }
    }
}

impl From<CpuRasterImage> for RasterImage {
    fn from(image: CpuRasterImage) -> Self {
        Self::Cpu(image)
    }
}

impl From<GpuSurface> for RasterImage {
    fn from(surface: GpuSurface) -> Self {
        Self::Gpu(surface)
    }
}

#[derive(Debug, Clone)]
pub struct CpuRasterImage {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub pixels: Bytes,
}

/// Backend-owned GPU image handle.
///
/// `tellur-core` deliberately keeps this opaque: concrete backends can store a
/// `wgpu::Texture`, texture view, command-graph node, or another device-local
/// handle behind the `Arc<dyn Any>`, while core remains dependency-free.
#[derive(Clone)]
pub struct GpuSurface {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    backend: &'static str,
    handle: Arc<dyn Any + Send + Sync>,
}

impl GpuSurface {
    pub fn new(
        width: u32,
        height: u32,
        format: PixelFormat,
        backend: &'static str,
        handle: Arc<dyn Any + Send + Sync>,
    ) -> Self {
        Self {
            width,
            height,
            format,
            backend,
            handle,
        }
    }

    pub fn backend(&self) -> &'static str {
        self.backend
    }

    pub fn handle(&self) -> &(dyn Any + Send + Sync) {
        self.handle.as_ref()
    }

    pub fn handle_arc(&self) -> Arc<dyn Any + Send + Sync> {
        Arc::clone(&self.handle)
    }

    pub fn downcast_handle<T: Any + Send + Sync>(&self) -> Option<&T> {
        self.handle.as_ref().downcast_ref::<T>()
    }
}

impl fmt::Debug for GpuSurface {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GpuSurface")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("format", &self.format)
            .field("backend", &self.backend)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PixelFormat {
    /// 8-bit per channel sRGB with straight (non-premultiplied) alpha.
    Rgba8,
    /// 16-bit float per channel linear with alpha. Used for HDR.
    Rgba16Float,
}

/// Target output resolution for a `RasterComponent::render` call, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

impl Resolution {
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }
}

/// A component that can produce a `RasterImage` at a parent-chosen
/// resolution. Mirrors [`VectorComponent`](crate::vector::VectorComponent)
/// with the same two-pass `layout` / `render` protocol, but `render`
/// takes an extra `target: Resolution` so that the parent can request a
/// specific pixel output size for the component's logical `size`.
///
/// 1. Parent calls [`layout`](RasterComponent::layout) with constraints;
///    child returns its layout size.
/// 2. Parent calls [`render`](RasterComponent::render) with that size
///    and the pixel resolution the child should produce.
/// 3. Optionally [`paint_bounds`](RasterComponent::paint_bounds) tells
///    the parent how far the component paints outside the layout box,
///    so `Layer` can grow the sub-resolution accordingly.
pub trait RasterComponent: DynEq + DynHash {
    /// Decide the layout size given the parent's constraints.
    fn layout(&self, constraints: Constraints) -> Vec2;

    /// Paint bounds for the component once `size` has been chosen. The
    /// default returns a rectangle whose `origin` is `(0, 0)` and whose
    /// `size` equals the layout size; effects override to widen it.
    fn paint_bounds(&self, size: Vec2) -> Rect {
        Rect {
            origin: Vec2::ZERO,
            size,
        }
    }

    /// Render the component at `size` (logical) into a `target`-sized
    /// pixel buffer. The pixel buffer covers exactly the `paint_bounds`
    /// rectangle for `size` — for default `paint_bounds`, that's the
    /// `(0, 0)..size` region.
    ///
    /// The `ctx` argument lets the component delegate child renders back
    /// through the driver (typically via `ctx.render(&*child, size,
    /// target)`) so cross-cutting policies such as memoization can be
    /// applied uniformly across the tree.
    fn render(&self, size: Vec2, target: Resolution, ctx: &mut dyn RenderContext) -> RasterImage;

    /// Whether this component should occupy its own cache slot. Pure
    /// pass-through wrappers (e.g. [`Positioned`](crate::placement::raster::Positioned))
    /// return [`CachePolicy::Transparent`] so the context times them but lets
    /// the child they delegate to own the cache entry. Defaults to
    /// [`CachePolicy::Memoize`].
    fn cache_policy(&self) -> CachePolicy {
        CachePolicy::Memoize
    }

    /// Type-erases `self` into a heap-allocated trait object. Useful for
    /// constructing heterogeneous containers like `Layer.children` in
    /// struct-literal form.
    fn boxed(self) -> Box<dyn RasterComponent>
    where
        Self: Sized + 'static,
    {
        Box::new(self)
    }
}

// Compile-time guarantee that `RasterComponent` is dyn-safe.
const _: Option<&dyn RasterComponent> = None;

impl PartialEq for dyn RasterComponent {
    fn eq(&self, other: &Self) -> bool {
        DynEq::dyn_eq(self, other.as_any())
    }
}

impl Hash for dyn RasterComponent {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Include the concrete TypeId so two components with coincidentally
        // identical internal hashes but different types remain distinct.
        Any::type_id(self.as_any()).hash(state);
        DynHash::dyn_hash(self, state);
    }
}

#[derive(Debug, Error)]
pub enum PngExportError {
    #[error("PNG export is not supported for pixel format {0:?}")]
    UnsupportedFormat(PixelFormat),
    #[error("pixel buffer size mismatch: expected {expected} bytes, got {actual}")]
    SizeMismatch { expected: usize, actual: usize },
    #[error("PNG export requires a CPU image; got GPU surface from backend {backend}")]
    GpuSurface { backend: &'static str },
    #[error("PNG encoding failed: {0}")]
    Encode(#[from] png::EncodingError),
}

impl CpuRasterImage {
    pub fn new(width: u32, height: u32, format: PixelFormat, pixels: impl Into<Bytes>) -> Self {
        Self {
            width,
            height,
            format,
            pixels: pixels.into(),
        }
    }

    /// Encodes the image as PNG and writes it to `writer`.
    ///
    /// Only `PixelFormat::Rgba8` is currently supported. HDR formats require
    /// linear-to-sRGB conversion and are not yet handled.
    pub fn export_png<W: Write>(&self, writer: W) -> Result<(), PngExportError> {
        if self.format != PixelFormat::Rgba8 {
            return Err(PngExportError::UnsupportedFormat(self.format));
        }

        let expected = (self.width as usize) * (self.height as usize) * 4;
        if self.pixels.len() != expected {
            return Err(PngExportError::SizeMismatch {
                expected,
                actual: self.pixels.len(),
            });
        }

        let mut encoder = png::Encoder::new(writer, self.width, self.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut png_writer = encoder.write_header()?;
        png_writer.write_image_data(&self.pixels)?;
        Ok(())
    }
}

impl RasterImage {
    /// Encodes a CPU image as PNG and writes it to `writer`.
    ///
    /// GPU images must be read back through the active render context first.
    pub fn export_png<W: Write>(&self, writer: W) -> Result<(), PngExportError> {
        match self {
            Self::Cpu(image) => image.export_png(writer),
            Self::Gpu(surface) => Err(PngExportError::GpuSurface {
                backend: surface.backend(),
            }),
        }
    }
}
