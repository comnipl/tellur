//! Ambient backdrop texture: faint horizon lines, two "field boundary" rings,
//! and 12 graduated tick marks. Every animation is a one-shot reveal driven by a
//! single `reveal: Phase`, so once the reveals saturate (`reveal == 1.0`, around
//! 1.33s) the component hashes equal every frame and `CachingRenderContext`
//! reuses its raster instead of re-rasterizing an identical image.

use std::f32::consts::{PI, TAU};

use tellur_core::fragment::Fragment;
use tellur_core::geometry::Vec2;
use tellur_core::layer::VectorLayer;
use tellur_core::phase::Phase;

use super::common::*;

// The reveal window: the earliest event start (first horizon line) to the latest
// event end (last tick). `reveal` is `time.phase(START, END)`, so it saturates to
// 1.0 once every sub-reveal has finished — which is what lets this component
// hash-equal across the steady-state span and be reused from the cache.
pub const BACKDROP_REVEAL_START: f32 = 0.05;
pub const BACKDROP_REVEAL_END: f32 = 1.332;
const BACKDROP_REVEAL_WIDTH: f32 = BACKDROP_REVEAL_END - BACKDROP_REVEAL_START;

// Sub-phase of an event spanning `[start, end]` (both measured from the reveal
// window start) at virtual elapsed time `t`. Mirrors `hud::local_phase`.
fn local_phase(t: f32, start: f32, end: f32) -> Phase {
    Phase::saturating((t - start) / (end - start))
}

#[tellur_core::component(vector)]
pub fn Backdrop(reveal: Phase, palette: Palette) -> impl VectorComponent {
    let p = palette;
    // Virtual elapsed seconds inside the reveal window. Once `reveal` saturates
    // this is constant, so every sub-`local_phase` below saturates too and the
    // built layer is identical frame to frame.
    let t = reveal.get() * BACKDROP_REVEAL_WIDTH;
    // bg color is painted by the outer `.background(...)`. This layer carries
    // only the faintest ambient texture so the foreground reads as the
    // intended subject.
    VectorLayer::builder()
        .size(SCENE_SIZE)
        // Faint horizon lines sliding in from the left.
        .children((0..18).map(move |i| {
            let y = 64.0 + i as f32 * 56.0;
            let reveal = ease_in_out_expo(local_phase(t, i as f32 * 0.008, 0.4 + i as f32 * 0.008));
            Rect::builder()
                .position(Vec2(lerp(-1920.0, 0.0, reveal), y))
                .size(Vec2(1920.0, 1.0))
                .color(alpha(p.paper, 0.022 * reveal))
        }))
        // Two extremely dim "field boundary" rings — just inside the HUD frame
        // and just outside it. They suggest "this scene happens inside a
        // measured field" and pull the eye toward center without actually
        // darkening the corners.
        .maybe_child({
            let ring_reveal = ease_in_out_expo(local_phase(t, 0.55, 1.05));
            (ring_reveal > 0.0).then(|| {
                [(720.0_f32, 1.0_f32), (860.0_f32, 0.55_f32)]
                    .into_iter()
                    .map(move |(r, a_mult)| {
                        Circle::builder()
                            .center(Vec2(CX, CY))
                            .radius(r * ring_reveal)
                            .stroke(alpha(p.paper, 0.05 * a_mult))
                            .stroke_width(1.0)
                    })
                    .collect::<Fragment>()
            })
        })
        // 12 micro tick marks along the outer "field boundary" at 30° spacing —
        // subliminal "graduated horizon" detail.
        .children((0..12).map(move |i| {
            let a = i as f32 / 12.0 * TAU - PI * 0.5;
            let reveal = ease_in_out_expo(local_phase(
                t,
                0.7 + i as f32 * 0.012,
                1.15 + i as f32 * 0.012,
            ));
            let major = i % 3 == 0;
            let r_base = 720.0;
            let length = if major { 16.0 } else { 8.0 };
            let mid_r = r_base + length * 0.5;
            let mid = Vec2(CX + a.cos() * mid_r, CY + a.sin() * mid_r);
            FxRect::builder()
                .center(mid)
                .size(Vec2(if major { 2.0 } else { 1.4 }, length * reveal))
                .angle(a + PI * 0.5)
                .color(alpha(p.paper, if major { 0.16 } else { 0.1 }))
                .opacity(1.0)
                .scale(Vec2(1.0, 1.0))
        }))
        .build()
}
