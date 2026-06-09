//! [`Phase`] — a finite `f32` constrained to `[0.0, 1.0]`, optionally tagged
//! with the seconds-extent of the time window it was sampled from.
//!
//! Phase is the unit-interval scalar Tellur uses as the **input** to easing
//! and interpolation. Easing happens via [`crate::easing::PhaseEasing`],
//! whose methods all take `(self, from, to)` and return `f32` — the
//! Phase-to-physical-quantity bridge lives there, not on `Phase` itself.
//! Phase stays a small "fraction with metadata" type, owning:
//!
//! - the unit-interval value ([`Phase::get`])
//! - the source window's seconds-width ([`Phase::width`]), set by
//!   [`crate::time::Time::phase`], used to carve sub-windows by elapsed
//!   seconds via [`Phase::sub_secs`]
//! - in-Phase remaps via [`Phase::map`] and predicates
//!   ([`Phase::is_active`] etc.) for gating and threshold checks
//!
//! ### Past saturation
//!
//! Phase is saturating by design: it forgets how far the cursor traveled
//! past the window end. For "5 seconds after this envelope saturates"-type
//! timing, reach for [`crate::window::Window`] — it packages the same
//! `[start, end)` interval with the cursor so `Window::elapsed` and
//! `Window::after` are available alongside the Phase.

use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Range;

use thiserror::Error;

use crate::dyn_compare::hash_f32;
use crate::interpolate::Interpolate;

/// A finite `f32` constrained to the unit interval `[0.0, 1.0]` (both
/// endpoints included), optionally tagged with the seconds-width of the
/// window it was sampled from.
///
/// Construct via [`Phase::new`] (validating) or [`Phase::saturating`]
/// (clamping). Phases that originate from [`crate::time::Time::phase`]
/// additionally carry a [`Phase::width`] equal to `end - start`, enabling
/// window-local seconds-based sub-phasing via [`Phase::sub_secs`].
///
/// `PartialEq`/`Eq`/`Hash`/`PartialOrd` consider the value only — width is
/// advisory metadata and does not affect identity, so cache keys remain
/// stable across rebuilds that re-derive the same value from a different
/// window.
#[derive(Debug, Clone, Copy)]
pub struct Phase {
    value: f32,
    width: Option<f32>,
}

impl Phase {
    pub const ZERO: Self = Self {
        value: 0.0,
        width: None,
    };
    pub const HALF: Self = Self {
        value: 0.5,
        width: None,
    };
    pub const ONE: Self = Self {
        value: 1.0,
        width: None,
    };

    /// Returns `Some(Self)` if `v` is finite and within `[0.0, 1.0]`.
    /// `NaN`, `±inf`, or out-of-range values yield `None`. The resulting
    /// Phase carries no [`width`](Self::width).
    pub fn new(v: f32) -> Option<Self> {
        if v.is_finite() && (0.0..=1.0).contains(&v) {
            // Canonicalize -0.0 to +0.0 so the bit-pattern-based `PartialEq`
            // / `Hash` agree with `PartialOrd`, which treats the two zeroes
            // as equal. `+ 0.0` is exact for every other finite value.
            Some(Self {
                value: v + 0.0,
                width: None,
            })
        } else {
            None
        }
    }

    /// Clamps `v` to `[0.0, 1.0]`. `NaN` is treated as `0.0`. The resulting
    /// Phase carries no [`width`](Self::width).
    pub fn saturating(v: f32) -> Self {
        Self {
            value: clamp_canonical(v),
            width: None,
        }
    }

    /// Constructs a saturating Phase carrying the seconds-width of the
    /// source window. Intended for [`crate::time::Time::phase`] and the
    /// sub-phasing methods on this type — direct callers usually want
    /// [`Self::saturating`] or `time.phase(...)` instead.
    pub(crate) fn windowed_saturating(v: f32, width: f32) -> Self {
        Self {
            value: clamp_canonical(v),
            width: Some(width),
        }
    }

    /// Returns the inner value, guaranteed to be a finite `f32` in `[0.0, 1.0]`.
    pub const fn get(self) -> f32 {
        self.value
    }

    /// Returns `1.0 - self`. The result is still a valid `Phase` (the
    /// closed unit interval is closed under this reflection). Width is
    /// preserved.
    pub fn invert(self) -> Self {
        Self {
            value: 1.0 - self.value,
            width: self.width,
        }
    }

    /// Linearly interpolates between `from` (at `self == 0`) and `to`
    /// (at `self == 1`). Convenience wrapper for `from.interpolate(to, self)`
    /// so the `Phase` reads as "the driver" in animation code.
    pub fn interpolate<T: Interpolate>(self, from: T, to: T) -> T {
        from.interpolate(to, self)
    }

    /// Returns the seconds-width of the source window if this Phase was
    /// produced via [`crate::time::Time::phase`] or a sub-phasing method.
    /// Returns `None` for Phases built directly (constants, [`Self::new`],
    /// [`Self::saturating`]).
    pub const fn width(self) -> Option<f32> {
        self.width
    }

    /// Applies `f` to the inner value and rewraps via saturating clamp.
    /// Width is preserved — `f` is interpreted as a value-space remap of the
    /// same window. Used internally by the easing methods and by callers
    /// that want a custom non-linear remap (e.g. `4x(1-x)` for a hat-shaped
    /// visibility curve).
    pub fn map(self, f: impl FnOnce(f32) -> f32) -> Phase {
        Self {
            value: clamp_canonical(f(self.value)),
            width: self.width,
        }
    }

    /// Reinterprets `range` (in unit-interval ratios of the source window)
    /// as the full extent of a new Phase. The returned Phase rises from 0
    /// at `range.start` to 1 at `range.end` and saturates outside. Width,
    /// when known, scales proportionally to the sub-window.
    pub fn sub_ratio(self, range: Range<f32>) -> Phase {
        let span = range.end - range.start;
        let inner = (self.value - range.start) / span;
        Self {
            value: clamp_canonical(inner),
            width: self.width.map(|w| w * span),
        }
    }

    /// Reinterprets `range` (in seconds from the source window's start) as
    /// the full extent of a new Phase. Requires this Phase to carry a
    /// [`width`](Self::width) — panics otherwise. The returned Phase's own
    /// width is `range.end - range.start`, so further `sub_secs` calls
    /// remain in window-local seconds.
    ///
    /// This is the natural inverse of the "elapsed seconds inside a window"
    /// pattern: instead of converting the Phase back to seconds, doing
    /// arithmetic, and re-wrapping, callers express each sub-event directly
    /// in window-local seconds.
    pub fn sub_secs(self, range: Range<f32>) -> Phase {
        let width = self.width.expect(
            "Phase::sub_secs requires a width — produce the Phase via time.phase(s, e) or sub_secs",
        );
        let elapsed = self.value * width;
        let span = range.end - range.start;
        let inner = (elapsed - range.start) / span;
        Self {
            value: clamp_canonical(inner),
            width: Some(span),
        }
    }

    /// `true` iff the inner value is exactly `0.0`.
    pub fn is_zero(self) -> bool {
        self.value == 0.0
    }

    /// `true` iff the inner value is exactly `1.0`.
    pub fn is_full(self) -> bool {
        self.value == 1.0
    }

    /// `true` iff the inner value is strictly greater than `0.0`. Useful for
    /// "render this only when there's any visibility" gates.
    pub fn is_active(self) -> bool {
        self.value > 0.0
    }

    /// `true` iff the inner value is strictly between `0.0` and `1.0`.
    /// Useful for transition wipes that should only paint mid-sweep.
    pub fn is_in_progress(self) -> bool {
        self.value > 0.0 && self.value < 1.0
    }
}

/// Clamps to `[0.0, 1.0]`, treating `NaN` as `0.0`, then canonicalizes
/// `-0.0` to `+0.0` so the bit-pattern-based equality / hashing matches
/// `PartialOrd` (which treats the two zeroes as equal).
fn clamp_canonical(v: f32) -> f32 {
    if v.is_nan() {
        0.0
    } else {
        v.clamp(0.0, 1.0) + 0.0
    }
}

impl PartialEq for Phase {
    fn eq(&self, other: &Self) -> bool {
        // Bit-pattern equality on the value — width is advisory metadata
        // and intentionally ignored so two Phases that resolved to the
        // same value from different windows still compare equal (and hash
        // identically, keeping cache keys stable).
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

    #[test]
    fn map_remaps_value_and_preserves_width() {
        let p = Phase::windowed_saturating(0.5, 1.2);
        let q = p.map(|x| 4.0 * x * (1.0 - x));
        assert!((q.get() - 1.0).abs() < 1e-6);
        assert_eq!(q.width(), Some(1.2));
    }

    #[test]
    fn map_saturates_overshoot() {
        let p = Phase::HALF.map(|_| 2.5);
        assert_eq!(p.get(), 1.0);
        let p = Phase::HALF.map(|_| -1.0);
        assert_eq!(p.get(), 0.0);
    }

    #[test]
    fn sub_ratio_carves_unit_interval_window() {
        // value 0.5 with no width — sub_ratio 0.2..0.8 → (0.5 - 0.2) / 0.6 = 0.5.
        let p = Phase::saturating(0.5).sub_ratio(0.2..0.8);
        assert!((p.get() - 0.5).abs() < 1e-6);
        assert!(p.width().is_none());
    }

    #[test]
    fn sub_ratio_scales_width_proportionally() {
        let p = Phase::windowed_saturating(0.5, 2.0).sub_ratio(0.0..0.5);
        // The new window covers half the parent, so its width is half.
        assert_eq!(p.width(), Some(1.0));
    }

    #[test]
    fn sub_ratio_saturates_outside_inner_window() {
        let p = Phase::saturating(0.1).sub_ratio(0.3..0.7);
        assert_eq!(p.get(), 0.0);
        let p = Phase::saturating(0.9).sub_ratio(0.3..0.7);
        assert_eq!(p.get(), 1.0);
    }

    #[test]
    fn sub_secs_uses_window_width() {
        // Source window is 2.0s long, current value is 0.5 → elapsed = 1.0s.
        // sub_secs(0.0..0.4) → (1.0 - 0.0) / 0.4 = 2.5 → saturates to 1.0.
        let p = Phase::windowed_saturating(0.5, 2.0).sub_secs(0.0..0.4);
        assert_eq!(p.get(), 1.0);
        assert_eq!(p.width(), Some(0.4));

        // value 0.1 of a 2.0s window → elapsed 0.2s.
        // sub_secs(0.0..0.4) → 0.2 / 0.4 = 0.5.
        let p = Phase::windowed_saturating(0.1, 2.0).sub_secs(0.0..0.4);
        assert!((p.get() - 0.5).abs() < 1e-6);
    }

    #[test]
    #[should_panic(expected = "Phase::sub_secs requires a width")]
    fn sub_secs_without_width_panics() {
        let _ = Phase::saturating(0.5).sub_secs(0.0..0.4);
    }

    #[test]
    fn predicates_distinguish_endpoints_and_interior() {
        assert!(Phase::ZERO.is_zero());
        assert!(!Phase::ZERO.is_active());
        assert!(!Phase::ZERO.is_in_progress());
        assert!(!Phase::ZERO.is_full());

        assert!(Phase::ONE.is_full());
        assert!(Phase::ONE.is_active());
        assert!(!Phase::ONE.is_in_progress());
        assert!(!Phase::ONE.is_zero());

        assert!(Phase::HALF.is_active());
        assert!(Phase::HALF.is_in_progress());
        assert!(!Phase::HALF.is_zero());
        assert!(!Phase::HALF.is_full());
    }

    #[test]
    fn equality_ignores_width() {
        let a = Phase::saturating(0.5);
        let b = Phase::windowed_saturating(0.5, 2.0);
        let c = Phase::windowed_saturating(0.5, 10.0);
        assert_eq!(a, b);
        assert_eq!(b, c);
        // Hash agrees with eq.
        use std::collections::hash_map::DefaultHasher;
        let h = |p: Phase| {
            let mut s = DefaultHasher::new();
            p.hash(&mut s);
            s.finish()
        };
        assert_eq!(h(a), h(b));
        assert_eq!(h(b), h(c));
    }
}
