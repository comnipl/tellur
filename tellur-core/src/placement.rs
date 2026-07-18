//! Placement: snap an anchor on a child to either an absolute point or an
//! anchor on the parent box, then apply an optional pixel offset.
//!
//! [`Positioned`] is the component-system replacement for the old `Placed`
//! `(position, child)` pair. Placement is now expressed as a real component
//! that a parent stores as a plain `Box<dyn _Component>`, so it composes with
//! [`Fragment`](crate::fragment::Fragment), the layout containers, and
//! everything else uniformly — there is no separate "placed" world.
//!
//! The fluent entry points mirror the geometry vocabulary:
//!
//! ```ignore
//! use tellur_core::placement::VectorPlacement;
//!
//! background.place_at(Vec2::ZERO);                       // top-left at a point
//! circle.anchored(Anchor::CENTER).snap_to(target_point); // child anchor → point
//! badge.anchored(Anchor::CENTER).snap_to(Anchor::TOP_RIGHT); // child → parent
//! ```
//!
//! The same `anchored().snap_to()` grammar works in both layout worlds:
//! [`SnapTarget::Point`] keeps the child out of flow and measures it
//! unbounded, while [`SnapTarget::Anchor`] fills the finite parent box and
//! resolves the target against that box at render time.

use crate::geometry::{Anchor, Constraints, Rect, Transform, Vec2};
use crate::layer::translate_rect;
use crate::vector::{Group, Node, VectorComponent, VectorGraphic};
use crate::Keyable;

/// The destination used by [`Positioned`].
#[derive(Debug, Clone, Copy, Keyable)]
pub enum SnapTarget {
    /// An absolute point in the parent's coordinate system (canvas world).
    Point(Vec2),
    /// A proportional point on the parent box, resolved from the size chosen
    /// during layout (flow world).
    Anchor(Anchor),
}

impl SnapTarget {
    fn point(self, size: Vec2) -> Vec2 {
        match self {
            Self::Point(point) => point,
            Self::Anchor(anchor) => anchor.point(size),
        }
    }
}

impl From<Vec2> for SnapTarget {
    fn from(point: Vec2) -> Self {
        Self::Point(point)
    }
}

impl From<Anchor> for SnapTarget {
    fn from(anchor: Anchor) -> Self {
        Self::Anchor(anchor)
    }
}

fn positioned_layout(
    target: SnapTarget,
    constraints: Constraints,
    child_layout: impl FnOnce(Constraints) -> Vec2,
) -> Vec2 {
    match target {
        SnapTarget::Point(_) => child_layout(Constraints::UNBOUNDED),
        SnapTarget::Anchor(_) => constraints.fill_size(),
    }
}

fn resolved_position(
    anchor: Anchor,
    target: SnapTarget,
    offset: Vec2,
    size: Vec2,
    child_size: Vec2,
) -> Vec2 {
    child_size.anchored(anchor).snap_to(target.point(size)) + offset
}

fn resolved_paint_bounds(child_bounds: Rect, position: Vec2) -> Rect {
    translate_rect(child_bounds, position)
}

/// Places a [`VectorComponent`] by snapping `anchor` on the child to `target`,
/// then translating it by `offset`.
///
/// A point target is out of flow and reports the child's unbounded intrinsic
/// size. An anchor target reports the finite parent maximum on each axis (and
/// collapses an unbounded axis to zero), so it can position the child relative
/// to the box chosen by a flow container.
#[derive(Clone, Keyable)]
pub struct Positioned {
    pub child: Box<dyn VectorComponent>,
    pub anchor: Anchor,
    pub target: SnapTarget,
    pub offset: Vec2,
}

impl Positioned {
    pub fn new(
        child: impl Into<Box<dyn VectorComponent>>,
        anchor: Anchor,
        target: impl Into<SnapTarget>,
    ) -> Self {
        Self {
            child: child.into(),
            anchor,
            target: target.into(),
            offset: Vec2::ZERO,
        }
    }

    /// Applies a constant pixel translation after snapping.
    pub fn offset(mut self, offset: Vec2) -> Self {
        self.offset = offset;
        self
    }

    fn child_size(&self, size: Vec2) -> Vec2 {
        match self.target {
            SnapTarget::Point(_) => size,
            SnapTarget::Anchor(_) => self.child.layout(Constraints::loose(size)),
        }
    }

    fn position(&self, size: Vec2, child_size: Vec2) -> Vec2 {
        resolved_position(self.anchor, self.target, self.offset, size, child_size)
    }
}

impl VectorComponent for Positioned {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        positioned_layout(self.target, constraints, |c| self.child.layout(c))
    }

    fn paint_bounds(&self, size: Vec2) -> Rect {
        let child_size = self.child_size(size);
        let position = self.position(size, child_size);
        resolved_paint_bounds(self.child.paint_bounds(child_size), position)
    }

    fn render(&self, size: Vec2) -> VectorGraphic {
        let child_size = self.child_size(size);
        let position = self.position(size, child_size);
        let inner = self.child.render(child_size);
        VectorGraphic {
            view_box: resolved_paint_bounds(inner.view_box, position),
            root: Node::Group(Group {
                transform: Transform::translate(position),
                opacity: 1.0,
                children: vec![inner.root],
            }),
        }
    }
}

impl From<Positioned> for Box<dyn VectorComponent> {
    fn from(positioned: Positioned) -> Self {
        Box::new(positioned)
    }
}

/// Extension trait that adds placement methods to every [`VectorComponent`].
///
/// Brought into scope alongside `use tellur_core::vector::VectorComponent`, it
/// lets callers write `circle.place_at(pos)` or
/// `circle.anchored(Anchor::CENTER).snap_to(target)` to obtain a
/// [`Positioned`] component.
pub trait VectorPlacement: VectorComponent + Sized + 'static {
    /// Places the component so its local origin `(0, 0)` lands at `position`.
    fn place_at(self, position: Vec2) -> Positioned {
        Positioned::new(self.boxed(), Anchor::TOP_LEFT, position)
    }

    /// Begins an anchor-based placement: `anchor` picks a point on the
    /// component's box, which a follow-up `snap_to` aligns onto a point or a
    /// parent-box anchor.
    fn anchored(self, anchor: Anchor) -> AnchoredVectorComponent<Self> {
        AnchoredVectorComponent {
            component: self,
            anchor,
        }
    }
}

impl<T: VectorComponent + 'static> VectorPlacement for T {}

/// Intermediate produced by [`VectorPlacement::anchored`]. Holds the component
/// and the chosen anchor until a snap target is provided.
pub struct AnchoredVectorComponent<C: VectorComponent> {
    component: C,
    anchor: Anchor,
}

impl<C: VectorComponent + 'static> AnchoredVectorComponent<C> {
    /// Places the component so the chosen child anchor lands on `target`.
    pub fn snap_to(self, target: impl Into<SnapTarget>) -> Positioned {
        Positioned::new(self.component.boxed(), self.anchor, target)
    }
}

/// Raster counterparts of the vector placement types. Same shape and
/// semantics, operating on `Box<dyn RasterComponent>`.
///
/// A raster [`Positioned`](raster::Positioned) carries its offset purely in its
/// `paint_bounds` origin; `render` delegates to the child untouched, because
/// the parent's `composite_children` pass applies the offset at compositing
/// time (`position + paint_bounds.origin - paint_rect.origin`).
pub mod raster {
    use super::{positioned_layout, resolved_paint_bounds, resolved_position, SnapTarget};
    use crate::geometry::{Anchor, Constraints, Rect, Vec2};
    use crate::raster::{RasterComponent, RasterImage, RasterResidency, Resolution};
    use crate::render_context::{CachePolicy, RenderContext};
    use crate::Keyable;

    /// Raster mirror of [`super::Positioned`].
    #[derive(Clone, Keyable)]
    pub struct Positioned {
        pub child: Box<dyn RasterComponent>,
        pub anchor: Anchor,
        pub target: SnapTarget,
        pub offset: Vec2,
    }

    impl Positioned {
        pub fn new(
            child: impl Into<Box<dyn RasterComponent>>,
            anchor: Anchor,
            target: impl Into<SnapTarget>,
        ) -> Self {
            Self {
                child: child.into(),
                anchor,
                target: target.into(),
                offset: Vec2::ZERO,
            }
        }

        /// Applies a constant pixel translation after snapping.
        pub fn offset(mut self, offset: Vec2) -> Self {
            self.offset = offset;
            self
        }

        fn child_size(&self, size: Vec2) -> Vec2 {
            match self.target {
                SnapTarget::Point(_) => size,
                SnapTarget::Anchor(_) => self.child.layout(Constraints::loose(size)),
            }
        }

        fn position(&self, size: Vec2, child_size: Vec2) -> Vec2 {
            resolved_position(self.anchor, self.target, self.offset, size, child_size)
        }
    }

    impl RasterComponent for Positioned {
        fn layout(&self, constraints: Constraints) -> Vec2 {
            positioned_layout(self.target, constraints, |c| self.child.layout(c))
        }

        fn paint_bounds(&self, size: Vec2) -> Rect {
            let child_size = self.child_size(size);
            let position = self.position(size, child_size);
            resolved_paint_bounds(self.child.paint_bounds(child_size), position)
        }

        fn cache_policy(&self) -> CachePolicy {
            // Position lives entirely in `paint_bounds`; the wrapper's pixels
            // are exactly the child's, so let the child own the cache entry.
            CachePolicy::Transparent
        }

        fn render(
            &self,
            size: Vec2,
            target: Resolution,
            residency: RasterResidency,
            ctx: &mut dyn RenderContext,
        ) -> RasterImage {
            let child_size = self.child_size(size);
            // The resolved translation rides in `paint_bounds`; the parent
            // applies it while compositing this child image.
            ctx.render(self.child.as_ref(), child_size, target, residency)
        }
    }

    impl From<Positioned> for Box<dyn RasterComponent> {
        fn from(positioned: Positioned) -> Self {
            Box::new(positioned)
        }
    }

    /// Raster mirror of [`VectorPlacement`](super::VectorPlacement).
    pub trait RasterPlacement: RasterComponent + Sized + 'static {
        fn place_at(self, position: Vec2) -> Positioned {
            Positioned::new(self.boxed(), Anchor::TOP_LEFT, position)
        }

        fn anchored(self, anchor: Anchor) -> AnchoredRasterComponent<Self> {
            AnchoredRasterComponent {
                component: self,
                anchor,
            }
        }
    }

    impl<T: RasterComponent + 'static> RasterPlacement for T {}

    /// Intermediate produced by [`RasterPlacement::anchored`].
    pub struct AnchoredRasterComponent<C: RasterComponent> {
        component: C,
        anchor: Anchor,
    }

    impl<C: RasterComponent + 'static> AnchoredRasterComponent<C> {
        pub fn snap_to(self, target: impl Into<SnapTarget>) -> Positioned {
            Positioned::new(self.component.boxed(), self.anchor, target)
        }
    }
}

// Re-export the raster placement trait at the module root so existing
// `use tellur_core::placement::RasterPlacement;` paths keep resolving.
pub use raster::RasterPlacement;

#[cfg(test)]
mod tests {
    use super::raster::RasterPlacement;
    use super::*;
    use crate::color::Color;
    use crate::raster::{PixelFormat, RasterComponent, RasterImage, RasterResidency, Resolution};
    use crate::render_context::{PassThrough, RenderContext};
    use crate::shapes::Circle;
    use crate::vector::{Paint, Stroke};

    fn big_circle() -> Circle {
        // Diameter 1440 — taller than a 1080-high canvas.
        Circle::builder()
            .radius(720.0)
            .fill(Paint::Solid(Color::rgb_u8(0, 0, 0)))
            .build()
    }

    fn root_translation(graphic: &VectorGraphic) -> Vec2 {
        let Node::Group(group) = &graphic.root else {
            panic!("positioned vector should render a translating group");
        };
        Vec2(group.transform.tx, group.transform.ty)
    }

    #[test]
    fn place_at_is_top_left_point_with_zero_offset() {
        let placed = big_circle().place_at(Vec2(12.0, 34.0));

        assert_eq!(placed.anchor, Anchor::TOP_LEFT);
        assert_eq!(placed.target, SnapTarget::Point(Vec2(12.0, 34.0)));
        assert_eq!(placed.offset, Vec2::ZERO);
    }

    #[test]
    fn placed_child_is_measured_unbounded() {
        // The canvas hands loose constraints smaller than the circle; the
        // placed child still reports its intrinsic size instead of getting
        // squashed into an ellipse.
        let placed = big_circle().place_at(Vec2::ZERO);
        let size = placed.layout(Constraints::loose(Vec2(1920.0, 1080.0)));
        assert_eq!(size, Vec2(1440.0, 1440.0));
    }

    #[test]
    fn point_target_resolves_from_unbounded_child_and_translates_paint_bounds() {
        let placed = big_circle()
            .anchored(Anchor::CENTER)
            .snap_to(Vec2(960.0, 540.0))
            .offset(Vec2(13.0, 9.0));
        let size = placed.layout(Constraints::loose(Vec2(1920.0, 1080.0)));
        assert_eq!(size, Vec2(1440.0, 1440.0));

        let expected = Rect {
            origin: Vec2(253.0, -171.0),
            size,
        };
        assert_eq!(placed.paint_bounds(size), expected);

        let graphic = placed.render(size);
        assert_eq!(graphic.view_box, expected);
        assert_eq!(root_translation(&graphic), expected.origin);
    }

    #[test]
    fn point_target_preserves_child_stroke_outset_while_translating_bounds() {
        let placed = Circle::builder()
            .radius(10.0)
            .stroke(Stroke::new(Paint::Solid(Color::rgb_u8(0, 0, 0)), 4.0))
            .build()
            .anchored(Anchor::CENTER)
            .snap_to(Vec2(50.0, 40.0))
            .offset(Vec2(3.0, -4.0));
        let size = placed.layout(Constraints::UNBOUNDED);
        let expected = Rect {
            // Child origin is (43,26); the 4 px centered stroke adds a
            // 2 px outset on every edge.
            origin: Vec2(41.0, 24.0),
            size: Vec2(24.0, 24.0),
        };

        assert_eq!(placed.paint_bounds(size), expected);
        let graphic = placed.render(size);
        assert_eq!(graphic.view_box, expected);
        assert_eq!(root_translation(&graphic), Vec2(43.0, 26.0));
    }

    #[test]
    fn anchor_target_fills_finite_constraints_and_collapses_unbounded_axes() {
        let placed = Circle::builder()
            .radius(10.0)
            .fill(Paint::Solid(Color::rgb_u8(0, 0, 0)))
            .build()
            .anchored(Anchor::CENTER)
            .snap_to(Anchor::BOTTOM_RIGHT);

        assert_eq!(
            placed.layout(Constraints::tight(Vec2(100.0, 60.0))),
            Vec2(100.0, 60.0)
        );
        assert_eq!(
            placed.layout(Constraints::loose(Vec2(100.0, 60.0))),
            Vec2(100.0, 60.0)
        );
        assert_eq!(placed.layout(Constraints::UNBOUNDED), Vec2::ZERO);
        assert_eq!(
            placed.layout(Constraints::loose(Vec2(f32::INFINITY, 60.0))),
            Vec2(0.0, 60.0)
        );
    }

    #[test]
    fn anchor_target_resolves_against_own_size_then_applies_offset() {
        let placed = Circle::builder()
            .radius(10.0)
            .fill(Paint::Solid(Color::rgb_u8(0, 0, 0)))
            .build()
            .anchored(Anchor::CENTER)
            .snap_to(Anchor::BOTTOM_RIGHT)
            .offset(Vec2(-5.0, 3.0));
        let size = Vec2(100.0, 60.0);
        let expected = Rect {
            // (100,60) - child center (10,10) + (-5,3)
            origin: Vec2(85.0, 53.0),
            size: Vec2(20.0, 20.0),
        };

        assert_eq!(placed.paint_bounds(size), expected);
        let graphic = placed.render(size);
        assert_eq!(graphic.view_box, expected);
        assert_eq!(root_translation(&graphic), expected.origin);
    }

    #[derive(Clone, PartialEq, Hash)]
    struct FixedRaster;

    impl RasterComponent for FixedRaster {
        fn layout(&self, constraints: Constraints) -> Vec2 {
            constraints.constrain(Vec2(2000.0, 2000.0))
        }

        fn render(
            &self,
            _s: Vec2,
            t: Resolution,
            _residency: RasterResidency,
            _ctx: &mut dyn RenderContext,
        ) -> RasterImage {
            let bytes = (t.width as usize) * (t.height as usize) * 4;
            RasterImage::cpu(t.width, t.height, PixelFormat::Rgba8, vec![0; bytes])
        }
    }

    #[test]
    fn raster_placed_child_is_measured_unbounded() {
        let placed = FixedRaster
            .place_at(Vec2(7.0, 11.0))
            .offset(Vec2(3.0, -2.0));
        let size = placed.layout(Constraints::loose(Vec2(100.0, 100.0)));
        assert_eq!(size, Vec2(2000.0, 2000.0));
        assert_eq!(
            placed.paint_bounds(size),
            Rect {
                origin: Vec2(10.0, 9.0),
                size,
            }
        );
    }

    #[derive(Clone, PartialEq, Hash)]
    struct SmallRaster;

    impl RasterComponent for SmallRaster {
        fn layout(&self, constraints: Constraints) -> Vec2 {
            constraints.constrain(Vec2(20.0, 10.0))
        }

        fn render(
            &self,
            size: Vec2,
            target: Resolution,
            _residency: RasterResidency,
            _ctx: &mut dyn RenderContext,
        ) -> RasterImage {
            let pixel = [size.0 as u8, size.1 as u8, 0, 255];
            RasterImage::cpu(
                target.width,
                target.height,
                PixelFormat::Rgba8,
                pixel.repeat((target.width * target.height) as usize),
            )
        }
    }

    #[test]
    fn raster_anchor_target_uses_box_for_position_but_child_size_for_pixels() {
        let placed = SmallRaster
            .anchored(Anchor::CENTER)
            .snap_to(Anchor::BOTTOM_RIGHT)
            .offset(Vec2(-5.0, 3.0));
        let size = placed.layout(Constraints::tight(Vec2(100.0, 60.0)));

        assert_eq!(
            placed.paint_bounds(size),
            Rect {
                origin: Vec2(85.0, 58.0),
                size: Vec2(20.0, 10.0),
            }
        );
        let image = placed
            .render(
                size,
                Resolution::new(1, 1),
                RasterResidency::Cpu,
                &mut PassThrough,
            )
            .into_cpu()
            .expect("small raster stays on CPU");
        assert_eq!(image.pixels.as_ref(), &[20, 10, 0, 255]);
    }
}
