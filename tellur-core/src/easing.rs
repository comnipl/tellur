//! Easing curves for normalized animation progress.
//!
//! Bounded easing functions return [`Phase`], preserving Tellur's normalized
//! progress type so callers can feed the result directly into
//! [`Phase::interpolate`]. Curves that intentionally overshoot the unit interval
//! return `f32` and are documented separately.

use std::f32::consts::PI;

use crate::phase::Phase;

/// Identity easing.
pub fn linear(p: Phase) -> Phase {
    p
}

/// Smoothstep easing: zero slope at both endpoints.
pub fn smoothstep(p: Phase) -> Phase {
    let x = p.get();
    Phase::saturating(x * x * (3.0 - 2.0 * x))
}

/// Cubic ease-out: fast start, gentle settle.
pub fn out_cubic(p: Phase) -> Phase {
    let x = p.get();
    Phase::saturating(1.0 - (1.0 - x).powi(3))
}

/// Quintic ease-out.
pub fn out_quint(p: Phase) -> Phase {
    let x = p.get();
    Phase::saturating(1.0 - (1.0 - x).powi(5))
}

/// Quintic ease-in-out.
pub fn in_out_quint(p: Phase) -> Phase {
    let x = p.get();
    let y = if x < 0.5 {
        16.0 * x.powi(5)
    } else {
        1.0 - (-2.0 * x + 2.0).powi(5) * 0.5
    };
    Phase::saturating(y)
}

/// Exponential ease-in-out.
pub fn in_out_expo(p: Phase) -> Phase {
    let x = p.get();
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

/// Back ease-in.
///
/// This curve intentionally dips below `0.0`, so it returns a scalar rather
/// than [`Phase`]. Clamp or map the output explicitly if bounded progress is
/// desired.
pub fn in_back(p: Phase) -> f32 {
    let x = p.get();
    let c1 = 1.70158;
    let c3 = c1 + 1.0;
    c3 * x.powi(3) - c1 * x.powi(2)
}

/// Elastic ease-out.
///
/// This curve intentionally overshoots above `1.0`, so it returns a scalar
/// rather than [`Phase`]. Clamp or map the output explicitly if bounded
/// progress is desired.
pub fn out_elastic(p: Phase) -> f32 {
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
        assert_near(smoothstep(Phase::HALF).get(), 0.5);
        assert_near(out_cubic(Phase::HALF).get(), 0.875);
        assert_near(out_quint(Phase::HALF).get(), 0.96875);
        assert_near(in_out_quint(Phase::HALF).get(), 0.5);
        assert_near(in_out_expo(Phase::HALF).get(), 0.5);
    }

    #[test]
    fn overshoot_curves_are_not_clamped_to_phase() {
        assert!(in_back(Phase::HALF) < 0.0);
        assert!(out_elastic(Phase::new(0.2).unwrap()) > 1.0);
    }
}
