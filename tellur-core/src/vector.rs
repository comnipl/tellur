use crate::color::Color;
use crate::geometry::{Constraints, Rect, Transform, Vec2};

/// A piece of vector content with a paint-bounds rectangle.
///
/// `view_box` is the rectangle (in the graphic's local coordinate space)
/// that should be rasterized to capture everything the graphic paints.
/// It may have a negative `origin` (e.g. an offset drop shadow that
/// spills to the upper-left) or a `size` larger than the layout size.
/// Place the graphic in a parent coordinate space by composing it
/// through a `Group` transform or a `VectorLayer`.
#[derive(Debug, Clone)]
pub struct VectorGraphic {
    pub view_box: Rect,
    pub root: Node,
}

/// A component that can produce a `VectorGraphic` through a two-pass
/// constraint-based layout protocol:
///
/// 1. The parent calls [`layout`](VectorComponent::layout) with a
///    [`Constraints`] block. The child returns the size it wants within
///    those constraints.
/// 2. The parent calls [`render`](VectorComponent::render) with the
///    chosen size to obtain the graphic.
///
/// Optionally the parent calls [`paint_bounds`](VectorComponent::paint_bounds)
/// with the chosen size to know how far the component paints outside the
/// layout box (useful for `Layer::render` sub-resolution sizing and for
/// rasterize buffer allocation).
///
/// Element components implement `layout` and `render` directly. Composite
/// components (produced by `#[vector_component]`) usually do the same,
/// internally building a child component and forwarding the protocol.
pub trait VectorComponent {
    /// Decide the layout size for this component given the parent's
    /// constraints. The returned `Vec2` must satisfy `min <= size <= max`
    /// on each axis.
    fn layout(&self, constraints: Constraints) -> Vec2;

    /// Paint bounds for the component once `size` has been chosen. The
    /// default returns a rectangle whose `origin` is `(0, 0)` and whose
    /// `size` equals the layout size. Effects that paint outside the
    /// layout box (drop shadows, blurs) override this to widen the
    /// rectangle.
    fn paint_bounds(&self, size: Vec2) -> Rect {
        Rect {
            origin: Vec2::ZERO,
            size,
        }
    }

    /// Produce the flattened graphic at `size`. `size` is always the
    /// value previously returned by `layout` for the same constraints,
    /// so children may rely on it without re-checking against the
    /// constraints.
    fn render(&self, size: Vec2) -> VectorGraphic;

    /// Type-erases `self` into a heap-allocated trait object. Useful for
    /// constructing heterogeneous containers like `VectorLayer.children`
    /// in struct-literal form.
    fn boxed(self) -> Box<dyn VectorComponent>
    where
        Self: Sized + 'static,
    {
        Box::new(self)
    }
}

// Compile-time guarantee that `VectorComponent` is dyn-safe.
const _: Option<&dyn VectorComponent> = None;

#[derive(Debug, Clone)]
pub enum Node {
    Group(Group),
    Path(Path),
}

#[derive(Debug, Clone)]
pub struct Group {
    pub transform: Transform,
    pub opacity: f32,
    pub children: Vec<Node>,
}

#[derive(Debug, Clone)]
pub struct Path {
    pub commands: Vec<PathCommand>,
    pub fill: Option<Fill>,
    pub stroke: Option<Stroke>,
    pub transform: Transform,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PathCommand {
    MoveTo(Vec2),
    LineTo(Vec2),
    QuadTo { control: Vec2, to: Vec2 },
    CubicTo { c1: Vec2, c2: Vec2, to: Vec2 },
    Close,
}

#[derive(Debug, Clone)]
pub struct Fill {
    pub paint: Paint,
}

#[derive(Debug, Clone)]
pub struct Stroke {
    pub paint: Paint,
    pub width: f32,
}

#[derive(Debug, Clone)]
pub enum Paint {
    Solid(Color),
}

impl From<Paint> for Fill {
    fn from(paint: Paint) -> Self {
        Self { paint }
    }
}

impl From<Paint> for Option<Fill> {
    fn from(paint: Paint) -> Self {
        Some(Fill { paint })
    }
}

impl From<Paint> for Stroke {
    fn from(paint: Paint) -> Self {
        // Default stroke width mirrors SVG's `stroke-width="1"`.
        Self { paint, width: 1.0 }
    }
}

impl From<Paint> for Option<Stroke> {
    fn from(paint: Paint) -> Self {
        Some(paint.into())
    }
}
