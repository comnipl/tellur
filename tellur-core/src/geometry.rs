//! 2D geometric primitives.
//!
//! The project uses a coordinate system with **origin at the top-left and Y axis
//! pointing down**.

use std::ops::{Add, Sub};

use crate::Keyable;

#[derive(Debug, Clone, Copy, Keyable)]
pub struct Vec2(pub f32, pub f32);

impl Vec2 {
    pub const ZERO: Self = Self(0.0, 0.0);

    /// Treats `self` as a size and pairs it with an anchor point on that box,
    /// ready to be snapped onto a target point via [`AnchoredSize::snap_to`].
    pub fn anchored(self, anchor: Anchor) -> AnchoredSize {
        AnchoredSize { size: self, anchor }
    }
}

impl Add for Vec2 {
    type Output = Vec2;
    fn add(self, rhs: Vec2) -> Vec2 {
        Vec2(self.0 + rhs.0, self.1 + rhs.1)
    }
}

impl Sub for Vec2 {
    type Output = Vec2;
    fn sub(self, rhs: Vec2) -> Vec2 {
        Vec2(self.0 - rhs.0, self.1 - rhs.1)
    }
}

/// Axis-aligned rectangle.
///
/// `origin` is the top-left corner (the smaller-coordinate side); `origin + size`
/// is the bottom-right corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rect {
    pub origin: Vec2,
    pub size: Vec2,
}

/// 2x3 affine transformation matrix.
///
/// ```text
/// | a c tx |
/// | b d ty |
/// | 0 0  1 |
/// ```
#[derive(Debug, Clone, Copy, Keyable)]
pub struct Transform {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub tx: f32,
    pub ty: f32,
}

impl Transform {
    pub const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        tx: 0.0,
        ty: 0.0,
    };

    pub const fn translate(offset: Vec2) -> Self {
        Self {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            tx: offset.0,
            ty: offset.1,
        }
    }

    pub const fn scale(scale: Vec2) -> Self {
        Self {
            a: scale.0,
            b: 0.0,
            c: 0.0,
            d: scale.1,
            tx: 0.0,
            ty: 0.0,
        }
    }

    pub fn rotate(radians: f32) -> Self {
        let cos = radians.cos();
        let sin = radians.sin();
        Self {
            a: cos,
            b: sin,
            c: -sin,
            d: cos,
            tx: 0.0,
            ty: 0.0,
        }
    }

    /// Matrix concatenation: `self * child`, applying `child` first and then
    /// `self` to points.
    pub fn concat(self, child: Self) -> Self {
        Self {
            a: self.a * child.a + self.c * child.b,
            b: self.b * child.a + self.d * child.b,
            c: self.a * child.c + self.c * child.d,
            d: self.b * child.c + self.d * child.d,
            tx: self.a * child.tx + self.c * child.ty + self.tx,
            ty: self.b * child.tx + self.d * child.ty + self.ty,
        }
    }

    /// Returns a transform that applies `self` first, then `next`.
    pub fn then(self, next: Self) -> Self {
        next.concat(self)
    }

    /// Applies `transform` around `point` instead of the origin.
    pub fn around_point(point: Vec2, transform: Self) -> Self {
        Self::translate(point)
            .concat(transform)
            .concat(Self::translate(Vec2(-point.0, -point.1)))
    }

    pub fn transform_point(self, point: Vec2) -> Vec2 {
        Vec2(
            self.a * point.0 + self.c * point.1 + self.tx,
            self.b * point.0 + self.d * point.1 + self.ty,
        )
    }

    pub fn transform_rect(self, rect: Rect) -> Rect {
        let p0 = self.transform_point(rect.origin);
        let p1 = self.transform_point(Vec2(rect.origin.0 + rect.size.0, rect.origin.1));
        let p2 = self.transform_point(Vec2(rect.origin.0, rect.origin.1 + rect.size.1));
        let p3 = self.transform_point(Vec2(
            rect.origin.0 + rect.size.0,
            rect.origin.1 + rect.size.1,
        ));

        let min_x = p0.0.min(p1.0).min(p2.0).min(p3.0);
        let max_x = p0.0.max(p1.0).max(p2.0).max(p3.0);
        let min_y = p0.1.min(p1.1).min(p2.1).min(p3.1);
        let max_y = p0.1.max(p1.1).max(p2.1).max(p3.1);

        Rect {
            origin: Vec2(min_x, min_y),
            size: Vec2(max_x - min_x, max_y - min_y),
        }
    }
}

/// A relative position within an axis-aligned box.
///
/// `(rx, ry)` are fractions in `[0, 1]`: `(0, 0)` is top-left, `(1, 1)` is
/// bottom-right, `(0.5, 0.5)` is the center. Values outside `[0, 1]` are
/// allowed and address points outside the box, which is occasionally useful.
#[derive(Debug, Clone, Copy, Keyable)]
pub struct Anchor {
    pub rx: f32,
    pub ry: f32,
}

impl Anchor {
    pub const TOP_LEFT: Self = Self::new(0.0, 0.0);
    pub const TOP_CENTER: Self = Self::new(0.5, 0.0);
    pub const TOP_RIGHT: Self = Self::new(1.0, 0.0);
    pub const CENTER_LEFT: Self = Self::new(0.0, 0.5);
    pub const CENTER: Self = Self::new(0.5, 0.5);
    pub const CENTER_RIGHT: Self = Self::new(1.0, 0.5);
    pub const BOTTOM_LEFT: Self = Self::new(0.0, 1.0);
    pub const BOTTOM_CENTER: Self = Self::new(0.5, 1.0);
    pub const BOTTOM_RIGHT: Self = Self::new(1.0, 1.0);

    pub const fn new(rx: f32, ry: f32) -> Self {
        Self { rx, ry }
    }

    /// Returns the absolute point this anchor refers to within a box of the
    /// given size, assuming the box's top-left is at the origin.
    pub fn point(self, size: Vec2) -> Vec2 {
        Vec2(size.0 * self.rx, size.1 * self.ry)
    }

    /// Pairs this anchor (on the child) with an anchor on the surrounding box,
    /// producing the [`Alignment`] that snaps the former onto the latter:
    /// `Anchor::CENTER.to(Anchor::BOTTOM_RIGHT)` pins the child's center to the
    /// box's bottom-right corner.
    pub const fn to(self, at: Anchor) -> Alignment {
        Alignment { child: self, at }
    }
}

/// How a child box is aligned inside a surrounding box: the `child` anchor
/// (a point on the child) is snapped onto the `at` anchor (a point on the
/// surrounding box).
///
/// The common case where both anchors coincide — centering, corner-pinning —
/// converts straight from a single [`Anchor`] (`Alignment::from(Anchor::CENTER)`
/// or just passing an `Anchor` to an `impl Into<Alignment>` slot). Build the
/// asymmetric form with [`Anchor::to`].
#[derive(Debug, Clone, Copy, Keyable)]
pub struct Alignment {
    /// The anchor on the child box.
    pub child: Anchor,
    /// The anchor on the surrounding box the child anchor lands on.
    pub at: Anchor,
}

impl Alignment {
    pub const TOP_LEFT: Self = Self::uniform(Anchor::TOP_LEFT);
    pub const CENTER: Self = Self::uniform(Anchor::CENTER);

    pub const fn new(child: Anchor, at: Anchor) -> Self {
        Self { child, at }
    }

    /// The same anchor on both boxes — centering, corner- or edge-pinning.
    pub const fn uniform(anchor: Anchor) -> Self {
        Self {
            child: anchor,
            at: anchor,
        }
    }
}

impl From<Anchor> for Alignment {
    fn from(anchor: Anchor) -> Self {
        Self::uniform(anchor)
    }
}

/// A size paired with an anchor on that size, produced by [`Vec2::anchored`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AnchoredSize {
    pub size: Vec2,
    pub anchor: Anchor,
}

impl AnchoredSize {
    /// Computes the offset for a box of `self.size` so that `self.anchor`
    /// on it lands on `target_point` (already in the parent's coordinate
    /// space). The returned `Vec2` is the top-left position of the placed
    /// box in that coordinate space.
    pub fn snap_to(self, target_point: Vec2) -> Vec2 {
        target_point - self.anchor.point(self.size)
    }
}

/// Per-edge offsets, mirroring CSS's `padding` / `margin` shorthand.
///
/// All values are in the same logical units as [`Vec2`]. Negative values
/// are permitted and produce overhangs (the inner box becomes larger than
/// the outer one), which can be useful for outset effects.
#[derive(Debug, Clone, Copy, Keyable)]
pub struct EdgeInsets {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl EdgeInsets {
    pub const ZERO: Self = Self {
        left: 0.0,
        top: 0.0,
        right: 0.0,
        bottom: 0.0,
    };

    /// Same inset on every side.
    pub const fn all(value: f32) -> Self {
        Self {
            left: value,
            top: value,
            right: value,
            bottom: value,
        }
    }

    /// Independent horizontal (left/right) and vertical (top/bottom) insets.
    pub const fn symmetric(horizontal: f32, vertical: f32) -> Self {
        Self {
            left: horizontal,
            top: vertical,
            right: horizontal,
            bottom: vertical,
        }
    }

    /// Explicit per-side construction, in CSS order (left, top, right, bottom).
    pub const fn only(left: f32, top: f32, right: f32, bottom: f32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    /// Total horizontal inset (`left + right`).
    pub fn horizontal(&self) -> f32 {
        self.left + self.right
    }

    /// Total vertical inset (`top + bottom`).
    pub fn vertical(&self) -> f32 {
        self.top + self.bottom
    }

    /// The top-left corner offset, i.e. where the inset content begins
    /// relative to the outer box's origin.
    pub fn top_left(&self) -> Vec2 {
        Vec2(self.left, self.top)
    }
}

/// Layout constraints handed from a parent to a child during the
/// `layout` pass. The child must return a size in the closed interval
/// `[min, max]` on each axis. `max` may be `f32::INFINITY` to express
/// "no upper bound" (the parent does not constrain this axis); `min` is
/// usually `0.0` for "no lower bound" and equals `max` for fully tight
/// constraints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Constraints {
    pub min: Vec2,
    pub max: Vec2,
}

impl Constraints {
    /// No upper bound on either axis. Children fall back to their
    /// intrinsic size.
    pub const UNBOUNDED: Self = Self {
        min: Vec2::ZERO,
        max: Vec2(f32::INFINITY, f32::INFINITY),
    };

    /// Tight constraints: the child must use exactly `size`.
    pub const fn tight(size: Vec2) -> Self {
        Self {
            min: size,
            max: size,
        }
    }

    /// Loose constraints: the child may use anywhere from zero up to
    /// `max` on each axis.
    pub const fn loose(max: Vec2) -> Self {
        Self {
            min: Vec2::ZERO,
            max,
        }
    }

    /// Clamps `size` into `[min, max]` on each axis. Children pass their
    /// preferred intrinsic size through this to obtain a legal result.
    pub fn constrain(&self, size: Vec2) -> Vec2 {
        Vec2(
            size.0.clamp(self.min.0, self.max.0),
            size.1.clamp(self.min.1, self.max.1),
        )
    }

    /// Tightens the constraints' max to the provided size on each axis
    /// (capped at the existing max), and clamps min not to exceed the
    /// new max.
    pub fn with_max(&self, max: Vec2) -> Self {
        let new_max = Vec2(max.0.min(self.max.0), max.1.min(self.max.1));
        Self {
            min: Vec2(self.min.0.min(new_max.0), self.min.1.min(new_max.1)),
            max: new_max,
        }
    }

    /// Shrinks `max` by `by` on each axis (clamped to zero from below).
    /// Used by `Padding` to subtract its insets before laying out the
    /// child.
    pub fn shrink(&self, by: Vec2) -> Self {
        let new_max = Vec2((self.max.0 - by.0).max(0.0), (self.max.1 - by.1).max(0.0));
        Self {
            min: Vec2((self.min.0 - by.0).max(0.0), (self.min.1 - by.1).max(0.0)),
            max: new_max,
        }
    }

    /// Replaces the cross-axis bound with a tight `value` while leaving
    /// the main axis unchanged. Used by `Flex`'s `CrossAlign::Stretch`.
    pub fn tighten_cross(&self, axis: Axis, value: f32) -> Self {
        match axis {
            Axis::Horizontal => Self {
                min: Vec2(self.min.0, value),
                max: Vec2(self.max.0, value),
            },
            Axis::Vertical => Self {
                min: Vec2(value, self.min.1),
                max: Vec2(value, self.max.1),
            },
        }
    }

    /// Replaces the main-axis bound with a tight `value` while leaving the
    /// cross axis unchanged. Used by `Flex` to hand a flexible child its
    /// share of the leftover main-axis space.
    pub fn tighten_main(&self, axis: Axis, value: f32) -> Self {
        match axis {
            Axis::Horizontal => Self {
                min: Vec2(value, self.min.1),
                max: Vec2(value, self.max.1),
            },
            Axis::Vertical => Self {
                min: Vec2(self.min.0, value),
                max: Vec2(self.max.0, value),
            },
        }
    }
}

/// One of the two axes of a 2D coordinate system. Re-exported by the
/// layout module for stack-axis selection, but kept in `geometry` so
/// helpers like [`Constraints::tighten_cross`] can refer to it without a
/// dependency cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::FRAC_PI_2;

    fn assert_near(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 1e-5,
            "expected {expected}, got {actual}"
        );
    }

    fn assert_vec_near(actual: Vec2, expected: Vec2) {
        assert_near(actual.0, expected.0);
        assert_near(actual.1, expected.1);
    }

    #[test]
    fn then_applies_transforms_in_reading_order() {
        let transform = Transform::scale(Vec2(2.0, 3.0)).then(Transform::rotate(FRAC_PI_2));
        assert_vec_near(transform.transform_point(Vec2(1.0, 0.0)), Vec2(0.0, 2.0));
    }

    #[test]
    fn around_point_keeps_pivot_fixed() {
        let pivot = Vec2(10.0, 20.0);
        let transform = Transform::around_point(pivot, Transform::scale(Vec2(2.0, 3.0)));
        assert_vec_near(transform.transform_point(pivot), pivot);
        assert_vec_near(
            transform.transform_point(Vec2(11.0, 21.0)),
            Vec2(12.0, 23.0),
        );
    }

    #[test]
    fn transform_rect_returns_axis_aligned_bounds() {
        let rect = Rect {
            origin: Vec2(0.0, 0.0),
            size: Vec2(2.0, 4.0),
        };
        let bounds = Transform::rotate(FRAC_PI_2).transform_rect(rect);
        assert_vec_near(bounds.origin, Vec2(-4.0, 0.0));
        assert_vec_near(bounds.size, Vec2(4.0, 2.0));
    }
}
