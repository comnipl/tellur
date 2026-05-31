//! Ambient backdrop texture: faint horizon lines, two "field boundary" rings,
//! and 12 graduated tick marks. Every animation here is a one-shot reveal, so
//! once the reveals saturate (~0.6s) the layer is byte-identical every frame
//! and `CachingRenderContext` reuses its raster.

use std::f32::consts::{PI, TAU};

use tellur_core::fragment::Fragment;
use tellur_core::geometry::Vec2;
use tellur_core::layer::VectorLayer;
use tellur_core::time::{Time, TimelineTime};

use super::common::*;

#[tellur_core::component(vector)]
pub fn Backdrop(time: TimelineTime, palette: Palette) -> impl VectorComponent {
    let p = palette;
    // bg color is painted by the outer `.background(...)`. This layer carries
    // only the faintest ambient texture so the foreground reads as the
    // intended subject.
    VectorLayer::builder()
        .size(SCENE_SIZE)
        // Faint horizon lines sliding in from the left.
        .children((0..18).map(move |i| {
            let y = 64.0 + i as f32 * 56.0;
            let reveal =
                ease_in_out_expo(time.phase(0.05 + i as f32 * 0.008, 0.45 + i as f32 * 0.008));
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
            let ring_reveal = ease_in_out_expo(time.phase(0.6, 1.1));
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
            let reveal =
                ease_in_out_expo(time.phase(0.75 + i as f32 * 0.012, 1.2 + i as f32 * 0.012));
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
