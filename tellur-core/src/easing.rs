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
//!
//! The curves also exist as values: [`Easing`] names each curve, and
//! [`Phase::eased`] reshapes a Phase **inside** the unit interval so it can
//! drive a typed interpolation
//! ([`Interpolate`](crate::interpolate::Interpolate)) instead of a bare
//! `f32` — `a.interpolate(b, p.eased(Easing::OutCubic))` eases a `Vec2` /
//! `Anchor` the same way `p.ease_out_cubic(from, to)` eases an `f32`.

use std::f32::consts::PI;

use crate::phase::Phase;
use crate::window::Window;

/// An easing curve as a value. Each variant names one of the
/// [`PhaseEasing`] methods; [`Easing::factor`] evaluates the raw curve and
/// [`Phase::eased`] applies it within the unit interval.
///
/// Derives [`crate::Keyable`] rather than the stock `PartialEq`/`Eq`/`Hash`:
/// [`Easing::CubicBezier`] carries `f32` control points, and floats implement
/// neither trait (`NaN != NaN`), so the derive compares/hashes every variant
/// by bit pattern instead (the same treatment `Window` and `TriggerKind` get).
#[derive(Debug, Clone, Copy, crate::Keyable)]
pub enum Easing {
    /// No curve — the identity.
    Linear,
    /// Smoothstep: zero slope at both endpoints.
    Smoothstep,
    /// Cubic ease-out: fast start, gentle settle.
    OutCubic,
    /// Quintic ease-out.
    OutQuint,
    /// Quintic ease-in-out.
    InOutQuint,
    /// Exponential ease-in-out.
    InOutExpo,
    /// Back ease-in. Overshoots below `0.0` for visual anticipation.
    InBack,
    /// Elastic ease-out. Overshoots above `1.0` for "spring snap" motion.
    OutElastic,
    /// A CSS-style `cubic-bezier(x1, y1, x2, y2)` curve: the cubic Bézier
    /// through `(0, 0)`, `(x1, y1)`, `(x2, y2)`, `(1, 1)`, sampled by solving
    /// for the Bézier parameter at the given `x` (Newton's method with a
    /// bisection fallback). `x1`/`x2` are expected in `[0.0, 1.0]` so the
    /// curve is a function of `x` (monotonic on that axis); `y1`/`y2` are
    /// unconstrained, so, like [`Easing::InBack`] / [`Easing::OutElastic`],
    /// this curve can overshoot `[0.0, 1.0]` when they fall outside it.
    CubicBezier { x1: f32, y1: f32, x2: f32, y2: f32 },
}

impl Easing {
    /// The raw curve value at `p` — **unclamped**, so the overshoot curves
    /// ([`Easing::InBack`], [`Easing::OutElastic`]) may leave `[0.0, 1.0]`.
    /// This is the single source of truth the [`PhaseEasing`] methods and
    /// [`Phase::eased`] both evaluate.
    pub fn factor(self, p: Phase) -> f32 {
        let x = p.get();
        match self {
            Easing::Linear => x,
            Easing::Smoothstep => x * x * (3.0 - 2.0 * x),
            Easing::OutCubic => 1.0 - (1.0 - x).powi(3),
            Easing::OutQuint => 1.0 - (1.0 - x).powi(5),
            Easing::InOutQuint => {
                if x < 0.5 {
                    16.0 * x.powi(5)
                } else {
                    1.0 - (-2.0 * x + 2.0).powi(5) * 0.5
                }
            }
            Easing::InOutExpo => {
                if x <= 0.0 {
                    0.0
                } else if x >= 1.0 {
                    1.0
                } else if x < 0.5 {
                    2.0_f32.powf(20.0 * x - 10.0) * 0.5
                } else {
                    (2.0 - 2.0_f32.powf(-20.0 * x + 10.0)) * 0.5
                }
            }
            Easing::InBack => {
                let c1 = 1.70158;
                let c3 = c1 + 1.0;
                c3 * x.powi(3) - c1 * x.powi(2)
            }
            Easing::OutElastic => {
                if x <= 0.0 {
                    0.0
                } else if x >= 1.0 {
                    1.0
                } else {
                    let c4 = (2.0 * PI) / 3.0;
                    2.0_f32.powf(-10.0 * x) * ((x * 10.0 - 0.75) * c4).sin() + 1.0
                }
            }
            Easing::CubicBezier { x1, y1, x2, y2 } => cubic_bezier(x, x1, y1, x2, y2),
        }
    }
}

/// Samples a CSS-style `cubic-bezier(x1, y1, x2, y2)` curve at `x`: solves
/// the Bézier parameter `t` such that `bezier_axis(t, x1, x2) == x`, then
/// evaluates the Y axis at that `t`. `x` is clamped to `[0.0, 1.0]` first, so
/// the search always has a root (`x1`/`x2` are expected in that range, which
/// keeps the X axis monotonic in `t`).
///
/// The root-find is 8 rounds of Newton's method — fast, but it can jump
/// outside the current bracket for a flat slope — falling back to 12 rounds
/// of bisection using whichever bracket Newton narrowed to, exactly the
/// ported approach (same iteration counts and the same `1e-6` thresholds).
fn cubic_bezier(x: f32, x1: f32, y1: f32, x2: f32, y2: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    if x == 0.0 || x == 1.0 {
        return x;
    }

    let mut lower = 0.0;
    let mut upper = 1.0;
    let mut t = x;

    for _ in 0..8 {
        let curve_x = cubic_bezier_axis(t, x1, x2);
        if curve_x < x {
            lower = t;
        } else {
            upper = t;
        }

        let slope = cubic_bezier_axis_derivative(t, x1, x2);
        if slope.abs() < 1e-6 {
            break;
        }

        let next = t - (curve_x - x) / slope;
        if next <= lower || next >= upper {
            break;
        }

        t = next;
    }

    for _ in 0..12 {
        let curve_x = cubic_bezier_axis(t, x1, x2);
        if (curve_x - x).abs() < 1e-6 {
            break;
        }

        if curve_x < x {
            lower = t;
        } else {
            upper = t;
        }
        t = (lower + upper) * 0.5;
    }

    cubic_bezier_axis(t, y1, y2)
}

/// One axis of the cubic Bézier through `(0, 0)`, `(p1, p2)` control points
/// tied to a shared parameter `t`, `(1, 1)` — the standard cubic Bézier
/// formula specialized to endpoints pinned at `0` and `1`.
fn cubic_bezier_axis(t: f32, p1: f32, p2: f32) -> f32 {
    let inv = 1.0 - t;
    3.0 * inv * inv * t * p1 + 3.0 * inv * t * t * p2 + t * t * t
}

/// Derivative of [`cubic_bezier_axis`] with respect to `t`, driving the
/// Newton step in [`cubic_bezier`].
fn cubic_bezier_axis_derivative(t: f32, p1: f32, p2: f32) -> f32 {
    let inv = 1.0 - t;
    3.0 * inv * inv * p1 + 6.0 * inv * t * (p2 - p1) + 3.0 * t * t * (1.0 - p2)
}

impl Phase {
    /// Reshapes this Phase through `easing`, staying in Phase — the
    /// Phase-to-Phase twin of the [`PhaseEasing`] methods, for driving a
    /// typed [`Interpolate`](crate::interpolate::Interpolate) instead of an
    /// `f32` range.
    ///
    /// Saturating: the overshoot curves ([`Easing::InBack`],
    /// [`Easing::OutElastic`]) clamp to the unit interval here, losing their
    /// snap-past. To keep an overshoot, ease into the value range directly
    /// via the matching `(from, to)` method (e.g.
    /// [`PhaseEasing::ease_out_elastic`]).
    pub fn eased(self, easing: Easing) -> Phase {
        Phase::saturating(easing.factor(self))
    }

    /// Eases through a CSS-style `cubic-bezier(x1, y1, x2, y2)` curve
    /// straight into the `(from, to)` value range — the direct-range twin of
    /// [`Easing::CubicBezier`], preserving overshoot the same way
    /// [`PhaseEasing::ease_out_elastic`] does (an out-of-range `y1`/`y2` is
    /// not clamped).
    ///
    /// Not a [`PhaseEasing`] method: every method there shares the
    /// `(self, from, to)` shape because each names one fully-fixed curve: a
    /// cubic-bezier curve additionally needs its four control points from
    /// the caller, which does not fit that uniform shape. This lives on
    /// `Phase` directly instead of forcing a mismatched arity onto the
    /// trait.
    pub fn ease_bezier(self, x1: f32, y1: f32, x2: f32, y2: f32, from: f32, to: f32) -> f32 {
        lerp_unbounded(from, to, cubic_bezier(self.get(), x1, y1, x2, y2))
    }
}

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
        lerp_unbounded(from, to, Easing::Linear.factor(self))
    }

    fn ease_smoothstep(self, from: f32, to: f32) -> f32 {
        lerp_unbounded(from, to, Easing::Smoothstep.factor(self))
    }

    fn ease_out_cubic(self, from: f32, to: f32) -> f32 {
        lerp_unbounded(from, to, Easing::OutCubic.factor(self))
    }

    fn ease_out_quint(self, from: f32, to: f32) -> f32 {
        lerp_unbounded(from, to, Easing::OutQuint.factor(self))
    }

    fn ease_in_out_quint(self, from: f32, to: f32) -> f32 {
        lerp_unbounded(from, to, Easing::InOutQuint.factor(self))
    }

    fn ease_in_out_expo(self, from: f32, to: f32) -> f32 {
        lerp_unbounded(from, to, Easing::InOutExpo.factor(self))
    }

    fn ease_in_back(self, from: f32, to: f32) -> f32 {
        lerp_unbounded(from, to, Easing::InBack.factor(self))
    }

    fn ease_out_elastic(self, from: f32, to: f32) -> f32 {
        lerp_unbounded(from, to, Easing::OutElastic.factor(self))
    }
}

/// Easing a [`Window`] eases its saturating [`Window::phase`] view — sugar
/// for `w.phase().ease_*(from, to)`, so a sub-windowed chain reads
/// `w.sub_secs(0.4..0.8).ease_out_cubic(0.0, 1.0)` without the intermediate
/// projection.
impl PhaseEasing for Window {
    fn linear(self, from: f32, to: f32) -> f32 {
        self.phase().linear(from, to)
    }

    fn ease_smoothstep(self, from: f32, to: f32) -> f32 {
        self.phase().ease_smoothstep(from, to)
    }

    fn ease_out_cubic(self, from: f32, to: f32) -> f32 {
        self.phase().ease_out_cubic(from, to)
    }

    fn ease_out_quint(self, from: f32, to: f32) -> f32 {
        self.phase().ease_out_quint(from, to)
    }

    fn ease_in_out_quint(self, from: f32, to: f32) -> f32 {
        self.phase().ease_in_out_quint(from, to)
    }

    fn ease_in_out_expo(self, from: f32, to: f32) -> f32 {
        self.phase().ease_in_out_expo(from, to)
    }

    fn ease_in_back(self, from: f32, to: f32) -> f32 {
        self.phase().ease_in_back(from, to)
    }

    fn ease_out_elastic(self, from: f32, to: f32) -> f32 {
        self.phase().ease_out_elastic(from, to)
    }
}

#[inline]
fn lerp_unbounded(from: f32, to: f32, factor: f32) -> f32 {
    from + (to - from) * factor
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

    #[test]
    fn eased_matches_the_f32_methods_for_bounded_curves() {
        for x in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let p = Phase::new(x).unwrap();
            assert_near(p.eased(Easing::OutCubic).get(), p.ease_out_cubic(0.0, 1.0));
            assert_near(
                p.eased(Easing::InOutExpo).get(),
                p.ease_in_out_expo(0.0, 1.0),
            );
            assert_near(p.eased(Easing::Linear).get(), p.get());
        }
    }

    #[test]
    fn eased_saturates_overshoot_curves() {
        // In-back dips below zero mid-curve; the Phase form clamps it to 0
        // while the (from, to) form keeps the overshoot.
        assert!(Phase::HALF.ease_in_back(0.0, 1.0) < 0.0);
        assert_eq!(Phase::HALF.eased(Easing::InBack).get(), 0.0);
    }

    #[test]
    fn eased_drives_typed_interpolation() {
        use crate::geometry::Vec2;
        use crate::interpolate::Interpolate;
        // OutCubic at 0.5 is 0.875; the eased Phase carries that into a Vec2.
        let p = Phase::HALF.eased(Easing::OutCubic);
        let v = Vec2(0.0, 0.0).interpolate(Vec2(10.0, 20.0), p);
        assert_near(v.0, 8.75);
        assert_near(v.1, 17.5);
    }

    #[test]
    fn cubic_bezier_hits_endpoints() {
        let curve = Easing::CubicBezier {
            x1: 0.25,
            y1: 0.1,
            x2: 0.25,
            y2: 1.0,
        };
        assert_near(curve.factor(Phase::ZERO), 0.0);
        assert_near(curve.factor(Phase::ONE), 1.0);
    }

    #[test]
    fn cubic_bezier_identity_control_points_are_linear() {
        // cubic-bezier(0, 0, 1, 1) traces the diagonal, i.e. the identity.
        let curve = Easing::CubicBezier {
            x1: 0.0,
            y1: 0.0,
            x2: 1.0,
            y2: 1.0,
        };
        for x in [0.0, 0.2, 0.5, 0.75, 1.0] {
            let p = Phase::new(x).unwrap();
            assert_near(curve.factor(p), x);
        }
    }

    #[test]
    fn cubic_bezier_matches_ported_reference_curve() {
        // `summon_ease` from the ported source
        // (movies/202606/shorts_sqrt2_plus_sqrt3/src/explanation.rs) is
        // `cubic_bezier(progress, 0.76, 0.0, 0.0, 0.93)`; this pins the port
        // to a value computed independently from that same formula.
        let curve = Easing::CubicBezier {
            x1: 0.76,
            y1: 0.0,
            x2: 0.0,
            y2: 0.93,
        };
        assert_near(curve.factor(Phase::HALF), 0.775_318_4);
    }

    #[test]
    fn cubic_bezier_can_overshoot_like_back_and_elastic() {
        // A back-style bezier (control points borrowed from the common
        // "easeInOutBack" approximation) dips below 0 before rising, the
        // same overshoot shape as `Easing::InBack`.
        let curve = Easing::CubicBezier {
            x1: 0.68,
            y1: -0.55,
            x2: 0.265,
            y2: 1.55,
        };
        assert!(curve.factor(Phase::new(0.1).unwrap()) < 0.0);
    }

    #[test]
    fn ease_bezier_preserves_overshoot_into_the_value_range() {
        let p = Phase::new(0.1).unwrap();
        assert!(p.ease_bezier(0.68, -0.55, 0.265, 1.55, 0.0, 1.0) < 0.0);
        assert!(p.ease_bezier(0.68, -0.55, 0.265, 1.55, 80.0, 300.0) < 80.0);
    }

    #[test]
    fn eased_cubic_bezier_saturates_overshoot() {
        // Mirrors `eased_saturates_overshoot_curves`: the raw factor dips
        // below 0, but `Phase::eased` clamps it back into the unit interval.
        let curve = Easing::CubicBezier {
            x1: 0.68,
            y1: -0.55,
            x2: 0.265,
            y2: 1.55,
        };
        let p = Phase::new(0.1).unwrap();
        assert!(curve.factor(p) < 0.0);
        assert_eq!(p.eased(curve).get(), 0.0);
    }

    #[test]
    fn cubic_bezier_variants_are_keyable_by_bit_pattern() {
        let a = Easing::CubicBezier {
            x1: 0.25,
            y1: 0.1,
            x2: 0.25,
            y2: 1.0,
        };
        let b = a;
        assert_eq!(a, b);

        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let hash_of = |e: Easing| {
            let mut s = DefaultHasher::new();
            e.hash(&mut s);
            s.finish()
        };
        assert_eq!(hash_of(a), hash_of(b));

        let different = Easing::CubicBezier {
            x1: 0.26,
            y1: 0.1,
            x2: 0.25,
            y2: 1.0,
        };
        assert_ne!(a, different);

        // Distinct variants (same underlying discriminant-free shape as
        // enums without payload) must still compare unequal.
        assert_ne!(a, Easing::Linear);
    }
}
