//! 03 / SCAN — a graduated dial: center reticle, an elastic hero ring with
//! 12 numbered tick marks and 8 indexed satellites, a rotating radar sweep,
//! live R / θ readouts, and an exit burst into the white flash.

use std::f32::consts::{PI, TAU};

use tellur_core::fragment::Fragment;
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
pub fn Scan(time: TimelineTime, palette: Palette) -> impl VectorComponent {
    let p = palette;
    if time.during(3.4, 5.5).is_none() {
        return VectorLayer::builder().size(SCENE_SIZE).build();
    }

    let life = envelope(
        time,
        (3.4, 3.8),
        (5.05, 5.45),
        |p| p.ease_in_out_expo(0.0, 1.0),
        |p| p.ease_in_out_expo(0.0, 1.0),
    );

    // Center reticle: a small crosshair + cyan dot.
    let reticle = time.phase(3.5, 3.95).ease_out_cubic(0.0, 1.0);
    let cross_arm = 64.0 * reticle;

    // Single hero ring — this is the scene's one elastic moment.
    let ring_r = time.phase(3.6, 4.4).ease_out_elastic(80.0, 300.0);

    // Radar window: spins from 3.95 (just after the reticle settles) until
    // the sweep-out finishes at 5.4. `elapsed()` drives the continuously-
    // accruing rotation angle; the same window is reused by the θ readout
    // below so both readouts speak in the same radar time-frame.
    let radar = time.window(3.95, 5.4);

    VectorLayer::builder()
        .size(SCENE_SIZE)
        // Cyan horizontal scan stripe sweeping vertically — the matching
        // transition wipe between FIELD and SCAN. Echoes the pink stripe at
        // the OVERTURE→FIELD handoff so the structure rhymes.
        .maybe_child({
            let intro_sweep = time.phase(3.35, 3.85).ease_in_out_expo(0.0, 1.0);
            (intro_sweep > 0.0 && intro_sweep < 1.0).then(|| {
                let y = lerp(-80.0, SCENE_SIZE.1 + 80.0, intro_sweep);
                let visibility = peak(intro_sweep);
                Rect::builder()
                    .position(Vec2(0.0, y - 3.0))
                    .size(Vec2(SCENE_SIZE.0, 6.0))
                    .color(p.cyan.with_alpha(visibility * 0.88))
            })
        })
        // Center reticle: crosshair arms + cyan dot. Reads as "this is the
        // origin", not "this is a hero blob".
        .child(
            Rect::builder()
                .position(Vec2(CX - cross_arm, CY - 1.0))
                .size(Vec2(cross_arm * 2.0, 2.0))
                .color(p.paper.with_alpha(life * 0.7)),
        )
        .child(
            Rect::builder()
                .position(Vec2(CX - 1.0, CY - cross_arm))
                .size(Vec2(2.0, cross_arm * 2.0))
                .color(p.paper.with_alpha(life * 0.7)),
        )
        .child(
            Circle::builder()
                .center(Vec2(CX, CY))
                .radius(9.0 * reticle)
                .fill(p.cyan.with_alpha(life)),
        )
        // Single hero ring.
        .child(
            Circle::builder()
                .center(Vec2(CX, CY))
                .radius(ring_r)
                .stroke(p.cyan.with_alpha(life * 0.75))
                .stroke_width(3.5),
        )
        // Inner secondary reticle — a thinner cyan ring at ~120 + 4 cardinal
        // mini-ticks. Layers visual depth between the crosshair and the hero ring.
        .maybe_child({
            let inner_ring_in = time.phase(3.7, 4.15).ease_out_cubic(0.0, 1.0);
            (inner_ring_in > 0.0).then(|| {
                let inner_r2 = 116.0;
                Fragment::builder()
                    .child(
                        Circle::builder()
                            .center(Vec2(CX, CY))
                            .radius(inner_r2 * inner_ring_in)
                            .stroke(p.cyan.with_alpha(life * 0.35))
                            .stroke_width(1.5),
                    )
                    .children((0..4).map(move |i| {
                        let a = i as f32 * PI * 0.5 - PI * 0.5;
                        let mid_r = inner_r2 - 6.0;
                        let mid = Vec2(CX + a.cos() * mid_r, CY + a.sin() * mid_r);
                        FxRect::builder()
                            .center(mid)
                            .size(Vec2(2.0, 10.0 * inner_ring_in))
                            .angle(a + PI * 0.5)
                            .color(p.paper.with_alpha(life * 0.45))
                            .opacity(1.0)
                            .scale(Vec2(1.0, 1.0))
                    }))
                    .build()
            })
        })
        // 12 graduated tick marks + angle numerals on the major ones.
        .children((0..12).map(move |i| {
            let angle_label = ANGLE_LABELS[i];
            let a = i as f32 / 12.0 * TAU - PI * 0.5;
            let stagger = i as f32 * 0.025;
            let tk = time
                .phase(3.85 + stagger, 4.3 + stagger)
                .ease_out_cubic(0.0, 1.0);
            let major = i % 3 == 0;
            let inner_off = if major { 22.0 } else { 12.0 };
            let outer_off = if major { 22.0 } else { 12.0 };
            let inner_r = ring_r - inner_off;
            let outer_r = ring_r + outer_off;
            let mid_r = (inner_r + outer_r) * 0.5;
            let length = outer_r - inner_r;
            let mid = Vec2(CX + a.cos() * mid_r, CY + a.sin() * mid_r);

            let tick = (tk > 0.0).then(|| {
                FxRect::builder()
                    .center(mid)
                    .size(Vec2(if major { 3.0 } else { 2.0 }, length * tk))
                    .angle(a + PI * 0.5)
                    .color(p.paper.with_alpha(life * if major { 0.85 } else { 0.55 }))
                    .opacity(1.0)
                    .scale(Vec2(1.0, 1.0))
            });

            // Angle label outside major ticks — that gradicule-numeral detail
            // pushes the SCAN scene from "geometric" to "instrument readout".
            // We skip i=3 (090°) — dedicated `R = 300 PX` readout — and
            // i=9 (270°) — dedicated `θ = NNN°` readout sits there.
            let numeral = (tk > 0.0 && major && i != 3 && i != 9)
                .then(|| {
                    let label_in = time
                        .phase(4.1 + i as f32 * 0.012, 4.5 + i as f32 * 0.012)
                        .ease_out_cubic(0.0, 1.0);
                    let label_alpha =
                        life * label_in * time.phase(5.0, 5.35).ease_in_out_expo(1.0, 0.0);
                    (label_alpha > 0.0).then(|| {
                        let label_r = outer_r + 32.0;
                        Label::builder()
                            .position(Vec2(CX + a.cos() * label_r, CY + a.sin() * label_r))
                            .anchor(Anchor::CENTER)
                            .text(angle_label)
                            .size(11.0)
                            .color(p.paper.with_alpha(label_alpha * 0.7))
                            .weight(Weight::NORMAL)
                    })
                })
                .flatten();

            Fragment::builder()
                .maybe_child(tick)
                .maybe_child(numeral)
                .build()
        }))
        // 8 satellites at the cardinal / intercardinal points on the ring,
        // each tagged with a tiny zero-padded index — "01..08".
        .children({
            let sat_label_in = time.phase(4.35, 4.75).ease_in_out_expo(0.0, 1.0)
                * time.phase(4.9, 5.2).ease_in_out_expo(1.0, 0.0);
            (0..8).map(move |i| {
                let a = i as f32 / 8.0 * TAU - PI * 0.5;
                let stagger = i as f32 * 0.04;
                let sp = time
                    .phase(4.05 + stagger, 4.55 + stagger)
                    .ease_out_cubic(0.0, 1.0);
                let pos = Vec2(CX + a.cos() * ring_r, CY + a.sin() * ring_r);
                let color = if i % 2 == 0 { p.pink } else { p.cyan };

                // i=2 (right cardinal) and i=6 (left cardinal) directions are
                // already occupied by the `R = 300 PX` / `θ = NNN°` readouts,
                // so we skip those two indices to avoid label collisions.
                let skip_label = i == 2 || i == 6;
                let tag = (sat_label_in > 0.0 && !skip_label).then(|| {
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
                    Label::builder()
                        .position(lpos)
                        .anchor(anchor)
                        .text(format!("0{}", i + 1))
                        .size(10.0)
                        .color(p.paper.with_alpha(life * sat_label_in * 0.55))
                        .weight(Weight::NORMAL)
                });

                Fragment::builder()
                    .child(
                        Circle::builder()
                            .center(pos)
                            .radius(12.0 * sp)
                            .fill(color.with_alpha(life)),
                    )
                    .maybe_child(tag)
                    .build()
            })
        })
        // Radar-style angular sweep: a fan of N narrowing rectangles whose
        // leading edge sweeps clockwise once, fading along its trail.
        .maybe_child({
            let sweep_in = time.phase(3.9, 4.2).ease_in_out_expo(0.0, 1.0);
            let sweep_out = time.phase(5.05, 5.4).ease_in_out_expo(1.0, 0.0);
            let sweep_life = sweep_in * sweep_out;
            (sweep_life > 0.0).then(|| {
                // Slower rotation (~2.4s per full revolution) with a wider,
                // longer trail so the sweep reads as deliberate observation
                // rather than a quick flyby.
                let base_angle = radar.elapsed() * (TAU / 2.4) - PI * 0.5;
                let trail_count = 32_i32;
                let trail_span = PI * 0.75;
                (0..trail_count)
                    .map(move |j| {
                        let frac = j as f32 / (trail_count - 1) as f32;
                        let a = base_angle - frac * trail_span;
                        let fade = (1.0 - frac).powi(2);
                        let r = ring_r * 0.95;
                        let mid = Vec2(CX + a.cos() * r * 0.5, CY + a.sin() * r * 0.5);
                        FxRect::builder()
                            .center(mid)
                            .size(Vec2(4.0, r))
                            .angle(a + PI * 0.5)
                            .color(p.pink.with_alpha(life * sweep_life * fade * 0.6))
                            .opacity(1.0)
                            .scale(Vec2(1.0, 1.0))
                    })
                    .collect::<Fragment>()
            })
        })
        // Single numeric annotation, attached to the ring at 0°.
        .maybe_child({
            let annot = time.phase(4.15, 4.5).ease_in_out_expo(0.0, 1.0)
                * time.phase(4.85, 5.2).ease_in_out_expo(1.0, 0.0);
            (annot > 0.0).then(|| {
                Fragment::builder()
                    // Mini connector tick from the ring to the label.
                    .child(
                        Rect::builder()
                            .position(Vec2(CX + ring_r, CY - 1.0))
                            .size(Vec2(28.0, 2.0))
                            .color(p.paper.with_alpha(life * annot * 0.7)),
                    )
                    .child(
                        Label::builder()
                            .position(Vec2(CX + ring_r + 36.0, CY - 6.0))
                            .anchor(Anchor::CENTER_LEFT)
                            .text("R = 300 PX")
                            .size(13.0)
                            .color(p.paper.with_alpha(life * annot * 0.85))
                            .weight(Weight::NORMAL),
                    )
                    // Sub-label one line down (smaller, dimmer).
                    .child(
                        Label::builder()
                            .position(Vec2(CX + ring_r + 36.0, CY + 10.0))
                            .anchor(Anchor::CENTER_LEFT)
                            .text("NODES = 08")
                            .size(11.0)
                            .color(p.paper.with_alpha(life * annot * 0.55))
                            .weight(Weight::NORMAL),
                    )
                    .build()
            })
        })
        // "θ" readout in the 270° direction. Tracks the radar sweep's current
        // angle so the SCAN scene has a live data feel.
        .maybe_child({
            let theta_in = time.phase(4.2, 4.55).ease_in_out_expo(0.0, 1.0)
                * time.phase(4.95, 5.25).ease_in_out_expo(1.0, 0.0);
            let sweep_in = time.phase(3.9, 4.2).ease_in_out_expo(0.0, 1.0);
            let sweep_active = sweep_in > 0.05;
            (theta_in > 0.0 && sweep_active).then(|| {
                let base_angle = radar.elapsed() * (TAU / 2.4);
                // Convert to degrees and wrap to [0, 360).
                let deg = (base_angle.to_degrees().rem_euclid(360.0)) as i32;
                Fragment::builder()
                    .child(
                        Rect::builder()
                            .position(Vec2(CX - ring_r - 28.0, CY - 1.0))
                            .size(Vec2(28.0, 2.0))
                            .color(p.paper.with_alpha(life * theta_in * 0.7)),
                    )
                    .child(
                        Label::builder()
                            .position(Vec2(CX - ring_r - 36.0, CY - 6.0))
                            .anchor(Anchor::CENTER_RIGHT)
                            .text(format!("θ = {:03}°", deg))
                            .size(13.0)
                            .color(p.paper.with_alpha(life * theta_in * 0.85))
                            .weight(Weight::NORMAL),
                    )
                    .build()
            })
        })
        // SCAN's exit burst — 24 short radial spokes shoot outward from the
        // hero ring just as the scene flashes white into RESOLVE.
        .maybe_child({
            let burst_kick = time.phase(4.88, 5.02).ease_out_quint(0.0, 1.0);
            let burst_fade = time.phase(5.05, 5.25).ease_in_out_expo(1.0, 0.0);
            let burst_life = burst_kick * burst_fade;
            (burst_life > 0.0).then(|| {
                (0..24)
                    .map(move |i| {
                        let a = i as f32 / 24.0 * TAU;
                        let inner_r = ring_r + 6.0;
                        let length = 40.0 + burst_kick * 110.0;
                        let mid_r = inner_r + length * 0.5;
                        let mid = Vec2(CX + a.cos() * mid_r, CY + a.sin() * mid_r);
                        let color = if i % 3 == 0 { p.paper } else { p.cyan };
                        FxRect::builder()
                            .center(mid)
                            .size(Vec2(2.5, length))
                            .angle(a + PI * 0.5)
                            .color(color.with_alpha(life * burst_life * 0.9))
                            .opacity(1.0)
                            .scale(Vec2(1.0, 1.0))
                    })
                    .collect::<Fragment>()
            })
        })
        .build()
}
