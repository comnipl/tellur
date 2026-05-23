//! 2D geometric primitives.
//!
//! The project uses a coordinate system with **origin at the top-left and Y axis
//! pointing down**.

use std::ops::{Add, Sub};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vec2(pub f32, pub f32);

impl Vec2 {
    pub const ZERO: Self = Self(0.0, 0.0);

    /// Treats `self` as a size and pairs it with an anchor point on that box,
    /// ready to be snapped to another anchored size via [`AnchoredSize::snap_to`].
    pub fn anchor(self, anchor: Anchor) -> AnchoredSize {
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
#[derive(Debug, Clone, Copy, PartialEq)]
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
#[derive(Debug, Clone, Copy, PartialEq)]
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
}

/// A relative position within an axis-aligned box.
///
/// `(rx, ry)` are fractions in `[0, 1]`: `(0, 0)` is top-left, `(1, 1)` is
/// bottom-right, `(0.5, 0.5)` is the center. Values outside `[0, 1]` are
/// allowed and address points outside the box, which is occasionally useful.
#[derive(Debug, Clone, Copy, PartialEq)]
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
}

/// A size paired with an anchor on that size, produced by [`Vec2::anchor`].
#[derive(Debug, Clone, Copy, PartialEq)]
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

    /// Snaps so that `self.anchor` lands on `target_anchor` of a parent box
    /// of `target_size`. Convenience for the common case where the target
    /// point is expressed as a fractional anchor on a known parent size:
    /// equivalent to `self.snap_to(target_anchor.point(target_size))`.
    pub fn snap_to_anchor(self, target_size: Vec2, target_anchor: Anchor) -> Vec2 {
        self.snap_to(target_anchor.point(target_size))
    }
}
