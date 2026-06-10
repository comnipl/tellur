//! Ambient backdrop texture: faint horizon lines, two "field boundary" rings,
//! and 12 graduated tick marks. Every animation is a one-shot reveal driven by a
//! single clamped `reveal: Window`, so once the reveals saturate (around 1.33s)
//! the snapshot freezes and the component hashes equal every frame —
//! `CachingRenderContext` reuses its raster instead of re-rasterizing an
//! identical image.

use std::f32::consts::{PI, TAU};

use tellur_core::fragment::Fragment;
use tellur_core::geometry::Vec2;
use tellur_core::layer::VectorLayer;
use tellur_core::window::Window;

use super::common::*;

// The reveal window: the earliest event start (first horizon line) to the latest
// event end (last tick). `reveal` is `time.window(START, END).clamped()`, so the
// snapshot freezes once every sub-reveal has finished — which is what lets this
// component hash-equal across the steady-state span and be reused from the cache.
pub const BACKDROP_REVEAL_START: f32 = 0.05;
pub const BACKDROP_REVEAL_END: f32 = 1.332;

#[tellur_core::component(vector)]
pub fn Backdrop(reveal: Window, palette: Palette) -> impl VectorComponent {
    let p = palette;
    // Sub-events are addressed in window-local seconds via `reveal.sub_secs(...)`,
    // so once the clamped `reveal` saturates every sub-window saturates too and
    // the built layer is identical frame to frame.
    VectorLayer::builder()
        .size(SCENE_SIZE)
        // Faint horizon lines sliding in from the left.
        .children((0..18).map(move |i| {
            let y = 64.0 + i as f32 * 56.0;
            let line = reveal.sub_secs((i as f32 * 0.008)..(0.4 + i as f32 * 0.008));
            Rect::builder()
                .position(Vec2(line.ease_in_out_expo(-1920.0, 0.0), y))
                .size(Vec2(1920.0, 1.0))
                .color(p.paper.with_alpha(line.ease_in_out_expo(0.0, 0.022)))
        }))
        // Two extremely dim "field boundary" rings — just inside the HUD frame
        // and just outside it. They suggest "this scene happens inside a
        // measured field" and pull the eye toward center without actually
        // darkening the corners.
        .maybe_child({
            let ring_reveal = reveal.sub_secs(0.55..1.05).ease_in_out_expo(0.0, 1.0);
            (ring_reveal > 0.0).then(|| {
                [(720.0_f32, 1.0_f32), (860.0_f32, 0.55_f32)]
                    .into_iter()
                    .map(move |(r, a_mult)| {
                        Circle::builder()
                            .center(Vec2(CX, CY))
                            .radius(r * ring_reveal)
                            .stroke(p.paper.with_alpha(0.05 * a_mult))
                            .stroke_width(1.0)
                    })
                    .collect::<Fragment>()
            })
        })
        // 12 micro tick marks along the outer "field boundary" at 30° spacing —
        // subliminal "graduated horizon" detail.
        .children((0..12).map(move |i| {
            let a = i as f32 / 12.0 * TAU - PI * 0.5;
            let tick_in = reveal
                .sub_secs((0.7 + i as f32 * 0.012)..(1.15 + i as f32 * 0.012))
                .ease_in_out_expo(0.0, 1.0);
            let major = i % 3 == 0;
            let r_base = 720.0;
            let length = if major { 16.0 } else { 8.0 };
            let mid_r = r_base + length * 0.5;
            let mid = Vec2(CX + a.cos() * mid_r, CY + a.sin() * mid_r);
            FxRect::builder()
                .center(mid)
                .size(Vec2(if major { 2.0 } else { 1.4 }, length * tick_in))
                .angle(a + PI * 0.5)
                .color(p.paper.with_alpha(if major { 0.16 } else { 0.1 }))
        }))
        .build()
}
