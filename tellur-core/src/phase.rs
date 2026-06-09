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
use crate::scalar::clamp_unit;

/// A finite `f32` constrained to the unit interval `[0.0, 1.0]` (both
/// endpoints included), optionally tagged with the seconds-width of the
/// window it was sampled from.
///
/// Construct via [`Phase::new`] (validating) or [`Phase::saturating`]
/// (clamping). Phases that originate from [`crate::time::Time::phase`]
/// additionally carry a [`Phase::width`] equal to `end - start`, enabling
/// window-local seconds-based sub-phasing via [`Phase::sub_secs`].
///
/// `PartialEq`/`Eq`/`Hash` include both the value and the optional width.
/// A `Phase` whose downstream `sub_secs` behavior differs must not collide
/// in cache keys just because its visible unit-interval value matches.
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
            value: clamp_unit(v),
            width: None,
        }
    }

    /// Constructs a saturating Phase carrying the seconds-width of the
    /// source window. Intended for [`crate::time::Time::phase`] and the
    /// sub-phasing methods on this type — direct callers usually want
    /// [`Self::saturating`] or `time.phase(...)` instead.
    pub(crate) fn windowed_saturating(v: f32, width: f32) -> Self {
        assert!(
            width.is_finite() && width > 0.0,
            "Phase::windowed_saturating requires a finite positive width"
        );
        Self {
            value: clamp_unit(v),
            width: Some(width),
        }
    }

    /// Returns the inner value, guaranteed to be a finite `f32` in `[0.0, 1.0]`.
    pub const fn get(self) -> f32 {
        self.value
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
    ///
    /// Because this remaps value-space, not wall-clock time, call
    /// `sub_secs` before `map` when a sub-window should be addressed by
    /// elapsed seconds in the original source window.
    pub fn map(self, f: impl FnOnce(f32) -> f32) -> Phase {
        Self {
            value: clamp_unit(f(self.value)),
            width: self.width,
        }
    }

    /// Reinterprets `range` (in unit-interval ratios of the source window)
    /// as the full extent of a new Phase. Returns `None` unless `range` is
    /// finite, non-empty, ordered, and inside `[0.0, 1.0]`.
    ///
    /// The returned Phase rises from 0 at `range.start` to 1 at `range.end`
    /// and saturates outside. Width, when known, scales proportionally to
    /// the sub-window.
    pub fn sub_ratio(self, range: Range<f32>) -> Option<Phase> {
        let span = checked_ratio_span(&range)?;
        let inner = (self.value - range.start) / span;
        Some(Self {
            value: clamp_unit(inner),
            width: self.width.map(|w| w * span),
        })
    }

    /// Reinterprets `range` (in seconds from the source window's start) as
    /// the full extent of a new Phase. Requires this Phase to carry a
    /// [`width`](Self::width). Returns `None` if the Phase has no width, or
    /// if `range` is finite, ordered, non-empty, and inside the source
    /// window's seconds-width.
    ///
    /// The returned Phase's own width is `range.end - range.start`, so
    /// further `sub_secs` calls remain in window-local seconds.
    ///
    /// This is the natural inverse of the "elapsed seconds inside a window"
    /// pattern: instead of converting the Phase back to seconds, doing
    /// arithmetic, and re-wrapping, callers express each sub-event directly
    /// in window-local seconds.
    pub fn sub_secs(self, range: Range<f32>) -> Option<Phase> {
        let width = self.width?;
        let span = checked_seconds_span(&range, width)?;
        let elapsed = self.value * width;
        let inner = (elapsed - range.start) / span;
        Some(Self {
            value: clamp_unit(inner),
            width: Some(span),
        })
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

fn checked_ratio_span(range: &Range<f32>) -> Option<f32> {
    let span = range.end - range.start;
    (range.start.is_finite()
        && range.end.is_finite()
        && range.start >= 0.0
        && range.end <= 1.0
        && span.is_finite()
        && span > 0.0)
        .then_some(span)
}

fn checked_seconds_span(range: &Range<f32>, source_width: f32) -> Option<f32> {
    const EPSILON: f32 = 1.0e-6;
    let span = range.end - range.start;
    (range.start.is_finite()
        && range.end.is_finite()
        && source_width.is_finite()
        && source_width > 0.0
        && range.start >= 0.0
        && range.end <= source_width + EPSILON
        && span.is_finite()
        && span > 0.0)
        .then_some(span)
}

impl PartialEq for Phase {
    fn eq(&self, other: &Self) -> bool {
        self.value.to_bits() == other.value.to_bits()
            && match (self.width, other.width) {
                (Some(a), Some(b)) => a.to_bits() == b.to_bits(),
                (None, None) => true,
                _ => false,
            }
    }
}

impl Eq for Phase {}

impl Hash for Phase {
    fn hash<H: Hasher>(&self, state: &mut H) {
        hash_f32(self.value, state);
        match self.width {
            Some(width) => {
                1_u8.hash(state);
                hash_f32(width, state);
            }
            None => 0_u8.hash(state),
        }
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
        let p = Phase::saturating(0.5).sub_ratio(0.2..0.8).unwrap();
        assert!((p.get() - 0.5).abs() < 1e-6);
        assert!(p.width().is_none());
    }

    #[test]
    fn sub_ratio_scales_width_proportionally() {
        let p = Phase::windowed_saturating(0.5, 2.0)
            .sub_ratio(0.0..0.5)
            .unwrap();
        // The new window covers half the parent, so its width is half.
        assert_eq!(p.width(), Some(1.0));
    }

    #[test]
    fn sub_ratio_saturates_outside_inner_window() {
        let p = Phase::saturating(0.1).sub_ratio(0.3..0.7).unwrap();
        assert_eq!(p.get(), 0.0);
        let p = Phase::saturating(0.9).sub_ratio(0.3..0.7).unwrap();
        assert_eq!(p.get(), 1.0);
    }

    #[test]
    fn sub_ratio_rejects_empty_reversed_and_out_of_unit_ranges() {
        let p = Phase::HALF;

        assert!(p.sub_ratio(0.5..0.5).is_none());
        assert!(p.sub_ratio(0.6..0.2).is_none());
        assert!(p.sub_ratio(-0.1..0.5).is_none());
        assert!(p.sub_ratio(0.5..1.1).is_none());
    }

    #[test]
    fn sub_secs_uses_window_width() {
        // Source window is 2.0s long, current value is 0.5 → elapsed = 1.0s.
        // sub_secs(0.0..0.4) → (1.0 - 0.0) / 0.4 = 2.5 → saturates to 1.0.
        let p = Phase::windowed_saturating(0.5, 2.0)
            .sub_secs(0.0..0.4)
            .unwrap();
        assert_eq!(p.get(), 1.0);
        assert_eq!(p.width(), Some(0.4));

        // value 0.1 of a 2.0s window → elapsed 0.2s.
        // sub_secs(0.0..0.4) → 0.2 / 0.4 = 0.5.
        let p = Phase::windowed_saturating(0.1, 2.0)
            .sub_secs(0.0..0.4)
            .unwrap();
        assert!((p.get() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn sub_secs_without_width_returns_none() {
        assert!(Phase::saturating(0.5).sub_secs(0.0..0.4).is_none());
    }

    #[test]
    fn sub_secs_rejects_empty_reversed_and_out_of_source_ranges() {
        let p = Phase::windowed_saturating(0.5, 1.0);

        assert!(p.sub_secs(0.5..0.5).is_none());
        assert!(p.sub_secs(0.6..0.2).is_none());
        assert!(p.sub_secs(-0.1..0.5).is_none());
        assert!(p.sub_secs(0.5..1.1).is_none());
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
    fn equality_and_hash_include_width() {
        let a = Phase::saturating(0.5);
        let b = Phase::windowed_saturating(0.5, 2.0);
        let c = Phase::windowed_saturating(0.5, 10.0);
        let d = Phase::windowed_saturating(0.5, 2.0);
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_eq!(b, d);
        // Hash agrees with eq.
        use std::collections::hash_map::DefaultHasher;
        let h = |p: Phase| {
            let mut s = DefaultHasher::new();
            p.hash(&mut s);
            s.finish()
        };
        assert_ne!(h(a), h(b));
        assert_ne!(h(b), h(c));
        assert_eq!(h(b), h(d));
    }
}
