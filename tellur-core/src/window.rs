//! [`Window`] — a time interval `[start, end)` paired with the current time
//! cursor. Where [`Phase`] is the pure "how far along am I" scalar, `Window`
//! is the view that still knows the interval: it can carve sub-windows in
//! seconds ([`Window::sub_secs`]), report unbounded durations
//! ([`Window::elapsed`] / [`Window::after`]) that a saturating Phase cannot
//! represent, and project down to a [`Phase`] via [`Window::phase`].
//!
//! ```
//! use tellur_core::time::{Time, TimelineTime};
//! let t = TimelineTime::new(7.0);
//! let w = t.window(5.0, 6.0);
//! assert_eq!(w.phase().get(), 1.0);       // saturated
//! assert_eq!(w.elapsed(), 2.0);           // 2s since the window opened
//! assert_eq!(w.after(), 1.0);             // 1s past close
//! ```
//!
//! ### Sub-windows
//!
//! [`Window::sub_secs`] reinterprets a seconds range (relative to the
//! window's start) as a new window over the same cursor. Staggered
//! sub-events fall out naturally:
//!
//! ```
//! use tellur_core::time::{Time, TimelineTime};
//! let reveal = TimelineTime::new(1.0).window(0.5, 2.5);
//! let line_in = reveal.sub_secs(0.0..0.4).phase();   // first 0.4s of the reveal
//! let tick_in = reveal.sub_secs(0.7..1.1).phase();   // a later slice
//! assert_eq!(line_in.get(), 1.0);
//! assert_eq!(tick_in.get(), 0.0);
//! ```
//!
//! ### Memo-friendly snapshots
//!
//! A live `Window` changes every frame (its cursor keeps moving), so it is a
//! poor cache-key term for a memoized component. [`Window::clamped`] clamps
//! the cursor into `[start, end]`: before the window opens the snapshot is
//! constant at `(start, end, start)`, and once it saturates it is constant at
//! `(start, end, end)` — frame-to-frame stable exactly when the component's
//! output is.

use std::ops::Range;

use crate::phase::Phase;
use crate::Keyable;

/// A time interval `[start, end)` plus the current time cursor — the pair
/// [`Phase`] needs to talk about coordinates outside the window.
///
/// Construct via [`crate::time::Time::window`]. All accessors are derived
/// from `(start, end, current)`; the struct itself stores those three `f32`s
/// directly so it can be `Keyable`-hashed for cache keys (see
/// [`Window::clamped`] for the frame-stable form).
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
        self.span()
    }

    /// The saturating [`Phase`] view: `0.0` before `start`, `1.0` after
    /// `end`, linearly interpolated in between.
    pub fn phase(self) -> Phase {
        Phase::saturating((self.current - self.start) / self.span())
    }

    /// Reinterprets `range` (in seconds from this window's start) as a new
    /// `Window` over the same cursor. Total: the sub-window may extend past
    /// this window's end (the cursor still measures correctly); only a
    /// finite, non-empty, ordered `range` is required.
    ///
    /// This is the "stagger sub-events in window-local seconds" primitive:
    /// `w.sub_secs(0.4..0.8).phase()` rises across `[start + 0.4, start + 0.8)`.
    pub fn sub_secs(self, range: Range<f32>) -> Window {
        assert!(
            range.start.is_finite() && range.end.is_finite() && range.end > range.start,
            "Window::sub_secs requires a finite range with end > start"
        );
        Self {
            start: self.start + range.start,
            end: self.start + range.end,
            current: self.current,
        }
    }

    /// Clamps the cursor into `[start, end]`, turning this live view into a
    /// saturating snapshot: constant `(start, end, start)` before the window
    /// opens and constant `(start, end, end)` once it closes.
    ///
    /// Use this when a `Window` crosses a memoized component boundary as a
    /// field — the raw cursor changes every frame and would defeat the
    /// cache, while the clamped snapshot is stable exactly when every Phase
    /// derived from it is. [`Self::elapsed`] / [`Self::after`] intentionally
    /// lose their unbounded reading on a clamped window.
    pub fn clamped(self) -> Window {
        Self {
            start: self.start,
            end: self.end,
            current: self.current.clamp(self.start, self.end),
        }
    }

    /// Seconds the cursor has lived past `start`, clamped at `0` before the
    /// window opens. **Not** clamped at the end — keeps counting once the
    /// window closes, which is exactly the "ongoing motion since this
    /// anchor" case [`Phase`] cannot express.
    pub fn elapsed(self) -> f32 {
        (self.current - self.start).max(0.0)
    }

    /// Seconds remaining until the window closes, `0` once it has. The
    /// countdown twin of [`Self::elapsed`].
    pub fn remaining(self) -> f32 {
        (self.end - self.current).max(0.0)
    }

    /// A fade envelope over this window: rises 0 → 1 across the first
    /// `fade_in` seconds after `start`, holds 1, falls 1 → 0 across the
    /// last `fade_out` seconds before `end`, and stays 0 outside the
    /// window. A non-positive fade skips that edge (the envelope is already
    /// at 1 there). A self-contained appear/disappear for captions and
    /// other windowed content.
    pub fn envelope(self, fade_in: f32, fade_out: f32) -> Phase {
        let rise = if fade_in <= 0.0 {
            1.0
        } else {
            (self.elapsed() / fade_in).min(1.0)
        };
        let fall = if fade_out <= 0.0 {
            1.0
        } else {
            (self.remaining() / fade_out).min(1.0)
        };
        Phase::saturating(rise * fall)
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
        (self.current - self.start) / self.span()
    }

    fn span(self) -> f32 {
        assert!(
            self.start.is_finite() && self.end.is_finite() && self.end > self.start,
            "Window requires finite start/end with end > start"
        );
        self.end - self.start
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
    fn sub_secs_offsets_against_the_start() {
        // Parent [3, 5), cursor 4.0. Sub-window 0.5..1.5 covers [3.5, 4.5).
        let sub = w(3.0, 5.0, 4.0).sub_secs(0.5..1.5);
        assert_eq!(sub.start(), 3.5);
        assert_eq!(sub.end(), 4.5);
        assert_eq!(sub.current(), 4.0);
        assert!((sub.phase().get() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn sub_secs_saturates_outside_inner_window() {
        let parent = w(3.0, 5.0, 3.1);
        assert_eq!(parent.sub_secs(0.5..1.0).phase().get(), 0.0);
        let parent = w(3.0, 5.0, 4.9);
        assert_eq!(parent.sub_secs(0.5..1.0).phase().get(), 1.0);
    }

    #[test]
    fn sub_secs_may_extend_past_the_parent_end() {
        // The sub-window is allowed to reach past the parent's declared end;
        // the cursor still measures correctly.
        let sub = w(3.0, 5.0, 5.5).sub_secs(1.5..3.0);
        assert_eq!(sub.end(), 6.0);
        assert!((sub.phase().get() - (5.5 - 4.5) / 1.5).abs() < 1e-6);
    }

    #[test]
    fn sub_secs_chains_in_local_seconds() {
        // Chaining stays relative to each new start: 0.5 into the parent,
        // then 0.25 into that sub-window.
        let sub = w(3.0, 5.0, 4.0).sub_secs(0.5..2.0).sub_secs(0.25..0.75);
        assert_eq!(sub.start(), 3.75);
        assert_eq!(sub.end(), 4.25);
    }

    #[test]
    #[should_panic(expected = "Window::sub_secs requires a finite range with end > start")]
    fn sub_secs_rejects_empty_range() {
        let _ = w(3.0, 5.0, 4.0).sub_secs(0.5..0.5);
    }

    #[test]
    fn clamped_freezes_outside_the_window() {
        // Before: cursor pinned to start.
        let snap = w(3.0, 5.0, 1.0).clamped();
        assert_eq!(snap.current(), 3.0);
        // Inside: unchanged.
        assert_eq!(w(3.0, 5.0, 4.2).clamped().current(), 4.2);
        // After: pinned to end — stable however far the cursor travels.
        assert_eq!(w(3.0, 5.0, 6.0).clamped(), w(3.0, 5.0, 99.0).clamped());
        assert_eq!(w(3.0, 5.0, 6.0).clamped().phase().get(), 1.0);
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
    fn remaining_counts_down_then_zero() {
        // Before the window opens the close is still 3 seconds away — the
        // countdown is to `end`, not capped at the width.
        assert_eq!(w(3.0, 5.0, 2.0).remaining(), 3.0);
        assert!((w(3.0, 5.0, 4.5).remaining() - 0.5).abs() < 1e-6);
        assert_eq!(w(3.0, 5.0, 5.0).remaining(), 0.0);
        assert_eq!(w(3.0, 5.0, 9.0).remaining(), 0.0);
    }

    #[test]
    fn envelope_rises_holds_falls() {
        let e = |c: f32| w(0.0, 3.0, c).envelope(0.5, 0.5);
        assert_eq!(e(-1.0).get(), 0.0);
        assert_eq!(e(0.0).get(), 0.0);
        assert!((e(0.25).get() - 0.5).abs() < 1e-6);
        assert!((e(0.5).get() - 1.0).abs() < 1e-6);
        assert!((e(1.5).get() - 1.0).abs() < 1e-6);
        assert!((e(2.75).get() - 0.5).abs() < 1e-6);
        assert_eq!(e(3.0).get(), 0.0);
        assert_eq!(e(4.0).get(), 0.0);
    }

    #[test]
    fn envelope_skips_non_positive_fades() {
        // fade_in == 0: already at full the moment the window opens.
        assert_eq!(w(0.0, 3.0, 0.0).envelope(0.0, 0.5).get(), 1.0);
        // fade_out == 0: holds full right up to the close.
        assert_eq!(w(0.0, 3.0, 3.0).envelope(0.5, 0.0).get(), 1.0);
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

    #[test]
    #[should_panic(expected = "Window requires finite start/end with end > start")]
    fn phase_rejects_equal_bounds() {
        let _ = w(5.0, 5.0, 5.0).phase();
    }

    #[test]
    #[should_panic(expected = "Window requires finite start/end with end > start")]
    fn raw_progress_rejects_equal_bounds() {
        let _ = w(5.0, 5.0, 7.0).raw_progress();
    }
}
