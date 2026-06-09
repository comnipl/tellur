//! Easing curves for normalized animation progress.
//!
//! Every method on [`PhaseEasing`] takes the [`Phase`] as the driver plus a
//! `(from, to)` output range and returns an `f32` — the eased value already
//! interpolated into the caller's quantity (alpha, length, radius, …). The
//! uniform shape means callers never juggle "does this return Phase or
//! f32?" — they pick the curve and the range. Bounded curves
//! (`linear` / smoothstep / cubic / quint / expo) stay inside
//! `[from, to]`; overshoot curves (`ease_in_back` / `ease_out_elastic`)
//! intentionally exceed the range — that visual "snap-past" is the point.

use std::f32::consts::PI;

use crate::phase::Phase;

/// Easing methods on [`Phase`]. Every method takes `(from, to)` as the
/// output range and returns the eased `f32` interpolated into it. Callers
/// pick `(0.0, 1.0)` for an alpha-style factor and `(start, end)` for a
/// physical quantity (radius, x-position, …).
pub trait PhaseEasing {
    /// Linear interpolation — no easing curve. `phase.linear(from, to)`
    /// returns `from + (to - from) * phase.get()`. The identity-easing
    /// shape, kept on the trait so callers reach for it the same way they
    /// reach for the eased variants.
    fn linear(self, from: f32, to: f32) -> f32;
    /// Smoothstep easing: zero slope at both endpoints.
    fn ease_smoothstep(self, from: f32, to: f32) -> f32;
    /// Cubic ease-out: fast start, gentle settle.
    fn ease_out_cubic(self, from: f32, to: f32) -> f32;
    /// Quintic ease-out.
    fn ease_out_quint(self, from: f32, to: f32) -> f32;
    /// Quintic ease-in-out.
    fn ease_in_out_quint(self, from: f32, to: f32) -> f32;
    /// Exponential ease-in-out.
    fn ease_in_out_expo(self, from: f32, to: f32) -> f32;
    /// Back ease-in. Intentionally dips before `from` before snapping to
    /// `to` — used for visual anticipation.
    fn ease_in_back(self, from: f32, to: f32) -> f32;
    /// Elastic ease-out. Intentionally overshoots past `to` before
    /// settling — used for "spring snap" motion.
    fn ease_out_elastic(self, from: f32, to: f32) -> f32;
}

impl PhaseEasing for Phase {
    fn linear(self, from: f32, to: f32) -> f32 {
        lerp_unbounded(from, to, self.get())
    }

    fn ease_smoothstep(self, from: f32, to: f32) -> f32 {
        let x = self.get();
        lerp_unbounded(from, to, x * x * (3.0 - 2.0 * x))
    }

    fn ease_out_cubic(self, from: f32, to: f32) -> f32 {
        let x = self.get();
        lerp_unbounded(from, to, 1.0 - (1.0 - x).powi(3))
    }

    fn ease_out_quint(self, from: f32, to: f32) -> f32 {
        let x = self.get();
        lerp_unbounded(from, to, 1.0 - (1.0 - x).powi(5))
    }

    fn ease_in_out_quint(self, from: f32, to: f32) -> f32 {
        let x = self.get();
        let y = if x < 0.5 {
            16.0 * x.powi(5)
        } else {
            1.0 - (-2.0 * x + 2.0).powi(5) * 0.5
        };
        lerp_unbounded(from, to, y)
    }

    fn ease_in_out_expo(self, from: f32, to: f32) -> f32 {
        let x = self.get();
        let y = if x <= 0.0 {
            0.0
        } else if x >= 1.0 {
            1.0
        } else if x < 0.5 {
            2.0_f32.powf(20.0 * x - 10.0) * 0.5
        } else {
            (2.0 - 2.0_f32.powf(-20.0 * x + 10.0)) * 0.5
        };
        lerp_unbounded(from, to, y)
    }

    fn ease_in_back(self, from: f32, to: f32) -> f32 {
        lerp_unbounded(from, to, in_back_factor(self))
    }

    fn ease_out_elastic(self, from: f32, to: f32) -> f32 {
        lerp_unbounded(from, to, out_elastic_factor(self))
    }
}

#[inline]
fn lerp_unbounded(from: f32, to: f32, factor: f32) -> f32 {
    from + (to - from) * factor
}

fn in_back_factor(p: Phase) -> f32 {
    let x = p.get();
    let c1 = 1.70158;
    let c3 = c1 + 1.0;
    c3 * x.powi(3) - c1 * x.powi(2)
}

fn out_elastic_factor(p: Phase) -> f32 {
    let x = p.get();
    if x <= 0.0 {
        0.0
    } else if x >= 1.0 {
        1.0
    } else {
        let c4 = (2.0 * PI) / 3.0;
        2.0_f32.powf(-10.0 * x) * ((x * 10.0 - 0.75) * c4).sin() + 1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_near(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 1e-6,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn bounded_curves_hit_endpoints() {
        let curves: [fn(Phase) -> f32; 6] = [
            |p| p.linear(0.0, 1.0),
            |p| p.ease_smoothstep(0.0, 1.0),
            |p| p.ease_out_cubic(0.0, 1.0),
            |p| p.ease_out_quint(0.0, 1.0),
            |p| p.ease_in_out_quint(0.0, 1.0),
            |p| p.ease_in_out_expo(0.0, 1.0),
        ];

        for curve in curves {
            assert_near(curve(Phase::ZERO), 0.0);
            assert_near(curve(Phase::ONE), 1.0);
        }
    }

    #[test]
    fn linear_is_the_lerp() {
        assert_near(Phase::HALF.linear(0.0, 10.0), 5.0);
        assert_near(Phase::HALF.linear(10.0, 0.0), 5.0);
        assert_near(Phase::ZERO.linear(80.0, 300.0), 80.0);
        assert_near(Phase::ONE.linear(80.0, 300.0), 300.0);
    }

    #[test]
    fn bounded_curves_match_known_midpoints() {
        assert_near(Phase::HALF.ease_smoothstep(0.0, 1.0), 0.5);
        assert_near(Phase::HALF.ease_out_cubic(0.0, 1.0), 0.875);
        assert_near(Phase::HALF.ease_out_quint(0.0, 1.0), 0.96875);
        assert_near(Phase::HALF.ease_in_out_quint(0.0, 1.0), 0.5);
        assert_near(Phase::HALF.ease_in_out_expo(0.0, 1.0), 0.5);
    }

    #[test]
    fn bounded_curves_scale_to_arbitrary_range() {
        // Linear half between (80, 300) is 190.
        assert_near(Phase::HALF.linear(80.0, 300.0), 190.0);
        // Cubic at 0.5 is 0.875 (eased into 1.0); scaled to (10, 30) is 27.5.
        assert_near(Phase::HALF.ease_out_cubic(10.0, 30.0), 27.5);
    }

    #[test]
    fn overshoot_curves_leave_the_range() {
        assert!(Phase::HALF.ease_in_back(0.0, 1.0) < 0.0);
        assert!(Phase::new(0.2).unwrap().ease_out_elastic(0.0, 1.0) > 1.0);
        assert!(Phase::new(0.2).unwrap().ease_out_elastic(80.0, 300.0) > 300.0);
    }

    #[test]
    fn swapping_from_to_reverses_curve() {
        // Linear: (0→1) and (1→0) are mirror images.
        for x in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let p = Phase::new(x).unwrap();
            assert_near(p.linear(1.0, 0.0), 1.0 - p.linear(0.0, 1.0));
            assert_near(p.ease_out_cubic(1.0, 0.0), 1.0 - p.ease_out_cubic(0.0, 1.0));
        }
    }
}
