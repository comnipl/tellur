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

/// A size paired with an anchor on that size, produced by [`Vec2::anchored`].
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
}

/// Per-edge offsets, mirroring CSS's `padding` / `margin` shorthand.
///
/// All values are in the same logical units as [`Vec2`]. Negative values
/// are permitted and produce overhangs (the inner box becomes larger than
/// the outer one), which can be useful for outset effects.
#[derive(Debug, Clone, Copy, PartialEq)]
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
