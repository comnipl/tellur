use std::any::Any;
use std::fmt;
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Cursor, Seek, Write};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use bytes::Bytes;
use thiserror::Error;

use crate::color::Color;
use crate::dyn_compare::{DynEq, DynHash};
use crate::geometry::{Constraints, Rect, Vec2};
use crate::render_context::{CachePolicy, RenderContext};
use crate::scalar::clamp_unit;
use crate::Keyable;

#[derive(Debug, Clone)]
pub enum RasterImage {
    Cpu(CpuRasterImage),
    Gpu(GpuSurface),
}

/// Identifies one pixel-storage allocation, assigned once when it's created
/// (in [`CpuRasterImage::new`]) from a global monotonic counter.
///
/// A raw `Bytes` pointer looks like a stable identity but isn't: once the
/// last clone of a `Bytes` drops, its allocation can be freed and a later,
/// unrelated allocation can land at the exact same address (ABA). A
/// `PixelStorageId` is never reused, so two `CpuRasterImage`s compare equal
/// under this only when one was cloned from the other (or from a common
/// ancestor) — never merely because they happen to share an address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PixelStorageId(u64);

impl PixelStorageId {
    fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// Pixel storage for a [`CpuRasterImage`]: a `Bytes` allocation paired with
/// the [`PixelStorageId`] minted for it.
///
/// The pairing — not a sibling field on `CpuRasterImage` — is what makes the
/// id trustworthy: every route to a `PixelBytes` value is a `From` impl
/// below, and each one mints a fresh id. So the common
/// `image.pixels = new_pixels.into()` mutation pattern (in-place alpha
/// multiply, etc. — see `Opacity::render`) can never leave a stale id
/// paired with new content, which a plain `Bytes` field plus a separate id
/// field on `CpuRasterImage` would allow (the assignment overwrites
/// `pixels` but not the sibling id).
///
/// Derefs to `Bytes` so existing read-only call sites (`.len()`, `.as_ref()`,
/// `.to_vec()`, `.chunks_exact(_)`, passing `&image.pixels` where `&[u8]` is
/// expected, ...) keep working unchanged.
#[derive(Debug, Clone)]
pub struct PixelBytes {
    bytes: Bytes,
    id: PixelStorageId,
}

impl PixelBytes {
    /// Identity of this allocation. Stable across `clone()`; distinct from
    /// every other allocation, including ones the allocator later places at
    /// the same address.
    pub fn id(&self) -> PixelStorageId {
        self.id
    }

    pub fn into_inner(self) -> Bytes {
        self.bytes
    }
}

impl From<Bytes> for PixelBytes {
    fn from(bytes: Bytes) -> Self {
        Self {
            bytes,
            id: PixelStorageId::next(),
        }
    }
}

impl From<Vec<u8>> for PixelBytes {
    fn from(bytes: Vec<u8>) -> Self {
        Bytes::from(bytes).into()
    }
}

impl Deref for PixelBytes {
    type Target = Bytes;

    fn deref(&self) -> &Bytes {
        &self.bytes
    }
}

impl AsRef<[u8]> for PixelBytes {
    fn as_ref(&self) -> &[u8] {
        self.bytes.as_ref()
    }
}

// Content-based, deliberately ignoring `id`: two allocations with identical
// bytes should still compare equal, exactly as plain `Bytes` would.
impl PartialEq for PixelBytes {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}

impl Eq for PixelBytes {}

impl Hash for PixelBytes {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.bytes.hash(state);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum RasterStorageId {
    Cpu {
        width: u32,
        height: u32,
        format: PixelFormat,
        id: PixelStorageId,
    },
    Gpu {
        width: u32,
        height: u32,
        format: PixelFormat,
        backend: &'static str,
        ptr: usize,
    },
}

impl RasterImage {
    pub fn cpu(width: u32, height: u32, format: PixelFormat, pixels: impl Into<Bytes>) -> Self {
        Self::Cpu(CpuRasterImage::new(width, height, format, pixels))
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
            (Self::Cpu(a), Self::Cpu(b)) => a.storage_id() == b.storage_id(),
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

    pub(crate) fn storage_id(&self) -> RasterStorageId {
        match self {
            Self::Cpu(image) => RasterStorageId::Cpu {
                width: image.width,
                height: image.height,
                format: image.format,
                id: image.storage_id(),
            },
            Self::Gpu(surface) => RasterStorageId::Gpu {
                width: surface.width,
                height: surface.height,
                format: surface.format,
                backend: surface.backend,
                ptr: Arc::as_ptr(&surface.handle) as *const () as usize,
            },
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

// `PixelBytes` carries its own content-based `PartialEq`/`Hash` (ignoring the
// storage id), so deriving here gives `CpuRasterImage` the same "equal iff
// same width/height/format/content" semantics it had before storage ids
// existed.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CpuRasterImage {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub pixels: PixelBytes,
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

/// Bytes needed to store one pixel of `format`. Used by cache byte-accounting
/// to bound the VRAM/RAM a cached `RasterImage` pins.
pub(crate) fn pixel_stride(format: PixelFormat) -> usize {
    match format {
        PixelFormat::Rgba8 => 4,
        PixelFormat::Rgba16Float => 8,
    }
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

/// A solid-color raster component that fills its assigned layout area.
///
/// `Background` has no intrinsic size of its own. Under finite loose
/// constraints it takes the full available size; under unbounded constraints it
/// collapses to the minimum allowed size. During rendering it fills every pixel
/// in the requested target resolution with `color`.
#[crate::component(raster)]
#[derive(Keyable)]
pub struct Background {
    pub color: Color,
}

impl Background {
    pub const fn new(color: Color) -> Self {
        Self { color }
    }
}

impl RasterComponent for Background {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        let size = Vec2(
            finite_or_min(constraints.max.0, constraints.min.0),
            finite_or_min(constraints.max.1, constraints.min.1),
        );
        constraints.constrain(size)
    }

    fn render(&self, _size: Vec2, target: Resolution, ctx: &mut dyn RenderContext) -> RasterImage {
        if ctx.prefers_gpu() {
            if let Some(gpu) = ctx.gpu_backend() {
                if let Some(image) = gpu.solid_fill(target, self.color) {
                    return image;
                }
            }
        }

        let pixels = (target.width as usize) * (target.height as usize);
        let mut buf = Vec::with_capacity(pixels * 4);
        let [r, g, b, a] = color_rgba8(self.color);
        for _ in 0..pixels {
            buf.push(r);
            buf.push(g);
            buf.push(b);
            buf.push(a);
        }
        RasterImage::cpu(target.width, target.height, PixelFormat::Rgba8, buf)
    }
}

fn finite_or_min(max: f32, min: f32) -> f32 {
    if max.is_finite() {
        max
    } else {
        min
    }
}

fn color_rgba8(color: Color) -> [u8; 4] {
    [
        (color.r * 255.0).round().clamp(0.0, 255.0) as u8,
        (color.g * 255.0).round().clamp(0.0, 255.0) as u8,
        (color.b * 255.0).round().clamp(0.0, 255.0) as u8,
        (color.a * 255.0).round().clamp(0.0, 255.0) as u8,
    ]
}

/// A [`RasterComponent`] that fades its child to `opacity` of full alpha.
///
/// Raster counterpart of [`Transformed::opacity`](crate::vector::Transformed::opacity):
/// `layout` and `paint_bounds` forward to the child unchanged, since fading
/// never changes layout or the painted extent. `render` renders the child
/// through `ctx` (so the child keeps its own cache slot), and — only when
/// `opacity < 1.0` — reads the result back to CPU and scales every pixel's
/// straight alpha channel. RGB channels are left untouched, since
/// [`PixelFormat::Rgba8`] is non-premultiplied; the rounding matches the
/// fixed-point scheme used throughout this crate's compositing
/// (`(a * opacity_u16 + 127) / 255`).
///
/// `opacity` is clamped to `[0, 1]` at render time (out-of-range values and
/// `NaN` behave as their clamped equivalent); the field itself is stored as
/// authored so cache keys stay bit-exact with what was requested.
///
/// TODO(gpu-opacity): this always reads the child back to CPU once
/// `opacity < 1.0`. A `GpuRasterBackend` hook for a shader-side alpha
/// multiply (mirroring `composite` / `solid_fill`) would let a GPU-resident
/// child stay on the GPU; add a `ctx.gpu_backend()` branch here, the same way
/// [`Background::render`] does, once that hook exists.
#[crate::component(raster)]
#[derive(Keyable)]
pub struct Opacity {
    #[builder(default = 1.0)]
    pub opacity: f32,
    #[effect]
    #[builder(into)]
    pub child: Box<dyn RasterComponent>,
}

impl RasterComponent for Opacity {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        self.child.layout(constraints)
    }

    fn paint_bounds(&self, size: Vec2) -> Rect {
        self.child.paint_bounds(size)
    }

    fn render(&self, size: Vec2, target: Resolution, ctx: &mut dyn RenderContext) -> RasterImage {
        let rendered = ctx.render(self.child.as_ref(), size, target);
        let alpha = clamp_unit(self.opacity);
        if alpha >= 1.0 {
            // Full opacity is a no-op: skip the readback entirely so a
            // GPU-resident child image stays on the GPU untouched.
            return rendered;
        }

        let mut image = ctx.readback(rendered);
        assert_eq!(
            image.format,
            PixelFormat::Rgba8,
            "Opacity only supports straight-alpha Rgba8 images",
        );

        let alpha_u16 = (alpha * 255.0).round() as u16;
        let mut pixels = image.pixels.to_vec();
        for pixel in pixels.chunks_exact_mut(4) {
            pixel[3] = ((pixel[3] as u16 * alpha_u16 + 127) / 255) as u8;
        }
        image.pixels = pixels.into();
        RasterImage::Cpu(image)
    }
}

/// Extension trait adding opacity wrapping to raster components, mirroring
/// [`VectorTransform`](crate::vector::VectorTransform) on the vector side.
pub trait RasterTransform: RasterComponent + Sized + 'static {
    fn opacity(self, opacity: f32) -> Opacity {
        Opacity {
            opacity,
            child: Box::new(self),
        }
    }
}

impl<T: RasterComponent + 'static> RasterTransform for T {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render_context::PassThrough;

    #[test]
    fn background_layout_fills_finite_constraints() {
        let bg = Background::new(Color::rgb_u8(1, 2, 3));

        assert_eq!(
            bg.layout(Constraints::loose(Vec2(640.0, 360.0))),
            Vec2(640.0, 360.0)
        );
        assert_eq!(
            bg.layout(Constraints::tight(Vec2(320.0, 180.0))),
            Vec2(320.0, 180.0)
        );
        assert_eq!(bg.layout(Constraints::UNBOUNDED), Vec2::ZERO);
    }

    #[test]
    fn background_fills_every_target_pixel() {
        let bg = Background::new(Color::rgba_u8(10, 20, 30, 128));
        let mut ctx = PassThrough;
        let image = bg.render(Vec2(3.0, 2.0), Resolution::new(3, 2), &mut ctx);
        let cpu = image.as_cpu().expect("background renders CPU image");

        assert_eq!(cpu.width, 3);
        assert_eq!(cpu.height, 2);
        assert_eq!(cpu.format, PixelFormat::Rgba8);
        for pixel in cpu.pixels.chunks_exact(4) {
            assert_eq!(pixel, [10, 20, 30, 128]);
        }
    }

    #[test]
    fn complete_background_builder_boxes_as_raster_component() {
        let _boxed: Box<dyn RasterComponent> =
            Background::builder().color(Color::rgb_u8(1, 2, 3)).into();
    }

    #[derive(PartialEq, Hash)]
    struct SolidAlpha {
        rgba: [u8; 4],
    }

    impl RasterComponent for SolidAlpha {
        fn layout(&self, constraints: Constraints) -> Vec2 {
            constraints.constrain(Vec2(2.0, 2.0))
        }

        fn render(
            &self,
            _size: Vec2,
            target: Resolution,
            _ctx: &mut dyn RenderContext,
        ) -> RasterImage {
            let pixels = (target.width as usize) * (target.height as usize);
            let mut buf = Vec::with_capacity(pixels * 4);
            for _ in 0..pixels {
                buf.extend_from_slice(&self.rgba);
            }
            RasterImage::cpu(target.width, target.height, PixelFormat::Rgba8, buf)
        }
    }

    #[test]
    fn opacity_scales_alpha_channel_only() {
        let solid = SolidAlpha {
            rgba: [200, 100, 50, 255],
        };
        let faded = solid.opacity(0.5);
        let mut ctx = PassThrough;
        let image = faded
            .render(Vec2(2.0, 2.0), Resolution::new(2, 2), &mut ctx)
            .into_cpu()
            .expect("opacity renders on CPU");

        for pixel in image.pixels.chunks_exact(4) {
            assert_eq!(
                &pixel[..3],
                &[200, 100, 50],
                "RGB must stay untouched under straight (non-premultiplied) alpha"
            );
            assert_eq!(
                pixel[3], 128,
                "alpha scaled by 0.5 with (a * alpha_u16 + 127) / 255 rounding"
            );
        }
    }

    #[test]
    fn opacity_clamps_out_of_range_and_nan() {
        let solid = SolidAlpha {
            rgba: [1, 2, 3, 200],
        };
        let mut ctx = PassThrough;

        let over = solid
            .opacity(2.0)
            .render(Vec2(2.0, 2.0), Resolution::new(2, 2), &mut ctx);
        assert_eq!(
            over.as_cpu().unwrap().pixels[3],
            200,
            "opacity > 1 clamps to 1 (no-op)"
        );

        let solid = SolidAlpha {
            rgba: [1, 2, 3, 200],
        };
        let nan = solid
            .opacity(f32::NAN)
            .render(Vec2(2.0, 2.0), Resolution::new(2, 2), &mut ctx);
        assert_eq!(
            nan.as_cpu().unwrap().pixels[3],
            0,
            "NaN opacity clamps to 0"
        );
    }

    /// A `RenderContext` stub that always answers `render` with a GPU-tagged
    /// image, regardless of the requested component, and panics if
    /// `readback` is ever called. Used to prove that `Opacity::render` skips
    /// the readback path entirely at full opacity.
    struct GpuOnlyContext {
        readback_calls: usize,
    }

    impl RenderContext for GpuOnlyContext {
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }

        fn render(
            &mut self,
            _component: &dyn RasterComponent,
            _size: Vec2,
            target: Resolution,
        ) -> RasterImage {
            RasterImage::Gpu(GpuSurface::new(
                target.width,
                target.height,
                PixelFormat::Rgba8,
                "test",
                std::sync::Arc::new(()),
            ))
        }

        fn readback(&mut self, image: RasterImage) -> CpuRasterImage {
            self.readback_calls += 1;
            match image {
                RasterImage::Cpu(image) => image,
                RasterImage::Gpu(_) => panic!("test stub cannot read back GPU images"),
            }
        }
    }

    #[test]
    fn opacity_one_skips_readback_and_leaves_gpu_image_untouched() {
        let mut ctx = GpuOnlyContext { readback_calls: 0 };
        let faded = SolidAlpha { rgba: [0, 0, 0, 0] }.opacity(1.0);

        let image = faded.render(Vec2(1.0, 1.0), Resolution::new(1, 1), &mut ctx);

        assert!(matches!(image, RasterImage::Gpu(_)));
        assert_eq!(ctx.readback_calls, 0);
    }

    #[test]
    fn raster_transform_opacity_sets_the_field() {
        let faded = Background::new(Color::rgb_u8(4, 5, 6)).opacity(0.75);
        assert_eq!(faded.opacity, 0.75);
    }

    #[test]
    fn raster_builder_transform_opacity_sets_the_field() {
        use crate::builder::RasterBuilderTransform;

        let faded = Background::builder()
            .color(Color::rgb_u8(1, 2, 3))
            .opacity(0.3);
        assert_eq!(faded.opacity, 0.3);
    }

    #[test]
    fn storage_id_is_unique_per_allocation_even_with_identical_content() {
        let a = CpuRasterImage::new(1, 1, PixelFormat::Rgba8, vec![1, 2, 3, 4]);
        let b = CpuRasterImage::new(1, 1, PixelFormat::Rgba8, vec![1, 2, 3, 4]);

        // Two independently constructed images never share an id, even when
        // their content (and, potentially, their address after `a`'s buffer
        // frees and the allocator reuses the spot for `b`) is identical.
        // This is what makes a `PixelStorageId`-keyed cache immune to the ABA
        // collision a raw `pixels.as_ptr()` key was vulnerable to.
        assert_ne!(a.storage_id(), b.storage_id());

        // But equality/hash semantics are unchanged: content-identical images
        // still compare equal, exactly as before `storage_id` existed.
        assert_eq!(a, b);
    }

    #[test]
    fn storage_id_is_shared_by_clone() {
        let a = CpuRasterImage::new(2, 1, PixelFormat::Rgba8, vec![9, 9, 9, 9, 9, 9, 9, 9]);
        let cloned = a.clone();

        assert_eq!(a.storage_id(), cloned.storage_id());
    }

    #[test]
    fn reassigning_pixels_after_clone_mints_a_fresh_id() {
        // Mirrors the `Opacity`/`SafeZoneOverlay` pattern: read back a cached
        // image, then locally patch its pixels in place —
        // `image.pixels = pixels.into()` — and hand the patched copy on
        // while the original (e.g. still held by a render cache) is
        // untouched. If the id lived on `CpuRasterImage` as a sibling field
        // instead of inside `PixelBytes`, this reassignment would silently
        // leave the *old* id attached to the *new* content, recreating the
        // exact same-id-different-content hazard the storage id exists to
        // prevent.
        let cached = CpuRasterImage::new(1, 1, PixelFormat::Rgba8, vec![1, 2, 3, 4]);
        let mut local = cached.clone();
        assert_eq!(cached.storage_id(), local.storage_id());

        let mut pixels = local.pixels.to_vec();
        pixels[3] = 128;
        local.pixels = pixels.into();

        assert_ne!(
            cached.storage_id(),
            local.storage_id(),
            "in-place pixel patching must mint a new id, not keep the stale one"
        );
        assert_ne!(cached, local);
    }

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
        assert_eq!(image.pixels.as_ref(), &[255, 0, 0, 255, 0, 0, 255, 128]);
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
        assert_eq!(component.layout(Constraints::UNBOUNDED), Vec2(2.0, 1.0));

        let rendered = component.render(Vec2(2.0, 1.0), Resolution::new(2, 1), &mut PassThrough);
        let cpu = rendered.into_cpu().expect("still image renders to CPU");
        assert_eq!(cpu.pixels.as_ref(), sample_image().pixels.as_ref());
    }
}

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
            pixels: PixelBytes::from(pixels.into()),
        }
    }

    /// Identity of this image's pixel-storage allocation. Stable across
    /// `clone()`, and across reassigning `self.pixels` from any of the same
    /// `Bytes`/`Vec<u8>` conversion routes used to construct `Self` (every
    /// route through [`PixelBytes`]'s `From` impls mints a fresh id, so this
    /// changes whenever the storage genuinely does); distinct from every
    /// other allocation, including ones the allocator later places at the
    /// same address.
    pub fn storage_id(&self) -> PixelStorageId {
        self.pixels.id()
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
