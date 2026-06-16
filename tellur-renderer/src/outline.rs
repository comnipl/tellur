//! Outline (hard-edge stroke) effect for raster components.
//!
//! Wraps a `RasterComponent` and paints a solid-colored ring around the
//! outside of the child's alpha shape. The ring is produced by dilating
//! the child's alpha by the outline width and subtracting the original
//! alpha, so the stroke never bleeds inside the child. `paint_bounds`
//! expands by `width` so the surrounding `Layer` allocates enough
//! pixels; `layout_box` is left unchanged so outlines do not disturb
//! layout.

use tellur_core::color::Color;
use tellur_core::composite::composite_at;
use tellur_core::geometry::{Constraints, Rect, Vec2};
use tellur_core::raster::{CpuRasterImage, PixelFormat, RasterComponent, RasterImage, Resolution};
use tellur_core::render_context::{OutlineInput, RenderContext};
use tellur_core::Keyable;

#[tellur_core::component(raster)]
#[derive(Keyable)]
pub struct Outline {
    /// Stroke width on the outside of the child, in logical units.
    pub width: f32,
    /// Stroke color (its alpha is multiplied with the ring alpha).
    pub color: Color,
    #[effect]
    #[builder(into)]
    pub child: Box<dyn RasterComponent>,
}

impl RasterComponent for Outline {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        self.child.layout(constraints)
    }

    fn paint_bounds(&self, size: Vec2) -> Rect {
        let inner = self.child.paint_bounds(size);
        let extent = self.width.max(0.0);
        Rect {
            origin: Vec2(inner.origin.0 - extent, inner.origin.1 - extent),
            size: Vec2(inner.size.0 + 2.0 * extent, inner.size.1 + 2.0 * extent),
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

        // Dilate the child alpha by `width` logical units and subtract
        // the original alpha so only the ring outside the child
        // remains. The dilation radius is computed independently along
        // each axis so it stays exactly in lockstep with `paint_bounds`
        // (which expands by `width` logical units in both directions);
        // otherwise an anisotropic pixel ratio would push the outline
        // past the buffer edge and get clipped.
        let width_px_x = (self.width.max(0.0) * sx).round() as u32;
        let width_px_y = (self.width.max(0.0) * sy).round() as u32;
        let child_px_w = inner_axis_pixels(target.width, width_px_x);
        let child_px_h = inner_axis_pixels(target.height, width_px_y);
        // Render the child through the context so its output is memoized
        // independently of the outline — matches the shadow component's
        // strategy for static subtrees.
        let child_image = ctx.render(
            self.child.as_ref(),
            size,
            Resolution::new(child_px_w, child_px_h),
        );

        let pad_lu_x = width_px_x as f32 / sx;
        let pad_lu_y = width_px_y as f32 / sy;
        let outline_local_x = (child_paint.origin.0 - pad_lu_x) - paint.origin.0;
        let outline_local_y = (child_paint.origin.1 - pad_lu_y) - paint.origin.1;
        let outline_px_x = (outline_local_x * sx).round() as i32;
        let outline_px_y = (outline_local_y * sy).round() as i32;
        let child_local_x = child_paint.origin.0 - paint.origin.0;
        let child_local_y = child_paint.origin.1 - paint.origin.1;
        let child_px_x = (child_local_x * sx).round() as i32;
        let child_px_y = (child_local_y * sy).round() as i32;

        if gpu_available {
            let input = OutlineInput {
                child: &child_image,
                target,
                child_offset_x: child_px_x,
                child_offset_y: child_px_y,
                outline_offset_x: outline_px_x,
                outline_offset_y: outline_px_y,
                radius_x: width_px_x,
                radius_y: width_px_y,
                color: self.color,
            };
            if let Some(gpu) = ctx.gpu_backend() {
                if let Some(image) = gpu.outline(input) {
                    return image;
                }
            }
        }

        let child_image = ctx.readback(child_image);
        let outline_image = make_outline(&child_image, width_px_x, width_px_y, self.color);

        let mut accum = vec![0u8; (target.width as usize) * (target.height as usize) * 4];

        composite_at(
            &mut accum,
            target,
            &outline_image,
            outline_px_x,
            outline_px_y,
        );

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

fn inner_axis_pixels(target: u32, pad: u32) -> u32 {
    target.saturating_sub(pad.saturating_mul(2)).max(1)
}

fn make_outline(
    image: &CpuRasterImage,
    width_px_x: u32,
    width_px_y: u32,
    color: Color,
) -> CpuRasterImage {
    assert_eq!(image.format, PixelFormat::Rgba8);
    let pad_x = width_px_x as usize;
    let pad_y = width_px_y as usize;
    let in_w = image.width as usize;
    let in_h = image.height as usize;
    let out_w = in_w + 2 * pad_x;
    let out_h = in_h + 2 * pad_y;

    let mut alpha = vec![0u8; out_w * out_h];
    let pixels = image.pixels.as_ref();
    for y in 0..in_h {
        for x in 0..in_w {
            let src_idx = (y * in_w + x) * 4 + 3;
            let dst_idx = (y + pad_y) * out_w + (x + pad_x);
            alpha[dst_idx] = pixels[src_idx];
        }
    }

    // Dilate the alpha by an antialiased elliptical structuring
    // element so the outline tracks the shape's curvature. A separable
    // square SE is cheaper but visibly flattens curved tips (the
    // top/bottom of a circle becomes a horizontal cap); the ellipse
    // keeps the contour following the original shape, even when sx ≠ sy.
    let dilated = if pad_x > 0 || pad_y > 0 {
        dilate_ellipse_antialiased(&alpha, out_w, out_h, pad_x, pad_y)
    } else {
        alpha.clone()
    };

    let r = (color.r * 255.0).round().clamp(0.0, 255.0) as u8;
    let g = (color.g * 255.0).round().clamp(0.0, 255.0) as u8;
    let b = (color.b * 255.0).round().clamp(0.0, 255.0) as u8;
    let alpha_scale = color.a.clamp(0.0, 1.0);

    let mut out = Vec::with_capacity(out_w * out_h * 4);
    for i in 0..dilated.len() {
        // Ring = dilated - original. Saturating sub means pixels fully
        // inside the child contribute zero, leaving only the outside
        // band.
        let ring = dilated[i].saturating_sub(alpha[i]);
        let a = ((ring as f32) * alpha_scale).round().clamp(0.0, 255.0) as u8;
        out.push(r);
        out.push(g);
        out.push(b);
        out.push(a);
    }

    CpuRasterImage::new(out_w as u32, out_h as u32, PixelFormat::Rgba8, out)
}

/// Morphological dilation by an axis-aligned ellipse with semi-axes
/// `rx` and `ry` (in pixels). For each output pixel, the result is the
/// max of nearby source alpha, weighted by a 1px linear coverage ramp at
/// the ellipse boundary. The ramp keeps a 1:1 render (notably 1080p
/// when logical units map directly to pixels) from exposing a jagged
/// binary dilation edge.
///
/// Not separable: the ellipse SE has to be applied as a 2-D
/// neighborhood. Cost is O(W·H·|SE|) ≈ O(W·H·π·rx·ry), which is fine
/// here because the outline is memoized on a per-subtree basis.
fn dilate_ellipse_antialiased(src: &[u8], w: usize, h: usize, rx: usize, ry: usize) -> Vec<u8> {
    let mut dst = vec![0u8; w * h];
    if w == 0 || h == 0 || (rx == 0 && ry == 0) {
        dst.copy_from_slice(src);
        return dst;
    }
    let rx_i = rx as i64;
    let ry_i = ry as i64;
    let mut offsets: Vec<(i64, i64, f32)> = Vec::new();
    for dy in -(ry_i + 1)..=(ry_i + 1) {
        for dx in -(rx_i + 1)..=(rx_i + 1) {
            let coverage = ellipse_coverage(dx, dy, rx, ry);
            if coverage > 0.0 {
                offsets.push((dx, dy, coverage));
            }
        }
    }
    let w_i = w as i64;
    let h_i = h as i64;
    for y in 0..h {
        for x in 0..w {
            let mut m: u8 = 0;
            for &(dx, dy, coverage) in &offsets {
                let nx = x as i64 + dx;
                let ny = y as i64 + dy;
                if nx >= 0 && nx < w_i && ny >= 0 && ny < h_i {
                    let src_alpha = src[(ny as usize) * w + (nx as usize)] as f32;
                    let v = (src_alpha * coverage).round().clamp(0.0, 255.0) as u8;
                    if v > m {
                        m = v;
                    }
                }
            }
            dst[y * w + x] = m;
        }
    }
    dst
}

fn ellipse_coverage(dx: i64, dy: i64, rx: usize, ry: usize) -> f32 {
    match (rx, ry) {
        (0, 0) => {
            if dx == 0 && dy == 0 {
                1.0
            } else {
                0.0
            }
        }
        (0, ry) => {
            if dx != 0 {
                return 0.0;
            }
            let edge_distance = dy.unsigned_abs() as f32 - ry as f32;
            (0.5 - edge_distance).clamp(0.0, 1.0)
        }
        (rx, 0) => {
            if dy != 0 {
                return 0.0;
            }
            let edge_distance = dx.unsigned_abs() as f32 - rx as f32;
            (0.5 - edge_distance).clamp(0.0, 1.0)
        }
        (rx, ry) => {
            let dx_f = dx as f32;
            let dy_f = dy as f32;
            let center_distance = (dx_f * dx_f + dy_f * dy_f).sqrt();
            if center_distance == 0.0 {
                return 1.0;
            }

            let nx = dx_f / rx as f32;
            let ny = dy_f / ry as f32;
            let normalized = (nx * nx + ny * ny).sqrt();
            let radius_along_ray = center_distance / normalized;
            let edge_distance = center_distance - radius_along_ray;
            (0.5 - edge_distance).clamp(0.0, 1.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opaque_pixel() -> CpuRasterImage {
        CpuRasterImage::new(1, 1, PixelFormat::Rgba8, vec![9, 8, 7, 255])
    }

    fn alpha_at(image: &CpuRasterImage, x: usize, y: usize) -> u8 {
        image.pixels[(y * image.width as usize + x) * 4 + 3]
    }

    #[test]
    fn inner_axis_pixels_keeps_padding_in_lockstep_with_target() {
        assert_eq!(inner_axis_pixels(101, 5), 91);
        assert_eq!(inner_axis_pixels(8, 5), 1);
    }

    #[test]
    fn outline_antialiases_outer_ellipse_edge() {
        let outline = make_outline(&opaque_pixel(), 2, 2, Color::rgba_u8(1, 2, 3, 255));

        assert_eq!(outline.width, 5);
        assert_eq!(outline.height, 5);
        assert_eq!(alpha_at(&outline, 2, 2), 0);
        assert_eq!(alpha_at(&outline, 0, 0), 0);
        assert!(alpha_at(&outline, 2, 0) > 0);
        assert!(alpha_at(&outline, 2, 0) < 255);
        assert_eq!(alpha_at(&outline, 2, 1), 255);
    }
}
