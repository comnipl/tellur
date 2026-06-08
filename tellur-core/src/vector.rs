use std::any::Any;
use std::hash::{Hash, Hasher};

use crate::color::Color;
use crate::dyn_compare::{DynEq, DynHash};
use crate::geometry::{Constraints, Rect, Transform, Vec2};
use crate::Keyable;

/// A piece of vector content with a paint-bounds rectangle.
///
/// `view_box` is the rectangle (in the graphic's local coordinate space)
/// that should be rasterized to capture everything the graphic paints.
/// It may have a negative `origin` (e.g. an offset drop shadow that
/// spills to the upper-left) or a `size` larger than the layout size.
/// Place the graphic in a parent coordinate space by composing it
/// through a `Group` transform or a `VectorLayer`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
pub trait VectorComponent: DynEq + DynHash {
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

    /// Display name for this vector component, symmetric with
    /// [`RasterComponent::arrangement_name`](crate::raster::RasterComponent::arrangement_name).
    /// A vector component only reaches a timeline after `.rasterize()` wraps it
    /// in a raster component, so this name is NOT currently threaded into the
    /// arrangement tree (see the macro's vector arm); it exists for symmetry and
    /// future use. `None` by default.
    fn arrangement_name(&self) -> Option<String> {
        None
    }

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

impl PartialEq for dyn VectorComponent {
    fn eq(&self, other: &Self) -> bool {
        DynEq::dyn_eq(self, other.as_any())
    }
}

impl Hash for dyn VectorComponent {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Any::type_id(self.as_any()).hash(state);
        DynHash::dyn_hash(self, state);
    }
}

/// A [`VectorComponent`] wrapped in an affine transform and group opacity.
#[derive(Keyable)]
pub struct Transformed {
    pub transform: Transform,
    pub opacity: f32,
    pub child: Box<dyn VectorComponent>,
}

impl Transformed {
    pub fn new<C: VectorComponent + 'static>(transform: Transform, child: C) -> Self {
        Self {
            transform,
            opacity: 1.0,
            child: Box::new(child),
        }
    }

    pub fn from_box(transform: Transform, child: Box<dyn VectorComponent>) -> Self {
        Self {
            transform,
            opacity: 1.0,
            child,
        }
    }

    pub fn opacity(mut self, opacity: f32) -> Self {
        self.opacity = opacity;
        self
    }
}

impl VectorComponent for Transformed {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        self.child.layout(constraints)
    }

    fn paint_bounds(&self, size: Vec2) -> Rect {
        self.transform.transform_rect(self.child.paint_bounds(size))
    }

    fn render(&self, size: Vec2) -> VectorGraphic {
        let inner = self.child.render(size);
        VectorGraphic {
            view_box: self.transform.transform_rect(inner.view_box),
            root: Node::Group(Group {
                transform: self.transform,
                opacity: self.opacity.clamp(0.0, 1.0),
                children: vec![inner.root],
            }),
        }
    }
}

impl From<Transformed> for Box<dyn VectorComponent> {
    fn from(transformed: Transformed) -> Self {
        Box::new(transformed)
    }
}

/// Extension trait adding transform wrappers to vector components.
pub trait VectorTransform: VectorComponent + Sized + 'static {
    fn transform(self, transform: Transform) -> Transformed {
        Transformed::new(transform, self)
    }

    fn opacity(self, opacity: f32) -> Transformed {
        Transformed::new(Transform::IDENTITY, self).opacity(opacity)
    }
}

impl<T: VectorComponent + 'static> VectorTransform for T {}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Node {
    Group(Group),
    Path(Path),
}

#[derive(Debug, Clone, Keyable)]
pub struct Group {
    pub transform: Transform,
    pub opacity: f32,
    pub children: Vec<Node>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Path {
    pub commands: Vec<PathCommand>,
    pub fill: Option<Fill>,
    pub stroke: Option<Stroke>,
    pub transform: Transform,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PathCommand {
    MoveTo(Vec2),
    LineTo(Vec2),
    QuadTo { control: Vec2, to: Vec2 },
    CubicTo { c1: Vec2, c2: Vec2, to: Vec2 },
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Fill {
    pub paint: Paint,
}

#[derive(Debug, Clone, Keyable)]
pub struct Stroke {
    pub paint: Paint,
    pub width: f32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Paint {
    Solid(Color),
}

impl Paint {
    pub const fn solid(color: Color) -> Self {
        Self::Solid(color)
    }
}

impl From<Color> for Paint {
    fn from(color: Color) -> Self {
        Self::solid(color)
    }
}

impl From<Color> for Option<Paint> {
    fn from(color: Color) -> Self {
        Some(color.into())
    }
}

impl From<Paint> for Fill {
    fn from(paint: Paint) -> Self {
        Self { paint }
    }
}

impl From<Color> for Fill {
    fn from(color: Color) -> Self {
        Paint::from(color).into()
    }
}

impl From<Paint> for Option<Fill> {
    fn from(paint: Paint) -> Self {
        Some(Fill { paint })
    }
}

impl From<Color> for Option<Fill> {
    fn from(color: Color) -> Self {
        Some(color.into())
    }
}

impl From<Paint> for Stroke {
    fn from(paint: Paint) -> Self {
        // Default stroke width mirrors SVG's `stroke-width="1"`.
        Self { paint, width: 1.0 }
    }
}

impl From<Color> for Stroke {
    fn from(color: Color) -> Self {
        Paint::from(color).into()
    }
}

impl From<Paint> for Option<Stroke> {
    fn from(paint: Paint) -> Self {
        Some(paint.into())
    }
}

impl From<Color> for Option<Stroke> {
    fn from(color: Color) -> Self {
        Some(color.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_converts_to_paint_fill_and_stroke() {
        let color = Color::rgb_u8(10, 20, 30);

        assert_eq!(Paint::solid(color), Paint::Solid(color));
        assert_eq!(Paint::from(color), Paint::Solid(color));
        assert_eq!(Fill::from(color).paint, Paint::Solid(color));

        let stroke = Stroke::from(color);
        assert_eq!(stroke.paint, Paint::Solid(color));
        assert_eq!(stroke.width, 1.0);
    }

    #[test]
    fn color_converts_to_optional_paint_styles() {
        let color = Color::rgb_u8(40, 50, 60);

        assert_eq!(Option::<Paint>::from(color), Some(Paint::Solid(color)));
        assert_eq!(
            Option::<Fill>::from(color),
            Some(Fill {
                paint: Paint::Solid(color),
            })
        );
        assert_eq!(
            Option::<Stroke>::from(color),
            Some(Stroke {
                paint: Paint::Solid(color),
                width: 1.0,
            })
        );
    }
}
