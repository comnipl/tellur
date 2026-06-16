//! Outline (hard-edge stroke) effect for raster components.
//!
//! Wraps a `RasterComponent` and paints a solid-colored ring around the
//! outside of the child's alpha shape. The stroke layer is produced by
//! dilating the child's alpha by the outline width, painting that
//! silhouette behind the child, and then drawing the child on top. Keeping
//! the full dilated alpha behind antialiased child edges avoids background
//! fringes between stacked outlines. `paint_bounds` expands by `width` so
//! the surrounding `Layer` allocates enough pixels; `layout_box` is left
//! unchanged so outlines do not disturb layout.

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

        // Dilate the child alpha by `width` logical units and paint that
        // silhouette behind the child. Radius and offset are rounded
        // separately because half-pixel logical widths (e.g. 4.5 at 1x)
        // cannot be represented as symmetric integer padding: the child
        // must be copied into the target-sized alpha buffer at the same
        // pixel offset where it will later be composited.
        let width_px_x = (self.width.max(0.0) * sx).round() as u32;
        let width_px_y = (self.width.max(0.0) * sy).round() as u32;
        let child_px_w = (child_paint.size.0 * sx).round().max(1.0) as u32;
        let child_px_h = (child_paint.size.1 * sy).round().max(1.0) as u32;
        // Render the child through the context so its output is memoized
        // independently of the outline — matches the shadow component's
        // strategy for static subtrees.
        let child_image = ctx.render(
            self.child.as_ref(),
            size,
            Resolution::new(child_px_w, child_px_h),
        );

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
        let outline_image = make_outline(
            &child_image,
            target,
            child_px_x,
            child_px_y,
            width_px_x,
            width_px_y,
            self.color,
        );

        let mut accum = vec![0u8; (target.width as usize) * (target.height as usize) * 4];

        composite_at(&mut accum, target, &outline_image, 0, 0);

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

fn make_outline(
    image: &CpuRasterImage,
    target: Resolution,
    offset_x: i32,
    offset_y: i32,
    width_px_x: u32,
    width_px_y: u32,
    color: Color,
) -> CpuRasterImage {
    assert_eq!(image.format, PixelFormat::Rgba8);
    let in_w = image.width as usize;
    let in_h = image.height as usize;
    let out_w = target.width as usize;
    let out_h = target.height as usize;

    let mut alpha = vec![0u8; out_w * out_h];
    let pixels = image.pixels.as_ref();
    for y in 0..in_h {
        for x in 0..in_w {
            let dst_x = x as i32 + offset_x;
            let dst_y = y as i32 + offset_y;
            if dst_x < 0
                || dst_y < 0
                || dst_x >= target.width as i32
                || dst_y >= target.height as i32
            {
                continue;
            }
            let src_idx = (y * in_w + x) * 4 + 3;
            let dst_idx = dst_y as usize * out_w + dst_x as usize;
            alpha[dst_idx] = pixels[src_idx];
        }
    }

    // Dilate the alpha by an antialiased elliptical structuring
    // element so the outline tracks the shape's curvature. A separable
    // square SE is cheaper but visibly flattens curved tips (the
    // top/bottom of a circle becomes a horizontal cap); the ellipse
    // keeps the contour following the original shape, even when sx ≠ sy.
    let dilated = if width_px_x > 0 || width_px_y > 0 {
        dilate_ellipse_antialiased(
            &alpha,
            out_w,
            out_h,
            width_px_x as usize,
            width_px_y as usize,
        )
    } else {
        alpha.clone()
    };

    let r = (color.r * 255.0).round().clamp(0.0, 255.0) as u8;
    let g = (color.g * 255.0).round().clamp(0.0, 255.0) as u8;
    let b = (color.b * 255.0).round().clamp(0.0, 255.0) as u8;
    let alpha_scale = color.a.clamp(0.0, 1.0);

    let mut out = Vec::with_capacity(out_w * out_h * 4);
    for alpha in &dilated {
        // Paint the full dilated silhouette behind the child instead of
        // subtracting the original alpha. The later child composite covers
        // opaque interiors, while antialiased child edges blend against
        // outline color rather than the background.
        let a = ((*alpha as f32) * alpha_scale).round().clamp(0.0, 255.0) as u8;
        out.push(r);
        out.push(g);
        out.push(b);
        out.push(a);
    }

    CpuRasterImage::new(target.width, target.height, PixelFormat::Rgba8, out)
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
    use tellur_core::render_context::PassThrough;

    #[derive(PartialEq, Eq, Hash)]
    struct UnitPixel;

    impl RasterComponent for UnitPixel {
        fn layout(&self, constraints: Constraints) -> Vec2 {
            constraints.constrain(Vec2(1.0, 1.0))
        }

        fn render(
            &self,
            _size: Vec2,
            target: Resolution,
            _ctx: &mut dyn RenderContext,
        ) -> RasterImage {
            RasterImage::cpu(
                target.width,
                target.height,
                PixelFormat::Rgba8,
                vec![9, 8, 7, 255],
            )
        }
    }

    fn opaque_pixel() -> CpuRasterImage {
        CpuRasterImage::new(1, 1, PixelFormat::Rgba8, vec![9, 8, 7, 255])
    }

    fn alpha_at(image: &CpuRasterImage, x: usize, y: usize) -> u8 {
        image.pixels[(y * image.width as usize + x) * 4 + 3]
    }

    #[test]
    fn outline_antialiases_outer_ellipse_edge() {
        let outline = make_outline(
            &opaque_pixel(),
            Resolution::new(5, 5),
            2,
            2,
            2,
            2,
            Color::rgba_u8(1, 2, 3, 255),
        );

        assert_eq!(outline.width, 5);
        assert_eq!(outline.height, 5);
        assert_eq!(alpha_at(&outline, 2, 2), 255);
        assert_eq!(alpha_at(&outline, 0, 0), 0);
        assert!(alpha_at(&outline, 2, 0) > 0);
        assert!(alpha_at(&outline, 2, 0) < 255);
        assert_eq!(alpha_at(&outline, 2, 1), 255);
    }

    #[test]
    fn half_pixel_width_keeps_outline_centered_on_child_offset() {
        let outline = Outline {
            width: 4.5,
            color: Color::rgba_u8(1, 2, 3, 255),
            child: Box::new(UnitPixel),
        };
        let mut ctx = PassThrough;
        let rendered = outline
            .render(Vec2(1.0, 1.0), Resolution::new(10, 10), &mut ctx)
            .into_cpu()
            .expect("CPU outline render");

        let center = (5 * rendered.width as usize + 5) * 4;
        assert_eq!(&rendered.pixels[center..center + 4], &[9, 8, 7, 255]);
        assert!(alpha_at(&rendered, 4, 5) > 0);
        assert!(alpha_at(&rendered, 5, 4) > 0);
        assert!(alpha_at(&rendered, 6, 5) > 0);
        assert!(alpha_at(&rendered, 5, 6) > 0);
    }
}
