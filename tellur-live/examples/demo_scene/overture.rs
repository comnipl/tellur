//! 01 / OVERTURE — clamp bars frame a spinning hero square dressed with a
//! reticle, registration dots, axis rays, a measurement tag, and an exit
//! sweep that hands the frame off to FIELD.

use std::f32::consts::PI;

use tellur_core::color::Color;
use tellur_core::geometry::{Anchor, Vec2};
use tellur_core::layer::VectorLayer;
use tellur_core::placement::Placed;
use tellur_core::text::Weight;
use tellur_core::time::{Time, TimelineTime};
use tellur_core::vector::VectorComponent;

use super::common::*;

#[tellur_core::component(vector)]
pub fn overture(time: TimelineTime, palette: Palette) -> impl VectorComponent {
    let p = palette;
    if time.during(0.0, 2.2).is_none() {
        return VectorLayer::builder().size(SCENE_SIZE).build();
    }

    // Hero square: controlled ease_out_cubic pop (no rubbery elastic),
    // a slow drift-spin, and an ease_in_back dismissal.
    let hero_in = ease_out_cubic(time.phase(0.55, 1.2));
    let hero_out = ease_in_back(time.phase(1.7, 2.15));
    let hero_life = (hero_in * (1.0 - hero_out)).clamp(0.0, 1.0);
    let spin = time.seconds() * 0.35 + PI * 0.25;
    let scale = (0.55 + hero_in * 0.45) * (1.0 - hero_out);
    let s_clamped = scale.max(0.001);

    // Registration markings inside the hero square — a reticle (cross arms +
    // a center ring + gap dashes) plus four small corner dots, all painted
    // in the bg color so they read as "cut into the paper". They rotate
    // with the square via the same `spin`.
    let mark_color = alpha(p.bg, hero_life * 0.55);
    let cross_arm = 38.0 * s_clamped;
    let arm_inner_gap = 8.0 * s_clamped;
    let arm_outer = cross_arm;
    let arm_segment = arm_outer - arm_inner_gap;
    let arm_mid = (arm_inner_gap + arm_outer) * 0.5;
    let cs2 = spin.cos();
    let sn2 = spin.sin();
    let corner_off = 110.0 * s_clamped;
    let cs = spin.cos();
    let sn = spin.sin();

    let ray_in = ease_out_cubic(time.phase(0.85, 1.4)) * (1.0 - hero_out);
    let tag_in =
        ease_in_out_expo(time.phase(0.95, 1.35)) * (1.0 - ease_in_out_expo(time.phase(1.55, 2.0)));
    let sweep = ease_in_out_expo(time.phase(1.45, 2.0));

    // Two clamp bars — one cyan above, one pink below — that frame the
    // central hero. Snappy ease_in_out_expo in, ease_in_back out (lifts off
    // the center toward their respective edges). End-cap mini bracket ticks
    // turn each bar into a "measurement span" instead of an anonymous slab.
    let bars: [(f32, f32, Color); 2] = [(-200.0, -1.0, p.cyan), (200.0, 1.0, p.pink)];
    let bar_w = 1200.0_f32;
    let bar_h = 24.0_f32;

    VectorLayer::builder()
        .size(SCENE_SIZE)
        .children(
            bars.into_iter()
                .enumerate()
                .flat_map(move |(i, (dy, side, color))| {
                    let stagger = i as f32 * 0.06;
                    let enter = ease_in_out_expo(time.phase(0.32 + stagger, 0.92 + stagger));
                    let leave =
                        ease_in_back(time.phase(1.55 + stagger * 0.5, 2.05 + stagger * 0.5));
                    let bar_x = lerp(side * 2400.0 + CX, CX, enter);
                    let exit_dy = leave * if side > 0.0 { 320.0 } else { -320.0 };
                    let alpha_factor = (enter * (1.0 - leave)).clamp(0.0, 1.0) * 0.92;
                    let y_center = CY + dy + exit_dy;

                    let bar = fx_rect(
                        Vec2(bar_x, y_center),
                        Vec2(bar_w, bar_h),
                        0.0,
                        color,
                        alpha_factor,
                        Vec2(1.0, 1.0),
                    );

                    // End-cap mini brackets at each bar end — small perpendicular
                    // ticks that turn the bar into a clear measurement span. Same color
                    // as the bar; staggered tiny so they "arrive" with the bar.
                    let cap_pop = ease_out_cubic(time.phase(0.65 + stagger, 1.05 + stagger))
                        * (1.0
                            - ease_in_back(time.phase(1.55 + stagger * 0.5, 1.95 + stagger * 0.5)));
                    let caps = (cap_pop > 0.0)
                        .then(|| {
                            let cap_h = 28.0;
                            let cap_w = 3.0;
                            [-1.0_f32, 1.0].into_iter().filter_map(move |cap_side| {
                                let cap_x = bar_x + cap_side * (bar_w * 0.5 + 1.0);
                                rect(
                                    Vec2(cap_x - cap_w * 0.5, y_center - cap_h * 0.5),
                                    Vec2(cap_w, cap_h * cap_pop),
                                    alpha(color, alpha_factor),
                                )
                            })
                        })
                        .into_iter()
                        .flatten();

                    bar.into_iter().chain(caps)
                }),
        )
        .maybe_child(fx_rect(
            Vec2(CX, CY),
            Vec2(280.0, 280.0),
            spin,
            p.paper,
            hero_life,
            Vec2(s_clamped, s_clamped),
        ))
        .maybe_child(fx_outline_rect(
            Vec2(CX, CY),
            Vec2(420.0, 420.0),
            -spin * 0.4,
            alpha(p.pink, hero_life * 0.82),
            1.0,
            Vec2(s_clamped, s_clamped),
            3.0,
        ))
        // Crosshair arms — note the gap in the middle (drawn as 4 short
        // segments) so the reticle reads as a scope mark, not a solid plus.
        // Four offset directions: +x, -x, +y, -y in local space.
        .children(
            [
                (1.0_f32, 0.0_f32, false),
                (-1.0, 0.0, false),
                (0.0, 1.0, true),
                (0.0, -1.0, true),
            ]
            .into_iter()
            .filter_map(move |(dx, dy, vertical)| {
                let lx = dx * arm_mid;
                let ly = dy * arm_mid;
                let pos = Vec2(CX + lx * cs2 - ly * sn2, CY + lx * sn2 + ly * cs2);
                let (w, h) = if vertical {
                    (2.0, arm_segment)
                } else {
                    (arm_segment, 2.0)
                };
                fx_rect(pos, Vec2(w, h), spin, mark_color, 1.0, Vec2(1.0, 1.0))
            }),
        )
        // Small open ring at the center of the reticle.
        .maybe_child(circle(
            Vec2(CX, CY),
            6.0 * s_clamped,
            None,
            Some((mark_color, 1.5)),
        ))
        // A tiny solid dot at the very center for the bullseye.
        .maybe_child(circle(
            Vec2(CX, CY),
            1.5 * s_clamped,
            Some(mark_color),
            None,
        ))
        // Four corner dots inside the paper square, offset 110 from center then
        // rotated by `spin` to follow the square's orientation.
        .children(
            [(-1.0_f32, -1.0_f32), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)]
                .into_iter()
                .filter_map(move |(dx, dy)| {
                    let lx = dx * corner_off;
                    let ly = dy * corner_off;
                    let pos = Vec2(CX + lx * cs - ly * sn, CY + lx * sn + ly * cs);
                    circle(pos, 3.5 * hero_life, Some(mark_color), None)
                }),
        )
        // Four registration dots locked onto the outline corners, alternating
        // pink/cyan so the framing reads as intentional design pairs. Each is
        // linked back to the center by a dim hairline ray, which turns the
        // outside dots from "floating decorations" into "axis terminators".
        .children((0..4).flat_map(move |s| {
            let a = s as f32 * PI * 0.5 + spin * 0.5 + PI * 0.25;
            let r = 230.0;
            let pos = Vec2(CX + a.cos() * r, CY + a.sin() * r);

            // Hairline from outside the paper-square corner (~200) to just
            // inside the outside dot. The outline frame's diagonal corner
            // sits at ~297, so the ray crosses through it visually.
            let ray = (ray_in > 0.0)
                .then(|| {
                    let inner_r = 200.0;
                    let outer_r = r - 10.0;
                    let length = (outer_r - inner_r) * ray_in;
                    let mid_r = inner_r + length * 0.5;
                    let mid = Vec2(CX + a.cos() * mid_r, CY + a.sin() * mid_r);
                    fx_rect(
                        mid,
                        Vec2(1.5, length),
                        a + PI * 0.5,
                        alpha(p.paper, hero_life * 0.45),
                        1.0,
                        Vec2(1.0, 1.0),
                    )
                })
                .flatten();

            let dot = circle(
                pos,
                6.0 * hero_life,
                Some(alpha(if s % 2 == 0 { p.pink } else { p.cyan }, hero_life)),
                None,
            );

            // Small index tag next to each outside dot. The tag follows the
            // dot's rotated position so it always reads on the outside.
            let label_in = ease_out_cubic(time.phase(1.0 + s as f32 * 0.04, 1.4 + s as f32 * 0.04));
            let label_alpha =
                hero_life * label_in * (1.0 - ease_in_back(time.phase(1.7, 2.05))) * 0.6;
            let tag = (label_alpha > 0.0)
                .then(|| {
                    let label_r = r + 18.0;
                    let lpos = Vec2(CX + a.cos() * label_r, CY + a.sin() * label_r);
                    let tag_text = format!("0{}", s + 1);
                    // Anchor toward the outward direction.
                    let anchor = if a.cos().abs() > a.sin().abs() {
                        if a.cos() > 0.0 {
                            Anchor::CENTER_LEFT
                        } else {
                            Anchor::CENTER_RIGHT
                        }
                    } else if a.sin() > 0.0 {
                        Anchor::TOP_CENTER
                    } else {
                        Anchor::BOTTOM_CENTER
                    };
                    label(
                        lpos,
                        anchor,
                        &tag_text,
                        10.0,
                        alpha(p.paper, label_alpha.clamp(0.0, 1.0)),
                        Weight::NORMAL,
                    )
                })
                .flatten();

            ray.into_iter().chain(dot).chain(tag)
        }))
        // Length tag beneath the central composition — small data-design touch
        // that gives the OVERTURE a "measurement readout" character.
        .maybe_children((tag_in > 0.0).then(|| length_tag(p, hero_life, tag_in)))
        // Pink horizontal scan stripe sweeping vertically as the scene exits —
        // a transition wipe that hands the frame off to FIELD.
        .maybe_child(if sweep > 0.0 && sweep < 1.0 {
            let y = lerp(-80.0, SCENE_SIZE.1 + 80.0, sweep);
            let visibility = 4.0 * sweep * (1.0 - sweep);
            rect(
                Vec2(0.0, y - 3.0),
                Vec2(SCENE_SIZE.0, 6.0),
                alpha(p.pink, visibility * 0.88),
            )
        } else {
            None
        })
        .build()
}

// The OVERTURE's measurement readout: two end ticks, a growing span bar, and
// a `L = 280 PX` caption, emitted in that order.
fn length_tag(
    p: Palette,
    hero_life: f32,
    tag_in: f32,
) -> impl Iterator<Item = Placed<dyn VectorComponent>> {
    let y = CY + 280.0;
    let tick_h = 8.0;
    let half_span = 90.0;
    [
        rect(
            Vec2(CX - half_span - 1.0, y - tick_h * 0.5),
            Vec2(2.0, tick_h),
            alpha(p.paper, hero_life * tag_in * 0.65),
        ),
        rect(
            Vec2(CX + half_span - 1.0, y - tick_h * 0.5),
            Vec2(2.0, tick_h),
            alpha(p.paper, hero_life * tag_in * 0.65),
        ),
        rect(
            Vec2(CX - half_span * tag_in, y - 1.0),
            Vec2(half_span * 2.0 * tag_in, 2.0),
            alpha(p.paper, hero_life * tag_in * 0.55),
        ),
        label(
            Vec2(CX, y + 18.0),
            Anchor::TOP_CENTER,
            "L = 280 PX",
            12.0,
            alpha(p.paper, hero_life * tag_in * 0.75),
            Weight::NORMAL,
        ),
    ]
    .into_iter()
    .flatten()
}
