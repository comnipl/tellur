use std::io::Write;

use bytes::Bytes;
use thiserror::Error;

use crate::component::Component;

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

/// A `Component` that can produce a `RasterImage` at a caller-specified
/// resolution.
///
/// `target` flows from the top-level call down through the component tree.
/// Each intermediate component decides what resolution to request from its
/// own children so that the final image is produced with the minimum work
/// needed to fill `target`.
pub trait RasterComponent: Component {
    fn render(&self, target: Resolution) -> RasterImage;
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
