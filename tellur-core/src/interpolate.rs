//! [`Interpolate`] — linear interpolation parameterized by [`Phase`].
//!
//! The [`Phase`] holds the "where am I between 0 and 1", and the
//! [`Interpolate`] implementation knows how to combine two values of its
//! type given that fraction. Provide an impl for a new type so callers can
//! lerp it with a Phase: `a.interpolate(b, phase)`.
//!
//! For `f32`, the canonical entry point is
//! [`PhaseEasing::linear`](crate::easing::PhaseEasing::linear) (and the
//! eased variants on the same trait) — they wrap this trait so callers
//! reach `phase.linear(from, to)` directly. This trait exists primarily so
//! richer value types (`Vec2`, `Anchor`, …) can be lerped the same way. To
//! ease a typed interpolation, reshape the Phase first via
//! [`Phase::eased`]: `a.interpolate(b, p.eased(Easing::OutCubic))`.

use crate::color::Color;
use crate::geometry::{Anchor, Vec2};
use crate::phase::Phase;

/// Linear interpolation between two values of the same type, parameterized
/// by a [`Phase`].
pub trait Interpolate {
    /// Returns the value at `p` between `self` (at `p == 0`) and `other`
    /// (at `p == 1`).
    fn interpolate(self, other: Self, p: Phase) -> Self;
}

impl Interpolate for f32 {
    fn interpolate(self, other: Self, p: Phase) -> Self {
        self + (other - self) * p.get()
    }
}

impl Interpolate for Vec2 {
    fn interpolate(self, other: Self, p: Phase) -> Self {
        Vec2(
            self.0.interpolate(other.0, p),
            self.1.interpolate(other.1, p),
        )
    }
}

impl Interpolate for Anchor {
    fn interpolate(self, other: Self, p: Phase) -> Self {
        Anchor::new(
            self.rx.interpolate(other.rx, p),
            self.ry.interpolate(other.ry, p),
        )
    }
}

/// Straight per-channel lerp in sRGB space (the same numbers [`Color`]
/// already stores) — NOT a linear-light blend. Mixing in sRGB is what a
/// hand-rolled `r + (other.r - r) * t` lerp does, so this matches every
/// existing manual color lerp in the codebase byte-for-byte; it does not
/// convert to linear light and back the way a "physically correct" color
/// mix would.
impl Interpolate for Color {
    fn interpolate(self, other: Self, p: Phase) -> Self {
        Color {
            r: self.r.interpolate(other.r, p),
            g: self.g.interpolate(other.g, p),
            b: self.b.interpolate(other.b, p),
            a: self.a.interpolate(other.a, p),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f32_endpoints() {
        assert_eq!(2.0_f32.interpolate(5.0, Phase::ZERO), 2.0);
        assert_eq!(2.0_f32.interpolate(5.0, Phase::ONE), 5.0);
    }

    #[test]
    fn f32_half_is_midpoint() {
        assert_eq!(2.0_f32.interpolate(5.0, Phase::HALF), 3.5);
    }

    #[test]
    fn f32_handles_negative_direction() {
        // from=5, to=2: at half should be 3.5.
        assert_eq!(5.0_f32.interpolate(2.0, Phase::HALF), 3.5);
    }

    #[test]
    fn vec2_endpoints() {
        let a = Vec2(0.0, 10.0);
        let b = Vec2(10.0, 0.0);
        assert_eq!(a.interpolate(b, Phase::ZERO), a);
        assert_eq!(a.interpolate(b, Phase::ONE), b);
    }

    #[test]
    fn vec2_componentwise_midpoint() {
        let a = Vec2(0.0, 10.0);
        let b = Vec2(10.0, 0.0);
        let mid = a.interpolate(b, Phase::HALF);
        assert_eq!(mid, Vec2(5.0, 5.0));
    }

    #[test]
    fn anchor_endpoints() {
        let left = Anchor::CENTER_LEFT;
        let right = Anchor::CENTER_RIGHT;
        assert_eq!(left.interpolate(right, Phase::ZERO), left);
        assert_eq!(left.interpolate(right, Phase::ONE), right);
    }

    #[test]
    fn anchor_midpoint_is_center() {
        // Halfway between left-center and right-center should be CENTER.
        let mid = Anchor::CENTER_LEFT.interpolate(Anchor::CENTER_RIGHT, Phase::HALF);
        assert_eq!(mid, Anchor::CENTER);
    }

    #[test]
    fn color_endpoints() {
        let a = Color::rgba_u8(0, 0, 0, 0);
        let b = Color::rgba_u8(255, 255, 255, 255);
        assert_eq!(a.interpolate(b, Phase::ZERO), a);
        assert_eq!(a.interpolate(b, Phase::ONE), b);
    }

    #[test]
    fn color_componentwise_midpoint_includes_alpha() {
        let a = Color {
            r: 0.0,
            g: 0.2,
            b: 1.0,
            a: 0.0,
        };
        let b = Color {
            r: 1.0,
            g: 0.8,
            b: 0.0,
            a: 1.0,
        };
        let mid = a.interpolate(b, Phase::HALF);
        assert_eq!(mid.r, 0.5);
        assert_eq!(mid.g, 0.5);
        assert_eq!(mid.b, 0.5);
        assert_eq!(mid.a, 0.5);
    }

    #[test]
    fn color_matches_a_hand_rolled_srgb_lerp() {
        // The exact shape the ported source (`PersistentPiScene::pi_color` in
        // movies/202606/shorts_sqrt2_plus_sqrt3/src/explanation.rs) computed
        // by hand: independent per-channel `a + (b - a) * t`.
        let ink = Color::rgb_u8(24, 28, 36);
        let muted = Color::rgb_u8(112, 121, 132);
        let t = 0.3;
        let p = Phase::new(t).unwrap();

        let expected = Color {
            r: ink.r + (muted.r - ink.r) * t,
            g: ink.g + (muted.g - ink.g) * t,
            b: ink.b + (muted.b - ink.b) * t,
            a: ink.a + (muted.a - ink.a) * t,
        };
        assert_eq!(ink.interpolate(muted, p), expected);
    }
}
