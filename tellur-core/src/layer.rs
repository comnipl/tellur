//! Layer types for composing components into a scene.
//!
//! Both layer types share the same coordinate model: each layer has a
//! fixed logical `size` defining its coordinate space (top-left at
//! `(0, 0)`), and children are placed at logical positions within it via
//! [`Placed`].
//!
//! Layers participate in the constraint-based layout protocol:
//! `layout(constraints)` returns `size` (clamped to the constraints), and
//! `render(size)` lays out each child with constraints loose to `size`,
//! then composes them at their stored positions.
//!
//! `VectorLayer` composes `VectorComponent` children into a single
//! `VectorGraphic`. Each child is wrapped in a translating `Group` so
//! the composed result remains pure vector data.
//!
//! `Layer` composes `RasterComponent` children by rendering each one at
//! a pixel sub-resolution matching its logical paint bounds and
//! source-over compositing it onto the output at the corresponding pixel
//! offset.

use bytes::Bytes;

use crate::geometry::{Constraints, Rect, Transform, Vec2};
use crate::placement::Placed;
use crate::raster::{PixelFormat, RasterComponent, RasterImage, Resolution};
use crate::render_context::RenderContext;
use crate::vector::{Group, Node, VectorComponent, VectorGraphic};

#[derive(PartialEq, Hash)]
pub struct VectorLayer {
    pub size: Vec2,
    pub children: Vec<Placed<dyn VectorComponent>>,
}

impl VectorLayer {
    pub fn new(size: Vec2) -> Self {
        Self {
            size,
            children: Vec::new(),
        }
    }

    pub fn add(&mut self, child: Placed<dyn VectorComponent>) -> &mut Self {
        self.children.push(child);
        self
    }
}

impl VectorComponent for VectorLayer {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        constraints.constrain(self.size)
    }

    fn render(&self, size: Vec2) -> VectorGraphic {
        let child_constraints = Constraints::loose(size);
        let children: Vec<Node> = self
            .children
            .iter()
            .map(|placed| {
                let child_size = placed.child.layout(child_constraints);
                let child_graphic = placed.child.render(child_size);
                Node::Group(Group {
                    transform: Transform::translate(placed.position),
                    opacity: 1.0,
                    children: vec![child_graphic.root],
                })
            })
            .collect();
        VectorGraphic {
            view_box: Rect {
                origin: Vec2::ZERO,
                size,
            },
            root: Node::Group(Group {
                transform: Transform::IDENTITY,
                opacity: 1.0,
                children,
            }),
        }
    }
}

#[derive(PartialEq, Hash)]
pub struct Layer {
    pub size: Vec2,
    pub children: Vec<Placed<dyn RasterComponent>>,
}

impl Layer {
    pub fn new(size: Vec2) -> Self {
        Self {
            size,
            children: Vec::new(),
        }
    }

    pub fn add(&mut self, child: Placed<dyn RasterComponent>) -> &mut Self {
        self.children.push(child);
        self
    }
}

impl RasterComponent for Layer {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        constraints.constrain(self.size)
    }

    fn paint_bounds(&self, size: Vec2) -> Rect {
        let child_constraints = Constraints::loose(size);
        let mut bounds = Rect {
            origin: Vec2::ZERO,
            size,
        };
        for placed in &self.children {
            let child_size = placed.child.layout(child_constraints);
            let child_paint = placed.child.paint_bounds(child_size);
            bounds = union_rect(bounds, translate_rect(child_paint, placed.position));
        }
        bounds
    }

    fn render(&self, size: Vec2, target: Resolution, ctx: &mut dyn RenderContext) -> RasterImage {
        let paint_rect = self.paint_bounds(size);
        let child_constraints = Constraints::loose(size);
        let placed: Vec<(Vec2, Vec2, &dyn RasterComponent)> = self
            .children
            .iter()
            .map(|p| {
                let child_size = p.child.layout(child_constraints);
                (p.position, child_size, p.child.as_ref())
            })
            .collect();
        composite_children(paint_rect, target, &placed, ctx)
    }
}

/// Rasterizes a set of placed-and-sized raster components into the
/// `paint_rect` logical region and returns the composited image at
/// `target` pixel resolution.
///
/// `paint_rect` is the parent's own paint bounds expressed in the
/// parent's logical coordinate space (its origin may be negative).
/// `target` pixels span exactly that rectangle, so 1 target pixel
/// equals `paint_rect.size / target` logical units on each axis.
///
/// Each entry's tuple is `(position, child_size, child)`:
/// - `position` is the child's layout origin in the parent's logical
///   coordinate space (i.e. relative to `paint_rect.origin = (0,0)` in
///   the layout sense, not relative to `paint_rect.origin`).
/// - `child_size` is the size returned by the child's `layout`.
/// - The child's `paint_bounds(child_size)` decides the actual pixel
///   region (the rectangle may have a negative origin or be larger than
///   `child_size` for effects like drop shadows); the child renders
///   into a buffer matching that paint-bounds size and the parent
///   composites it at `position + child_paint_bounds.origin -
///   paint_rect.origin` (i.e. shifted into the buffer's local space).
///
/// Any spill beyond the buffer is clipped at the buffer's edge — that
/// is how containers like `DecoratedBox` (whose own paint_bounds equals
/// its layout box) act as natural clip rectangles.
pub(crate) fn composite_children(
    paint_rect: Rect,
    target: Resolution,
    placed: &[(Vec2, Vec2, &dyn RasterComponent)],
    ctx: &mut dyn RenderContext,
) -> RasterImage {
    let pixel_count = (target.width as usize) * (target.height as usize);
    let mut accum = vec![0u8; pixel_count * 4];

    let scale_x = target.width as f32 / paint_rect.size.0;
    let scale_y = target.height as f32 / paint_rect.size.1;

    for (position, child_size, child) in placed {
        let bounds = child.paint_bounds(*child_size);
        let child_px_w = (bounds.size.0 * scale_x).round().max(1.0) as u32;
        let child_px_h = (bounds.size.1 * scale_y).round().max(1.0) as u32;
        let paint_x = position.0 + bounds.origin.0 - paint_rect.origin.0;
        let paint_y = position.1 + bounds.origin.1 - paint_rect.origin.1;
        let offset_x = (paint_x * scale_x).round() as i32;
        let offset_y = (paint_y * scale_y).round() as i32;

        // Route the child render through the context so cache lookups
        // can intercept it before the underlying `render` runs.
        let image = ctx.render(*child, *child_size, Resolution::new(child_px_w, child_px_h));
        composite_at(&mut accum, target, &image, offset_x, offset_y);
    }

    RasterImage {
        width: target.width,
        height: target.height,
        format: PixelFormat::Rgba8,
        pixels: Bytes::from(accum),
    }
}

/// Smallest axis-aligned rectangle containing both `a` and `b`.
pub(crate) fn union_rect(a: Rect, b: Rect) -> Rect {
    let a_end = Vec2(a.origin.0 + a.size.0, a.origin.1 + a.size.1);
    let b_end = Vec2(b.origin.0 + b.size.0, b.origin.1 + b.size.1);
    let origin = Vec2(a.origin.0.min(b.origin.0), a.origin.1.min(b.origin.1));
    let end = Vec2(a_end.0.max(b_end.0), a_end.1.max(b_end.1));
    Rect {
        origin,
        size: Vec2(end.0 - origin.0, end.1 - origin.1),
    }
}

/// Translates a rect by `delta`, leaving its size unchanged.
pub(crate) fn translate_rect(r: Rect, delta: Vec2) -> Rect {
    Rect {
        origin: Vec2(r.origin.0 + delta.0, r.origin.1 + delta.1),
        size: r.size,
    }
}

// Source-over compositing of `src` onto `dst` at pixel offset
// `(offset_x, offset_y)`. Both buffers hold 8-bit straight-alpha RGBA.
// Pixels of `src` that fall outside `dst_size` are clipped away.
fn composite_at(
    dst: &mut [u8],
    dst_size: Resolution,
    src: &RasterImage,
    offset_x: i32,
    offset_y: i32,
) {
    assert_eq!(
        src.format,
        PixelFormat::Rgba8,
        "Layer only supports Rgba8 children for now"
    );
    let src_pixels = src.pixels.as_ref();
    let dst_w = dst_size.width as i32;
    let dst_h = dst_size.height as i32;
    let src_w = src.width as i32;
    let src_h = src.height as i32;

    // Iterate only over the overlapping rectangle to skip clipped rows/cols.
    let x_start = offset_x.max(0);
    let y_start = offset_y.max(0);
    let x_end = (offset_x + src_w).min(dst_w);
    let y_end = (offset_y + src_h).min(dst_h);

    for dy in y_start..y_end {
        for dx in x_start..x_end {
            let sx = dx - offset_x;
            let sy = dy - offset_y;
            let src_idx = ((sy * src_w + sx) * 4) as usize;
            let dst_idx = ((dy * dst_w + dx) * 4) as usize;

            let sr = src_pixels[src_idx] as f32 / 255.0;
            let sg = src_pixels[src_idx + 1] as f32 / 255.0;
            let sb = src_pixels[src_idx + 2] as f32 / 255.0;
            let sa = src_pixels[src_idx + 3] as f32 / 255.0;
            let dr = dst[dst_idx] as f32 / 255.0;
            let dg = dst[dst_idx + 1] as f32 / 255.0;
            let db = dst[dst_idx + 2] as f32 / 255.0;
            let da = dst[dst_idx + 3] as f32 / 255.0;

            let inv_sa = 1.0 - sa;
            let out_a = sa + da * inv_sa;
            let (out_r, out_g, out_b) = if out_a > 0.0 {
                (
                    (sr * sa + dr * da * inv_sa) / out_a,
                    (sg * sa + dg * da * inv_sa) / out_a,
                    (sb * sa + db * da * inv_sa) / out_a,
                )
            } else {
                (0.0, 0.0, 0.0)
            };

            dst[dst_idx] = (out_r * 255.0).round().clamp(0.0, 255.0) as u8;
            dst[dst_idx + 1] = (out_g * 255.0).round().clamp(0.0, 255.0) as u8;
            dst[dst_idx + 2] = (out_b * 255.0).round().clamp(0.0, 255.0) as u8;
            dst[dst_idx + 3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
}
