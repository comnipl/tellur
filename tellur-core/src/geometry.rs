//! 2D geometric primitives.
//!
//! The project uses a coordinate system with **origin at the top-left and Y axis
//! pointing down**.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vec2(pub f32, pub f32);

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
}
