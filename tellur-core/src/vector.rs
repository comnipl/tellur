use crate::color::Color;
use crate::geometry::{Transform, Vec2};

/// A piece of vector content with an intrinsic size.
///
/// The graphic's coordinate space spans `(0, 0)..view_box` (top-left origin).
/// Anything outside that box may still be present in the path commands but
/// will be clipped when rasterized into the box-sized output region. Place
/// the graphic in a parent coordinate space by composing it through a
/// `Group` transform or a `VectorLayer`.
#[derive(Debug, Clone)]
pub struct VectorGraphic {
    pub view_box: Vec2,
    pub root: Node,
}

/// A component that can produce a `VectorGraphic`.
///
/// Two roles share this trait. **Element components** are leaves that own a
/// concrete `render()` implementation (e.g. `Rectangle`, `VectorLayer`); they
/// keep the default `body()` returning `VectorBody::Element` and override
/// `render()` themselves. **Composite components** are usually user-defined
/// and delegate to another component by overriding `body()` to return
/// `VectorBody::Of(...)`; the default `render()` then walks the chain until
/// it reaches an element.
///
/// Implementors must keep `view_box()` consistent with `render().view_box`,
/// so callers can query the intrinsic size without paying for a full render.
pub trait VectorComponent {
    fn view_box(&self) -> Vec2;

    /// Identifies whether this component is a render-owning element or a
    /// delegate to another component. Default is `Element` so that leaf types
    /// only need to override `render()`.
    fn body(&self) -> VectorBody {
        VectorBody::Element
    }

    /// Produces the flattened graphic. The default walks `body()` until it
    /// reaches an `Element`; elements must override this with their own
    /// implementation.
    fn render(&self) -> VectorGraphic {
        match self.body() {
            VectorBody::Element => unimplemented!(
                "VectorComponent with body() == VectorBody::Element must override render()"
            ),
            VectorBody::Of(c) => c.render(),
        }
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

/// Discriminates element components (own their `render`) from composite
/// components (delegate to another component).
pub enum VectorBody {
    /// This component owns a concrete `render()` implementation.
    Element,
    /// Delegate to another component — `render()` walks through it.
    Of(Box<dyn VectorComponent>),
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
