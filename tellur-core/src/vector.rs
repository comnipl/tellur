use std::any::Any;
use std::hash::{Hash, Hasher};

use crate::color::Color;
use crate::dyn_compare::{DynEq, DynHash};
use crate::geometry::{Anchor, Constraints, Rect, Transform, Vec2};
use crate::scalar::clamp_unit;
use crate::Keyable;

/// A piece of vector content with a paint-bounds rectangle.
///
/// `view_box` is the rectangle (in the graphic's local coordinate space)
/// that should be rasterized to capture everything the graphic paints.
/// A component must set it to the same rectangle returned by
/// [`VectorComponent::paint_bounds`] for the rendered size; `Rasterize`
/// defensively enforces that contract before dispatching to a raster backend.
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
/// with the chosen size to know the layout box plus anything the component
/// paints outside it (useful for `Layer::render` sub-resolution sizing and for
/// rasterize buffer allocation). Implementations should include the layout
/// rectangle `(0, 0)..size` in the returned bounds when they establish a fixed
/// canvas. Transparent auto-fit groups such as
/// [`Fragment`](crate::fragment::Fragment) may instead report only the union
/// of what their children paint.
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
    /// constraints. The returned [`VectorGraphic::view_box`] must equal
    /// [`paint_bounds`](Self::paint_bounds) for this `size`.
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
///
/// Transforms are layout-neutral: `layout` forwards to the child unchanged.
/// The transform is reflected in `paint_bounds`, `render().view_box`, and the
/// emitted node tree, while the untransformed layout box remains inside those
/// bounds. Anchor-based placement therefore snaps the pre-transform intrinsic
/// box; callers that need transformed geometry to affect layout should wrap the
/// transformed component in an explicit container.
///
/// `pivot` anchors the transform on the child's layout box: it is resolved
/// against the laid-out size at paint time, so "rotate around the center"
/// needs no knowledge of the size at the call site (see
/// [`VectorTransform::transform_around`]). The default pivot is the origin
/// ([`Anchor::TOP_LEFT`]), which applies `transform` verbatim.
#[derive(Keyable)]
pub struct Transformed {
    pub transform: Transform,
    pub pivot: Anchor,
    pub opacity: f32,
    pub child: Box<dyn VectorComponent>,
}

impl Transformed {
    pub fn new<C: VectorComponent + 'static>(transform: Transform, child: C) -> Self {
        Self::from_box(transform, Box::new(child))
    }

    pub fn from_box(transform: Transform, child: Box<dyn VectorComponent>) -> Self {
        Self {
            transform,
            pivot: Anchor::TOP_LEFT,
            opacity: 1.0,
            child,
        }
    }

    pub fn opacity(mut self, opacity: f32) -> Self {
        self.opacity = opacity;
        self
    }

    /// The transform with the pivot folded in, resolved against `size`.
    fn effective_transform(&self, size: Vec2) -> Transform {
        if self.pivot == Anchor::TOP_LEFT {
            // The origin pivot is the identity wrapping — keep the raw
            // transform bit-for-bit so existing cache keys and outputs are
            // untouched.
            return self.transform;
        }
        let point = Vec2(size.0 * self.pivot.rx, size.1 * self.pivot.ry);
        Transform::around_point(point, self.transform)
    }
}

impl VectorComponent for Transformed {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        self.child.layout(constraints)
    }

    fn paint_bounds(&self, size: Vec2) -> Rect {
        transformed_bounds(
            size,
            self.child.paint_bounds(size),
            self.effective_transform(size),
        )
    }

    fn render(&self, size: Vec2) -> VectorGraphic {
        // Invisible content culls itself: a zero-opacity (or NaN) group can
        // contribute no ink, so skip building and rendering the subtree.
        if self.opacity.is_nan() || self.opacity <= 0.0 {
            return VectorGraphic {
                view_box: Rect {
                    origin: Vec2::ZERO,
                    size,
                },
                root: Node::empty(),
            };
        }
        let inner = self.child.render(size);
        let transform = self.effective_transform(size);
        VectorGraphic {
            view_box: transformed_bounds(size, inner.view_box, transform),
            root: Node::single_group(transform, self.opacity, inner.root),
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

    /// Like [`transform`](Self::transform), but pivots the transform on
    /// `anchor` of this component's layout box. The pivot is resolved
    /// against the laid-out size at paint time, so spinning a box in place
    /// is `rect.transform_around(Anchor::CENTER, Transform::rotate(a))` —
    /// no size restated, no [`Transform::around_point`] arithmetic.
    fn transform_around(self, anchor: Anchor, transform: Transform) -> Transformed {
        let mut transformed = Transformed::new(transform, self);
        transformed.pivot = anchor;
        transformed
    }

    fn opacity(self, opacity: f32) -> Transformed {
        Transformed::new(Transform::IDENTITY, self).opacity(opacity)
    }
}

impl<T: VectorComponent + 'static> VectorTransform for T {}

fn transformed_bounds(size: Vec2, child_bounds: Rect, transform: Transform) -> Rect {
    let layout_bounds = Rect {
        origin: Vec2::ZERO,
        size,
    };
    let base_bounds = union_rect(layout_bounds, child_bounds);
    union_rect(base_bounds, transform.transform_rect(base_bounds))
}

fn union_rect(a: Rect, b: Rect) -> Rect {
    let a_end = Vec2(a.origin.0 + a.size.0, a.origin.1 + a.size.1);
    let b_end = Vec2(b.origin.0 + b.size.0, b.origin.1 + b.size.1);
    let origin = Vec2(a.origin.0.min(b.origin.0), a.origin.1.min(b.origin.1));
    let end = Vec2(a_end.0.max(b_end.0), a_end.1.max(b_end.1));
    Rect {
        origin,
        size: Vec2(end.0 - origin.0, end.1 - origin.1),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Node {
    Group(Group),
    SingleGroup(SingleGroup),
    ClipGroup(ClipGroup),
    Path(Path),
}

impl Node {
    pub fn single_group(transform: Transform, opacity: f32, child: Node) -> Self {
        Self::SingleGroup(SingleGroup {
            transform,
            opacity: clamp_unit(opacity),
            child: Box::new(child),
        })
    }

    /// A node that paints nothing — what invisible content renders as.
    pub fn empty() -> Self {
        Self::Group(Group {
            transform: Transform::IDENTITY,
            opacity: 1.0,
            children: Vec::new(),
        })
    }

    /// `true` iff this node cannot produce visible ink.
    pub(crate) fn is_empty(&self) -> bool {
        match self {
            Node::Group(group) => group.opacity <= 0.0 || group.children.iter().all(Node::is_empty),
            Node::SingleGroup(group) => group.opacity <= 0.0 || group.child.is_empty(),
            Node::ClipGroup(group) => group.child.is_empty(),
            Node::Path(path) => {
                !path.fill.as_ref().is_some_and(Fill::is_visible)
                    && !path.stroke.as_ref().is_some_and(Stroke::is_visible)
            }
        }
    }
}

#[derive(Debug, Clone, Keyable)]
pub struct Group {
    pub transform: Transform,
    pub opacity: f32,
    pub children: Vec<Node>,
}

#[derive(Debug, Clone, Keyable)]
pub struct SingleGroup {
    pub transform: Transform,
    pub opacity: f32,
    pub child: Box<Node>,
}

#[derive(Debug, Clone, Keyable)]
pub struct ClipGroup {
    pub commands: Vec<PathCommand>,
    pub transform: Transform,
    pub child: Box<Node>,
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

impl Fill {
    /// `true` iff this fill can produce visible ink.
    pub fn is_visible(&self) -> bool {
        self.paint.is_visible()
    }
}

#[derive(Debug, Clone, Keyable)]
pub struct Stroke {
    pub paint: Paint,
    pub width: f32,
    /// Optional dash pattern (SVG `stroke-dasharray`/`stroke-dashoffset`
    /// equivalent). `None` strokes solid.
    pub dash: Option<DashPattern>,
}

impl Stroke {
    pub fn new(paint: impl Into<Paint>, width: f32) -> Self {
        Self {
            paint: paint.into(),
            width,
            dash: None,
        }
    }

    /// `true` iff this stroke can produce visible ink.
    pub fn is_visible(&self) -> bool {
        self.width > 0.0 && self.paint.is_visible()
    }

    /// Returns this stroke with the given dash pattern.
    pub fn with_dash(mut self, dash: DashPattern) -> Self {
        self.dash = Some(dash);
        self
    }
}

/// A dash pattern for [`Stroke`], mirroring SVG's `stroke-dasharray` /
/// `stroke-dashoffset`: `lengths` alternates visible ("on") and gap ("off")
/// run lengths in logical units, starting `offset` units into the pattern.
///
/// An odd number of lengths is a valid SVG dasharray (it is conceptually
/// repeated once so the pattern still alternates on/off) — renderers should
/// read the pattern through [`DashPattern::normalized_lengths`] rather than
/// `lengths` directly, so they see the doubled, always-even sequence.
#[derive(Debug, Clone, Keyable)]
pub struct DashPattern {
    pub lengths: Vec<f32>,
    pub offset: f32,
}

impl DashPattern {
    pub fn new(lengths: impl Into<Vec<f32>>, offset: f32) -> Self {
        Self {
            lengths: lengths.into(),
            offset,
        }
    }

    /// `lengths`, doubled if its length is odd (SVG's `stroke-dasharray`
    /// rule) so the result always alternates on/off. `None` if the pattern
    /// cannot draw any dashes: empty, containing a negative run length, or
    /// summing to zero.
    pub fn normalized_lengths(&self) -> Option<Vec<f32>> {
        if self.lengths.is_empty() || self.lengths.iter().any(|&len| len < 0.0) {
            return None;
        }
        let total: f32 = self.lengths.iter().sum();
        if total <= 0.0 {
            return None;
        }
        if self.lengths.len().is_multiple_of(2) {
            Some(self.lengths.clone())
        } else {
            let mut doubled = self.lengths.clone();
            doubled.extend_from_slice(&self.lengths);
            Some(doubled)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Paint {
    Solid(Color),
}

impl Paint {
    pub const fn solid(color: Color) -> Self {
        Self::Solid(color)
    }

    /// `true` iff this paint can produce visible ink (positive alpha).
    pub fn is_visible(&self) -> bool {
        match self {
            Paint::Solid(c) => c.a > 0.0,
        }
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
        Self {
            paint,
            width: 1.0,
            dash: None,
        }
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
    use crate::shapes::Rectangle;

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
                dash: None,
            })
        );
    }

    #[test]
    fn transformed_layout_is_layout_neutral() {
        let transformed = Rectangle {
            size: Vec2(10.0, 20.0),
            fill: None,
            stroke: None,
        }
        .transform(Transform::translate(Vec2(5.0, 7.0)));

        assert_eq!(transformed.layout(Constraints::UNBOUNDED), Vec2(10.0, 20.0));
        assert_eq!(
            transformed.paint_bounds(Vec2(10.0, 20.0)),
            Rect {
                origin: Vec2::ZERO,
                size: Vec2(15.0, 27.0),
            }
        );
        assert_eq!(
            transformed.render(Vec2(10.0, 20.0)).view_box,
            Rect {
                origin: Vec2::ZERO,
                size: Vec2(15.0, 27.0),
            }
        );
    }

    #[test]
    fn transformed_bounds_do_not_shrink_the_layout_box() {
        let transformed = Rectangle {
            size: Vec2(4.0, 20.0),
            fill: None,
            stroke: None,
        }
        .transform(Transform {
            a: 0.0,
            b: 1.0,
            c: -1.0,
            d: 0.0,
            tx: 12.0,
            ty: 8.0,
        });
        let expected = Rect {
            origin: Vec2(-8.0, 0.0),
            size: Vec2(20.0, 20.0),
        };

        assert_eq!(transformed.layout(Constraints::UNBOUNDED), Vec2(4.0, 20.0));
        assert_eq!(transformed.paint_bounds(Vec2(4.0, 20.0)), expected);
        assert_eq!(transformed.render(Vec2(4.0, 20.0)).view_box, expected);
    }

    #[test]
    fn transform_around_resolves_the_pivot_against_the_size() {
        let angle = 0.5_f32;
        let transformed = Rectangle {
            size: Vec2(4.0, 2.0),
            fill: None,
            stroke: None,
        }
        .transform_around(Anchor::CENTER, Transform::rotate(angle));

        let graphic = transformed.render(Vec2(4.0, 2.0));
        let Node::SingleGroup(group) = graphic.root else {
            panic!("Transformed should render as a single-child group");
        };
        // The emitted transform pivots on the box center (2, 1).
        assert_eq!(
            group.transform,
            Transform::around_point(Vec2(2.0, 1.0), Transform::rotate(angle))
        );
    }

    #[test]
    fn transform_with_origin_pivot_stays_verbatim() {
        let transform = Transform::rotate(0.5);
        let transformed = Rectangle {
            size: Vec2(4.0, 2.0),
            fill: None,
            stroke: None,
        }
        .transform(transform);

        let graphic = transformed.render(Vec2(4.0, 2.0));
        let Node::SingleGroup(group) = graphic.root else {
            panic!("Transformed should render as a single-child group");
        };
        assert_eq!(group.transform, transform);
    }

    #[test]
    fn transformed_nan_or_zero_opacity_renders_nothing() {
        // A fully transparent wrapper culls its subtree (NaN counts as 0).
        for opacity in [0.0, -1.0, f32::NAN] {
            let transformed = Rectangle {
                size: Vec2(1.0, 1.0),
                fill: Option::<Fill>::from(Color::rgb_u8(255, 0, 0)),
                stroke: None,
            }
            .opacity(opacity);
            assert_eq!(transformed.render(Vec2(1.0, 1.0)).root, Node::empty());
        }
    }

    #[test]
    fn invisible_shapes_render_no_ink() {
        // No paint at all, or only an alpha-0 fill: the shape culls itself
        // to an empty node while keeping its layout box as the view box.
        let bare = Rectangle {
            size: Vec2(4.0, 2.0),
            fill: None,
            stroke: None,
        };
        assert_eq!(bare.render(Vec2(4.0, 2.0)).root, Node::empty());

        let ghost = Rectangle {
            size: Vec2(4.0, 2.0),
            fill: Option::<Fill>::from(Color::rgba_u8(255, 0, 0, 0)),
            stroke: None,
        };
        let graphic = ghost.render(Vec2(4.0, 2.0));
        assert_eq!(graphic.root, Node::empty());
        assert_eq!(graphic.view_box.size, Vec2(4.0, 2.0));
    }

    #[test]
    fn invisible_fill_is_dropped_but_visible_stroke_keeps_painting() {
        let outlined = Rectangle {
            size: Vec2(4.0, 2.0),
            fill: Option::<Fill>::from(Color::rgba_u8(255, 0, 0, 0)),
            stroke: Option::<Stroke>::from(Color::rgb_u8(0, 0, 0)),
        };
        let Node::Path(path) = outlined.render(Vec2(4.0, 2.0)).root else {
            panic!("a stroked rectangle still paints");
        };
        assert!(path.fill.is_none());
        assert!(path.stroke.is_some());
    }

    #[test]
    fn stroke_new_has_no_dash() {
        let stroke = Stroke::new(Color::rgb_u8(0, 0, 0), 2.0);
        assert_eq!(stroke.dash, None);

        let dashed = stroke.with_dash(DashPattern::new(vec![4.0, 2.0], 0.0));
        assert!(dashed.dash.is_some());
    }

    #[test]
    fn even_length_dash_pattern_is_unchanged() {
        let dash = DashPattern::new(vec![4.0, 2.0, 1.0, 2.0], 0.0);
        assert_eq!(dash.normalized_lengths(), Some(vec![4.0, 2.0, 1.0, 2.0]));
    }

    #[test]
    fn odd_length_dash_pattern_is_doubled() {
        // SVG's `stroke-dasharray` rule: an odd count is conceptually
        // repeated once so the pattern still alternates on/off.
        let dash = DashPattern::new(vec![18.0], 0.0);
        assert_eq!(dash.normalized_lengths(), Some(vec![18.0, 18.0]));

        let dash = DashPattern::new(vec![10.0, 5.0, 3.0], 0.0);
        assert_eq!(
            dash.normalized_lengths(),
            Some(vec![10.0, 5.0, 3.0, 10.0, 5.0, 3.0])
        );
    }

    #[test]
    fn degenerate_dash_patterns_normalize_to_none() {
        assert_eq!(DashPattern::new(vec![], 0.0).normalized_lengths(), None);
        assert_eq!(
            DashPattern::new(vec![0.0, 0.0], 0.0).normalized_lengths(),
            None
        );
        assert_eq!(
            DashPattern::new(vec![-1.0, 2.0], 0.0).normalized_lengths(),
            None
        );
    }

    #[test]
    fn strokes_with_different_dash_patterns_compare_unequal() {
        let solid = Stroke::new(Color::rgb_u8(0, 0, 0), 2.0);
        let dashed = solid
            .clone()
            .with_dash(DashPattern::new(vec![4.0, 2.0], 0.0));
        assert_ne!(solid, dashed);

        let same_dash = solid.with_dash(DashPattern::new(vec![4.0, 2.0], 0.0));
        assert_eq!(dashed, same_dash);
    }
}
