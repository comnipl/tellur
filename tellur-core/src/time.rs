//! Time abstractions for timeline-driven rendering.
//!
//! Two concrete time types are distinguished by what coordinate space they
//! live in:
//!
//! - [`TimelineTime`] — a point on the global timeline. This is what
//!   [`crate::timeline::Timeline::build`] receives.
//! - [`LocalTime`] — a remapped time, produced by [`Time::during_ripple`]
//!   or [`Time::lerp`]. Its `seconds()` is no longer relative to the global
//!   timeline origin but to whatever local frame the remap established.
//!
//! Both implement the [`Time`] trait, which provides the gating /
//! remapping / quantization combinators. The trait methods are defaulted so
//! the operations work identically on either type; the only difference is
//! which type a method returns.
//!
//! ```ignore
//! // `t: TimelineTime` from Timeline::build.
//! if let Some(t) = t.during(3.0, 5.0) {
//!     // Pure gate: `t` is still `TimelineTime`, `t.seconds()` ∈ [3.0, 5.0).
//! }
//!
//! if let Some(t) = t.during_ripple(3.0, 5.0) {
//!     // Gate + rebase: `t` is `LocalTime`, `t.seconds()` ∈ [0.0, 2.0).
//! }
//!
//! let warped = t.lerp((3.0, 5.0), (0.0, 4.0));
//! // `warped: LocalTime`, 2x speed of the [3, 5) source range.
//! ```

/// A point in time, measured in seconds.
///
/// All combinators (`during`, `during_ripple`, `lerp`, `fps`) are provided
/// as default methods so implementors only need to express their own
/// representation via [`Self::seconds`] and [`Self::from_seconds`].
pub trait Time: Copy + Sized {
    fn seconds(&self) -> f32;
    fn from_seconds(seconds: f32) -> Self;

    /// Gate-only combinator: returns `Some(self)` (unchanged time, same type)
    /// when `seconds()` is within `[start, end)`, otherwise `None`. Useful
    /// when you want to keep operating in the original coordinate system.
    fn during(&self, start: f32, end: f32) -> Option<Self> {
        let s = self.seconds();
        if s >= start && s < end {
            Some(*self)
        } else {
            None
        }
    }

    /// Gate + rebase to zero: returns `Some(LocalTime)` whose `seconds()`
    /// starts at zero when `self` is within `[start, end)`. Outside the
    /// range, returns `None`. Use this to open a fresh local timeline for
    /// the gated block.
    fn during_ripple(&self, start: f32, end: f32) -> Option<LocalTime> {
        let s = self.seconds();
        if s >= start && s < end {
            Some(LocalTime::from_seconds(s - start))
        } else {
            None
        }
    }

    /// Affine remap from `src` to `dst`. Outside `src`, the mapping is
    /// extrapolated linearly (no clamping, no gating). The output is always
    /// a [`LocalTime`] because the warp invalidates any prior coordinate
    /// system.
    fn lerp(&self, src: (f32, f32), dst: (f32, f32)) -> LocalTime {
        let u = (self.seconds() - src.0) / (src.1 - src.0);
        LocalTime::from_seconds(dst.0 + u * (dst.1 - dst.0))
    }

    /// Floors `seconds()` to a `1/fps`-second grid. Type is preserved, so
    /// quantizing a [`TimelineTime`] yields a [`TimelineTime`] (and likewise
    /// for [`LocalTime`]). `fps == 0` is treated as "no quantization".
    fn fps(&self, fps: u32) -> Self {
        if fps == 0 {
            return *self;
        }
        let step = 1.0 / fps as f32;
        Self::from_seconds((self.seconds() / step).floor() * step)
    }
}

/// A point on the global timeline that a [`crate::timeline::Timeline`] is
/// being sampled at. Produced by the renderer; users typically don't
/// construct it directly.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimelineTime {
    seconds: f32,
}

impl TimelineTime {
    pub const fn new(seconds: f32) -> Self {
        Self { seconds }
    }
}

impl Time for TimelineTime {
    fn seconds(&self) -> f32 {
        self.seconds
    }
    fn from_seconds(seconds: f32) -> Self {
        Self { seconds }
    }
}

/// A remapped time, no longer relative to the global timeline origin.
/// Produced by [`Time::during_ripple`] and [`Time::lerp`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LocalTime {
    seconds: f32,
}

impl LocalTime {
    pub const fn new(seconds: f32) -> Self {
        Self { seconds }
    }
}

impl Time for LocalTime {
    fn seconds(&self) -> f32 {
        self.seconds
    }
    fn from_seconds(seconds: f32) -> Self {
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
        Self {
            seconds: t.seconds,
        }
    }
}

/// Mirrors [`Time`] on `Option<T: Time>` so combinators can be chained
/// across operations that may yield `None` (e.g., `during`/`during_ripple`).
pub trait TimeOptionExt<T: Time> {
    fn during(self, start: f32, end: f32) -> Option<T>;
    fn during_ripple(self, start: f32, end: f32) -> Option<LocalTime>;
    fn lerp(self, src: (f32, f32), dst: (f32, f32)) -> Option<LocalTime>;
    fn fps(self, fps: u32) -> Option<T>;
}

impl<T: Time> TimeOptionExt<T> for Option<T> {
    fn during(self, start: f32, end: f32) -> Option<T> {
        self.and_then(|t| t.during(start, end))
    }
    fn during_ripple(self, start: f32, end: f32) -> Option<LocalTime> {
        self.and_then(|t| t.during_ripple(start, end))
    }
    fn lerp(self, src: (f32, f32), dst: (f32, f32)) -> Option<LocalTime> {
        self.map(|t| t.lerp(src, dst))
    }
    fn fps(self, fps: u32) -> Option<T> {
        self.map(|t| t.fps(fps))
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
    fn during_ripple_rebases_to_local_time() {
        let t = TimelineTime::new(4.2);
        let local = t.during_ripple(3.0, 5.0).expect("in range");
        assert!((local.seconds() - 1.2).abs() < 1e-6);
    }

    #[test]
    fn during_ripple_out_of_range_is_none() {
        let t = TimelineTime::new(6.0);
        assert!(t.during_ripple(3.0, 5.0).is_none());
    }

    #[test]
    fn lerp_inside_src_maps_linearly() {
        let t = TimelineTime::new(4.0);
        // (3, 5) → (0, 4): width-doubling, t=4 is halfway → output 2.0.
        let out = t.lerp((3.0, 5.0), (0.0, 4.0));
        assert!((out.seconds() - 2.0).abs() < 1e-6);
    }

    #[test]
    fn lerp_outside_src_extrapolates() {
        let t = TimelineTime::new(2.0);
        // (3, 5) → (0, 4): t=2 is one unit before src start, so u = -0.5,
        // output = 0 + (-0.5) * 4 = -2.0.
        let out = t.lerp((3.0, 5.0), (0.0, 4.0));
        assert!((out.seconds() - (-2.0)).abs() < 1e-6);
    }

    #[test]
    fn fps_floors_and_preserves_type() {
        let t = TimelineTime::new(1.234);
        let q = t.fps(24);
        let expected = (29.0_f32) / 24.0;
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
        // during_ripple yields LocalTime regardless.
        let rerebased = t.during_ripple(0.5, 1.0).expect("in range");
        assert!((rerebased.seconds() - 0.3).abs() < 1e-6);
    }

    #[test]
    fn option_chain_during_then_during_ripple() {
        let t = TimelineTime::new(4.5);
        let chained = t.during(3.0, 5.0).during_ripple(3.0, 5.0);
        let inner = chained.expect("in range");
        assert!((inner.seconds() - 1.5).abs() < 1e-6);
    }

    #[test]
    fn option_chain_propagates_none() {
        let t = TimelineTime::new(10.0);
        assert!(t.during(3.0, 5.0).during_ripple(3.0, 5.0).is_none());
        assert!(t.during_ripple(3.0, 5.0).fps(24).is_none());
    }

    #[test]
    fn option_chain_lerp_is_always_some() {
        let t = TimelineTime::new(4.0);
        let warped = t.during(3.0, 5.0).lerp((3.0, 5.0), (0.0, 4.0));
        // 4.0 maps to 2.0 (halfway of dst).
        assert!((warped.expect("in range").seconds() - 2.0).abs() < 1e-6);
    }
}
