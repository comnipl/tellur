use std::borrow::Cow;
use std::sync::Arc;

use tellur_core::color::Color;
use tellur_core::raster::{CpuRasterImage, GpuSurface, PixelFormat, RasterImage, Resolution};
use tellur_core::render_context::{
    CompositeInput, DropShadowInput, GpuRasterBackend, OutlineInput,
};
use wgpu::util::DeviceExt;

const BACKEND: &str = "tellur-wgpu-buffer-v1";
const WORKGROUP: u32 = 16;

pub struct GpuRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    composite_pipeline: wgpu::ComputePipeline,
    copy_alpha_pipeline: wgpu::ComputePipeline,
    blur_pipeline: wgpu::ComputePipeline,
    shadow_pipeline: wgpu::ComputePipeline,
    outline_pipeline: wgpu::ComputePipeline,
    stats: GpuRenderStats,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct GpuRenderStats {
    pub composites: u64,
    pub drop_shadows: u64,
    pub outlines: u64,
    pub readbacks: u64,
}

impl GpuRenderStats {
    pub fn total_ops(self) -> u64 {
        self.composites + self.drop_shadows + self.outlines
    }
}

struct GpuBufferImage {
    width: u32,
    height: u32,
    format: PixelFormat,
    buffer: wgpu::Buffer,
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
    pad_x: u32,
    pad_y: u32,
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
                    required_limits: wgpu::Limits::downlevel_defaults(),
                },
                None,
            )
            .await
            .map_err(|e| format!("failed to create GPU device: {e}"))?;

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
            device,
            queue,
            stats: GpuRenderStats::default(),
        })
    }

    pub fn stats(&self) -> GpuRenderStats {
        self.stats
    }

    fn upload(&self, image: &CpuRasterImage) -> Option<Arc<GpuBufferImage>> {
        if image.format != PixelFormat::Rgba8 {
            return None;
        }
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tellur-gpu-upload"),
            size: image.pixels.len() as u64,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        self.queue.write_buffer(&buffer, 0, &image.pixels);
        Some(Arc::new(GpuBufferImage {
            width: image.width,
            height: image.height,
            format: image.format,
            buffer,
        }))
    }

    fn image_ref(&self, image: &RasterImage) -> Option<Arc<GpuBufferImage>> {
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

    fn empty_image(&self, resolution: Resolution) -> Arc<GpuBufferImage> {
        let len = (resolution.width as usize) * (resolution.height as usize) * 4;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tellur-gpu-target"),
            size: len as u64,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        self.queue.write_buffer(&buffer, 0, &vec![0u8; len]);
        Arc::new(GpuBufferImage {
            width: resolution.width,
            height: resolution.height,
            format: PixelFormat::Rgba8,
            buffer,
        })
    }

    fn alpha_image(&self, width: u32, height: u32) -> GpuBufferImage {
        let len = (width as usize) * (height as usize) * 4;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tellur-gpu-alpha"),
            size: len as u64,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        GpuBufferImage {
            width,
            height,
            format: PixelFormat::Rgba8,
            buffer,
        }
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
            &dst.buffer,
            &src.buffer,
            &params,
            src.width,
            src.height,
        );
    }

    fn copy_alpha(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        src: &GpuBufferImage,
        alpha: &GpuBufferImage,
        pad_x: u32,
        pad_y: u32,
    ) {
        let params = CopyAlphaParams {
            src_w: src.width,
            src_h: src.height,
            out_w: alpha.width,
            out_h: alpha.height,
            pad_x,
            pad_y,
            _pad0: 0,
            _pad1: 0,
        };
        dispatch_three_buffer(
            &self.device,
            encoder,
            &self.copy_alpha_pipeline,
            &src.buffer,
            &alpha.buffer,
            &params,
            alpha.width,
            alpha.height,
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
            &src.buffer,
            &dst.buffer,
            &params,
            src.width,
            src.height,
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
            &dst.buffer,
            &alpha.buffer,
            &params,
            alpha.width,
            alpha.height,
        );
    }

    fn composite_outline_alpha(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        dst: &GpuBufferImage,
        alpha: &GpuBufferImage,
        offset_x: i32,
        offset_y: i32,
        radius_x: u32,
        radius_y: u32,
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
            radius_x,
            radius_y,
        };
        dispatch_three_buffer(
            &self.device,
            encoder,
            &self.outline_pipeline,
            &dst.buffer,
            &alpha.buffer,
            &params,
            alpha.width,
            alpha.height,
        );
    }
}

impl GpuRasterBackend for GpuRenderer {
    fn composite(
        &mut self,
        target: Resolution,
        inputs: &[CompositeInput<'_>],
    ) -> Option<RasterImage> {
        let target_image = self.empty_image(target);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("tellur-gpu-composite"),
            });

        for input in inputs {
            let src = self.image_ref(input.image)?;
            if src.format != PixelFormat::Rgba8 {
                return None;
            }
            self.composite_one(
                &mut encoder,
                &target_image,
                &src,
                input.offset_x,
                input.offset_y,
            );
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
        let alpha_a = self.alpha_image(shadow_w, shadow_h);
        let alpha_b = self.alpha_image(shadow_w, shadow_h);
        let target = self.empty_image(input.target);

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("tellur-gpu-drop-shadow"),
            });
        self.copy_alpha(&mut encoder, &child, &alpha_a, pad, pad);
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
        let outline_w = child.width.checked_add(input.radius_x.checked_mul(2)?)?;
        let outline_h = child.height.checked_add(input.radius_y.checked_mul(2)?)?;
        let alpha = self.alpha_image(outline_w, outline_h);
        let target = self.empty_image(input.target);

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("tellur-gpu-outline"),
            });
        self.copy_alpha(&mut encoder, &child, &alpha, input.radius_x, input.radius_y);
        self.composite_outline_alpha(
            &mut encoder,
            &target,
            &alpha,
            input.outline_offset_x,
            input.outline_offset_y,
            input.radius_x,
            input.radius_y,
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

    fn readback(&mut self, image: RasterImage) -> Option<CpuRasterImage> {
        match image {
            RasterImage::Cpu(image) => Some(image),
            RasterImage::Gpu(surface) if surface.backend() == BACKEND => {
                let image = Arc::downcast::<GpuBufferImage>(surface.handle_arc()).ok()?;
                let byte_len = (image.width as usize) * (image.height as usize) * 4;
                let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("tellur-gpu-readback"),
                    size: byte_len as u64,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                });
                let mut encoder =
                    self.device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("tellur-gpu-readback"),
                        });
                encoder.copy_buffer_to_buffer(&image.buffer, 0, &staging, 0, byte_len as u64);
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
    })
}

fn dispatch_three_buffer<P: bytemuck::Pod>(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    pipeline: &wgpu::ComputePipeline,
    a: &wgpu::Buffer,
    b: &wgpu::Buffer,
    params: &P,
    width: u32,
    height: u32,
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
                resource: a.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: b.as_entire_binding(),
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
    pass.dispatch_workgroups(div_ceil(width, WORKGROUP), div_ceil(height, WORKGROUP), 1);
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
    pad_x: u32,
    pad_y: u32,
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
    if (x < params.pad_x || y < params.pad_y) {
        alpha[out_idx] = 0u;
        return;
    }
    let sx = x - params.pad_x;
    let sy = y - params.pad_y;
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

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let x = id.x;
    let y = id.y;
    if (x >= params.src_w || y >= params.src_h) {
        return;
    }

    let rx = i32(params.radius_x);
    let ry = i32(params.radius_y);
    let rx2 = max(rx * rx, 1);
    let ry2 = max(ry * ry, 1);
    let limit = rx2 * ry2;
    var m = 0u;
    var oy = -ry;
    loop {
        var ox = -rx;
        loop {
            if (ox * ox * ry2 + oy * oy * rx2 <= limit) {
                let sx = i32(x) + ox;
                let sy = i32(y) + oy;
                if (sx >= 0 && sy >= 0 && sx < i32(params.src_w) && sy < i32(params.src_h)) {
                    m = max(m, alpha[u32(sy) * params.src_w + u32(sx)]);
                }
            }
            if (ox >= rx) {
                break;
            }
            ox = ox + 1;
        }
        if (oy >= ry) {
            break;
        }
        oy = oy + 1;
    }

    let orig = alpha[y * params.src_w + x];
    let ring = select(0u, m - orig, m > orig);
    let a = (ring * params.a + 127u) / 255u;
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

#[cfg(test)]
mod tests {
    use super::*;
    use tellur_core::composite::composite_at;
    use tellur_core::render_context::{
        CompositeInput, DropShadowInput, GpuRasterBackend, OutlineInput,
    };

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
            outline_offset_x: 0,
            outline_offset_y: 0,
            radius_x: 1,
            radius_y: 1,
            color: Color::rgba_u8(1, 2, 3, 255),
        };

        let rendered = GpuRasterBackend::outline(&mut gpu, input).unwrap();
        let rendered = readback(&mut gpu, rendered);

        assert_eq!(
            rendered.pixels.as_ref(),
            &[
                0, 0, 0, 0, 1, 2, 3, 255, 0, 0, 0, 0, //
                1, 2, 3, 255, 9, 8, 7, 255, 1, 2, 3, 255, //
                0, 0, 0, 0, 1, 2, 3, 255, 0, 0, 0, 0,
            ]
        );
    }
}
