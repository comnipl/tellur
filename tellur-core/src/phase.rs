//! [`Phase`] — a finite `f32` constrained to `[0.0, 1.0]`.
//!
//! Phase is the unit-interval scalar Tellur uses as the **input** to easing
//! and interpolation. Easing happens via [`crate::easing::PhaseEasing`],
//! whose methods all take `(self, from, to)` and return `f32` — the
//! Phase-to-physical-quantity bridge lives there, not on `Phase` itself.
//! Phase stays a small "validated fraction" type, owning:
//!
//! - the unit-interval value ([`Phase::get`])
//! - value-space remaps via [`Phase::map`]
//!
//! Phase deliberately knows nothing about seconds. Everything that needs
//! the time interval back — sub-windows carved in seconds, elapsed/remaining
//! durations, post-saturation timing — lives on [`crate::window::Window`],
//! which packages the `[start, end)` interval with the cursor and projects
//! down to a Phase via [`Window::phase`](crate::window::Window::phase).

use std::fmt;
use std::hash::{Hash, Hasher};

use thiserror::Error;

use crate::dyn_compare::hash_f32;
use crate::scalar::clamp_unit;

/// A finite `f32` constrained to the unit interval `[0.0, 1.0]` (both
/// endpoints included).
///
/// Construct via [`Phase::new`] (validating) or [`Phase::saturating`]
/// (clamping); time-driven Phases come out of [`crate::time::Time::phase`]
/// and [`crate::window::Window::phase`].
///
/// `PartialEq`/`Eq`/`Hash` compare the value's bit pattern (with `-0.0`
/// canonicalized to `+0.0` at construction), so a `Phase` is a sound
/// cache-key term.
#[derive(Debug, Clone, Copy)]
pub struct Phase {
    value: f32,
}

impl Phase {
    pub const ZERO: Self = Self { value: 0.0 };
    pub const HALF: Self = Self { value: 0.5 };
    pub const ONE: Self = Self { value: 1.0 };

    /// Returns `Some(Self)` if `v` is finite and within `[0.0, 1.0]`.
    /// `NaN`, `±inf`, or out-of-range values yield `None`.
    pub fn new(v: f32) -> Option<Self> {
        if v.is_finite() && (0.0..=1.0).contains(&v) {
            // Canonicalize -0.0 to +0.0 so the bit-pattern-based `PartialEq`
            // / `Hash` agree with `PartialOrd`, which treats the two zeroes
            // as equal. `+ 0.0` is exact for every other finite value.
            Some(Self { value: v + 0.0 })
        } else {
            None
        }
    }

    /// Clamps `v` to `[0.0, 1.0]`. `NaN` is treated as `0.0`.
    pub fn saturating(v: f32) -> Self {
        Self {
            value: clamp_unit(v),
        }
    }

    /// Returns the inner value, guaranteed to be a finite `f32` in `[0.0, 1.0]`.
    pub const fn get(self) -> f32 {
        self.value
    }

    /// Applies `f` to the inner value and rewraps via saturating clamp.
    /// Used internally by the easing methods and by callers that want a
    /// custom non-linear remap (e.g. `4x(1-x)` for a hat-shaped visibility
    /// curve).
    pub fn map(self, f: impl FnOnce(f32) -> f32) -> Phase {
        Self::saturating(f(self.value))
    }
}

impl PartialEq for Phase {
    fn eq(&self, other: &Self) -> bool {
        self.value.to_bits() == other.value.to_bits()
    }
}

impl Eq for Phase {}

impl Hash for Phase {
    fn hash<H: Hasher>(&self, state: &mut H) {
        hash_f32(self.value, state);
    }
}

impl PartialOrd for Phase {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.value.partial_cmp(&other.value)
    }
}

impl From<Phase> for f32 {
    fn from(p: Phase) -> f32 {
        p.value
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
        write!(f, "{}", self.value)
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
    fn map_remaps_value() {
        let p = Phase::HALF.map(|x| 4.0 * x * (1.0 - x));
        assert!((p.get() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn map_saturates_overshoot() {
        let p = Phase::HALF.map(|_| 2.5);
        assert_eq!(p.get(), 1.0);
        let p = Phase::HALF.map(|_| -1.0);
        assert_eq!(p.get(), 0.0);
    }

    #[test]
    fn negative_zero_equals_positive_zero() {
        let a = Phase::new(-0.0).unwrap();
        let b = Phase::ZERO;
        assert_eq!(a, b);
        use std::collections::hash_map::DefaultHasher;
        let h = |p: Phase| {
            let mut s = DefaultHasher::new();
            p.hash(&mut s);
            s.finish()
        };
        assert_eq!(h(a), h(b));
    }
}
