//! STREAK — the motion-blur showcase: a comet that rests on the lower band,
//! darts across the frame, rests, and darts back.
//!
//! The element is deliberately bursty: the dashes are fast enough that the
//! wrapping `MotionBlur` draws a visible trail, and the rests in between
//! saturate every phase so the rasterized layer collapses back to one
//! cached entry (the blur's static short-circuit) — the same frame both
//! shows off the effect and exercises its cache behavior.

use tellur_core::geometry::Vec2;
use tellur_core::layer::VectorLayer;
use tellur_core::time::{LocalTime, Time};

use super::common::*;

/// Trailing shutter for the comet, in seconds. One 60 fps frame: the
/// dash's eased midpoint peaks around 14 000 logical px/s, so even this
/// short shutter draws a ~240 px trail; 16 samples keep the dots
/// overlapping into a continuous streak.
pub const STREAK_SHUTTER: f32 = 1.0 / 60.0;

/// Sample count matched to [`STREAK_SHUTTER`] (see above).
pub const STREAK_SAMPLES: u32 = 16;

const FADE_IN_START: f32 = 1.7;
const FADE_IN_END: f32 = 2.0;
const FADE_OUT_START: f32 = 6.9;
const FADE_OUT_END: f32 = 7.3;

const DASH_LTR_START: f32 = 2.3;
const DASH_LTR_END: f32 = 2.8;
const DASH_RTL_START: f32 = 4.7;
const DASH_RTL_END: f32 = 5.2;

const X_REST: f32 = 250.0;
const X_SPAN: f32 = 1420.0;
const STREAK_Y: f32 = 924.0;
const CORE_RADIUS: f32 = 9.0;
const GLOW_RADIUS: f32 = 17.0;

#[tellur_core::component(vector)]
pub fn Streak(time: LocalTime, palette: Palette) -> impl VectorComponent {
    let p = palette;
    let visible = time.phase(FADE_IN_START, FADE_IN_END).get()
        * (1.0 - time.phase(FADE_OUT_START, FADE_OUT_END).get());

    // Two eased dashes share one axis: `fwd - back` rises 0 → 1 on the
    // left-to-right dash and falls back 1 → 0 on the return. Both phases
    // saturate outside their windows, so the baked position is constant
    // during every rest.
    let fwd = time
        .phase(DASH_LTR_START, DASH_LTR_END)
        .eased(Easing::InOutQuint);
    let back = time
        .phase(DASH_RTL_START, DASH_RTL_END)
        .eased(Easing::InOutQuint);
    let x = X_REST + (fwd.get() - back.get()) * X_SPAN;

    VectorLayer::builder()
        .size(SCENE_SIZE)
        .child(
            Circle::builder()
                .radius(GLOW_RADIUS)
                .fill(p.pink.with_alpha(0.22 * visible))
                .place_at(Vec2(x - GLOW_RADIUS, STREAK_Y - GLOW_RADIUS)),
        )
        .child(
            Circle::builder()
                .radius(CORE_RADIUS)
                .fill(p.cyan.with_alpha(visible))
                .place_at(Vec2(x - CORE_RADIUS, STREAK_Y - CORE_RADIUS)),
        )
        .build()
}
