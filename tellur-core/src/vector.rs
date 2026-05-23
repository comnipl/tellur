use crate::color::Color;
use crate::component::Component;
use crate::geometry::{Rect, Transform, Vec2};

pub struct VectorGraphic {
    pub view_box: Rect,
    pub root: Node,
}

/// A `Component` that can produce a `VectorGraphic`.
pub trait VectorComponent: Component {
    fn render(&self) -> VectorGraphic;
}

// Compile-time guarantee that `VectorComponent` is dyn-safe.
const _: Option<&dyn VectorComponent> = None;

pub enum Node {
    Group(Group),
    Path(Path),
}

pub struct Group {
    pub transform: Transform,
    pub opacity: f32,
    pub children: Vec<Node>,
}

pub struct Path {
    pub commands: Vec<PathCommand>,
    pub fill: Option<Fill>,
    pub stroke: Option<Stroke>,
    pub transform: Transform,
}

pub enum PathCommand {
    MoveTo(Vec2),
    LineTo(Vec2),
    QuadTo { control: Vec2, to: Vec2 },
    CubicTo { c1: Vec2, c2: Vec2, to: Vec2 },
    Close,
}

pub struct Fill {
    pub paint: Paint,
    pub opacity: f32,
}

pub struct Stroke {
    pub paint: Paint,
    pub width: f32,
}

pub enum Paint {
    Solid(Color),
}
