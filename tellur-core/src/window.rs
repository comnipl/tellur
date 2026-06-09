//! [`Window`] — a time interval `[start, end)` paired with the current time
//! cursor, packaging together the saturating [`Phase`] view and the unbounded
//! "seconds since the anchor" / "seconds past the close" durations that the
//! [`Phase`] alone cannot represent.
//!
//! `Window` exists because [`Phase`] is saturating by design: once the cursor
//! passes the window end, the Phase clamps at `1.0` and forgets how far past
//! it went. For envelopes (rise / saturate / fall) that is exactly right;
//! for "spin since this moment", "5 seconds after the intro finishes", or
//! any ongoing motion that outlives the window, callers need the absolute
//! time anchors back. `Window` keeps them attached.
//!
//! ```
//! use tellur_core::time::{Time, TimelineTime};
//! let t = TimelineTime::new(7.0);
//! let w = t.window(5.0, 6.0);
//! assert_eq!(w.phase().get(), 1.0);       // saturated
//! assert_eq!(w.elapsed(), 2.0);           // 2s since the window opened
//! assert_eq!(w.after(), 1.0);             // 1s past close
//! ```

use crate::phase::Phase;
use crate::Keyable;

/// A time interval `[start, end)` plus the current time cursor — the pair
/// `Phase` needs to talk about coordinates outside the window.
///
/// Construct via [`crate::time::Time::window`]. All accessors are derived
/// from `(start, end, current)`; the struct itself stores those three `f32`s
/// directly so it can be `Keyable`-hashed for cache keys.
#[derive(Debug, Clone, Copy, Keyable)]
pub struct Window {
    start: f32,
    end: f32,
    current: f32,
}

impl Window {
    /// Constructs a `Window` directly. Most callers should reach for
    /// [`crate::time::Time::window`] instead; this is exposed for tests and
    /// the rare case of building a window without a `Time` in hand.
    pub const fn new(start: f32, end: f32, current: f32) -> Self {
        Self {
            start,
            end,
            current,
        }
    }

    /// The window's start time in absolute seconds.
    pub const fn start(self) -> f32 {
        self.start
    }

    /// The window's end time in absolute seconds.
    pub const fn end(self) -> f32 {
        self.end
    }

    /// The cursor's absolute time, copied from the `Time` that built this
    /// `Window`. Can be before [`Self::start`] or after [`Self::end`].
    pub const fn current(self) -> f32 {
        self.current
    }

    /// `end - start`. Always reflects the declared window; does not collapse
    /// when the cursor is outside the window.
    pub fn width(self) -> f32 {
        self.end - self.start
    }

    /// The saturating [`Phase`] view: `0.0` before `start`, `1.0` after
    /// `end`, linearly interpolated in between. Carries [`Phase::width`]
    /// so the resulting Phase can be further carved with
    /// [`Phase::sub_secs`].
    pub fn phase(self) -> Phase {
        let u = (self.current - self.start) / (self.end - self.start);
        Phase::windowed_saturating(u, self.end - self.start)
    }

    /// Seconds the cursor has lived past `start`, clamped at `0` before the
    /// window opens. **Not** clamped at the end — keeps counting once the
    /// window closes, which is exactly the "ongoing motion since this
    /// anchor" case [`Phase`] cannot express.
    pub fn elapsed(self) -> f32 {
        (self.current - self.start).max(0.0)
    }

    /// Seconds remaining until the window opens, `0` once it has.
    pub fn before(self) -> f32 {
        (self.start - self.current).max(0.0)
    }

    /// Seconds the cursor has lived past `end`, `0` before the window
    /// closes. The companion to [`Self::elapsed`] for post-saturation
    /// timing (e.g. "5 seconds after the intro finishes").
    pub fn after(self) -> f32 {
        (self.current - self.end).max(0.0)
    }

    /// `true` iff the cursor is within `[start, end)` — the same gate as
    /// [`crate::time::Time::during`].
    pub fn is_inside(self) -> bool {
        self.current >= self.start && self.current < self.end
    }

    /// `true` iff the cursor has not reached `start` yet.
    pub fn is_before(self) -> bool {
        self.current < self.start
    }

    /// `true` iff the cursor has reached or passed `end`.
    pub fn is_after(self) -> bool {
        self.current >= self.end
    }

    /// Unclamped `(current - start) / (end - start)`. Goes negative before
    /// the window opens, exceeds `1.0` after it closes. Useful when the
    /// downstream consumer wants the linear progress without the saturating
    /// clamp — most callers want [`Self::phase`] instead.
    pub fn raw_progress(self) -> f32 {
        (self.current - self.start) / (self.end - self.start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(start: f32, end: f32, current: f32) -> Window {
        Window::new(start, end, current)
    }

    #[test]
    fn phase_saturates_at_endpoints() {
        assert_eq!(w(3.0, 5.0, 2.0).phase().get(), 0.0);
        assert_eq!(w(3.0, 5.0, 6.0).phase().get(), 1.0);
        assert!((w(3.0, 5.0, 4.0).phase().get() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn phase_carries_width() {
        assert_eq!(w(3.0, 5.0, 4.0).phase().width(), Some(2.0));
    }

    #[test]
    fn elapsed_keeps_counting_past_end() {
        // Before: 0.
        assert_eq!(w(3.0, 5.0, 2.0).elapsed(), 0.0);
        // Inside: current - start.
        assert!((w(3.0, 5.0, 4.0).elapsed() - 1.0).abs() < 1e-6);
        // After: still current - start, NOT clamped at width.
        assert!((w(3.0, 5.0, 7.5).elapsed() - 4.5).abs() < 1e-6);
    }

    #[test]
    fn before_counts_down_then_zero() {
        assert!((w(3.0, 5.0, 1.0).before() - 2.0).abs() < 1e-6);
        assert_eq!(w(3.0, 5.0, 3.0).before(), 0.0);
        assert_eq!(w(3.0, 5.0, 4.0).before(), 0.0);
        assert_eq!(w(3.0, 5.0, 6.0).before(), 0.0);
    }

    #[test]
    fn after_zero_until_close_then_climbs() {
        assert_eq!(w(3.0, 5.0, 4.0).after(), 0.0);
        assert_eq!(w(3.0, 5.0, 5.0).after(), 0.0);
        assert!((w(3.0, 5.0, 6.5).after() - 1.5).abs() < 1e-6);
    }

    #[test]
    fn gates_partition_the_timeline() {
        let win = w(3.0, 5.0, 2.0);
        assert!(win.is_before() && !win.is_inside() && !win.is_after());
        let win = w(3.0, 5.0, 4.0);
        assert!(!win.is_before() && win.is_inside() && !win.is_after());
        // End is exclusive, like Time::during.
        let win = w(3.0, 5.0, 5.0);
        assert!(!win.is_before() && !win.is_inside() && win.is_after());
        let win = w(3.0, 5.0, 6.0);
        assert!(!win.is_before() && !win.is_inside() && win.is_after());
    }

    #[test]
    fn raw_progress_is_unclamped() {
        assert!((w(3.0, 5.0, 2.0).raw_progress() - (-0.5)).abs() < 1e-6);
        assert!((w(3.0, 5.0, 7.0).raw_progress() - 2.0).abs() < 1e-6);
    }

    #[test]
    fn width_matches_declared_span() {
        assert_eq!(w(3.0, 5.0, 999.0).width(), 2.0);
    }
}
