//! 03 / SCAN — a graduated dial: center reticle, an elastic hero ring with
//! 12 numbered tick marks and 8 indexed satellites, a rotating radar sweep,
//! live R / θ readouts, and an exit burst into the white flash.

use std::f32::consts::{PI, TAU};

use tellur_core::geometry::{Anchor, Vec2};
use tellur_core::layer::VectorLayer;
use tellur_core::text::Weight;
use tellur_core::time::{Time, TimelineTime};

use super::common::*;

// 12 tick marks at the ring (every 30°). Every 3rd is taller (a graduated
// dial look). Quartz-watch fidelity.
const ANGLE_LABELS: [&str; 12] = [
    "000", "030", "060", "090", "120", "150", "180", "210", "240", "270", "300", "330",
];

#[tellur_core::component(vector)]
pub fn scan(time: TimelineTime, palette: Palette) -> impl VectorComponent {
    let p = palette;
    if time.during(3.4, 5.5).is_none() {
        return VectorLayer::builder().size(SCENE_SIZE).build();
    }

    let life = envelope(
        time,
        (3.4, 3.8),
        (5.05, 5.45),
        ease_in_out_expo,
        ease_in_out_expo,
    );

    // Center reticle: a small crosshair + cyan dot.
    let reticle = ease_out_cubic(time.phase(3.5, 3.95));
    let cross_arm = 64.0 * reticle;

    // Single hero ring — this is the scene's one elastic moment.
    let ring_pop = ease_out_elastic(time.phase(3.6, 4.4)).max(0.0);
    let ring_r = lerp(80.0, 300.0, ring_pop);

    VectorLayer::builder()
        .size(SCENE_SIZE)
        // Cyan horizontal scan stripe sweeping vertically — the matching
        // transition wipe between FIELD and SCAN. Echoes the pink stripe at
        // the OVERTURE→FIELD handoff so the structure rhymes.
        .maybe_child({
            let intro_sweep = ease_in_out_expo(time.phase(3.35, 3.85));
            if intro_sweep > 0.0 && intro_sweep < 1.0 {
                let y = lerp(-80.0, SCENE_SIZE.1 + 80.0, intro_sweep);
                let visibility = 4.0 * intro_sweep * (1.0 - intro_sweep);
                rect(
                    Vec2(0.0, y - 3.0),
                    Vec2(SCENE_SIZE.0, 6.0),
                    alpha(p.cyan, visibility * 0.88),
                )
            } else {
                None
            }
        })
        // Center reticle: crosshair arms + cyan dot. Reads as "this is the
        // origin", not "this is a hero blob".
        .maybe_child(rect(
            Vec2(CX - cross_arm, CY - 1.0),
            Vec2(cross_arm * 2.0, 2.0),
            alpha(p.paper, life * 0.7),
        ))
        .maybe_child(rect(
            Vec2(CX - 1.0, CY - cross_arm),
            Vec2(2.0, cross_arm * 2.0),
            alpha(p.paper, life * 0.7),
        ))
        .maybe_child(circle(
            Vec2(CX, CY),
            9.0 * reticle,
            Some(alpha(p.cyan, life)),
            None,
        ))
        // Single hero ring.
        .maybe_child(circle(
            Vec2(CX, CY),
            ring_r,
            None,
            Some((alpha(p.cyan, life * 0.75), 3.5)),
        ))
        // Inner secondary reticle — a thinner cyan ring at ~120 + 4 cardinal
        // mini-ticks. Layers visual depth between the crosshair and the hero ring.
        .maybe_children({
            let inner_ring_in = ease_out_cubic(time.phase(3.7, 4.15));
            (inner_ring_in > 0.0).then(|| {
                let inner_r2 = 116.0;
                let ring = circle(
                    Vec2(CX, CY),
                    inner_r2 * inner_ring_in,
                    None,
                    Some((alpha(p.cyan, life * 0.35), 1.5)),
                );
                let ticks = (0..4).filter_map(move |i| {
                    let a = i as f32 * PI * 0.5 - PI * 0.5;
                    let mid_r = inner_r2 - 6.0;
                    let mid = Vec2(CX + a.cos() * mid_r, CY + a.sin() * mid_r);
                    fx_rect(
                        mid,
                        Vec2(2.0, 10.0 * inner_ring_in),
                        a + PI * 0.5,
                        alpha(p.paper, life * 0.45),
                        1.0,
                        Vec2(1.0, 1.0),
                    )
                });
                ring.into_iter().chain(ticks)
            })
        })
        // 12 graduated tick marks + angle numerals on the major ones.
        .children((0..12).flat_map(move |i| {
            let angle_label = ANGLE_LABELS[i];
            let a = i as f32 / 12.0 * TAU - PI * 0.5;
            let stagger = i as f32 * 0.025;
            let tk = ease_out_cubic(time.phase(3.85 + stagger, 4.3 + stagger));
            let major = i % 3 == 0;
            let inner_off = if major { 22.0 } else { 12.0 };
            let outer_off = if major { 22.0 } else { 12.0 };
            let inner_r = ring_r - inner_off;
            let outer_r = ring_r + outer_off;
            let mid_r = (inner_r + outer_r) * 0.5;
            let length = outer_r - inner_r;
            let mid = Vec2(CX + a.cos() * mid_r, CY + a.sin() * mid_r);

            let tick = (tk > 0.0)
                .then(|| {
                    fx_rect(
                        mid,
                        Vec2(if major { 3.0 } else { 2.0 }, length * tk),
                        a + PI * 0.5,
                        alpha(p.paper, life * if major { 0.85 } else { 0.55 }),
                        1.0,
                        Vec2(1.0, 1.0),
                    )
                })
                .flatten();

            // Angle label outside major ticks — that gradicule-numeral detail
            // pushes the SCAN scene from "geometric" to "instrument readout".
            // We skip i=3 (090°) — dedicated `R = 300 PX` readout — and
            // i=9 (270°) — dedicated `θ = NNN°` readout sits there.
            let numeral = (tk > 0.0 && major && i != 3 && i != 9)
                .then(|| {
                    let label_in =
                        ease_out_cubic(time.phase(4.1 + i as f32 * 0.012, 4.5 + i as f32 * 0.012));
                    let label_alpha =
                        life * label_in * (1.0 - ease_in_out_expo(time.phase(5.0, 5.35)));
                    (label_alpha > 0.0).then(|| {
                        let label_r = outer_r + 32.0;
                        label(
                            Vec2(CX + a.cos() * label_r, CY + a.sin() * label_r),
                            Anchor::CENTER,
                            angle_label,
                            11.0,
                            alpha(p.paper, label_alpha * 0.7),
                            Weight::NORMAL,
                        )
                    })
                })
                .flatten()
                .flatten();

            tick.into_iter().chain(numeral)
        }))
        // 8 satellites at the cardinal / intercardinal points on the ring,
        // each tagged with a tiny zero-padded index — "01..08".
        .children({
            let sat_label_in = ease_in_out_expo(time.phase(4.35, 4.75))
                * (1.0 - ease_in_out_expo(time.phase(4.9, 5.2)));
            (0..8).flat_map(move |i| {
                let a = i as f32 / 8.0 * TAU - PI * 0.5;
                let stagger = i as f32 * 0.04;
                let sp = ease_out_cubic(time.phase(4.05 + stagger, 4.55 + stagger));
                let pos = Vec2(CX + a.cos() * ring_r, CY + a.sin() * ring_r);
                let color = if i % 2 == 0 { p.pink } else { p.cyan };
                let sat = circle(pos, 12.0 * sp, Some(alpha(color, life)), None);

                // i=2 (right cardinal) and i=6 (left cardinal) directions are
                // already occupied by the `R = 300 PX` / `θ = NNN°` readouts,
                // so we skip those two indices to avoid label collisions.
                let skip_label = i == 2 || i == 6;
                let tag = (sat_label_in > 0.0 && !skip_label)
                    .then(|| {
                        let label_r = ring_r + 22.0;
                        let lpos = Vec2(CX + a.cos() * label_r, CY + a.sin() * label_r);
                        // Choose anchor based on quadrant so labels read toward
                        // the empty side (away from center).
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
                        // Slight push along the chosen axis to avoid the satellite.
                        let push = 6.0;
                        let lpos = Vec2(lpos.0 + a.cos() * push, lpos.1 + a.sin() * push);
                        let label_text = format!("0{}", i + 1);
                        label(
                            lpos,
                            anchor,
                            &label_text,
                            10.0,
                            alpha(p.paper, life * sat_label_in * 0.55),
                            Weight::NORMAL,
                        )
                    })
                    .flatten();

                sat.into_iter().chain(tag)
            })
        })
        // Radar-style angular sweep: a fan of N narrowing rectangles whose
        // leading edge sweeps clockwise once, fading along its trail.
        .maybe_children({
            let sweep_in = ease_in_out_expo(time.phase(3.9, 4.2));
            let sweep_out = ease_in_out_expo(time.phase(5.05, 5.4));
            let sweep_life = (sweep_in * (1.0 - sweep_out)).clamp(0.0, 1.0);
            (sweep_life > 0.0).then(|| {
                // Slower rotation (~2.4s per full revolution) with a wider,
                // longer trail so the sweep reads as deliberate observation
                // rather than a quick flyby.
                let base_angle = (time.seconds() - 3.95).max(0.0) * (TAU / 2.4) - PI * 0.5;
                let trail_count = 32_i32;
                let trail_span = PI * 0.75;
                (0..trail_count).filter_map(move |j| {
                    let frac = j as f32 / (trail_count - 1) as f32;
                    let a = base_angle - frac * trail_span;
                    let fade = (1.0 - frac).powi(2);
                    let r = ring_r * 0.95;
                    let mid = Vec2(CX + a.cos() * r * 0.5, CY + a.sin() * r * 0.5);
                    fx_rect(
                        mid,
                        Vec2(4.0, r),
                        a + PI * 0.5,
                        alpha(p.pink, life * sweep_life * fade * 0.6),
                        1.0,
                        Vec2(1.0, 1.0),
                    )
                })
            })
        })
        // Single numeric annotation, attached to the ring at 0°.
        .maybe_children({
            let annot = ease_in_out_expo(time.phase(4.15, 4.5))
                * (1.0 - ease_in_out_expo(time.phase(4.85, 5.2)));
            (annot > 0.0).then(|| {
                [
                    // Mini connector tick from the ring to the label.
                    rect(
                        Vec2(CX + ring_r, CY - 1.0),
                        Vec2(28.0, 2.0),
                        alpha(p.paper, life * annot * 0.7),
                    ),
                    label(
                        Vec2(CX + ring_r + 36.0, CY - 6.0),
                        Anchor::CENTER_LEFT,
                        "R = 300 PX",
                        13.0,
                        alpha(p.paper, life * annot * 0.85),
                        Weight::NORMAL,
                    ),
                    // Sub-label one line down (smaller, dimmer).
                    label(
                        Vec2(CX + ring_r + 36.0, CY + 10.0),
                        Anchor::CENTER_LEFT,
                        "NODES = 08",
                        11.0,
                        alpha(p.paper, life * annot * 0.55),
                        Weight::NORMAL,
                    ),
                ]
                .into_iter()
                .flatten()
            })
        })
        // "θ" readout in the 270° direction. Tracks the radar sweep's current
        // angle so the SCAN scene has a live data feel.
        .maybe_children({
            let theta_in = ease_in_out_expo(time.phase(4.2, 4.55))
                * (1.0 - ease_in_out_expo(time.phase(4.95, 5.25)));
            let sweep_in = ease_in_out_expo(time.phase(3.9, 4.2));
            let sweep_active = sweep_in > 0.05;
            (theta_in > 0.0 && sweep_active).then(|| {
                let base_angle = (time.seconds() - 3.95).max(0.0) * (TAU / 2.4);
                // Convert to degrees and wrap to [0, 360).
                let deg = (base_angle.to_degrees().rem_euclid(360.0)) as i32;
                let theta_text = format!("θ = {:03}°", deg);
                [
                    rect(
                        Vec2(CX - ring_r - 28.0, CY - 1.0),
                        Vec2(28.0, 2.0),
                        alpha(p.paper, life * theta_in * 0.7),
                    ),
                    label(
                        Vec2(CX - ring_r - 36.0, CY - 6.0),
                        Anchor::CENTER_RIGHT,
                        &theta_text,
                        13.0,
                        alpha(p.paper, life * theta_in * 0.85),
                        Weight::NORMAL,
                    ),
                ]
                .into_iter()
                .flatten()
            })
        })
        // SCAN's exit burst — 24 short radial spokes shoot outward from the
        // hero ring just as the scene flashes white into RESOLVE.
        .maybe_children({
            let burst_kick = ease_out_quint(time.phase(4.88, 5.02));
            let burst_fade = ease_in_out_expo(time.phase(5.05, 5.25));
            let burst_life = (burst_kick * (1.0 - burst_fade)).clamp(0.0, 1.0);
            (burst_life > 0.0).then(|| {
                (0..24).filter_map(move |i| {
                    let a = i as f32 / 24.0 * TAU;
                    let inner_r = ring_r + 6.0;
                    let length = 40.0 + burst_kick * 110.0;
                    let mid_r = inner_r + length * 0.5;
                    let mid = Vec2(CX + a.cos() * mid_r, CY + a.sin() * mid_r);
                    let color = if i % 3 == 0 { p.paper } else { p.cyan };
                    fx_rect(
                        mid,
                        Vec2(2.5, length),
                        a + PI * 0.5,
                        alpha(color, life * burst_life * 0.9),
                        1.0,
                        Vec2(1.0, 1.0),
                    )
                })
            })
        })
        .build()
}
