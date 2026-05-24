use std::io::Write;

use bytes::Bytes;
use thiserror::Error;

use crate::geometry::{Constraints, Rect, Vec2};

#[derive(Debug, Clone)]
pub struct RasterImage {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub pixels: Bytes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// 8-bit per channel sRGB with straight (non-premultiplied) alpha.
    Rgba8,
    /// 16-bit float per channel linear with alpha. Used for HDR.
    Rgba16Float,
}

/// Target output resolution for a `RasterComponent::render` call, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
pub trait RasterComponent {
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
    fn render(&self, size: Vec2, target: Resolution) -> RasterImage;

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

#[derive(Debug, Error)]
pub enum PngExportError {
    #[error("PNG export is not supported for pixel format {0:?}")]
    UnsupportedFormat(PixelFormat),
    #[error("pixel buffer size mismatch: expected {expected} bytes, got {actual}")]
    SizeMismatch { expected: usize, actual: usize },
    #[error("PNG encoding failed: {0}")]
    Encode(#[from] png::EncodingError),
}

impl RasterImage {
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
