//! Drop-shadow effect for raster components.
//!
//! Wraps a `RasterComponent` and paints a blurred, color-tinted copy of
//! the child's alpha shape behind it. The component's `paint_bounds`
//! expands to include the shadow so the surrounding `Layer` allocates
//! enough pixels; its `layout_box` is left unchanged so shadows do not
//! disturb layout.

use std::hash::{Hash, Hasher};

use tellur_core::color::Color;
use tellur_core::composite::composite_at;
use tellur_core::dyn_compare::hash_f32;
use tellur_core::geometry::{Constraints, Rect, Vec2};
use tellur_core::raster::{CpuRasterImage, PixelFormat, RasterComponent, RasterImage, Resolution};
use tellur_core::render_context::{DropShadowInput, RenderContext};

#[tellur_core::component(raster)]
pub struct DropShadow {
    /// Offset of the shadow relative to the child, in logical units.
    #[builder(default = Vec2::ZERO)]
    pub offset: Vec2,
    /// Gaussian-equivalent blur radius (logical units).
    pub blur: f32,
    /// Shadow color (the alpha channel is multiplied with the child's).
    pub color: Color,
    #[effect]
    #[builder(into)]
    pub child: Box<dyn RasterComponent>,
}

impl PartialEq for DropShadow {
    fn eq(&self, other: &Self) -> bool {
        self.offset == other.offset
            && self.blur.to_bits() == other.blur.to_bits()
            && self.color == other.color
            && *self.child == *other.child
    }
}

impl Hash for DropShadow {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.offset.hash(state);
        hash_f32(self.blur, state);
        self.color.hash(state);
        self.child.hash(state);
    }
}

/// 3-pass box blur with kernel radius `r` has a total convolution
/// support of `3 * r` on each side of the source. Both `paint_bounds`
/// and the per-pixel `make_shadow` padding must agree on this extent so
/// the shadow does not get hard-cut at the edge of the paint region.
const BLUR_EXTENT_MULTIPLIER: f32 = 3.0;

impl RasterComponent for DropShadow {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        self.child.layout(constraints)
    }

    fn paint_bounds(&self, size: Vec2) -> Rect {
        let inner = self.child.paint_bounds(size);
        let blur_extent = self.blur.max(0.0) * BLUR_EXTENT_MULTIPLIER;
        let shadow_origin = Vec2(
            inner.origin.0 + self.offset.0 - blur_extent,
            inner.origin.1 + self.offset.1 - blur_extent,
        );
        let shadow_end = Vec2(
            inner.origin.0 + inner.size.0 + self.offset.0 + blur_extent,
            inner.origin.1 + inner.size.1 + self.offset.1 + blur_extent,
        );
        let inner_end = Vec2(inner.origin.0 + inner.size.0, inner.origin.1 + inner.size.1);
        let union_origin = Vec2(
            inner.origin.0.min(shadow_origin.0),
            inner.origin.1.min(shadow_origin.1),
        );
        let union_end = Vec2(inner_end.0.max(shadow_end.0), inner_end.1.max(shadow_end.1));
        Rect {
            origin: union_origin,
            size: Vec2(union_end.0 - union_origin.0, union_end.1 - union_origin.1),
        }
    }

    fn render(&self, size: Vec2, target: Resolution, ctx: &mut dyn RenderContext) -> RasterImage {
        let paint = self.paint_bounds(size);
        let child_paint = self.child.paint_bounds(size);
        if paint.size.0 <= 0.0 || paint.size.1 <= 0.0 {
            return blank_image(target);
        }
        let sx = target.width as f32 / paint.size.0;
        let sy = target.height as f32 / paint.size.1;
        let gpu_available = ctx.prefers_gpu() && ctx.gpu_backend().is_some();

        // Render the child through the context so its output is memoized
        // independently of the shadow — that's the key win for static
        // subtrees where the heavy work is the child render itself.
        let child_px_w = (child_paint.size.0 * sx).round().max(1.0) as u32;
        let child_px_h = (child_paint.size.1 * sy).round().max(1.0) as u32;
        let child_image = ctx.render(
            self.child.as_ref(),
            size,
            Resolution::new(child_px_w, child_px_h),
        );

        let blur_px = (self.blur * sx.max(sy)).round().max(0.0) as u32;
        let pad_px = blur_px as f32 * BLUR_EXTENT_MULTIPLIER;
        let pad_lu_x = pad_px / sx;
        let pad_lu_y = pad_px / sy;
        let shadow_local_x = (child_paint.origin.0 + self.offset.0 - pad_lu_x) - paint.origin.0;
        let shadow_local_y = (child_paint.origin.1 + self.offset.1 - pad_lu_y) - paint.origin.1;
        let shadow_px_x = (shadow_local_x * sx).round() as i32;
        let shadow_px_y = (shadow_local_y * sy).round() as i32;
        let child_local_x = child_paint.origin.0 - paint.origin.0;
        let child_local_y = child_paint.origin.1 - paint.origin.1;
        let child_px_x = (child_local_x * sx).round() as i32;
        let child_px_y = (child_local_y * sy).round() as i32;

        if gpu_available {
            let input = DropShadowInput {
                child: &child_image,
                target,
                child_offset_x: child_px_x,
                child_offset_y: child_px_y,
                shadow_offset_x: shadow_px_x,
                shadow_offset_y: shadow_px_y,
                blur_radius: blur_px,
                color: self.color,
            };
            if let Some(gpu) = ctx.gpu_backend() {
                if let Some(image) = gpu.drop_shadow(input) {
                    return image;
                }
            }
        }

        let child_image = ctx.readback(child_image);

        // Build a padded shadow image whose alpha is a blurred copy of
        // the child's alpha, tinted with `color`. Padding equals the
        // 3-pass box-blur extent (3 * radius) so the shadow can spread
        // beyond the child's own bounds.
        let shadow_image = make_shadow(&child_image, blur_px, self.color);

        // Composite shadow then child into a buffer covering `paint`.
        let mut accum = vec![0u8; (target.width as usize) * (target.height as usize) * 4];

        composite_at(&mut accum, target, &shadow_image, shadow_px_x, shadow_px_y);

        // Position the child relative to the paint-bounds origin.
        composite_at(&mut accum, target, &child_image, child_px_x, child_px_y);

        RasterImage::cpu(target.width, target.height, PixelFormat::Rgba8, accum)
    }
}

fn blank_image(target: Resolution) -> RasterImage {
    let bytes = (target.width as usize) * (target.height as usize) * 4;
    RasterImage::cpu(
        target.width,
        target.height,
        PixelFormat::Rgba8,
        vec![0u8; bytes],
    )
}

fn make_shadow(image: &CpuRasterImage, blur_radius: u32, color: Color) -> CpuRasterImage {
    assert_eq!(image.format, PixelFormat::Rgba8);
    let pad = (blur_radius as f32 * BLUR_EXTENT_MULTIPLIER).round() as usize;
    let in_w = image.width as usize;
    let in_h = image.height as usize;
    let out_w = in_w + 2 * pad;
    let out_h = in_h + 2 * pad;

    let mut alpha = vec![0u8; out_w * out_h];
    let pixels = image.pixels.as_ref();
    for y in 0..in_h {
        for x in 0..in_w {
            let src_idx = (y * in_w + x) * 4 + 3;
            let dst_idx = (y + pad) * out_w + (x + pad);
            alpha[dst_idx] = pixels[src_idx];
        }
    }

    if blur_radius > 0 {
        box_blur_3pass(&mut alpha, out_w, out_h, blur_radius as usize);
    }

    let r = (color.r * 255.0).round().clamp(0.0, 255.0) as u8;
    let g = (color.g * 255.0).round().clamp(0.0, 255.0) as u8;
    let b = (color.b * 255.0).round().clamp(0.0, 255.0) as u8;
    let alpha_scale = color.a.clamp(0.0, 1.0);

    let mut out = Vec::with_capacity(out_w * out_h * 4);
    for &alpha_value in &alpha {
        let a = ((alpha_value as f32) * alpha_scale)
            .round()
            .clamp(0.0, 255.0) as u8;
        out.push(r);
        out.push(g);
        out.push(b);
        out.push(a);
    }

    CpuRasterImage::new(out_w as u32, out_h as u32, PixelFormat::Rgba8, out)
}

fn box_blur_3pass(buf: &mut [u8], w: usize, h: usize, radius: usize) {
    let mut temp = vec![0u8; buf.len()];
    for _ in 0..3 {
        box_blur_h(buf, &mut temp, w, h, radius);
        box_blur_v(&temp, buf, w, h, radius);
    }
}

fn box_blur_h(src: &[u8], dst: &mut [u8], w: usize, h: usize, radius: usize) {
    if w == 0 || h == 0 {
        return;
    }
    for y in 0..h {
        let row = y * w;
        let mut sum: u32 = 0;
        let mut count: u32 = 0;
        // Initialize window covering [0, radius].
        let init_end = radius.min(w - 1);
        for x in 0..=init_end {
            sum += src[row + x] as u32;
            count += 1;
        }
        for x in 0..w {
            dst[row + x] = (sum / count) as u8;
            // Slide: add x+radius+1 if in range, drop x-radius if in range.
            let add_idx = x + radius + 1;
            if add_idx < w {
                sum += src[row + add_idx] as u32;
                count += 1;
            }
            if x >= radius {
                sum -= src[row + x - radius] as u32;
                count -= 1;
            }
        }
    }
}

fn box_blur_v(src: &[u8], dst: &mut [u8], w: usize, h: usize, radius: usize) {
    if w == 0 || h == 0 {
        return;
    }
    for x in 0..w {
        let mut sum: u32 = 0;
        let mut count: u32 = 0;
        let init_end = radius.min(h - 1);
        for y in 0..=init_end {
            sum += src[y * w + x] as u32;
            count += 1;
        }
        for y in 0..h {
            dst[y * w + x] = (sum / count) as u8;
            let add_idx = y + radius + 1;
            if add_idx < h {
                sum += src[add_idx * w + x] as u32;
                count += 1;
            }
            if y >= radius {
                sum -= src[(y - radius) * w + x] as u32;
                count -= 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tellur_core::builder::RasterEffect;

    /// Minimal raster leaf used as the base in effect-stack tests.
    #[derive(PartialEq, Hash)]
    struct Stub;

    impl RasterComponent for Stub {
        fn layout(&self, _constraints: Constraints) -> Vec2 {
            Vec2::ZERO
        }
        fn paint_bounds(&self, _size: Vec2) -> Rect {
            Rect {
                origin: Vec2::ZERO,
                size: Vec2::ZERO,
            }
        }
        fn render(&self, _size: Vec2, target: Resolution, _ctx: &mut dyn RenderContext) -> RasterImage {
            blank_image(target)
        }
    }

    #[test]
    fn effect_stack_equals_manual_nesting() {
        // First `.effect()` is innermost (closest to base); the last is outermost,
        // so this must equal `dark { child: halo { child: base } }`.
        let chained = Stub
            .effect(DropShadow::builder().blur(18.0).color(Color::rgba_u8(0, 0, 0, 40)))
            .effect(
                DropShadow::builder()
                    .offset(Vec2(0.0, 22.0))
                    .blur(26.0)
                    .color(Color::rgba_u8(0, 0, 0, 170)),
            );

        let manual = DropShadow {
            offset: Vec2(0.0, 22.0),
            blur: 26.0,
            color: Color::rgba_u8(0, 0, 0, 170),
            child: Box::new(DropShadow {
                offset: Vec2::ZERO,
                blur: 18.0,
                color: Color::rgba_u8(0, 0, 0, 40),
                child: Box::new(Stub),
            }),
        };

        // `DropShadow` is not `Debug`, so compare with `==` rather than `assert_eq!`.
        assert!(chained == manual);
    }

    #[test]
    fn effect_with_closure_wraps_base() {
        let wrapped = Stub.effect_with(|child| {
            DropShadow::builder()
                .blur(5.0)
                .color(Color::rgba_u8(0, 0, 0, 10))
                .child(child)
                .build()
        });
        assert_eq!(wrapped.blur.to_bits(), 5.0f32.to_bits());
        assert_eq!(wrapped.color, Color::rgba_u8(0, 0, 0, 10));
    }
}
