//! Outline (hard-edge stroke) effect for raster components.
//!
//! Wraps a `RasterComponent` and paints a solid-colored ring around the
//! outside of the child's alpha shape. The ring is produced by dilating
//! the child's alpha by the outline width and subtracting the original
//! alpha, so the stroke never bleeds inside the child. `paint_bounds`
//! expands by `width` so the surrounding `Layer` allocates enough
//! pixels; `layout_box` is left unchanged so outlines do not disturb
//! layout.

use std::hash::{Hash, Hasher};

use bytes::Bytes;
use tellur_core::color::Color;
use tellur_core::composite::composite_at;
use tellur_core::dyn_compare::hash_f32;
use tellur_core::geometry::{Constraints, Rect, Vec2};
use tellur_core::raster::{PixelFormat, RasterComponent, RasterImage, Resolution};
use tellur_core::render_context::RenderContext;

pub struct Outline {
    /// Stroke width on the outside of the child, in logical units.
    pub width: f32,
    /// Stroke color (its alpha is multiplied with the ring alpha).
    pub color: Color,
    pub child: Box<dyn RasterComponent>,
}

impl PartialEq for Outline {
    fn eq(&self, other: &Self) -> bool {
        self.width.to_bits() == other.width.to_bits()
            && self.color == other.color
            && *self.child == *other.child
    }
}

impl Hash for Outline {
    fn hash<H: Hasher>(&self, state: &mut H) {
        hash_f32(self.width, state);
        self.color.hash(state);
        self.child.hash(state);
    }
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

        // Render the child through the context so its output is memoized
        // independently of the outline — matches the shadow component's
        // strategy for static subtrees.
        let child_px_w = (child_paint.size.0 * sx).round().max(1.0) as u32;
        let child_px_h = (child_paint.size.1 * sy).round().max(1.0) as u32;
        let child_image = ctx.render(
            self.child.as_ref(),
            size,
            Resolution::new(child_px_w, child_px_h),
        );

        // Dilate the child alpha by `width` logical units and subtract
        // the original alpha so only the ring outside the child
        // remains. The dilation radius is computed independently along
        // each axis so it stays exactly in lockstep with `paint_bounds`
        // (which expands by `width` logical units in both directions);
        // otherwise an anisotropic pixel ratio would push the outline
        // past the buffer edge and get clipped.
        let width_px_x = (self.width.max(0.0) * sx).round() as u32;
        let width_px_y = (self.width.max(0.0) * sy).round() as u32;
        let outline_image = make_outline(&child_image, width_px_x, width_px_y, self.color);

        let mut accum = vec![0u8; (target.width as usize) * (target.height as usize) * 4];

        let pad_lu_x = width_px_x as f32 / sx;
        let pad_lu_y = width_px_y as f32 / sy;
        let outline_local_x = (child_paint.origin.0 - pad_lu_x) - paint.origin.0;
        let outline_local_y = (child_paint.origin.1 - pad_lu_y) - paint.origin.1;
        let outline_px_x = (outline_local_x * sx).round() as i32;
        let outline_px_y = (outline_local_y * sy).round() as i32;
        composite_at(
            &mut accum,
            target,
            &outline_image,
            outline_px_x,
            outline_px_y,
        );

        let child_local_x = child_paint.origin.0 - paint.origin.0;
        let child_local_y = child_paint.origin.1 - paint.origin.1;
        let child_px_x = (child_local_x * sx).round() as i32;
        let child_px_y = (child_local_y * sy).round() as i32;
        composite_at(&mut accum, target, &child_image, child_px_x, child_px_y);

        RasterImage {
            width: target.width,
            height: target.height,
            format: PixelFormat::Rgba8,
            pixels: Bytes::from(accum),
        }
    }
}

fn blank_image(target: Resolution) -> RasterImage {
    let bytes = (target.width as usize) * (target.height as usize) * 4;
    RasterImage {
        width: target.width,
        height: target.height,
        format: PixelFormat::Rgba8,
        pixels: Bytes::from(vec![0u8; bytes]),
    }
}

fn make_outline(
    image: &RasterImage,
    width_px_x: u32,
    width_px_y: u32,
    color: Color,
) -> RasterImage {
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

    // Dilate the alpha by an elliptical structuring element so the
    // outline tracks the shape's curvature. A separable square SE is
    // cheaper but visibly flattens curved tips (the top/bottom of a
    // circle becomes a horizontal cap); the ellipse keeps the contour
    // following the original shape, even when sx ≠ sy.
    let dilated = if pad_x > 0 || pad_y > 0 {
        dilate_ellipse(&alpha, out_w, out_h, pad_x, pad_y)
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

    RasterImage {
        width: out_w as u32,
        height: out_h as u32,
        format: PixelFormat::Rgba8,
        pixels: Bytes::from(out),
    }
}

/// Morphological dilation by an axis-aligned ellipse with semi-axes
/// `rx` and `ry` (in pixels). For each output pixel, the result is the
/// max of all source pixels `(x+dx, y+dy)` whose offset satisfies
/// `(dx/rx)^2 + (dy/ry)^2 <= 1`.
///
/// Not separable: the ellipse SE has to be applied as a 2-D
/// neighborhood. Cost is O(W·H·|SE|) ≈ O(W·H·π·rx·ry), which is fine
/// here because the outline is memoized on a per-subtree basis.
fn dilate_ellipse(src: &[u8], w: usize, h: usize, rx: usize, ry: usize) -> Vec<u8> {
    let mut dst = vec![0u8; w * h];
    if w == 0 || h == 0 || (rx == 0 && ry == 0) {
        dst.copy_from_slice(src);
        return dst;
    }
    let rx_i = rx as i64;
    let ry_i = ry as i64;
    let rx2 = (rx_i * rx_i).max(1);
    let ry2 = (ry_i * ry_i).max(1);
    let mut offsets: Vec<(i64, i64)> = Vec::new();
    for dy in -ry_i..=ry_i {
        for dx in -rx_i..=rx_i {
            if dx * dx * ry2 + dy * dy * rx2 <= rx2 * ry2 {
                offsets.push((dx, dy));
            }
        }
    }
    let w_i = w as i64;
    let h_i = h as i64;
    for y in 0..h {
        for x in 0..w {
            let mut m: u8 = 0;
            for &(dx, dy) in &offsets {
                let nx = x as i64 + dx;
                let ny = y as i64 + dy;
                if nx >= 0 && nx < w_i && ny >= 0 && ny < h_i {
                    let v = src[(ny as usize) * w + (nx as usize)];
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
