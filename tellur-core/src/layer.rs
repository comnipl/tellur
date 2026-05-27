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

use crate::composite::composite_at;
use crate::geometry::{Constraints, Rect, Transform, Vec2};
use crate::placement::Placed;
use crate::raster::{PixelFormat, RasterComponent, RasterImage, Resolution};
use crate::render_context::RenderContext;
use crate::vector::{Group, Node, VectorComponent, VectorGraphic};

#[derive(PartialEq, Hash)]
pub struct VectorLayer {
    /// `Some(size)` for a fixed extent; `None` to auto-fit the
    /// bounding box of the children's placed paint bounds (the layer's
    /// `view_box.origin` then matches that bounding rect's origin, so
    /// children with negative positions are not clipped).
    pub size: Option<Vec2>,
    pub children: Vec<Placed<dyn VectorComponent>>,
}

impl VectorLayer {
    /// Fixed-size layer of the given extent.
    pub fn new(size: Vec2) -> Self {
        Self {
            size: Some(size),
            children: Vec::new(),
        }
    }

    /// Auto-fit layer that shrinks to the children's bounding box.
    pub fn fit() -> Self {
        Self {
            size: None,
            children: Vec::new(),
        }
    }

    pub fn add(&mut self, child: Placed<dyn VectorComponent>) -> &mut Self {
        self.children.push(child);
        self
    }

    /// Bounding rect of all children's placed paint bounds, computed
    /// with each child laid out under `Constraints::UNBOUNDED`. Returns
    /// a zero rect when there are no children.
    fn children_bounds(&self) -> Rect {
        let mut iter = self.children.iter().map(|placed| {
            let child_size = placed.child.layout(Constraints::UNBOUNDED);
            let child_paint = placed.child.paint_bounds(child_size);
            translate_rect(child_paint, placed.position)
        });
        let Some(first) = iter.next() else {
            return Rect {
                origin: Vec2::ZERO,
                size: Vec2::ZERO,
            };
        };
        iter.fold(first, union_rect)
    }
}

impl VectorComponent for VectorLayer {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        let intrinsic = match self.size {
            Some(s) => s,
            None => self.children_bounds().size,
        };
        constraints.constrain(intrinsic)
    }

    fn render(&self, size: Vec2) -> VectorGraphic {
        // For auto-fit, the view_box origin tracks the children's
        // bounding rect so children that paint at negative positions
        // are inside the box rather than clipped.
        let view_origin = match self.size {
            Some(_) => Vec2::ZERO,
            None => self.children_bounds().origin,
        };
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
                origin: view_origin,
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
    /// `Some(size)` for a fixed extent; `None` to auto-fit the
    /// bounding box of the children's placed paint bounds.
    pub size: Option<Vec2>,
    pub children: Vec<Placed<dyn RasterComponent>>,
}

impl Layer {
    /// Fixed-size layer of the given extent.
    pub fn new(size: Vec2) -> Self {
        Self {
            size: Some(size),
            children: Vec::new(),
        }
    }

    /// Auto-fit layer that shrinks to the children's bounding box.
    pub fn fit() -> Self {
        Self {
            size: None,
            children: Vec::new(),
        }
    }

    pub fn add(&mut self, child: Placed<dyn RasterComponent>) -> &mut Self {
        self.children.push(child);
        self
    }

    /// Bounding rect of all children's placed paint bounds, computed
    /// with each child laid out under `Constraints::UNBOUNDED`.
    fn children_bounds(&self) -> Rect {
        let mut iter = self.children.iter().map(|placed| {
            let child_size = placed.child.layout(Constraints::UNBOUNDED);
            let child_paint = placed.child.paint_bounds(child_size);
            translate_rect(child_paint, placed.position)
        });
        let Some(first) = iter.next() else {
            return Rect {
                origin: Vec2::ZERO,
                size: Vec2::ZERO,
            };
        };
        iter.fold(first, union_rect)
    }
}

impl RasterComponent for Layer {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        let intrinsic = match self.size {
            Some(s) => s,
            None => self.children_bounds().size,
        };
        constraints.constrain(intrinsic)
    }

    fn paint_bounds(&self, size: Vec2) -> Rect {
        // For auto-fit, the children's bounding rect *is* the paint
        // region (the layout `size` was derived from it). For fixed
        // size, start from the `(0,0)..size` rect and grow it to
        // include any children that overflow the box.
        if self.size.is_none() {
            return self.children_bounds();
        }
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
        let image = ctx.readback(image);
        composite_at(&mut accum, target, &image, offset_x, offset_y);
    }

    RasterImage::cpu(target.width, target.height, PixelFormat::Rgba8, accum)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Color;
    use crate::placement::VectorPlacement;
    use crate::shapes::Rectangle;
    use crate::vector::Paint;

    fn rect(w: f32, h: f32) -> Rectangle {
        Rectangle {
            size: Vec2(w, h),
            fill: Paint::Solid(Color::rgb_u8(0, 0, 0)).into(),
            stroke: None,
        }
    }

    #[test]
    fn vector_layer_fit_to_single_child_size() {
        let layer = VectorLayer {
            size: None,
            children: vec![rect(80.0, 40.0).at(Vec2(10.0, 20.0))],
        };
        assert_eq!(layer.layout(Constraints::UNBOUNDED), Vec2(80.0, 40.0));
    }

    #[test]
    fn vector_layer_fit_unions_disjoint_children() {
        let layer = VectorLayer {
            size: None,
            children: vec![
                rect(50.0, 50.0).at(Vec2(0.0, 0.0)),
                rect(50.0, 50.0).at(Vec2(100.0, 100.0)),
            ],
        };
        // bounding (0,0)..(150,150) → size (150, 150)
        assert_eq!(layer.layout(Constraints::UNBOUNDED), Vec2(150.0, 150.0));
    }

    #[test]
    fn vector_layer_fit_handles_negative_positions() {
        let layer = VectorLayer {
            size: None,
            children: vec![
                rect(100.0, 100.0).at(Vec2(-30.0, 0.0)),
                rect(100.0, 100.0).at(Vec2(50.0, 20.0)),
            ],
        };
        // bounding (-30,0)..(150,120) → size (180, 120)
        assert_eq!(layer.layout(Constraints::UNBOUNDED), Vec2(180.0, 120.0));
    }

    #[test]
    fn vector_layer_fixed_size_unchanged() {
        let layer = VectorLayer {
            size: Some(Vec2(500.0, 300.0)),
            children: vec![rect(80.0, 40.0).at(Vec2(10.0, 20.0))],
        };
        assert_eq!(layer.layout(Constraints::UNBOUNDED), Vec2(500.0, 300.0));
    }

    #[test]
    fn vector_layer_fit_empty_is_zero() {
        let layer = VectorLayer {
            size: None,
            children: vec![],
        };
        assert_eq!(layer.layout(Constraints::UNBOUNDED), Vec2::ZERO);
    }
}
