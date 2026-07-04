//! 01 / OVERTURE — clamp bars frame a spinning hero square dressed with a
//! reticle, registration dots, axis rays, a measurement tag, and an exit
//! sweep that hands the frame off to FIELD.

use std::f32::consts::PI;

use tellur_core::color::Color;
use tellur_core::fragment::Fragment;
use tellur_core::geometry::{Anchor, Vec2};
use tellur_core::layer::VectorLayer;
use tellur_core::text::Weight;
use tellur_core::time::{LocalTime, Time};

use super::common::*;

#[tellur_core::component(vector)]
pub fn Overture(time: LocalTime, palette: Palette) -> impl VectorComponent {
    let p = palette;
    if time.during(0.0, 2.2).is_none() {
        return VectorLayer::builder().size(SCENE_SIZE).build();
    }

    // Hero square: controlled ease_out_cubic pop (no rubbery elastic),
    // a slow drift-spin, and an ease_in_back dismissal.
    let hero_in = time.phase(0.55, 1.2).eased(Easing::OutCubic);
    let hero_remain = time.phase(1.7, 2.15).ease_in_back(1.0, 0.0);
    let hero_life = (hero_in.get() * hero_remain).clamp(0.0, 1.0);
    let spin = time.seconds() * 0.35 + PI * 0.25;
    let scale = hero_in.linear(0.55, 1.0) * hero_remain;
    let s_clamped = scale.max(0.001);

    // Registration markings inside the hero square — a reticle (cross arms +
    // a center ring + gap dashes) plus four small corner dots, all painted
    // in the bg color so they read as "cut into the paper". They rotate
    // with the square via the same `spin`.
    let mark_color = p.bg.with_alpha(hero_life * 0.55);
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

    let ray_in = time.phase(0.85, 1.4).ease_out_cubic(0.0, 1.0) * hero_remain;
    let tag_in = time.phase(0.95, 1.35).ease_in_out_expo(0.0, 1.0)
        * time.phase(1.55, 2.0).ease_in_out_expo(1.0, 0.0);
    let sweep = time.phase(1.45, 2.0).eased(Easing::InOutExpo);

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
                .map(move |(i, (dy, side, color))| {
                    let stagger = i as f32 * 0.06;
                    let enter = time
                        .phase(0.32 + stagger, 0.92 + stagger)
                        .eased(Easing::InOutExpo);
                    let leave = time
                        .phase(1.55 + stagger * 0.5, 2.05 + stagger * 0.5)
                        .ease_in_back(0.0, 1.0);
                    let bar_x = enter.linear(side * 2400.0 + CX, CX);
                    let exit_dy = leave * if side > 0.0 { 320.0 } else { -320.0 };
                    let alpha_factor = (enter.get() * (1.0 - leave)).clamp(0.0, 1.0) * 0.92;
                    let y_center = CY + dy + exit_dy;

                    // End-cap mini brackets at each bar end — small perpendicular
                    // ticks that turn the bar into a clear measurement span. Same color
                    // as the bar; staggered tiny so they "arrive" with the bar.
                    let cap_pop = time
                        .phase(0.65 + stagger, 1.05 + stagger)
                        .ease_out_cubic(0.0, 1.0)
                        * time
                            .phase(1.55 + stagger * 0.5, 1.95 + stagger * 0.5)
                            .ease_in_back(1.0, 0.0);

                    Fragment::builder()
                        .child(
                            Rectangle::builder()
                                .size(Vec2(bar_w, bar_h))
                                .fill(color)
                                .opacity(alpha_factor)
                                .anchored(Anchor::CENTER)
                                .snap_to(Vec2(bar_x, y_center)),
                        )
                        .children(
                            (cap_pop > 0.0)
                                .then(|| {
                                    let cap_h = 28.0;
                                    let cap_w = 3.0;
                                    [-1.0_f32, 1.0].into_iter().map(move |cap_side| {
                                        let cap_x = bar_x + cap_side * (bar_w * 0.5 + 1.0);
                                        Rectangle::builder()
                                            .size(Vec2(cap_w, cap_h * cap_pop))
                                            .fill(color.with_alpha(alpha_factor))
                                            .place_at(Vec2(
                                                cap_x - cap_w * 0.5,
                                                y_center - cap_h * 0.5,
                                            ))
                                    })
                                })
                                .into_iter()
                                .flatten(),
                        )
                        .build()
                }),
        )
        .child(
            Rectangle::builder()
                .size(Vec2(280.0, 280.0))
                .fill(p.paper)
                .transform_around(
                    Anchor::CENTER,
                    Transform::scale(Vec2(s_clamped, s_clamped)).then(Transform::rotate(spin)),
                )
                .opacity(hero_life)
                .anchored(Anchor::CENTER)
                .snap_to(Vec2(CX, CY)),
        )
        .child(
            Rectangle::builder()
                .size(Vec2(420.0, 420.0))
                .stroke(Stroke {
                    paint: p.pink.with_alpha(hero_life * 0.82).into(),
                    width: 3.0,
                    dash: None,
                })
                .transform_around(
                    Anchor::CENTER,
                    Transform::scale(Vec2(s_clamped, s_clamped))
                        .then(Transform::rotate(-spin * 0.4)),
                )
                .anchored(Anchor::CENTER)
                .snap_to(Vec2(CX, CY)),
        )
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
            .map(move |(dx, dy, vertical)| {
                let lx = dx * arm_mid;
                let ly = dy * arm_mid;
                let pos = Vec2(CX + lx * cs2 - ly * sn2, CY + lx * sn2 + ly * cs2);
                let (w, h) = if vertical {
                    (2.0, arm_segment)
                } else {
                    (arm_segment, 2.0)
                };
                Rectangle::builder()
                    .size(Vec2(w, h))
                    .fill(mark_color)
                    .transform_around(Anchor::CENTER, Transform::rotate(spin))
                    .anchored(Anchor::CENTER)
                    .snap_to(pos)
            }),
        )
        // Small open ring at the center of the reticle.
        .child(
            Circle::builder()
                .radius(6.0 * s_clamped)
                .stroke(Stroke::new(mark_color, 1.5))
                .anchored(Anchor::CENTER)
                .snap_to(Vec2(CX, CY)),
        )
        // A tiny solid dot at the very center for the bullseye.
        .child(
            Circle::builder()
                .radius(1.5 * s_clamped)
                .fill(mark_color)
                .anchored(Anchor::CENTER)
                .snap_to(Vec2(CX, CY)),
        )
        // Four corner dots inside the paper square, offset 110 from center then
        // rotated by `spin` to follow the square's orientation.
        .children(
            [(-1.0_f32, -1.0_f32), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)]
                .into_iter()
                .map(move |(dx, dy)| {
                    let lx = dx * corner_off;
                    let ly = dy * corner_off;
                    let pos = Vec2(CX + lx * cs - ly * sn, CY + lx * sn + ly * cs);
                    Circle::builder()
                        .radius(3.5 * hero_life)
                        .fill(mark_color)
                        .anchored(Anchor::CENTER)
                        .snap_to(pos)
                }),
        )
        // Four registration dots locked onto the outline corners, alternating
        // pink/cyan so the framing reads as intentional design pairs. Each is
        // linked back to the center by a dim hairline ray, which turns the
        // outside dots from "floating decorations" into "axis terminators".
        .children((0..4).map(move |s| {
            let a = s as f32 * PI * 0.5 + spin * 0.5 + PI * 0.25;
            let r = 230.0;
            let pos = Vec2(CX + a.cos() * r, CY + a.sin() * r);

            // Hairline from outside the paper-square corner (~200) to just
            // inside the outside dot.
            let ray = (ray_in > 0.0).then(|| {
                let inner_r = 200.0;
                let outer_r = r - 10.0;
                let length = (outer_r - inner_r) * ray_in;
                let mid_r = inner_r + length * 0.5;
                let mid = Vec2(CX + a.cos() * mid_r, CY + a.sin() * mid_r);
                Rectangle::builder()
                    .size(Vec2(1.5, length))
                    .fill(p.paper.with_alpha(hero_life * 0.45))
                    .transform_around(Anchor::CENTER, Transform::rotate(a + PI * 0.5))
                    .anchored(Anchor::CENTER)
                    .snap_to(mid)
            });

            // Small index tag next to each outside dot. The tag follows the
            // dot's rotated position so it always reads on the outside.
            let label_in = time
                .phase(1.0 + s as f32 * 0.04, 1.4 + s as f32 * 0.04)
                .ease_out_cubic(0.0, 1.0);
            let label_alpha =
                hero_life * label_in * time.phase(1.7, 2.05).ease_in_back(1.0, 0.0) * 0.6;
            let tag = (label_alpha > 0.0).then(|| {
                let label_r = r + 18.0;
                let lpos = Vec2(CX + a.cos() * label_r, CY + a.sin() * label_r);
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
                Text::builder()
                    .font(MONOSPACE.clone())
                    .size(10.0)
                    .weight(Weight::NORMAL)
                    .fill(p.paper.with_alpha(label_alpha.clamp(0.0, 1.0)))
                    .span(TextSpan::plain(format!("0{}", s + 1)))
                    .anchored(anchor)
                    .snap_to(lpos)
            });

            Fragment::builder()
                .maybe_child(ray)
                .child(
                    Circle::builder()
                        .radius(6.0 * hero_life)
                        .fill((if s % 2 == 0 { p.pink } else { p.cyan }).with_alpha(hero_life))
                        .anchored(Anchor::CENTER)
                        .snap_to(pos),
                )
                .maybe_child(tag)
                .build()
        }))
        // Length tag beneath the central composition — small data-design touch
        // that gives the OVERTURE a "measurement readout" character.
        .maybe_child((tag_in > 0.0).then(|| {
            LengthTag::builder()
                .palette(p)
                .hero_life(hero_life)
                .tag_in(tag_in)
        }))
        // Pink horizontal scan stripe sweeping vertically as the scene exits —
        // a transition wipe that hands the frame off to FIELD.
        .maybe_child((sweep.get() > 0.0 && sweep.get() < 1.0).then(|| {
            let y = sweep.linear(-80.0, SCENE_SIZE.1 + 80.0);
            let visibility = peak(sweep.get());
            Rectangle::builder()
                .size(Vec2(SCENE_SIZE.0, 6.0))
                .fill(p.pink.with_alpha(visibility * 0.88))
                .place_at(Vec2(0.0, y - 3.0))
        }))
        .build()
}

// The OVERTURE's measurement readout: two end ticks, a growing span bar, and
// a `L = 280 PX` caption, emitted in that order.
#[tellur_core::component(vector)]
fn LengthTag(palette: Palette, hero_life: f32, tag_in: f32) -> impl VectorComponent {
    let p = palette;
    let y = CY + 280.0;
    let tick_h = 8.0;
    let half_span = 90.0;
    Fragment::builder()
        .child(
            Rectangle::builder()
                .size(Vec2(2.0, tick_h))
                .fill(p.paper.with_alpha(hero_life * tag_in * 0.65))
                .place_at(Vec2(CX - half_span - 1.0, y - tick_h * 0.5)),
        )
        .child(
            Rectangle::builder()
                .size(Vec2(2.0, tick_h))
                .fill(p.paper.with_alpha(hero_life * tag_in * 0.65))
                .place_at(Vec2(CX + half_span - 1.0, y - tick_h * 0.5)),
        )
        .child(
            Rectangle::builder()
                .size(Vec2(half_span * 2.0 * tag_in, 2.0))
                .fill(p.paper.with_alpha(hero_life * tag_in * 0.55))
                .place_at(Vec2(CX - half_span * tag_in, y - 1.0)),
        )
        .child(
            Text::builder()
                .font(MONOSPACE.clone())
                .size(12.0)
                .weight(Weight::NORMAL)
                .fill(p.paper.with_alpha(hero_life * tag_in * 0.75))
                .span(TextSpan::plain("L = 280 PX"))
                .anchored(Anchor::TOP_CENTER)
                .snap_to(Vec2(CX, y + 18.0)),
        )
        .build()
}
