use std::borrow::Cow;
use std::num::NonZeroUsize;
use std::sync::Arc;

use lru::LruCache;
use tellur_core::cache_budget::{
    configured_vram_bytes, try_reserve_vram, vram_used_bytes, BudgetReservation,
};
use tellur_core::color::Color;
use tellur_core::geometry::{Transform, Vec2};
use tellur_core::raster::{CpuRasterImage, GpuSurface, PixelFormat, RasterImage, Resolution};
use tellur_core::render_context::{
    CompositeInput, DropShadowInput, GpuRasterBackend, OutlineInput,
};
use tellur_core::vector::{
    ClipGroup as TellurClipGroup, Node, Paint, Path as TellurPath, PathCommand, VectorGraphic,
};
use vello::kurbo::{Affine, BezPath, Rect as VelloRect, Stroke as VelloStroke};
use wgpu::util::DeviceExt;

const BACKEND: &str = "tellur-wgpu-buffer-v1";
const WORKGROUP: u32 = 16;
const CPU_UPLOAD_CACHE_ENTRIES: usize = 64;
const CPU_UPLOAD_CACHE_INITIAL_FRACTION_DIVISOR: usize = 8;
const CPU_UPLOAD_CACHE_MAX_FRACTION_DIVISOR: usize = 4;
const GPU_CACHE_SHRINK_NUMERATOR: usize = 3;
const GPU_CACHE_SHRINK_DENOMINATOR: usize = 4;
const GPU_CACHE_GROW_SUCCESS_STREAK: u8 = 64;
const GPU_CACHE_GROW_FRACTION_DIVISOR: usize = 64;
const MIB: usize = 1024 * 1024;

pub struct GpuRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    composite_pipeline: wgpu::ComputePipeline,
    copy_alpha_pipeline: wgpu::ComputePipeline,
    blur_pipeline: wgpu::ComputePipeline,
    shadow_pipeline: wgpu::ComputePipeline,
    outline_pipeline: wgpu::ComputePipeline,
    texture_to_buffer_pipeline: wgpu::ComputePipeline,
    fill_pipeline: wgpu::ComputePipeline,
    motion_accum_pipeline: wgpu::ComputePipeline,
    motion_resolve_pipeline: wgpu::ComputePipeline,
    vello_renderer: Option<vello::Renderer>,
    stats: GpuRenderStats,
    // Per-resolution scratch reused across frames instead of reallocated each
    // call: the vello render-target texture and the readback staging buffer.
    // Both are fully overwritten on every use, so reuse is byte-identical; only
    // their size has to match (recreated when the resolution changes).
    vello_target: Option<(u32, u32, wgpu::Texture, BudgetReservation)>,
    readback_staging: Option<(wgpu::Buffer, BudgetReservation)>,
    cpu_upload_cache: LruCache<CpuUploadCacheKey, Arc<GpuBufferImage>>,
    cpu_upload_cache_bytes: usize,
    cpu_upload_cache_cap_bytes: usize,
    cpu_upload_cache_max_bytes: usize,
    vram_spare_successes: u8,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct GpuRenderStats {
    pub composites: u64,
    pub drop_shadows: u64,
    pub outlines: u64,
    pub rasterizes: u64,
    pub fills: u64,
    pub temporal_averages: u64,
    pub readbacks: u64,
    pub vram_reserve_failures: u64,
    pub vram_cache_evictions: u64,
}

impl GpuRenderStats {
    pub fn total_ops(self) -> u64 {
        self.composites
            + self.drop_shadows
            + self.outlines
            + self.rasterizes
            + self.fills
            + self.temporal_averages
    }
}

struct GpuBufferImage {
    width: u32,
    height: u32,
    format: PixelFormat,
    known_opaque: bool,
    buffer: wgpu::Buffer,
    _reservation: BudgetReservation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct CpuUploadCacheKey {
    width: u32,
    height: u32,
    format: PixelFormat,
    ptr: usize,
    len: usize,
}

impl CpuUploadCacheKey {
    fn new(image: &CpuRasterImage) -> Self {
        Self {
            width: image.width,
            height: image.height,
            format: image.format,
            ptr: image.pixels.as_ptr() as usize,
            len: image.pixels.len(),
        }
    }
}

fn upload_cache_limits() -> (usize, usize) {
    let limit = configured_vram_bytes();
    let max = limit / CPU_UPLOAD_CACHE_MAX_FRACTION_DIVISOR;
    let cap = (limit / CPU_UPLOAD_CACHE_INITIAL_FRACTION_DIVISOR).min(max);
    (cap, max)
}

fn pixel_stride(format: PixelFormat) -> usize {
    match format {
        PixelFormat::Rgba8 => 4,
        PixelFormat::Rgba16Float => 8,
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CompositeParams {
    dst_w: u32,
    dst_h: u32,
    src_w: u32,
    src_h: u32,
    offset_x: i32,
    offset_y: i32,
    _pad0: u32,
    _pad1: u32,
}

unsafe impl bytemuck::Zeroable for CompositeParams {}
unsafe impl bytemuck::Pod for CompositeParams {}

#[repr(C)]
#[derive(Clone, Copy)]
struct CopyAlphaParams {
    src_w: u32,
    src_h: u32,
    out_w: u32,
    out_h: u32,
    offset_x: i32,
    offset_y: i32,
    _pad0: u32,
    _pad1: u32,
}

unsafe impl bytemuck::Zeroable for CopyAlphaParams {}
unsafe impl bytemuck::Pod for CopyAlphaParams {}

#[repr(C)]
#[derive(Clone, Copy)]
struct BlurParams {
    width: u32,
    height: u32,
    radius: u32,
    horizontal: u32,
}

unsafe impl bytemuck::Zeroable for BlurParams {}
unsafe impl bytemuck::Pod for BlurParams {}

#[repr(C)]
#[derive(Clone, Copy)]
struct ColorCompositeParams {
    dst_w: u32,
    dst_h: u32,
    src_w: u32,
    src_h: u32,
    offset_x: i32,
    offset_y: i32,
    r: u32,
    g: u32,
    b: u32,
    a: u32,
    radius_x: u32,
    radius_y: u32,
}

unsafe impl bytemuck::Zeroable for ColorCompositeParams {}
unsafe impl bytemuck::Pod for ColorCompositeParams {}

#[repr(C)]
#[derive(Clone, Copy)]
struct TextureToBufferParams {
    width: u32,
    height: u32,
    _pad0: u32,
    _pad1: u32,
}

unsafe impl bytemuck::Zeroable for TextureToBufferParams {}
unsafe impl bytemuck::Pod for TextureToBufferParams {}

#[repr(C)]
#[derive(Clone, Copy)]
struct FillParams {
    width: u32,
    height: u32,
    color: u32,
    _pad0: u32,
}

unsafe impl bytemuck::Zeroable for FillParams {}
unsafe impl bytemuck::Pod for FillParams {}

/// Shared by the motion accumulate and resolve passes; the accumulate pass
/// ignores `total`.
#[repr(C)]
#[derive(Clone, Copy)]
struct MotionPassParams {
    width: u32,
    height: u32,
    total: u32,
    _pad0: u32,
}

unsafe impl bytemuck::Zeroable for MotionPassParams {}
unsafe impl bytemuck::Pod for MotionPassParams {}

impl GpuRenderer {
    pub fn new() -> Result<Self, String> {
        pollster::block_on(Self::new_async())
    }

    async fn new_async() -> Result<Self, String> {
        let instance = wgpu::Instance::default();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .ok_or("no GPU adapter available")?;
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("tellur-gpu-device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                },
                None,
            )
            .await
            .map_err(|e| format!("failed to create GPU device: {e}"))?;
        let (cpu_upload_cache_cap_bytes, cpu_upload_cache_max_bytes) = upload_cache_limits();

        Ok(Self {
            composite_pipeline: compute_pipeline(
                &device,
                "tellur-composite",
                &format!("{COMMON_WGSL}{COMPOSITE_SHADER}"),
            ),
            copy_alpha_pipeline: compute_pipeline(&device, "tellur-copy-alpha", COPY_ALPHA_SHADER),
            blur_pipeline: compute_pipeline(&device, "tellur-box-blur", BLUR_SHADER),
            shadow_pipeline: compute_pipeline(
                &device,
                "tellur-shadow-composite",
                &format!("{COMMON_WGSL}{SHADOW_SHADER}"),
            ),
            outline_pipeline: compute_pipeline(
                &device,
                "tellur-outline-composite",
                &format!("{COMMON_WGSL}{OUTLINE_SHADER}"),
            ),
            texture_to_buffer_pipeline: compute_pipeline(
                &device,
                "tellur-texture-to-buffer",
                TEXTURE_TO_BUFFER_SHADER,
            ),
            fill_pipeline: compute_pipeline(&device, "tellur-solid-fill", FILL_SHADER),
            motion_accum_pipeline: compute_pipeline(
                &device,
                "tellur-motion-accum",
                &format!("{COMMON_WGSL}{MOTION_ACCUM_SHADER}"),
            ),
            motion_resolve_pipeline: compute_pipeline(
                &device,
                "tellur-motion-resolve",
                &format!("{COMMON_WGSL}{MOTION_RESOLVE_SHADER}"),
            ),
            device,
            queue,
            vello_renderer: None,
            stats: GpuRenderStats::default(),
            vello_target: None,
            readback_staging: None,
            cpu_upload_cache: LruCache::new(
                NonZeroUsize::new(CPU_UPLOAD_CACHE_ENTRIES)
                    .expect("CPU upload cache capacity must be non-zero"),
            ),
            cpu_upload_cache_bytes: 0,
            cpu_upload_cache_cap_bytes,
            cpu_upload_cache_max_bytes,
            vram_spare_successes: 0,
        })
    }

    pub fn stats(&self) -> GpuRenderStats {
        self.stats
    }

    fn reserve_render_vram(&mut self, bytes: usize) -> Option<BudgetReservation> {
        if let Some(reservation) = try_reserve_vram(bytes) {
            self.note_vram_reserve_success();
            return Some(reservation);
        }

        self.stats.vram_reserve_failures = self.stats.vram_reserve_failures.saturating_add(1);
        self.shrink_upload_cache_budget();
        if let Some(reservation) = try_reserve_vram(bytes) {
            self.note_vram_reserve_success();
            return Some(reservation);
        }

        self.cpu_upload_cache_cap_bytes = 0;
        self.evict_upload_cache_to_fit(0);
        let reservation = try_reserve_vram(bytes)?;
        self.note_vram_reserve_success();
        Some(reservation)
    }

    fn note_vram_reserve_success(&mut self) {
        let limit = configured_vram_bytes();
        let used = vram_used_bytes();
        if self.cpu_upload_cache_cap_bytes >= self.cpu_upload_cache_max_bytes
            || used.saturating_mul(4) >= limit.saturating_mul(3)
        {
            self.vram_spare_successes = 0;
            return;
        }

        self.vram_spare_successes = self.vram_spare_successes.saturating_add(1);
        if self.vram_spare_successes >= GPU_CACHE_GROW_SUCCESS_STREAK {
            let step = (limit / GPU_CACHE_GROW_FRACTION_DIVISOR).max(MIB);
            self.cpu_upload_cache_cap_bytes = self
                .cpu_upload_cache_cap_bytes
                .saturating_add(step)
                .min(self.cpu_upload_cache_max_bytes);
            self.vram_spare_successes = 0;
        }
    }

    fn shrink_upload_cache_budget(&mut self) {
        self.vram_spare_successes = 0;
        let old = self.cpu_upload_cache_cap_bytes;
        self.cpu_upload_cache_cap_bytes =
            old.saturating_mul(GPU_CACHE_SHRINK_NUMERATOR) / GPU_CACHE_SHRINK_DENOMINATOR;
        if old > 0 && self.cpu_upload_cache_cap_bytes == old {
            self.cpu_upload_cache_cap_bytes = old - 1;
        }
        self.evict_upload_cache_to_fit(0);
    }

    fn evict_upload_cache_to_fit(&mut self, needed: usize) {
        while self.cpu_upload_cache_bytes.saturating_add(needed) > self.cpu_upload_cache_cap_bytes {
            match self.cpu_upload_cache.pop_lru() {
                Some((_, image)) => {
                    self.cpu_upload_cache_bytes = self
                        .cpu_upload_cache_bytes
                        .saturating_sub(Self::buffer_image_bytes(&image));
                    self.stats.vram_cache_evictions =
                        self.stats.vram_cache_evictions.saturating_add(1);
                }
                None => break,
            }
        }
    }

    fn insert_upload_cache(&mut self, key: CpuUploadCacheKey, image: Arc<GpuBufferImage>) {
        let bytes = Self::buffer_image_bytes(&image);
        if bytes > self.cpu_upload_cache_cap_bytes {
            return;
        }
        self.evict_upload_cache_to_fit(bytes);
        if self.cpu_upload_cache_bytes.saturating_add(bytes) > self.cpu_upload_cache_cap_bytes {
            return;
        }
        if let Some(old) = self.cpu_upload_cache.put(key, image) {
            self.cpu_upload_cache_bytes = self
                .cpu_upload_cache_bytes
                .saturating_sub(Self::buffer_image_bytes(&old));
        }
        self.cpu_upload_cache_bytes = self.cpu_upload_cache_bytes.saturating_add(bytes);
    }

    fn buffer_image_bytes(image: &GpuBufferImage) -> usize {
        (image.width as usize)
            .saturating_mul(image.height as usize)
            .saturating_mul(pixel_stride(image.format))
    }

    fn upload(&mut self, image: &CpuRasterImage) -> Option<Arc<GpuBufferImage>> {
        if image.format != PixelFormat::Rgba8 {
            return None;
        }
        let key = CpuUploadCacheKey::new(image);
        if let Some(cached) = self.cpu_upload_cache.get(&key) {
            return Some(Arc::clone(cached));
        }
        let reservation = self.reserve_render_vram(image.pixels.len())?;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tellur-gpu-upload"),
            size: image.pixels.len() as u64,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        self.queue.write_buffer(&buffer, 0, &image.pixels);
        let uploaded = Arc::new(GpuBufferImage {
            width: image.width,
            height: image.height,
            format: image.format,
            known_opaque: false,
            buffer,
            _reservation: reservation,
        });
        self.insert_upload_cache(key, Arc::clone(&uploaded));
        Some(uploaded)
    }

    fn image_ref(&mut self, image: &RasterImage) -> Option<Arc<GpuBufferImage>> {
        match image {
            RasterImage::Cpu(image) => self.upload(image),
            RasterImage::Gpu(surface) if surface.backend() == BACKEND => {
                Arc::downcast::<GpuBufferImage>(surface.handle_arc()).ok()
            }
            RasterImage::Gpu(_) => None,
        }
    }

    fn raster_image(&self, image: Arc<GpuBufferImage>) -> RasterImage {
        RasterImage::Gpu(GpuSurface::new(
            image.width,
            image.height,
            image.format,
            BACKEND,
            image,
        ))
    }

    /// Allocates a target-sized transparent storage buffer.
    ///
    /// wgpu (per the WebGPU spec) zero-initializes a freshly created buffer
    /// before its first use, so the blend paths (composite / shadow / outline)
    /// that read-modify-write the destination still start from transparent
    /// without any explicit fill. This deliberately does **not** issue the
    /// `vec![0u8; len]` + whole-buffer `write_buffer` it used to: that per-frame
    /// ~8 MiB CPU alloc + memset + CPU→GPU upload was the dominant CPU cost of
    /// the GPU render path, and it was entirely redundant (the blend paths get
    /// transparency from zero-init; the full-overwrite `texture_to_buffer` copy
    /// never depended on the contents at all).
    fn empty_image(&mut self, resolution: Resolution) -> Option<Arc<GpuBufferImage>> {
        self.empty_image_with_opacity(resolution, false)
    }

    fn empty_image_with_opacity(
        &mut self,
        resolution: Resolution,
        known_opaque: bool,
    ) -> Option<Arc<GpuBufferImage>> {
        let len = (resolution.width as usize) * (resolution.height as usize) * 4;
        let reservation = self.reserve_render_vram(len)?;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tellur-gpu-target"),
            size: len as u64,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        Some(Arc::new(GpuBufferImage {
            width: resolution.width,
            height: resolution.height,
            format: PixelFormat::Rgba8,
            known_opaque,
            buffer,
            _reservation: reservation,
        }))
    }

    fn alpha_image(&mut self, width: u32, height: u32) -> Option<GpuBufferImage> {
        let len = (width as usize) * (height as usize) * 4;
        let reservation = self.reserve_render_vram(len)?;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tellur-gpu-alpha"),
            size: len as u64,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        Some(GpuBufferImage {
            width,
            height,
            format: PixelFormat::Rgba8,
            known_opaque: false,
            buffer,
            _reservation: reservation,
        })
    }

    fn composite_one(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        dst: &GpuBufferImage,
        src: &GpuBufferImage,
        offset_x: i32,
        offset_y: i32,
    ) {
        let params = CompositeParams {
            dst_w: dst.width,
            dst_h: dst.height,
            src_w: src.width,
            src_h: src.height,
            offset_x,
            offset_y,
            _pad0: 0,
            _pad1: 0,
        };
        dispatch_three_buffer(
            &self.device,
            encoder,
            &self.composite_pipeline,
            [&dst.buffer, &src.buffer],
            &params,
            DispatchSize::new(src.width, src.height),
        );
    }

    fn copy_alpha(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        src: &GpuBufferImage,
        alpha: &GpuBufferImage,
        offset_x: i32,
        offset_y: i32,
    ) {
        let params = CopyAlphaParams {
            src_w: src.width,
            src_h: src.height,
            out_w: alpha.width,
            out_h: alpha.height,
            offset_x,
            offset_y,
            _pad0: 0,
            _pad1: 0,
        };
        dispatch_three_buffer(
            &self.device,
            encoder,
            &self.copy_alpha_pipeline,
            [&src.buffer, &alpha.buffer],
            &params,
            DispatchSize::new(alpha.width, alpha.height),
        );
    }

    fn blur_alpha(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        a: &GpuBufferImage,
        b: &GpuBufferImage,
        radius: u32,
    ) {
        if radius == 0 {
            return;
        }
        for _ in 0..3 {
            self.blur_pass(encoder, a, b, radius, true);
            self.blur_pass(encoder, b, a, radius, false);
        }
    }

    fn blur_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        src: &GpuBufferImage,
        dst: &GpuBufferImage,
        radius: u32,
        horizontal: bool,
    ) {
        let params = BlurParams {
            width: src.width,
            height: src.height,
            radius,
            horizontal: u32::from(horizontal),
        };
        dispatch_three_buffer(
            &self.device,
            encoder,
            &self.blur_pipeline,
            [&src.buffer, &dst.buffer],
            &params,
            DispatchSize::new(src.width, src.height),
        );
    }

    fn composite_shadow_alpha(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        dst: &GpuBufferImage,
        alpha: &GpuBufferImage,
        offset_x: i32,
        offset_y: i32,
        color: Color,
    ) {
        let [r, g, b, a] = color_u8(color);
        let params = ColorCompositeParams {
            dst_w: dst.width,
            dst_h: dst.height,
            src_w: alpha.width,
            src_h: alpha.height,
            offset_x,
            offset_y,
            r,
            g,
            b,
            a,
            radius_x: 0,
            radius_y: 0,
        };
        dispatch_three_buffer(
            &self.device,
            encoder,
            &self.shadow_pipeline,
            [&dst.buffer, &alpha.buffer],
            &params,
            DispatchSize::new(alpha.width, alpha.height),
        );
    }

    fn composite_outline_alpha(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        dst: &GpuBufferImage,
        alpha: &GpuBufferImage,
        offset: (i32, i32),
        radius: (u32, u32),
        color: Color,
    ) {
        let [r, g, b, a] = color_u8(color);
        let params = ColorCompositeParams {
            dst_w: dst.width,
            dst_h: dst.height,
            src_w: alpha.width,
            src_h: alpha.height,
            offset_x: offset.0,
            offset_y: offset.1,
            r,
            g,
            b,
            a,
            radius_x: radius.0,
            radius_y: radius.1,
        };
        dispatch_three_buffer(
            &self.device,
            encoder,
            &self.outline_pipeline,
            [&dst.buffer, &alpha.buffer],
            &params,
            DispatchSize::new(alpha.width, alpha.height),
        );
    }

    fn render_vello_graphic(
        &mut self,
        graphic: &VectorGraphic,
        target: Resolution,
    ) -> Option<Arc<GpuBufferImage>> {
        let scene = build_vello_scene(graphic, target)?;
        // Reuse a persisted vello render target; the resolution is stable across
        // frames, so the texture is allocated once. vello overwrites the whole
        // target each call (base_color TRANSPARENT), so reuse is byte-identical.
        if self.vello_target.as_ref().map(|(w, h, _, _)| (*w, *h))
            != Some((target.width, target.height))
        {
            let texture_bytes = (target.width as usize) * (target.height as usize) * 4;
            let reservation = self.reserve_render_vram(texture_bytes)?;
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("tellur-vello-target"),
                size: wgpu::Extent3d {
                    width: target.width,
                    height: target.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            self.vello_target = Some((target.width, target.height, texture, reservation));
        }
        let view = self
            .vello_target
            .as_ref()?
            .2
            .create_view(&wgpu::TextureViewDescriptor::default());
        if self.vello_renderer.is_none() {
            self.vello_renderer = Some(create_vello_renderer(&self.device)?);
        }
        self.vello_renderer
            .as_mut()?
            .render_to_texture(
                &self.device,
                &self.queue,
                &scene,
                &view,
                &vello::RenderParams {
                    base_color: vello::peniko::Color::TRANSPARENT,
                    width: target.width,
                    height: target.height,
                    antialiasing_method: vello::AaConfig::Area,
                },
            )
            .ok()?;

        let target_image = self.empty_image(target)?;
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("tellur-vello-copy-to-buffer"),
            });
        self.texture_to_buffer(&mut encoder, &view, &target_image);
        self.queue.submit(Some(encoder.finish()));
        Some(target_image)
    }

    fn texture_to_buffer(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        texture: &wgpu::TextureView,
        dst: &GpuBufferImage,
    ) {
        let params = TextureToBufferParams {
            width: dst.width,
            height: dst.height,
            _pad0: 0,
            _pad1: 0,
        };
        let params_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("tellur-texture-to-buffer-params"),
                contents: bytemuck::bytes_of(&params),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let layout = self.texture_to_buffer_pipeline.get_bind_group_layout(0);
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tellur-texture-to-buffer-bind-group"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(texture),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: dst.buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: params_buffer.as_entire_binding(),
                },
            ],
        });

        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("tellur-texture-to-buffer-pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.texture_to_buffer_pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(
            div_ceil(dst.width, WORKGROUP),
            div_ceil(dst.height, WORKGROUP),
            1,
        );
    }

    fn filled_image(&mut self, target: Resolution, packed: u32) -> Option<Arc<GpuBufferImage>> {
        let len = (target.width as usize) * (target.height as usize) * 4;
        let reservation = self.reserve_render_vram(len)?;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tellur-gpu-fill"),
            size: len as u64,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let image = Arc::new(GpuBufferImage {
            width: target.width,
            height: target.height,
            format: PixelFormat::Rgba8,
            known_opaque: (packed >> 24) == 255,
            buffer,
            _reservation: reservation,
        });

        let params = FillParams {
            width: image.width,
            height: image.height,
            color: packed,
            _pad0: 0,
        };
        let params_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("tellur-gpu-fill-params"),
                contents: bytemuck::bytes_of(&params),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let layout = self.fill_pipeline.get_bind_group_layout(0);
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tellur-gpu-fill-bind-group"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: image.buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: params_buffer.as_entire_binding(),
                },
            ],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("tellur-gpu-fill"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("tellur-gpu-fill-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.fill_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(
                div_ceil(image.width, WORKGROUP),
                div_ceil(image.height, WORKGROUP),
                1,
            );
        }
        self.queue.submit(Some(encoder.finish()));
        Some(image)
    }
}

impl GpuRasterBackend for GpuRenderer {
    fn composite(
        &mut self,
        target: Resolution,
        inputs: &[CompositeInput<'_>],
    ) -> Option<RasterImage> {
        let mut sources = Vec::with_capacity(inputs.len());
        for input in inputs {
            let src = self.image_ref(input.image)?;
            if src.format != PixelFormat::Rgba8 {
                return None;
            }
            sources.push((src, input.offset_x, input.offset_y));
        }

        let fills_target = |src: &GpuBufferImage, offset_x: i32, offset_y: i32| {
            src.known_opaque
                && offset_x == 0
                && offset_y == 0
                && src.width == target.width
                && src.height == target.height
        };
        let first_fills_target = sources
            .first()
            .is_some_and(|(src, offset_x, offset_y)| fills_target(src, *offset_x, *offset_y));
        let known_opaque = sources
            .iter()
            .any(|(src, offset_x, offset_y)| fills_target(src, *offset_x, *offset_y));

        let target_image = self.empty_image_with_opacity(target, known_opaque)?;
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("tellur-gpu-composite"),
            });

        let start = if first_fills_target {
            let (src, _, _) = &sources[0];
            let len = (target.width as u64) * (target.height as u64) * 4;
            encoder.copy_buffer_to_buffer(&src.buffer, 0, &target_image.buffer, 0, len);
            1
        } else {
            0
        };

        for (src, offset_x, offset_y) in sources.iter().skip(start) {
            self.composite_one(&mut encoder, &target_image, src, *offset_x, *offset_y);
        }

        self.queue.submit(Some(encoder.finish()));
        self.stats.composites = self.stats.composites.saturating_add(1);
        Some(self.raster_image(target_image))
    }

    fn drop_shadow(&mut self, input: DropShadowInput<'_>) -> Option<RasterImage> {
        let child = self.image_ref(input.child)?;
        if child.format != PixelFormat::Rgba8 {
            return None;
        }
        let pad = input.blur_radius.saturating_mul(3);
        let shadow_w = child.width.checked_add(pad.checked_mul(2)?)?;
        let shadow_h = child.height.checked_add(pad.checked_mul(2)?)?;
        let alpha_a = self.alpha_image(shadow_w, shadow_h)?;
        let alpha_b = self.alpha_image(shadow_w, shadow_h)?;
        let target = self.empty_image(input.target)?;

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("tellur-gpu-drop-shadow"),
            });
        self.copy_alpha(&mut encoder, &child, &alpha_a, pad as i32, pad as i32);
        self.blur_alpha(&mut encoder, &alpha_a, &alpha_b, input.blur_radius);
        self.composite_shadow_alpha(
            &mut encoder,
            &target,
            &alpha_a,
            input.shadow_offset_x,
            input.shadow_offset_y,
            input.color,
        );
        self.composite_one(
            &mut encoder,
            &target,
            &child,
            input.child_offset_x,
            input.child_offset_y,
        );

        self.queue.submit(Some(encoder.finish()));
        self.stats.drop_shadows = self.stats.drop_shadows.saturating_add(1);
        Some(self.raster_image(target))
    }

    fn outline(&mut self, input: OutlineInput<'_>) -> Option<RasterImage> {
        let child = self.image_ref(input.child)?;
        if child.format != PixelFormat::Rgba8 {
            return None;
        }
        let alpha = self.alpha_image(input.target.width, input.target.height)?;
        let target = self.empty_image(input.target)?;

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("tellur-gpu-outline"),
            });
        self.copy_alpha(
            &mut encoder,
            &child,
            &alpha,
            input.child_offset_x,
            input.child_offset_y,
        );
        self.composite_outline_alpha(
            &mut encoder,
            &target,
            &alpha,
            (0, 0),
            (input.radius_x, input.radius_y),
            input.color,
        );
        self.composite_one(
            &mut encoder,
            &target,
            &child,
            input.child_offset_x,
            input.child_offset_y,
        );

        self.queue.submit(Some(encoder.finish()));
        self.stats.outlines = self.stats.outlines.saturating_add(1);
        Some(self.raster_image(target))
    }

    fn rasterize(&mut self, graphic: &VectorGraphic, target: Resolution) -> Option<RasterImage> {
        let target_image = self.render_vello_graphic(graphic, target)?;
        self.stats.rasterizes = self.stats.rasterizes.saturating_add(1);
        Some(self.raster_image(target_image))
    }

    fn solid_fill(&mut self, target: Resolution, color: Color) -> Option<RasterImage> {
        let [r, g, b, a] = color_u8(color);
        let packed = r | (g << 8) | (b << 16) | (a << 24);
        let image = self.filled_image(target, packed)?;
        self.stats.fills = self.stats.fills.saturating_add(1);
        Some(self.raster_image(image))
    }

    fn temporal_average(
        &mut self,
        target: Resolution,
        frames: &[&RasterImage],
        total: u32,
    ) -> Option<RasterImage> {
        if total == 0 || frames.is_empty() || frames.len() as u32 > total {
            return None;
        }
        // Resolve every source up front so an unsupported frame bails before
        // any GPU work is encoded.
        let mut sources = Vec::with_capacity(frames.len());
        for frame in frames {
            if frame.width() != target.width || frame.height() != target.height {
                return None;
            }
            let src = self.image_ref(frame)?;
            if src.format != PixelFormat::Rgba8 {
                return None;
            }
            sources.push(src);
        }

        // Premultiplied u32 channel sums, 4 words per pixel. A fresh wgpu
        // buffer is zero-initialized, so accumulation starts from zero
        // without an explicit clear pass.
        let acc_len = (target.width as u64) * (target.height as u64) * 16;
        let _acc_reservation = self.reserve_render_vram(acc_len as usize)?;
        let acc = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tellur-gpu-motion-acc"),
            size: acc_len,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let out = self.empty_image(target)?;

        let params = MotionPassParams {
            width: target.width,
            height: target.height,
            total,
            _pad0: 0,
        };
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("tellur-gpu-temporal-average"),
            });
        for src in &sources {
            dispatch_three_buffer(
                &self.device,
                &mut encoder,
                &self.motion_accum_pipeline,
                [&acc, &src.buffer],
                &params,
                DispatchSize::new(target.width, target.height),
            );
        }
        dispatch_three_buffer(
            &self.device,
            &mut encoder,
            &self.motion_resolve_pipeline,
            [&acc, &out.buffer],
            &params,
            DispatchSize::new(target.width, target.height),
        );
        self.queue.submit(Some(encoder.finish()));
        self.stats.temporal_averages = self.stats.temporal_averages.saturating_add(1);
        Some(self.raster_image(out))
    }

    fn readback(&mut self, image: RasterImage) -> Option<CpuRasterImage> {
        match image {
            RasterImage::Cpu(image) => Some(image),
            RasterImage::Gpu(surface) if surface.backend() == BACKEND => {
                let image = Arc::downcast::<GpuBufferImage>(surface.handle_arc()).ok()?;
                let byte_len = (image.width as usize) * (image.height as usize) * 4;
                // Reuse a persisted MAP_READ staging buffer across frames (the
                // resolution is stable), so this allocates once instead of every
                // readback. The copy below fully overwrites it each time.
                if self
                    .readback_staging
                    .as_ref()
                    .map(|(buffer, _)| buffer.size())
                    != Some(byte_len as u64)
                {
                    let reservation = self.reserve_render_vram(byte_len)?;
                    self.readback_staging = Some((
                        self.device.create_buffer(&wgpu::BufferDescriptor {
                            label: Some("tellur-gpu-readback"),
                            size: byte_len as u64,
                            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                            mapped_at_creation: false,
                        }),
                        reservation,
                    ));
                }
                let staging = &self.readback_staging.as_ref()?.0;
                let mut encoder =
                    self.device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("tellur-gpu-readback"),
                        });
                encoder.copy_buffer_to_buffer(&image.buffer, 0, staging, 0, byte_len as u64);
                self.queue.submit(Some(encoder.finish()));

                let slice = staging.slice(..);
                let (tx, rx) = std::sync::mpsc::channel();
                slice.map_async(wgpu::MapMode::Read, move |result| {
                    let _ = tx.send(result);
                });
                self.device.poll(wgpu::Maintain::Wait);
                rx.recv().ok()?.ok()?;

                let data = {
                    let mapped = slice.get_mapped_range();
                    mapped.to_vec()
                };
                staging.unmap();
                self.stats.readbacks = self.stats.readbacks.saturating_add(1);
                Some(CpuRasterImage::new(
                    image.width,
                    image.height,
                    image.format,
                    data,
                ))
            }
            RasterImage::Gpu(_) => None,
        }
    }
}

fn compute_pipeline(
    device: &wgpu::Device,
    label: &'static str,
    source: &str,
) -> wgpu::ComputePipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(Cow::Owned(source.to_owned())),
    });
    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(label),
        layout: None,
        module: &shader,
        entry_point: "main",
        compilation_options: Default::default(),
    })
}

fn create_vello_renderer(device: &wgpu::Device) -> Option<vello::Renderer> {
    vello::Renderer::new(
        device,
        vello::RendererOptions {
            surface_format: None,
            use_cpu: false,
            antialiasing_support: vello::AaSupport::all(),
            num_init_threads: NonZeroUsize::new(1),
        },
    )
    .ok()
}

#[derive(Clone, Copy)]
struct DispatchSize {
    width: u32,
    height: u32,
}

impl DispatchSize {
    fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }
}

fn dispatch_three_buffer<P: bytemuck::Pod>(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    pipeline: &wgpu::ComputePipeline,
    buffers: [&wgpu::Buffer; 2],
    params: &P,
    size: DispatchSize,
) {
    let params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("tellur-gpu-params"),
        contents: bytemuck::bytes_of(params),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let layout = pipeline.get_bind_group_layout(0);
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("tellur-gpu-bind-group"),
        layout: &layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: buffers[0].as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: buffers[1].as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: params.as_entire_binding(),
            },
        ],
    });

    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
        label: Some("tellur-gpu-pass"),
        timestamp_writes: None,
    });
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, &bind_group, &[]);
    pass.dispatch_workgroups(
        div_ceil(size.width, WORKGROUP),
        div_ceil(size.height, WORKGROUP),
        1,
    );
}

fn div_ceil(n: u32, d: u32) -> u32 {
    if n == 0 {
        0
    } else {
        1 + (n - 1) / d
    }
}

fn color_u8(color: Color) -> [u32; 4] {
    [
        (color.r * 255.0).round().clamp(0.0, 255.0) as u32,
        (color.g * 255.0).round().clamp(0.0, 255.0) as u32,
        (color.b * 255.0).round().clamp(0.0, 255.0) as u32,
        (color.a * 255.0).round().clamp(0.0, 255.0) as u32,
    ]
}

fn build_vello_scene(graphic: &VectorGraphic, target: Resolution) -> Option<vello::Scene> {
    if target.width == 0
        || target.height == 0
        || graphic.view_box.size.0 <= 0.0
        || graphic.view_box.size.1 <= 0.0
    {
        return None;
    }

    let sx = target.width as f32 / graphic.view_box.size.0;
    let sy = target.height as f32 / graphic.view_box.size.1;
    let view = Transform {
        a: sx,
        b: 0.0,
        c: 0.0,
        d: sy,
        tx: -graphic.view_box.origin.0 * sx,
        ty: -graphic.view_box.origin.1 * sy,
    };
    let clip = VelloRect::new(0.0, 0.0, target.width as f64, target.height as f64);
    let mut scene = vello::Scene::new();
    encode_vello_node(&mut scene, &graphic.root, view, &clip, 1.0)?;
    Some(scene)
}

fn encode_vello_node(
    scene: &mut vello::Scene,
    node: &Node,
    transform: Transform,
    clip: &VelloRect,
    opacity: f32,
) -> Option<()> {
    match node {
        Node::Group(group) => {
            let opacity = opacity * group.opacity.clamp(0.0, 1.0);
            if opacity <= 0.0 {
                return Some(());
            }
            let transform = concat_transform(transform, group.transform);
            for child in &group.children {
                encode_vello_node(scene, child, transform, clip, opacity)?;
            }
        }
        Node::SingleGroup(group) => {
            let opacity = opacity * group.opacity.clamp(0.0, 1.0);
            if opacity <= 0.0 {
                return Some(());
            }
            let transform = concat_transform(transform, group.transform);
            encode_vello_node(scene, &group.child, transform, clip, opacity)?;
        }
        Node::ClipGroup(group) => encode_vello_clip_group(scene, group, transform, clip, opacity)?,
        Node::Path(path) => encode_vello_path(scene, path, transform, opacity)?,
    }
    Some(())
}

fn encode_vello_clip_group(
    scene: &mut vello::Scene,
    group: &TellurClipGroup,
    transform: Transform,
    outer_clip: &VelloRect,
    opacity: f32,
) -> Option<()> {
    let Some(clip_path) = build_vello_path(&group.commands) else {
        return Some(());
    };
    let clip_transform = concat_transform(transform, group.transform);
    scene.push_layer(
        vello::peniko::BlendMode::default(),
        1.0,
        to_vello_affine(clip_transform),
        &clip_path,
    );
    encode_vello_node(scene, &group.child, transform, outer_clip, opacity)?;
    scene.pop_layer();
    Some(())
}

fn encode_vello_path(
    scene: &mut vello::Scene,
    path: &TellurPath,
    transform: Transform,
    opacity: f32,
) -> Option<()> {
    if path.fill.is_none() && path.stroke.is_none() {
        return Some(());
    }
    let transform = concat_transform(transform, path.transform);
    let Some(vello_path) = build_vello_path(&path.commands) else {
        return Some(());
    };
    let transform = to_vello_affine(transform);

    if let Some(fill) = &path.fill {
        if let Some(paint) = to_vello_color(&fill.paint, opacity) {
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                paint,
                None,
                &vello_path,
            );
        }
    }

    if let Some(stroke) = &path.stroke {
        if stroke.width > 0.0 {
            if let Some(paint) = to_vello_color(&stroke.paint, opacity) {
                scene.stroke(
                    &VelloStroke::new(stroke.width as f64),
                    transform,
                    paint,
                    None,
                    &vello_path,
                );
            }
        }
    }

    Some(())
}

fn build_vello_path(commands: &[PathCommand]) -> Option<BezPath> {
    let mut path = BezPath::new();
    let mut has_open_subpath = false;
    for command in commands {
        match *command {
            PathCommand::MoveTo(p) => {
                path.move_to(to_vello_point(p));
                has_open_subpath = true;
            }
            PathCommand::LineTo(p) => {
                if has_open_subpath {
                    path.line_to(to_vello_point(p));
                }
            }
            PathCommand::QuadTo { control, to } => {
                if has_open_subpath {
                    path.quad_to(to_vello_point(control), to_vello_point(to));
                }
            }
            PathCommand::CubicTo { c1, c2, to } => {
                if has_open_subpath {
                    path.curve_to(to_vello_point(c1), to_vello_point(c2), to_vello_point(to));
                }
            }
            PathCommand::Close => {
                if has_open_subpath {
                    path.close_path();
                    has_open_subpath = false;
                }
            }
        }
    }
    (!path.elements().is_empty()).then_some(path)
}

fn to_vello_point(p: Vec2) -> (f64, f64) {
    (p.0 as f64, p.1 as f64)
}

fn to_vello_color(paint: &Paint, opacity: f32) -> Option<vello::peniko::Color> {
    let Paint::Solid(color) = paint;
    let color = color.multiply_alpha(opacity);
    if color.a <= 0.0 {
        return None;
    }
    let [r, g, b, a] = color_u8(color);
    Some(vello::peniko::Color::rgba8(
        r as u8, g as u8, b as u8, a as u8,
    ))
}

fn concat_transform(a: Transform, b: Transform) -> Transform {
    Transform {
        a: a.a * b.a + a.c * b.b,
        b: a.b * b.a + a.d * b.b,
        c: a.a * b.c + a.c * b.d,
        d: a.b * b.c + a.d * b.d,
        tx: a.a * b.tx + a.c * b.ty + a.tx,
        ty: a.b * b.tx + a.d * b.ty + a.ty,
    }
}

fn to_vello_affine(t: Transform) -> Affine {
    Affine::new([
        t.a as f64,
        t.b as f64,
        t.c as f64,
        t.d as f64,
        t.tx as f64,
        t.ty as f64,
    ])
}

const COMMON_WGSL: &str = r#"
fn unpack_rgba(px: u32) -> vec4<u32> {
    return vec4<u32>(
        px & 255u,
        (px >> 8u) & 255u,
        (px >> 16u) & 255u,
        (px >> 24u) & 255u,
    );
}

fn pack_rgba(c: vec4<u32>) -> u32 {
    return (c.x & 255u) | ((c.y & 255u) << 8u) | ((c.z & 255u) << 16u) | ((c.w & 255u) << 24u);
}

fn blend_over(dst_px: u32, src_px: u32) -> u32 {
    let s = unpack_rgba(src_px);
    let sa = s.w;
    if (sa == 0u) {
        return dst_px;
    }
    if (sa == 255u) {
        return src_px;
    }

    let d = unpack_rgba(dst_px);
    let inv_sa = 255u - sa;
    let out_a_x255 = sa * 255u + d.w * inv_sa;
    let half = out_a_x255 / 2u;
    let out_r = (s.x * sa * 255u + d.x * d.w * inv_sa + half) / out_a_x255;
    let out_g = (s.y * sa * 255u + d.y * d.w * inv_sa + half) / out_a_x255;
    let out_b = (s.z * sa * 255u + d.z * d.w * inv_sa + half) / out_a_x255;
    let out_a = (out_a_x255 + 127u) / 255u;
    return pack_rgba(vec4<u32>(out_r, out_g, out_b, out_a));
}
"#;

const COMPOSITE_SHADER: &str = r#"
struct Params {
    dst_w: u32,
    dst_h: u32,
    src_w: u32,
    src_h: u32,
    offset_x: i32,
    offset_y: i32,
    pad0: u32,
    pad1: u32,
}

@group(0) @binding(0) var<storage, read_write> dst: array<u32>;
@group(0) @binding(1) var<storage, read> src: array<u32>;
@group(0) @binding(2) var<storage, read> params: Params;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let x = id.x;
    let y = id.y;
    if (x >= params.src_w || y >= params.src_h) {
        return;
    }
    let dx = i32(x) + params.offset_x;
    let dy = i32(y) + params.offset_y;
    if (dx < 0 || dy < 0 || dx >= i32(params.dst_w) || dy >= i32(params.dst_h)) {
        return;
    }
    let sidx = y * params.src_w + x;
    let didx = u32(dy) * params.dst_w + u32(dx);
    dst[didx] = blend_over(dst[didx], src[sidx]);
}
"#;

const COPY_ALPHA_SHADER: &str = r#"
struct Params {
    src_w: u32,
    src_h: u32,
    out_w: u32,
    out_h: u32,
    offset_x: i32,
    offset_y: i32,
    pad0: u32,
    pad1: u32,
}

@group(0) @binding(0) var<storage, read> src: array<u32>;
@group(0) @binding(1) var<storage, read_write> alpha: array<u32>;
@group(0) @binding(2) var<storage, read> params: Params;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let x = id.x;
    let y = id.y;
    if (x >= params.out_w || y >= params.out_h) {
        return;
    }
    let out_idx = y * params.out_w + x;
    let sx_i = i32(x) - params.offset_x;
    let sy_i = i32(y) - params.offset_y;
    if (sx_i < 0 || sy_i < 0) {
        alpha[out_idx] = 0u;
        return;
    }
    let sx = u32(sx_i);
    let sy = u32(sy_i);
    if (sx >= params.src_w || sy >= params.src_h) {
        alpha[out_idx] = 0u;
        return;
    }
    let px = src[sy * params.src_w + sx];
    alpha[out_idx] = (px >> 24u) & 255u;
}
"#;

const BLUR_SHADER: &str = r#"
struct Params {
    width: u32,
    height: u32,
    radius: u32,
    horizontal: u32,
}

@group(0) @binding(0) var<storage, read> src: array<u32>;
@group(0) @binding(1) var<storage, read_write> dst: array<u32>;
@group(0) @binding(2) var<storage, read> params: Params;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let x = id.x;
    let y = id.y;
    if (x >= params.width || y >= params.height) {
        return;
    }

    var sum = 0u;
    var count = 0u;
    if (params.horizontal != 0u) {
        let start = select(0u, x - params.radius, x >= params.radius);
        let end = min(params.width - 1u, x + params.radius);
        var ix = start;
        loop {
            sum = sum + src[y * params.width + ix];
            count = count + 1u;
            if (ix >= end) {
                break;
            }
            ix = ix + 1u;
        }
    } else {
        let start = select(0u, y - params.radius, y >= params.radius);
        let end = min(params.height - 1u, y + params.radius);
        var iy = start;
        loop {
            sum = sum + src[iy * params.width + x];
            count = count + 1u;
            if (iy >= end) {
                break;
            }
            iy = iy + 1u;
        }
    }
    dst[y * params.width + x] = sum / count;
}
"#;

const SHADOW_SHADER: &str = r#"
struct Params {
    dst_w: u32,
    dst_h: u32,
    src_w: u32,
    src_h: u32,
    offset_x: i32,
    offset_y: i32,
    r: u32,
    g: u32,
    b: u32,
    a: u32,
    radius_x: u32,
    radius_y: u32,
}

@group(0) @binding(0) var<storage, read_write> dst: array<u32>;
@group(0) @binding(1) var<storage, read> alpha: array<u32>;
@group(0) @binding(2) var<storage, read> params: Params;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let x = id.x;
    let y = id.y;
    if (x >= params.src_w || y >= params.src_h) {
        return;
    }
    let dx = i32(x) + params.offset_x;
    let dy = i32(y) + params.offset_y;
    if (dx < 0 || dy < 0 || dx >= i32(params.dst_w) || dy >= i32(params.dst_h)) {
        return;
    }
    let a = (alpha[y * params.src_w + x] * params.a + 127u) / 255u;
    if (a == 0u) {
        return;
    }
    let src = pack_rgba(vec4<u32>(params.r, params.g, params.b, a));
    let didx = u32(dy) * params.dst_w + u32(dx);
    dst[didx] = blend_over(dst[didx], src);
}
"#;

const OUTLINE_SHADER: &str = r#"
struct Params {
    dst_w: u32,
    dst_h: u32,
    src_w: u32,
    src_h: u32,
    offset_x: i32,
    offset_y: i32,
    r: u32,
    g: u32,
    b: u32,
    a: u32,
    radius_x: u32,
    radius_y: u32,
}

@group(0) @binding(0) var<storage, read_write> dst: array<u32>;
@group(0) @binding(1) var<storage, read> alpha: array<u32>;
@group(0) @binding(2) var<storage, read> params: Params;

fn line_coverage(delta: i32, radius: u32) -> f32 {
    let edge_distance = f32(abs(delta)) - f32(radius);
    return clamp(0.5 - edge_distance, 0.0, 1.0);
}

fn ellipse_coverage(dx: i32, dy: i32, rx: u32, ry: u32) -> f32 {
    if (rx == 0u && ry == 0u) {
        return select(0.0, 1.0, dx == 0 && dy == 0);
    }
    if (rx == 0u) {
        if (dx != 0) {
            return 0.0;
        }
        return line_coverage(dy, ry);
    }
    if (ry == 0u) {
        if (dy != 0) {
            return 0.0;
        }
        return line_coverage(dx, rx);
    }

    let dx_f = f32(dx);
    let dy_f = f32(dy);
    let center_distance = sqrt(dx_f * dx_f + dy_f * dy_f);
    if (center_distance == 0.0) {
        return 1.0;
    }

    let nx = dx_f / f32(rx);
    let ny = dy_f / f32(ry);
    let normalized = sqrt(nx * nx + ny * ny);
    let radius_along_ray = center_distance / normalized;
    let edge_distance = center_distance - radius_along_ray;
    return clamp(0.5 - edge_distance, 0.0, 1.0);
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let x = id.x;
    let y = id.y;
    if (x >= params.src_w || y >= params.src_h) {
        return;
    }

    let rx = i32(params.radius_x);
    let ry = i32(params.radius_y);
    var m = 0.0;
    var oy = -(ry + 1);
    loop {
        var ox = -(rx + 1);
        loop {
            let coverage = ellipse_coverage(ox, oy, params.radius_x, params.radius_y);
            if (coverage > 0.0) {
                let sx = i32(x) + ox;
                let sy = i32(y) + oy;
                if (sx >= 0 && sy >= 0 && sx < i32(params.src_w) && sy < i32(params.src_h)) {
                    m = max(m, f32(alpha[u32(sy) * params.src_w + u32(sx)]) * coverage);
                }
            }
            if (ox >= rx + 1) {
                break;
            }
            ox = ox + 1;
        }
        if (oy >= ry + 1) {
            break;
        }
        oy = oy + 1;
    }

    let dilated = u32(round(clamp(m, 0.0, 255.0)));
    let a = (dilated * params.a + 127u) / 255u;
    if (a == 0u) {
        return;
    }
    let dx = i32(x) + params.offset_x;
    let dy = i32(y) + params.offset_y;
    if (dx < 0 || dy < 0 || dx >= i32(params.dst_w) || dy >= i32(params.dst_h)) {
        return;
    }
    let src = pack_rgba(vec4<u32>(params.r, params.g, params.b, a));
    let didx = u32(dy) * params.dst_w + u32(dx);
    dst[didx] = blend_over(dst[didx], src);
}
"#;

const TEXTURE_TO_BUFFER_SHADER: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pad0: u32,
    pad1: u32,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var<storage, read_write> dst: array<u32>;
@group(0) @binding(2) var<storage, read> params: Params;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let x = id.x;
    let y = id.y;
    if (x >= params.width || y >= params.height) {
        return;
    }
    let c = textureLoad(src, vec2<i32>(i32(x), i32(y)), 0);
    let r = u32(round(clamp(c.r, 0.0, 1.0) * 255.0));
    let g = u32(round(clamp(c.g, 0.0, 1.0) * 255.0));
    let b = u32(round(clamp(c.b, 0.0, 1.0) * 255.0));
    let a = u32(round(clamp(c.a, 0.0, 1.0) * 255.0));
    dst[y * params.width + x] = r | (g << 8u) | (b << 16u) | (a << 24u);
}
"#;

const FILL_SHADER: &str = r#"
struct Params {
    width: u32,
    height: u32,
    color: u32,
    pad0: u32,
}

@group(0) @binding(0) var<storage, read_write> dst: array<u32>;
@group(0) @binding(1) var<storage, read> params: Params;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let x = id.x;
    let y = id.y;
    if (x >= params.width || y >= params.height) {
        return;
    }
    dst[y * params.width + x] = params.color;
}
"#;

// One shutter sample into the premultiplied u32 sums (4 words per pixel).
// Integer math throughout so the GPU result is byte-identical to the CPU
// fallback in `motion_blur.rs` — keep the two in lockstep.
const MOTION_ACCUM_SHADER: &str = r#"
struct Params {
    width: u32,
    height: u32,
    total: u32,
    pad0: u32,
}

@group(0) @binding(0) var<storage, read_write> acc: array<u32>;
@group(0) @binding(1) var<storage, read> src: array<u32>;
@group(0) @binding(2) var<storage, read> params: Params;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let x = id.x;
    let y = id.y;
    if (x >= params.width || y >= params.height) {
        return;
    }
    let idx = y * params.width + x;
    let s = unpack_rgba(src[idx]);
    let base = idx * 4u;
    acc[base] = acc[base] + s.x * s.w;
    acc[base + 1u] = acc[base + 1u] + s.y * s.w;
    acc[base + 2u] = acc[base + 2u] + s.z * s.w;
    acc[base + 3u] = acc[base + 3u] + s.w;
}
"#;

// Resolve the sums into straight-alpha RGBA8: color = the alpha-weighted
// average of the straight source colors (rounded), alpha = the mean over
// ALL `total` shutter samples — missing samples counted as transparent.
const MOTION_RESOLVE_SHADER: &str = r#"
struct Params {
    width: u32,
    height: u32,
    total: u32,
    pad0: u32,
}

@group(0) @binding(0) var<storage, read> acc: array<u32>;
@group(0) @binding(1) var<storage, read_write> dst: array<u32>;
@group(0) @binding(2) var<storage, read> params: Params;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let x = id.x;
    let y = id.y;
    if (x >= params.width || y >= params.height) {
        return;
    }
    let idx = y * params.width + x;
    let base = idx * 4u;
    let sum_a = acc[base + 3u];
    if (sum_a == 0u) {
        dst[idx] = 0u;
        return;
    }
    let half = sum_a / 2u;
    let r = min((acc[base] + half) / sum_a, 255u);
    let g = min((acc[base + 1u] + half) / sum_a, 255u);
    let b = min((acc[base + 2u] + half) / sum_a, 255u);
    let a = min((sum_a + params.total / 2u) / params.total, 255u);
    dst[idx] = pack_rgba(vec4<u32>(r, g, b, a));
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use tellur_core::composite::composite_at;
    use tellur_core::geometry::Rect;
    use tellur_core::render_context::{
        CompositeInput, DropShadowInput, GpuRasterBackend, OutlineInput,
    };
    use tellur_core::vector::{ClipGroup, Fill, Group, Path, PathCommand, Stroke};

    fn gpu_or_skip() -> Option<GpuRenderer> {
        match GpuRenderer::new() {
            Ok(gpu) => Some(gpu),
            Err(err) => {
                eprintln!("skipping GPU smoke test: {err}");
                None
            }
        }
    }

    fn image(width: u32, height: u32, pixels: &[u8]) -> RasterImage {
        RasterImage::cpu(width, height, PixelFormat::Rgba8, pixels.to_vec())
    }

    fn readback(gpu: &mut GpuRenderer, image: RasterImage) -> CpuRasterImage {
        GpuRasterBackend::readback(gpu, image).expect("GPU image should read back")
    }

    #[test]
    #[ignore = "requires a GPU adapter"]
    fn temporal_average_matches_cpu_average() {
        let Some(mut gpu) = gpu_or_skip() else {
            return;
        };
        let target = Resolution::new(2, 2);
        let a = image(
            2,
            2,
            &[
                255, 0, 0, 255, //
                0, 255, 0, 128, //
                0, 0, 255, 0, //
                10, 20, 30, 40,
            ],
        );
        let b = image(
            2,
            2,
            &[
                0, 0, 0, 0, //
                255, 255, 255, 255, //
                0, 0, 255, 64, //
                200, 100, 50, 130,
            ],
        );
        // total = 3 leaves one sample missing, exercising the fade divisor.
        let total = 3;

        let rendered =
            GpuRasterBackend::temporal_average(&mut gpu, target, &[&a, &b], total).unwrap();
        let rendered = readback(&mut gpu, rendered);

        let cpu_frames: Vec<CpuRasterImage> = [&a, &b]
            .iter()
            .map(|img| img.as_cpu().expect("test inputs are CPU images").clone())
            .collect();
        let expected = crate::motion_blur::average_frames_cpu(&cpu_frames, total, target);
        let expected = expected.as_cpu().expect("CPU average is a CPU image");
        assert_eq!(rendered.pixels.as_ref(), expected.pixels.as_ref());
    }

    #[test]
    #[ignore = "requires a GPU adapter"]
    fn composite_matches_cpu_blend() {
        let Some(mut gpu) = gpu_or_skip() else {
            return;
        };
        let src = image(
            2,
            1,
            &[
                100, 0, 0, 128, //
                0, 255, 0, 255,
            ],
        );
        let target = Resolution::new(3, 2);
        let input = CompositeInput {
            image: &src,
            offset_x: 1,
            offset_y: 1,
        };

        let rendered = GpuRasterBackend::composite(&mut gpu, target, &[input]).unwrap();
        let rendered = readback(&mut gpu, rendered);

        let mut expected = vec![0u8; 3 * 2 * 4];
        let src = match src {
            RasterImage::Cpu(src) => src,
            RasterImage::Gpu(_) => unreachable!(),
        };
        composite_at(&mut expected, target, &src, 1, 1);
        assert_eq!(rendered.pixels.as_ref(), expected.as_slice());
    }

    #[test]
    #[ignore = "requires a GPU adapter"]
    fn composite_matches_cpu_with_opaque_full_frame_base() {
        let Some(mut gpu) = gpu_or_skip() else {
            return;
        };
        let target = Resolution::new(3, 2);
        let base = GpuRasterBackend::solid_fill(&mut gpu, target, Color::rgb_u8(10, 20, 30))
            .expect("solid fill should use GPU");
        let src = image(
            2,
            1,
            &[
                100, 0, 0, 128, //
                0, 255, 0, 255,
            ],
        );
        let inputs = [
            CompositeInput {
                image: &base,
                offset_x: 0,
                offset_y: 0,
            },
            CompositeInput {
                image: &src,
                offset_x: 1,
                offset_y: 1,
            },
        ];

        let rendered = GpuRasterBackend::composite(&mut gpu, target, &inputs).unwrap();
        let rendered = readback(&mut gpu, rendered);

        let mut expected = Vec::new();
        for _ in 0..(target.width as usize * target.height as usize) {
            expected.extend_from_slice(&[10, 20, 30, 255]);
        }
        let src = match src {
            RasterImage::Cpu(src) => src,
            RasterImage::Gpu(_) => unreachable!(),
        };
        composite_at(&mut expected, target, &src, 1, 1);
        assert_eq!(rendered.pixels.as_ref(), expected.as_slice());
    }

    #[test]
    #[ignore = "requires a GPU adapter"]
    fn drop_shadow_composites_shadow_then_child() {
        let Some(mut gpu) = gpu_or_skip() else {
            return;
        };
        let child = image(1, 1, &[200, 10, 20, 255]);
        let input = DropShadowInput {
            child: &child,
            target: Resolution::new(3, 1),
            child_offset_x: 1,
            child_offset_y: 0,
            shadow_offset_x: 0,
            shadow_offset_y: 0,
            blur_radius: 0,
            color: Color::rgba_u8(1, 2, 3, 128),
        };

        let rendered = GpuRasterBackend::drop_shadow(&mut gpu, input).unwrap();
        let rendered = readback(&mut gpu, rendered);

        assert_eq!(
            rendered.pixels.as_ref(),
            &[
                1, 2, 3, 128, //
                200, 10, 20, 255, //
                0, 0, 0, 0,
            ]
        );
    }

    #[test]
    #[ignore = "requires a GPU adapter"]
    fn outline_dilates_child_alpha() {
        let Some(mut gpu) = gpu_or_skip() else {
            return;
        };
        let child = image(1, 1, &[9, 8, 7, 255]);
        let input = OutlineInput {
            child: &child,
            target: Resolution::new(3, 3),
            child_offset_x: 1,
            child_offset_y: 1,
            radius_x: 1,
            radius_y: 1,
            color: Color::rgba_u8(1, 2, 3, 255),
        };

        let rendered = GpuRasterBackend::outline(&mut gpu, input).unwrap();
        let rendered = readback(&mut gpu, rendered);

        assert_eq!(
            rendered.pixels.as_ref(),
            &[
                1, 2, 3, 22, 1, 2, 3, 128, 1, 2, 3, 22, //
                1, 2, 3, 128, 9, 8, 7, 255, 1, 2, 3, 128, //
                1, 2, 3, 22, 1, 2, 3, 128, 1, 2, 3, 22,
            ]
        );
    }

    #[test]
    #[ignore = "requires a GPU adapter"]
    fn rasterize_fills_simple_rectangle() {
        let Some(mut gpu) = gpu_or_skip() else {
            return;
        };
        let graphic = VectorGraphic {
            view_box: Rect {
                origin: Vec2::ZERO,
                size: Vec2(4.0, 4.0),
            },
            root: Node::Path(Path {
                commands: vec![
                    PathCommand::MoveTo(Vec2(1.0, 1.0)),
                    PathCommand::LineTo(Vec2(3.0, 1.0)),
                    PathCommand::LineTo(Vec2(3.0, 3.0)),
                    PathCommand::LineTo(Vec2(1.0, 3.0)),
                    PathCommand::Close,
                ],
                fill: Some(Fill {
                    paint: Paint::Solid(Color::rgba_u8(8, 9, 10, 255)),
                }),
                stroke: None,
                transform: Transform::IDENTITY,
            }),
        };

        let rendered =
            GpuRasterBackend::rasterize(&mut gpu, &graphic, Resolution::new(4, 4)).unwrap();
        let rendered = readback(&mut gpu, rendered);

        let mut filled = 0;
        for pixel in rendered.pixels.chunks_exact(4) {
            if pixel[3] != 0 {
                assert_eq!(pixel, &[8, 9, 10, 255]);
                filled += 1;
            }
        }
        assert_eq!(filled, 4);
    }

    #[test]
    #[ignore = "requires a GPU adapter"]
    fn rasterize_preserves_straight_alpha() {
        let Some(mut gpu) = gpu_or_skip() else {
            return;
        };
        let graphic = VectorGraphic {
            view_box: Rect {
                origin: Vec2::ZERO,
                size: Vec2(4.0, 4.0),
            },
            root: Node::Path(Path {
                commands: vec![
                    PathCommand::MoveTo(Vec2(1.0, 1.0)),
                    PathCommand::LineTo(Vec2(3.0, 1.0)),
                    PathCommand::LineTo(Vec2(3.0, 3.0)),
                    PathCommand::LineTo(Vec2(1.0, 3.0)),
                    PathCommand::Close,
                ],
                fill: Some(Fill {
                    paint: Paint::Solid(Color::rgba_u8(80, 40, 20, 128)),
                }),
                stroke: None,
                transform: Transform::IDENTITY,
            }),
        };

        let rendered =
            GpuRasterBackend::rasterize(&mut gpu, &graphic, Resolution::new(4, 4)).unwrap();
        let rendered = readback(&mut gpu, rendered);
        let center_idx = (rendered.width as usize + 1) * 4;
        let center = &rendered.pixels[center_idx..center_idx + 4];

        assert_eq!(center, &[80, 40, 20, 128]);
    }

    #[test]
    #[ignore = "requires a GPU adapter"]
    fn rasterize_applies_group_opacity_over_existing_content() {
        let Some(mut gpu) = gpu_or_skip() else {
            return;
        };
        let rect = vec![
            PathCommand::MoveTo(Vec2(0.0, 0.0)),
            PathCommand::LineTo(Vec2(4.0, 0.0)),
            PathCommand::LineTo(Vec2(4.0, 4.0)),
            PathCommand::LineTo(Vec2(0.0, 4.0)),
            PathCommand::Close,
        ];
        let path = |color| {
            Node::Path(Path {
                commands: rect.clone(),
                fill: Some(Fill {
                    paint: Paint::Solid(color),
                }),
                stroke: None,
                transform: Transform::IDENTITY,
            })
        };
        let graphic = VectorGraphic {
            view_box: Rect {
                origin: Vec2::ZERO,
                size: Vec2(4.0, 4.0),
            },
            root: Node::Group(Group {
                transform: Transform::IDENTITY,
                opacity: 1.0,
                children: vec![
                    path(Color::rgba_u8(255, 255, 255, 255)),
                    Node::single_group(
                        Transform::IDENTITY,
                        0.25,
                        path(Color::rgba_u8(0, 0, 0, 255)),
                    ),
                ],
            }),
        };

        let rendered =
            GpuRasterBackend::rasterize(&mut gpu, &graphic, Resolution::new(4, 4)).unwrap();
        let rendered = readback(&mut gpu, rendered);
        let center_idx = (rendered.width as usize + 1) * 4;
        let center = &rendered.pixels[center_idx..center_idx + 4];

        assert!(center[0] > 150 && center[0] < 220, "{center:?}");
        assert_eq!(center[3], 255);
    }

    #[test]
    #[ignore = "requires a GPU adapter"]
    fn rasterize_preserves_group_opacity_on_transparent_background() {
        let Some(mut gpu) = gpu_or_skip() else {
            return;
        };
        let rect = vec![
            PathCommand::MoveTo(Vec2(0.0, 0.0)),
            PathCommand::LineTo(Vec2(4.0, 0.0)),
            PathCommand::LineTo(Vec2(4.0, 4.0)),
            PathCommand::LineTo(Vec2(0.0, 4.0)),
            PathCommand::Close,
        ];
        let graphic = VectorGraphic {
            view_box: Rect {
                origin: Vec2::ZERO,
                size: Vec2(4.0, 4.0),
            },
            root: Node::single_group(
                Transform::IDENTITY,
                0.25,
                Node::Path(Path {
                    commands: rect,
                    fill: Some(Fill {
                        paint: Paint::Solid(Color::rgba_u8(0, 0, 0, 255)),
                    }),
                    stroke: None,
                    transform: Transform::IDENTITY,
                }),
            ),
        };

        let rendered =
            GpuRasterBackend::rasterize(&mut gpu, &graphic, Resolution::new(4, 4)).unwrap();
        let rendered = readback(&mut gpu, rendered);
        let center_idx = (rendered.width as usize + 1) * 4;
        let center = &rendered.pixels[center_idx..center_idx + 4];

        assert!(center[3] > 50 && center[3] < 80, "{center:?}");
    }

    #[test]
    #[ignore = "requires a GPU adapter"]
    fn rasterize_applies_group_opacity_with_view_box_scale() {
        let Some(mut gpu) = gpu_or_skip() else {
            return;
        };
        let rect = vec![
            PathCommand::MoveTo(Vec2(0.0, 0.0)),
            PathCommand::LineTo(Vec2(10.0, 0.0)),
            PathCommand::LineTo(Vec2(10.0, 10.0)),
            PathCommand::LineTo(Vec2(0.0, 10.0)),
            PathCommand::Close,
        ];
        let graphic = VectorGraphic {
            view_box: Rect {
                origin: Vec2::ZERO,
                size: Vec2(20.0, 20.0),
            },
            root: Node::Group(Group {
                transform: Transform::IDENTITY,
                opacity: 1.0,
                children: vec![
                    Node::Path(Path {
                        commands: vec![
                            PathCommand::MoveTo(Vec2(0.0, 0.0)),
                            PathCommand::LineTo(Vec2(20.0, 0.0)),
                            PathCommand::LineTo(Vec2(20.0, 20.0)),
                            PathCommand::LineTo(Vec2(0.0, 20.0)),
                            PathCommand::Close,
                        ],
                        fill: Some(Fill {
                            paint: Paint::Solid(Color::rgba_u8(255, 255, 255, 255)),
                        }),
                        stroke: None,
                        transform: Transform::IDENTITY,
                    }),
                    Node::single_group(
                        Transform::IDENTITY,
                        0.25,
                        Node::Path(Path {
                            commands: rect,
                            fill: Some(Fill {
                                paint: Paint::Solid(Color::rgba_u8(0, 0, 0, 255)),
                            }),
                            stroke: None,
                            transform: Transform::IDENTITY,
                        }),
                    ),
                ],
            }),
        };

        let rendered =
            GpuRasterBackend::rasterize(&mut gpu, &graphic, Resolution::new(80, 80)).unwrap();
        let rendered = readback(&mut gpu, rendered);
        let center_idx = ((20 * rendered.width as usize) + 20) * 4;
        let center = &rendered.pixels[center_idx..center_idx + 4];

        assert!(center[0] > 150 && center[0] < 220, "{center:?}");
        assert_eq!(center[3], 255);
    }

    #[test]
    #[ignore = "requires a GPU adapter"]
    fn rasterize_applies_opacity_to_clipped_child() {
        let Some(mut gpu) = gpu_or_skip() else {
            return;
        };
        let rect = vec![
            PathCommand::MoveTo(Vec2(0.0, 0.0)),
            PathCommand::LineTo(Vec2(4.0, 0.0)),
            PathCommand::LineTo(Vec2(4.0, 4.0)),
            PathCommand::LineTo(Vec2(0.0, 4.0)),
            PathCommand::Close,
        ];
        let graphic = VectorGraphic {
            view_box: Rect {
                origin: Vec2::ZERO,
                size: Vec2(4.0, 4.0),
            },
            root: Node::Group(Group {
                transform: Transform::IDENTITY,
                opacity: 1.0,
                children: vec![
                    Node::Path(Path {
                        commands: rect.clone(),
                        fill: Some(Fill {
                            paint: Paint::Solid(Color::rgba_u8(255, 255, 255, 255)),
                        }),
                        stroke: None,
                        transform: Transform::IDENTITY,
                    }),
                    Node::single_group(
                        Transform::IDENTITY,
                        0.25,
                        Node::ClipGroup(ClipGroup {
                            commands: rect.clone(),
                            transform: Transform::IDENTITY,
                            child: Box::new(Node::Path(Path {
                                commands: vec![
                                    PathCommand::MoveTo(Vec2(0.0, 2.0)),
                                    PathCommand::LineTo(Vec2(4.0, 2.0)),
                                ],
                                fill: None,
                                stroke: Some(Stroke {
                                    paint: Paint::Solid(Color::rgba_u8(0, 0, 0, 255)),
                                    width: 4.0,
                                }),
                                transform: Transform::IDENTITY,
                            })),
                        }),
                    ),
                ],
            }),
        };

        let rendered =
            GpuRasterBackend::rasterize(&mut gpu, &graphic, Resolution::new(4, 4)).unwrap();
        let rendered = readback(&mut gpu, rendered);
        let center_idx = (rendered.width as usize + 1) * 4;
        let center = &rendered.pixels[center_idx..center_idx + 4];

        assert!(center[0] > 150 && center[0] < 220, "{center:?}");
        assert_eq!(center[3], 255);
    }

    #[test]
    #[ignore = "requires a GPU adapter"]
    fn solid_fill_writes_every_pixel() {
        let Some(mut gpu) = gpu_or_skip() else {
            return;
        };
        let target = Resolution::new(3, 2);
        let color = Color::rgba_u8(8, 9, 10, 200);

        let rendered = GpuRasterBackend::solid_fill(&mut gpu, target, color).unwrap();
        let rendered = readback(&mut gpu, rendered);

        for pixel in rendered.pixels.chunks_exact(4) {
            assert_eq!(pixel, &[8, 9, 10, 200]);
        }
    }
}
