use bytes::Bytes;

use crate::component::Component;

pub struct RasterImage {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub pixels: Bytes,
}

pub enum PixelFormat {
    /// 8-bit per channel sRGB with alpha.
    Rgba8,
    /// 16-bit float per channel linear with alpha. Used for HDR.
    Rgba16Float,
}

/// A `Component` that can produce a `RasterImage`.
pub trait RasterComponent: Component {
    fn render(&self) -> RasterImage;
}

// Compile-time guarantee that `RasterComponent` is dyn-safe.
const _: Option<&dyn RasterComponent> = None;
