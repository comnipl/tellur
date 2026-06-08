//! Easing curves for normalized animation progress.
//!
//! Bounded easing functions return [`Phase`], preserving Tellur's normalized
//! progress type so callers can feed the result directly into
//! [`Phase::interpolate`]. Curves that intentionally overshoot the unit interval
//! take an explicit output range and return the interpolated value.

use std::f32::consts::PI;

use crate::phase::Phase;

/// Easing methods on [`Phase`].
pub trait PhaseEasing {
    /// Identity easing.
    fn ease_linear(self) -> Phase;
    /// Smoothstep easing: zero slope at both endpoints.
    fn ease_smoothstep(self) -> Phase;
    /// Cubic ease-out: fast start, gentle settle.
    fn ease_out_cubic(self) -> Phase;
    /// Quintic ease-out.
    fn ease_out_quint(self) -> Phase;
    /// Quintic ease-in-out.
    fn ease_in_out_quint(self) -> Phase;
    /// Exponential ease-in-out.
    fn ease_in_out_expo(self) -> Phase;
    /// Back ease-in between `from` and `to`. Intentionally dips before `from`.
    fn ease_in_back_between(self, from: f32, to: f32) -> f32;
    /// Elastic ease-out between `from` and `to`. Intentionally overshoots past
    /// `to`.
    fn ease_out_elastic_between(self, from: f32, to: f32) -> f32;
}

impl PhaseEasing for Phase {
    fn ease_linear(self) -> Phase {
        self
    }

    fn ease_smoothstep(self) -> Phase {
        let x = self.get();
        Phase::saturating(x * x * (3.0 - 2.0 * x))
    }

    fn ease_out_cubic(self) -> Phase {
        let x = self.get();
        Phase::saturating(1.0 - (1.0 - x).powi(3))
    }

    fn ease_out_quint(self) -> Phase {
        let x = self.get();
        Phase::saturating(1.0 - (1.0 - x).powi(5))
    }

    fn ease_in_out_quint(self) -> Phase {
        let x = self.get();
        let y = if x < 0.5 {
            16.0 * x.powi(5)
        } else {
            1.0 - (-2.0 * x + 2.0).powi(5) * 0.5
        };
        Phase::saturating(y)
    }

    fn ease_in_out_expo(self) -> Phase {
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
        Phase::saturating(y)
    }

    fn ease_in_back_between(self, from: f32, to: f32) -> f32 {
        interpolate_unbounded(from, to, in_back_factor(self))
    }

    fn ease_out_elastic_between(self, from: f32, to: f32) -> f32 {
        interpolate_unbounded(from, to, out_elastic_factor(self))
    }
}

/// Identity easing.
pub fn linear(p: Phase) -> Phase {
    p.ease_linear()
}

/// Smoothstep easing: zero slope at both endpoints.
pub fn smoothstep(p: Phase) -> Phase {
    p.ease_smoothstep()
}

/// Cubic ease-out: fast start, gentle settle.
pub fn out_cubic(p: Phase) -> Phase {
    p.ease_out_cubic()
}

/// Quintic ease-out.
pub fn out_quint(p: Phase) -> Phase {
    p.ease_out_quint()
}

/// Quintic ease-in-out.
pub fn in_out_quint(p: Phase) -> Phase {
    p.ease_in_out_quint()
}

/// Exponential ease-in-out.
pub fn in_out_expo(p: Phase) -> Phase {
    p.ease_in_out_expo()
}

/// Back ease-in between `from` and `to`.
pub fn in_back_between(p: Phase, from: f32, to: f32) -> f32 {
    p.ease_in_back_between(from, to)
}

/// Elastic ease-out between `from` and `to`.
pub fn out_elastic_between(p: Phase, from: f32, to: f32) -> f32 {
    p.ease_out_elastic_between(from, to)
}

fn interpolate_unbounded(from: f32, to: f32, factor: f32) -> f32 {
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
    fn bounded_curves_preserve_endpoints() {
        let curves = [
            linear as fn(Phase) -> Phase,
            smoothstep,
            out_cubic,
            out_quint,
            in_out_quint,
            in_out_expo,
        ];

        for curve in curves {
            assert_eq!(curve(Phase::ZERO), Phase::ZERO);
            assert_eq!(curve(Phase::ONE), Phase::ONE);
        }
    }

    #[test]
    fn bounded_curves_return_phase_values() {
        assert_near(Phase::HALF.ease_smoothstep().get(), 0.5);
        assert_near(Phase::HALF.ease_out_cubic().get(), 0.875);
        assert_near(Phase::HALF.ease_out_quint().get(), 0.96875);
        assert_near(Phase::HALF.ease_in_out_quint().get(), 0.5);
        assert_near(Phase::HALF.ease_in_out_expo().get(), 0.5);
    }

    #[test]
    fn overshoot_curves_interpolate_outside_the_range() {
        assert!(Phase::HALF.ease_in_back_between(0.0, 1.0) < 0.0);
        assert!(Phase::new(0.2).unwrap().ease_out_elastic_between(0.0, 1.0) > 1.0);
        assert!(
            Phase::new(0.2)
                .unwrap()
                .ease_out_elastic_between(80.0, 300.0)
                > 300.0
        );
    }
}
