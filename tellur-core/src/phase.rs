//! [`Phase`] — a finite `f32` constrained to `[0.0, 1.0]`.
//!
//! Intended for normalized scalars such as animation progress, easing
//! parameters, and similar "fraction of something" quantities. The type
//! is a newtype wrapper that validates at construction so downstream code
//! can rely on the invariant without re-checking.

use std::fmt;

use thiserror::Error;

use crate::interpolate::Interpolate;

/// A finite `f32` constrained to the unit interval `[0.0, 1.0]` (both
/// endpoints included).
///
/// Construct via [`Phase::new`] (validating) or [`Phase::saturating`]
/// (clamping). The wrapped value is accessible through [`Phase::get`] and
/// `Into<f32>`.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Phase(f32);

impl Phase {
    pub const ZERO: Self = Self(0.0);
    pub const HALF: Self = Self(0.5);
    pub const ONE: Self = Self(1.0);

    /// Returns `Some(Self)` if `v` is finite and within `[0.0, 1.0]`.
    /// `NaN`, `±inf`, or out-of-range values yield `None`.
    pub fn new(v: f32) -> Option<Self> {
        if v.is_finite() && (0.0..=1.0).contains(&v) {
            Some(Self(v))
        } else {
            None
        }
    }

    /// Clamps `v` to `[0.0, 1.0]`. `NaN` is treated as `0.0`.
    pub fn saturating(v: f32) -> Self {
        if v.is_nan() {
            Self::ZERO
        } else {
            Self(v.clamp(0.0, 1.0))
        }
    }

    /// Returns the inner value, guaranteed to be a finite `f32` in `[0.0, 1.0]`.
    pub const fn get(self) -> f32 {
        self.0
    }

    /// Returns `1.0 - self`. The result is still a valid `Phase` (the
    /// closed unit interval is closed under this reflection).
    pub fn invert(self) -> Self {
        Self(1.0 - self.0)
    }

    /// Linearly interpolates between `from` (at `self == 0`) and `to`
    /// (at `self == 1`). Convenience wrapper for `from.interpolate(to, self)`
    /// so the `Phase` reads as "the driver" in animation code.
    pub fn interpolate<T: Interpolate>(self, from: T, to: T) -> T {
        from.interpolate(to, self)
    }
}

impl From<Phase> for f32 {
    fn from(p: Phase) -> f32 {
        p.0
    }
}

impl TryFrom<f32> for Phase {
    type Error = PhaseOutOfRange;
    fn try_from(v: f32) -> Result<Self, Self::Error> {
        Self::new(v).ok_or(PhaseOutOfRange(v))
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Error)]
#[error("phase value {0} is outside [0.0, 1.0] or non-finite")]
pub struct PhaseOutOfRange(pub f32);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_in_range() {
        assert_eq!(Phase::new(0.0).map(Phase::get), Some(0.0));
        assert_eq!(Phase::new(0.5).map(Phase::get), Some(0.5));
        assert_eq!(Phase::new(1.0).map(Phase::get), Some(1.0));
    }

    #[test]
    fn new_rejects_out_of_range_and_non_finite() {
        assert!(Phase::new(-0.0001).is_none());
        assert!(Phase::new(1.0001).is_none());
        assert!(Phase::new(f32::NAN).is_none());
        assert!(Phase::new(f32::INFINITY).is_none());
        assert!(Phase::new(f32::NEG_INFINITY).is_none());
    }

    #[test]
    fn saturating_clamps_and_handles_nan() {
        assert_eq!(Phase::saturating(-1.0).get(), 0.0);
        assert_eq!(Phase::saturating(2.0).get(), 1.0);
        assert_eq!(Phase::saturating(0.5).get(), 0.5);
        assert_eq!(Phase::saturating(f32::NAN).get(), 0.0);
    }

    #[test]
    fn invert_is_one_minus() {
        assert_eq!(Phase::ZERO.invert(), Phase::ONE);
        assert_eq!(Phase::ONE.invert(), Phase::ZERO);
        assert_eq!(
            Phase::new(0.25).unwrap().invert(),
            Phase::new(0.75).unwrap()
        );
    }

    #[test]
    fn try_from_reports_offending_value() {
        let err = Phase::try_from(2.5).unwrap_err();
        assert_eq!(err.0, 2.5);
    }

    #[test]
    fn into_f32_round_trips_via_get() {
        let p = Phase::new(0.42).unwrap();
        let v: f32 = p.into();
        assert_eq!(v, p.get());
    }

    #[test]
    fn consts_are_correct() {
        assert_eq!(Phase::ZERO.get(), 0.0);
        assert_eq!(Phase::HALF.get(), 0.5);
        assert_eq!(Phase::ONE.get(), 1.0);
    }

    #[test]
    fn interpolate_drives_f32_value() {
        assert_eq!(Phase::HALF.interpolate(0.0, 10.0), 5.0);
        assert_eq!(Phase::ZERO.interpolate(0.0, 10.0), 0.0);
        assert_eq!(Phase::ONE.interpolate(0.0, 10.0), 10.0);
    }
}
