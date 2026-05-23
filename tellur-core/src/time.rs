//! Time abstractions for timeline-driven rendering.
//!
//! [`Time`] represents a current playback time, measured in seconds. The
//! combinators [`Time::during`] and [`Time::fps`] let scene code gate and
//! quantize content along the time axis:
//!
//! ```ignore
//! if let Some(t) = t.during(3.0, 5.0).fps(24) {
//!     // Runs only when the outer time is in `[3.0, 5.0)`,
//!     // with `t.seconds()` in `[0.0, 2.0)` quantized to 1/24s steps.
//! }
//! ```
//!
//! `during` rebases time to start at zero inside the gated block, so child
//! animations can be authored in their own local timeline. `fps` floors
//! `seconds` to multiples of `1/fps`, useful for stop-motion or stutter
//! effects.
//!
//! [`TimeOptionExt`] extends `Option<T: Time>` with the same combinators so
//! they can be chained across a `during` that may return `None`.

/// A point in time on a timeline, measured in seconds from the start.
pub trait Time: Copy {
    /// Current time in seconds.
    fn seconds(&self) -> f32;

    /// Returns `Some(t)` when the current time is within `[start, end)`,
    /// where `t.seconds()` is rebased to start at zero (relative time).
    /// Returns `None` otherwise.
    fn during(&self, start: f32, end: f32) -> Option<TimeView>;

    /// Returns a time floored to the nearest `1/fps` second boundary.
    /// Causes the rendered output to advance in `fps`-Hz steps regardless
    /// of the encoder's frame rate.
    fn fps(&self, fps: u32) -> TimeView;
}

/// A concrete [`Time`] value. Returned by [`Time::during`] and [`Time::fps`]
/// so the combinators can be chained without forcing a particular root type.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimeView {
    seconds: f32,
}

impl TimeView {
    pub const fn new(seconds: f32) -> Self {
        Self { seconds }
    }
}

impl Time for TimeView {
    fn seconds(&self) -> f32 {
        self.seconds
    }

    fn during(&self, start: f32, end: f32) -> Option<TimeView> {
        if self.seconds >= start && self.seconds < end {
            Some(TimeView::new(self.seconds - start))
        } else {
            None
        }
    }

    fn fps(&self, fps: u32) -> TimeView {
        // `fps == 0` would divide by zero; treat it as "no quantization".
        if fps == 0 {
            return *self;
        }
        let step = 1.0 / fps as f32;
        let quantized = (self.seconds / step).floor() * step;
        TimeView::new(quantized)
    }
}

/// Extension trait that mirrors [`Time`] on `Option<T: Time>` so chains like
/// `t.during(3.0, 5.0).fps(24)` type-check: a `during` that yields `None`
/// propagates through the chain instead of forcing the caller to unwrap.
pub trait TimeOptionExt {
    fn during(self, start: f32, end: f32) -> Option<TimeView>;
    fn fps(self, fps: u32) -> Option<TimeView>;
}

impl<T: Time> TimeOptionExt for Option<T> {
    fn during(self, start: f32, end: f32) -> Option<TimeView> {
        self.and_then(|t| t.during(start, end))
    }

    fn fps(self, fps: u32) -> Option<TimeView> {
        self.map(|t| t.fps(fps))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn during_outside_range_returns_none() {
        let t = TimeView::new(2.0);
        assert!(t.during(3.0, 5.0).is_none());

        let t = TimeView::new(5.0);
        assert!(t.during(3.0, 5.0).is_none(), "end is exclusive");

        let t = TimeView::new(6.0);
        assert!(t.during(3.0, 5.0).is_none());
    }

    #[test]
    fn during_inside_range_rebases_to_zero() {
        let t = TimeView::new(3.0);
        assert_eq!(t.during(3.0, 5.0), Some(TimeView::new(0.0)));

        let t = TimeView::new(4.2);
        let inner = t.during(3.0, 5.0).expect("in range");
        assert!((inner.seconds() - 1.2).abs() < 1e-6);
    }

    #[test]
    fn fps_floors_to_step() {
        let t = TimeView::new(1.234);
        // 24fps step = ~0.04167s; floor(1.234 / 0.04167) = 29 -> 29 * 0.04167 ≈ 1.2083
        let q = t.fps(24);
        let expected = (29.0_f32) / 24.0;
        assert!((q.seconds() - expected).abs() < 1e-5);
    }

    #[test]
    fn fps_zero_passes_through() {
        let t = TimeView::new(1.234);
        assert_eq!(t.fps(0), t);
    }

    #[test]
    fn option_chain_during_then_fps() {
        let t = TimeView::new(4.5);
        let chained = t.during(3.0, 5.0).fps(24);
        // Inner relative time = 1.5; 24fps step = 1/24; floor(1.5 / (1/24)) = 36 -> 1.5
        let inner = chained.expect("in range");
        assert!((inner.seconds() - 1.5).abs() < 1e-5);
    }

    #[test]
    fn option_chain_propagates_none() {
        let t = TimeView::new(10.0);
        assert!(t.during(3.0, 5.0).fps(24).is_none());
    }
}
