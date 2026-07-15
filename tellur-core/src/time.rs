//! Time abstractions for timeline-driven rendering.
//!
//! Two concrete time types are distinguished by what coordinate space they
//! live in:
//!
//! - [`TimelineTime`] — a point on the global timeline. This is what
//!   [`crate::timeline::Timeline::build`] receives.
//! - [`LocalTime`] — a rebased time whose `seconds()` is relative to some
//!   local frame rather than the global timeline origin. The timeline
//!   world's [`Clock`](crate::timeline_component::Clock) hands one out as
//!   its local axis.
//!
//! Both implement the [`Time`] trait, which provides the gating /
//! quantization combinators, periodic decompositions ([`Time::cycle`],
//! [`Time::bounce`], [`Time::wave`]) and the two progress views:
//! [`Time::phase`] (a saturating [`Phase`]) and [`Time::window`] (a
//! [`Window`], which keeps the interval and cursor available). The trait
//! methods are defaulted so the operations work identically on either type.
//!
//! ```ignore
//! // `t: TimelineTime` from Timeline::build.
//! if let Some(t) = t.during(3.0, 5.0) {
//!     // Pure gate: `t` is still `TimelineTime`, `t.seconds()` ∈ [3.0, 5.0).
//! }
//!
//! let alpha = t.phase(3.0, 5.0).ease_out_cubic(0.0, 1.0);
//! let radar = t.window(3.95, 5.4);   // phase + elapsed/remaining in one view
//! ```

use crate::phase::Phase;
use crate::window::Window;
use crate::Keyable;

/// A point in time, measured in seconds.
///
/// All combinators (`during`, `fps`, `cycle`, `bounce`, `wave`, `phase`,
/// `window`) are provided as default methods so implementors only need to
/// express their own representation via [`Self::seconds`] and
/// [`Self::from_seconds`].
pub trait Time: Copy + Sized {
    fn seconds(&self) -> f64;
    fn from_seconds(seconds: f64) -> Self;

    /// Gate-only combinator: returns `Some(self)` (unchanged time, same type)
    /// when `seconds()` is within `[start, end)`, otherwise `None`.
    fn during(&self, start: f64, end: f64) -> Option<Self> {
        let s = self.seconds();
        if s >= start && s < end {
            Some(*self)
        } else {
            None
        }
    }

    /// Floors `seconds()` to a `1/fps`-second grid. Type is preserved, so
    /// quantizing a [`TimelineTime`] yields a [`TimelineTime`] (and likewise
    /// for [`LocalTime`]). `fps == 0` is treated as "no quantization".
    fn fps(&self, fps: u32) -> Self {
        if fps == 0 {
            return *self;
        }
        let step = 1.0 / fps as f64;
        Self::from_seconds((self.seconds() / step).floor() * step)
    }

    /// Sawtooth decomposition: the phase rises linearly 0 → 1 across each
    /// `period`-second cycle (cycle 0 starts at `seconds() == 0`) and resets.
    /// The cycle index, when needed, is `(seconds() / period).floor()`.
    fn cycle(&self, period: f64) -> Phase {
        assert_valid_period(period, "Time::cycle");
        Phase::saturating((self.seconds().rem_euclid(period) / period) as f32)
    }

    /// Triangle-wave decomposition: the phase rises 0 → 1 across the first
    /// half of each `period`-second cycle and falls 1 → 0 across the second
    /// half.
    fn bounce(&self, period: f64) -> Phase {
        assert_valid_period(period, "Time::bounce");
        let p = self.cycle(period).get();
        Phase::saturating(1.0 - (2.0 * p - 1.0).abs())
    }

    /// Sine-wave decomposition — the smooth sibling of [`Self::bounce`]:
    /// the phase rises 0 → 1 across the first half of each `period`-second
    /// cycle and falls 1 → 0 across the second half, with zero slope at the
    /// turnarounds (`(1 - cos(2πt/period)) / 2`). The natural driver for
    /// idle oscillation (drift, breathing, shimmer); a `±amp` swing is
    /// `t.wave(period).linear(-amp, amp)`.
    fn wave(&self, period: f64) -> Phase {
        assert_valid_period(period, "Time::wave");
        let u = self.seconds() / period;
        Phase::saturating((0.5 - 0.5 * (std::f64::consts::TAU * u).cos()) as f32)
    }

    /// Maps `[start, end]` to `[0.0, 1.0]` linearly, clamping outside.
    /// The workhorse for driving an easing: `t.phase(a, b).ease_out_cubic(from, to)`.
    ///
    /// When the interval itself is still needed downstream — sub-windows in
    /// seconds, elapsed/remaining durations — reach for [`Self::window`]
    /// instead and project to a Phase late via
    /// [`Window::phase`](crate::window::Window::phase).
    fn phase(&self, start: f64, end: f64) -> Phase {
        assert_valid_span(start, end, "Time::phase");
        Phase::saturating(((self.seconds() - start) / (end - start)) as f32)
    }

    /// Packages this time and a `[start, end)` interval into a [`Window`]
    /// that exposes the saturating [`Phase`] view alongside everything the
    /// Phase alone cannot represent: seconds-based sub-windows
    /// ([`Window::sub_secs`]), unbounded [`Window::elapsed`] /
    /// [`Window::after`] durations, and the countdown [`Window::remaining`].
    fn window(&self, start: f64, end: f64) -> Window {
        Window::new(start, end, self.seconds())
    }
}

fn assert_valid_span(start: f64, end: f64, caller: &str) {
    assert!(
        start.is_finite() && end.is_finite() && end > start,
        "{caller} requires finite start/end with end > start"
    );
}

fn assert_valid_period(period: f64, caller: &str) {
    assert!(
        period.is_finite() && period > 0.0,
        "{caller} requires a finite positive period"
    );
}

/// A point on the global timeline that a [`crate::timeline::Timeline`] is
/// being sampled at. Produced by the renderer; users typically don't
/// construct it directly.
#[derive(Debug, Clone, Copy, Keyable)]
pub struct TimelineTime {
    seconds: f64,
}

impl TimelineTime {
    pub const fn new(seconds: f64) -> Self {
        Self { seconds }
    }
}

impl Time for TimelineTime {
    fn seconds(&self) -> f64 {
        self.seconds
    }
    fn from_seconds(seconds: f64) -> Self {
        Self { seconds }
    }
}

/// A rebased time, no longer relative to the global timeline origin.
/// The timeline world's [`Clock`](crate::timeline_component::Clock)
/// produces one as its local axis.
#[derive(Debug, Clone, Copy, Keyable)]
pub struct LocalTime {
    seconds: f64,
}

impl LocalTime {
    pub const fn new(seconds: f64) -> Self {
        Self { seconds }
    }
}

impl Time for LocalTime {
    fn seconds(&self) -> f64 {
        self.seconds
    }
    fn from_seconds(seconds: f64) -> Self {
        Self { seconds }
    }
}

/// `TimelineTime` converts into `LocalTime` by treating the timeline
/// position as the local-frame value. This lets components that only
/// care about "some clock value, in seconds" accept either type via a
/// single `.into()` at the call site instead of being generic over
/// [`Time`].
impl From<TimelineTime> for LocalTime {
    fn from(t: TimelineTime) -> Self {
        Self { seconds: t.seconds }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn during_gates_without_rebase_and_preserves_type() {
        let t = TimelineTime::new(4.2);
        let gated = t.during(3.0, 5.0).expect("in range");
        // Type preserved (TimelineTime → TimelineTime) and seconds unchanged.
        assert_eq!(gated, TimelineTime::new(4.2));
    }

    #[test]
    fn during_out_of_range_is_none() {
        let t = TimelineTime::new(2.0);
        assert!(t.during(3.0, 5.0).is_none());
        // End is exclusive.
        let t = TimelineTime::new(5.0);
        assert!(t.during(3.0, 5.0).is_none());
    }

    #[test]
    fn fps_floors_and_preserves_type() {
        let t = TimelineTime::new(1.234);
        let q = t.fps(24);
        let expected = 29.0_f64 / 24.0;
        assert!((q.seconds() - expected).abs() < 1e-5);
        // Type preserved.
        let _is_timeline_time: TimelineTime = q;
    }

    #[test]
    fn fps_zero_is_identity() {
        let t = TimelineTime::new(1.234);
        assert_eq!(t.fps(0), t);
    }

    #[test]
    fn local_time_supports_same_ops() {
        let t = LocalTime::new(0.8);
        // during preserves LocalTime → LocalTime.
        let gated: LocalTime = t.during(0.0, 1.0).expect("in range");
        assert_eq!(gated, LocalTime::new(0.8));
    }

    #[test]
    fn timeline_time_distinguishes_adjacent_48khz_samples_at_large_offsets() {
        let base = 512.0;
        let next_sample = base + 1.0 / 48_000.0;
        assert_ne!(TimelineTime::new(base), TimelineTime::new(next_sample));
        assert_eq!(TimelineTime::new(next_sample).seconds(), next_sample);
    }

    #[test]
    fn cycle_rises_across_each_period() {
        let p = TimelineTime::new(2.5).cycle(1.0);
        assert!((p.get() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn cycle_resets_at_period_boundary() {
        assert_eq!(TimelineTime::new(1.0).cycle(1.0).get(), 0.0);
    }

    #[test]
    fn cycle_handles_negative_time() {
        // rem_euclid keeps the phase positive before the zero point.
        let p = LocalTime::new(-0.3).cycle(1.0);
        assert!((p.get() - 0.7).abs() < 1e-6);
    }

    #[test]
    fn bounce_peaks_at_half_period() {
        let p = TimelineTime::new(0.5).bounce(1.0);
        assert!((p.get() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn bounce_is_zero_at_period_boundaries() {
        assert_eq!(TimelineTime::new(0.0).bounce(1.0).get(), 0.0);
        assert_eq!(TimelineTime::new(1.0).bounce(1.0).get(), 0.0);
        assert_eq!(TimelineTime::new(2.0).bounce(1.0).get(), 0.0);
    }

    #[test]
    fn wave_matches_bounce_at_the_landmarks() {
        // 0 at cycle start, 1 at half period, 0 at the boundary — same
        // landmarks as bounce, smooth in between.
        assert!(TimelineTime::new(0.0).wave(2.0).get() < 1e-6);
        assert!((TimelineTime::new(1.0).wave(2.0).get() - 1.0).abs() < 1e-6);
        assert!(TimelineTime::new(2.0).wave(2.0).get() < 1e-5);
    }

    #[test]
    fn wave_is_smooth_not_linear() {
        // At 1/8 period the triangle reads 0.25 but the sine, easing out of
        // its zero-slope turnaround, is still below it.
        let triangle = TimelineTime::new(0.25).bounce(2.0).get();
        let sine = TimelineTime::new(0.25).wave(2.0).get();
        assert!((triangle - 0.25).abs() < 1e-6);
        assert!(sine < triangle);
    }

    #[test]
    #[should_panic(expected = "Time::cycle requires a finite positive period")]
    fn cycle_rejects_zero_period() {
        let _ = TimelineTime::new(1.0).cycle(0.0);
    }

    #[test]
    fn phase_maps_range_to_unit_interval() {
        let t = TimelineTime::new(4.0);
        // (4 - 3) / (5 - 3) = 0.5
        assert_eq!(t.phase(3.0, 5.0).get(), 0.5);
    }

    #[test]
    fn phase_clamps_outside_range() {
        let before = TimelineTime::new(2.0);
        assert_eq!(before.phase(3.0, 5.0).get(), Phase::ZERO.get());
        let after = TimelineTime::new(6.0);
        assert_eq!(after.phase(3.0, 5.0).get(), Phase::ONE.get());
    }

    #[test]
    #[should_panic(expected = "Time::phase requires finite start/end with end > start")]
    fn phase_rejects_equal_bounds() {
        let _ = TimelineTime::new(5.0).phase(5.0, 5.0);
    }

    #[test]
    #[should_panic(expected = "Time::phase requires finite start/end with end > start")]
    fn phase_rejects_reversed_bounds() {
        let _ = TimelineTime::new(1.0).phase(2.0, 1.0);
    }

    #[test]
    fn window_carries_the_interval_and_cursor() {
        let w = TimelineTime::new(4.0).window(3.0, 5.0);
        assert_eq!(w.start(), 3.0);
        assert_eq!(w.end(), 5.0);
        assert_eq!(w.current(), 4.0);
        assert_eq!(w.phase().get(), 0.5);
    }
}
