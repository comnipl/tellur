use std::any::Any;
use std::fmt;
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Cursor, Seek, Write};
use std::path::{Path, PathBuf};
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

    /// Whether two images share the same backing storage (one `Bytes`
    /// allocation / one GPU handle).
    ///
    /// A cheap identity check, not a pixel comparison: a caching render
    /// context hands out clones of one cache entry, so shared storage ⇒
    /// pixel-identical images. `false` carries no information — equal
    /// pixels in distinct buffers also compare `false`.
    pub fn shares_storage(&self, other: &RasterImage) -> bool {
        match (self, other) {
            (Self::Cpu(a), Self::Cpu(b)) => {
                a.pixels.len() == b.pixels.len() && a.pixels.as_ptr() == b.pixels.as_ptr()
            }
            (Self::Gpu(a), Self::Gpu(b)) => {
                // Compare the Arc data pointers as thin pointers so the
                // (non-unique) trait-object vtable half never participates.
                std::ptr::eq(
                    Arc::as_ptr(&a.handle) as *const (),
                    Arc::as_ptr(&b.handle) as *const (),
                )
            }
            _ => false,
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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

    /// Display name surfaced when this raster component is placed in a timeline
    /// (via the one-way `RasterComponent → TimelineComponent` blanket). `None`
    /// for plain raster primitives; a `#[component(...)]` fn overrides this to
    /// return its auto-derived or templated name, which the blanket
    /// [`arrangement`](crate::timeline_component::TimelineComponent::arrangement)
    /// stamps onto the node's [`name`](crate::timeline_component::Arrangement::name).
    fn arrangement_name(&self) -> Option<String> {
        None
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

    /// Loads an image from disk, selecting a decoder from the file extension.
    ///
    /// PNG is supported today. Unsupported extensions return
    /// [`ImageLoadError::UnsupportedFormat`] before opening the file, so callers
    /// get a clear format error instead of a decode failure.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ImageLoadError> {
        let path = path.as_ref();
        match image_extension(path).as_deref() {
            Some("png") => Self::load_png(path),
            extension => Err(ImageLoadError::UnsupportedFormat {
                path: path.to_path_buf(),
                extension: extension.map(str::to_owned),
            }),
        }
    }

    /// Loads a PNG image from disk into straight-alpha RGBA8 pixels.
    pub fn load_png(path: impl AsRef<Path>) -> Result<Self, ImageLoadError> {
        let path = path.as_ref();
        let file = File::open(path).map_err(|source| ImageLoadError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::read_png(BufReader::new(file))
    }

    /// Decodes a PNG image from memory into straight-alpha RGBA8 pixels.
    pub fn decode_png(bytes: &[u8]) -> Result<Self, ImageLoadError> {
        Self::read_png(BufReader::new(Cursor::new(bytes)))
    }

    /// Decodes a PNG stream into straight-alpha RGBA8 pixels.
    pub fn read_png<R: BufRead + Seek>(reader: R) -> Result<Self, ImageLoadError> {
        let mut decoder = png::Decoder::new(reader);
        decoder.set_transformations(png::Transformations::normalize_to_color8());
        let mut reader = decoder.read_info()?;
        let mut pixels = vec![
            0;
            reader
                .output_buffer_size()
                .ok_or(ImageLoadError::ImageTooLarge)?
        ];
        let info = reader.next_frame(&mut pixels)?;
        pixels.truncate(info.buffer_size());
        png_to_rgba8(
            info.width,
            info.height,
            info.color_type,
            info.bit_depth,
            pixels,
        )
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
    /// Loads an image from disk as a CPU [`RasterImage`].
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ImageLoadError> {
        CpuRasterImage::load(path).map(Self::Cpu)
    }

    /// Loads a PNG from disk as a CPU [`RasterImage`].
    pub fn load_png(path: impl AsRef<Path>) -> Result<Self, ImageLoadError> {
        CpuRasterImage::load_png(path).map(Self::Cpu)
    }

    /// Decodes PNG bytes as a CPU [`RasterImage`].
    pub fn decode_png(bytes: &[u8]) -> Result<Self, ImageLoadError> {
        CpuRasterImage::decode_png(bytes).map(Self::Cpu)
    }

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

#[derive(Debug, Error)]
pub enum ImageLoadError {
    #[error("unsupported image format {extension:?} for path {path:?}; supported formats: png")]
    UnsupportedFormat {
        path: PathBuf,
        extension: Option<String>,
    },
    #[error("failed to open image {path:?}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("decoded image is too large")]
    ImageTooLarge,
    #[error("unsupported PNG color type {color_type:?} with bit depth {bit_depth:?}")]
    UnsupportedPngColor {
        color_type: png::ColorType,
        bit_depth: png::BitDepth,
    },
    #[error("decoded PNG buffer size mismatch: expected {expected} bytes, got {actual}")]
    PngSizeMismatch { expected: usize, actual: usize },
    #[error("PNG decode failed: {0}")]
    PngDecode(#[from] png::DecodingError),
}

/// A loaded still image that participates in the raster component tree.
///
/// Its intrinsic layout size is the image's pixel dimensions expressed as
/// logical units. Rendering resamples the image to the parent-chosen target
/// resolution, so `Frame`, `Layer`, placement, timeline windows, and the render
/// cache can treat it like any other [`RasterComponent`].
#[crate::component(raster)]
#[derive(Clone, PartialEq, Hash)]
pub struct StillImage {
    pub image: CpuRasterImage,
}

impl StillImage {
    pub fn new(image: CpuRasterImage) -> Self {
        Self { image }
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self, ImageLoadError> {
        CpuRasterImage::load(path).map(Self::new)
    }

    pub fn load_png(path: impl AsRef<Path>) -> Result<Self, ImageLoadError> {
        CpuRasterImage::load_png(path).map(Self::new)
    }

    pub fn decode_png(bytes: &[u8]) -> Result<Self, ImageLoadError> {
        CpuRasterImage::decode_png(bytes).map(Self::new)
    }
}

impl RasterComponent for StillImage {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        constraints.constrain(Vec2(self.image.width as f32, self.image.height as f32))
    }

    fn render(&self, _size: Vec2, target: Resolution, _ctx: &mut dyn RenderContext) -> RasterImage {
        RasterImage::Cpu(resample_rgba8(&self.image, target))
    }
}

fn image_extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
}

fn png_to_rgba8(
    width: u32,
    height: u32,
    color_type: png::ColorType,
    bit_depth: png::BitDepth,
    pixels: Vec<u8>,
) -> Result<CpuRasterImage, ImageLoadError> {
    if bit_depth != png::BitDepth::Eight {
        return Err(ImageLoadError::UnsupportedPngColor {
            color_type,
            bit_depth,
        });
    }

    let rgba = match color_type {
        png::ColorType::Rgba => pixels,
        png::ColorType::Rgb => {
            let mut rgba = Vec::with_capacity(rgba_len(width, height)?);
            for rgb in pixels.chunks_exact(3) {
                rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
            rgba
        }
        png::ColorType::Grayscale => {
            let mut rgba = Vec::with_capacity(rgba_len(width, height)?);
            for gray in pixels {
                rgba.extend_from_slice(&[gray, gray, gray, 255]);
            }
            rgba
        }
        png::ColorType::GrayscaleAlpha => {
            let mut rgba = Vec::with_capacity(rgba_len(width, height)?);
            for ga in pixels.chunks_exact(2) {
                let gray = ga[0];
                rgba.extend_from_slice(&[gray, gray, gray, ga[1]]);
            }
            rgba
        }
        png::ColorType::Indexed => {
            return Err(ImageLoadError::UnsupportedPngColor {
                color_type,
                bit_depth,
            });
        }
    };

    let expected = rgba_len(width, height)?;
    if rgba.len() != expected {
        return Err(ImageLoadError::PngSizeMismatch {
            expected,
            actual: rgba.len(),
        });
    }

    Ok(CpuRasterImage::new(width, height, PixelFormat::Rgba8, rgba))
}

fn rgba_len(width: u32, height: u32) -> Result<usize, ImageLoadError> {
    (width as usize)
        .checked_mul(height as usize)
        .and_then(|px| px.checked_mul(4))
        .ok_or(ImageLoadError::ImageTooLarge)
}

fn resample_rgba8(image: &CpuRasterImage, target: Resolution) -> CpuRasterImage {
    assert_eq!(
        image.format,
        PixelFormat::Rgba8,
        "StillImage only supports Rgba8 images",
    );
    assert_eq!(
        image.pixels.len(),
        rgba_len(image.width, image.height).expect("source image dimensions fit in memory"),
        "StillImage source buffer must be tightly-packed RGBA8",
    );

    if image.width == target.width && image.height == target.height {
        return image.clone();
    }

    let len = rgba_len(target.width, target.height).expect("target image dimensions fit in memory");
    if target.width == 0 || target.height == 0 || image.width == 0 || image.height == 0 {
        return CpuRasterImage::new(
            target.width,
            target.height,
            PixelFormat::Rgba8,
            vec![0; len],
        );
    }

    let src = image.pixels.as_ref();
    let mut out = vec![0u8; len];
    for y in 0..target.height {
        let (y0, y1, wy) = sample_axis(y, target.height, image.height);
        for x in 0..target.width {
            let (x0, x1, wx) = sample_axis(x, target.width, image.width);
            let p00 = pixel(src, image.width, x0, y0);
            let p10 = pixel(src, image.width, x1, y0);
            let p01 = pixel(src, image.width, x0, y1);
            let p11 = pixel(src, image.width, x1, y1);
            let top = lerp_rgba8_premul(p00, p10, wx);
            let bottom = lerp_rgba8_premul(p01, p11, wx);
            let px = unpremul(lerp_premul(top, bottom, wy));
            let offset = ((y as usize) * (target.width as usize) + (x as usize)) * 4;
            out[offset..offset + 4].copy_from_slice(&px);
        }
    }

    CpuRasterImage::new(target.width, target.height, PixelFormat::Rgba8, out)
}

fn sample_axis(dst: u32, dst_len: u32, src_len: u32) -> (u32, u32, f32) {
    let pos = ((dst as f32 + 0.5) * src_len as f32 / dst_len as f32 - 0.5)
        .clamp(0.0, src_len.saturating_sub(1) as f32);
    let lo = pos.floor() as u32;
    let hi = (lo + 1).min(src_len - 1);
    (lo, hi, pos - lo as f32)
}

fn pixel(src: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let offset = ((y as usize) * (width as usize) + (x as usize)) * 4;
    [
        src[offset],
        src[offset + 1],
        src[offset + 2],
        src[offset + 3],
    ]
}

fn lerp_rgba8_premul(a: [u8; 4], b: [u8; 4], t: f32) -> [f32; 4] {
    let a = premul(a);
    let b = premul(b);
    lerp_premul(a, b, t)
}

fn lerp_premul(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    [
        lerp(a[0], b[0], t),
        lerp(a[1], b[1], t),
        lerp(a[2], b[2], t),
        lerp(a[3], b[3], t),
    ]
}

fn premul(px: [u8; 4]) -> [f32; 4] {
    let alpha = px[3] as f32 / 255.0;
    [
        px[0] as f32 * alpha,
        px[1] as f32 * alpha,
        px[2] as f32 * alpha,
        px[3] as f32,
    ]
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn unpremul(px: [f32; 4]) -> [u8; 4] {
    let alpha = px[3].round().clamp(0.0, 255.0);
    if alpha <= 0.0 {
        return [0, 0, 0, 0];
    }
    let unalpha = 255.0 / px[3];
    [
        (px[0] * unalpha).round().clamp(0.0, 255.0) as u8,
        (px[1] * unalpha).round().clamp(0.0, 255.0) as u8,
        (px[2] * unalpha).round().clamp(0.0, 255.0) as u8,
        alpha as u8,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render_context::PassThrough;

    fn sample_image() -> CpuRasterImage {
        CpuRasterImage::new(
            2,
            1,
            PixelFormat::Rgba8,
            vec![255, 0, 0, 255, 0, 0, 255, 128],
        )
    }

    fn sample_png_bytes() -> Vec<u8> {
        let mut bytes = Vec::new();
        sample_image()
            .export_png(&mut bytes)
            .expect("encode sample PNG");
        bytes
    }

    #[test]
    fn decode_png_produces_rgba8_cpu_image() {
        let image = CpuRasterImage::decode_png(&sample_png_bytes()).expect("decode PNG");
        assert_eq!(image.width, 2);
        assert_eq!(image.height, 1);
        assert_eq!(image.format, PixelFormat::Rgba8);
        assert_eq!(image.pixels.as_ref(), &[255, 0, 0, 255, 0, 0, 255, 128],);
    }

    #[test]
    fn load_png_reads_from_disk() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "tellur-still-image-{}-{}.png",
            std::process::id(),
            nonce,
        ));
        std::fs::write(&path, sample_png_bytes()).expect("write sample PNG");
        let image = StillImage::load(&path).expect("load still image");
        std::fs::remove_file(&path).expect("remove sample PNG");

        assert_eq!(image.image.width, 2);
        assert_eq!(image.image.height, 1);
        assert_eq!(image.image.pixels.as_ref(), sample_image().pixels.as_ref());
    }

    #[test]
    fn still_image_is_a_raster_component() {
        let component = StillImage::new(sample_image());
        assert_eq!(component.layout(Constraints::UNBOUNDED), Vec2(2.0, 1.0),);

        let rendered = component.render(Vec2(2.0, 1.0), Resolution::new(2, 1), &mut PassThrough);
        let cpu = rendered.into_cpu().expect("still image renders to CPU");
        assert_eq!(cpu.pixels.as_ref(), sample_image().pixels.as_ref());
    }
}
