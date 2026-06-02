//! 04 / RESOLVE — the aperture-close finale: the SCAN ring contracts, the 8
//! satellites slide inward into a double-ring cluster, a singularity flash and
//! shard burst punctuate the collapse, twin ripples wash outward, and a
//! layered central mark settles the piece.

use std::f32::consts::{PI, TAU};

use tellur_core::fragment::Fragment;
use tellur_core::geometry::Vec2;
use tellur_core::layer::VectorLayer;
use tellur_core::time::{Time, TimelineTime};

use super::common::*;

#[tellur_core::component(vector)]
pub fn Resolve(time: TimelineTime, palette: Palette) -> impl VectorComponent {
    let p = palette;
    if time.during(4.9, DURATION).is_none() {
        return VectorLayer::builder().size(SCENE_SIZE).build();
    }

    let life = ease_in_out_expo(time.phase(5.05, 5.5));

    // Phase 1: contracting hero ring (same R=300 that SCAN ended on).
    let contract = ease_in_out_expo(time.phase(5.2, 6.3));
    let ring_r = lerp(300.0, 22.0, contract);
    let ring_alpha = (1.0 - contract * 0.55) * life;

    // Phase 2 inputs. `cluster_spin` is shared by the satellites and the
    // surviving central mark's rays / field hash marks.
    let sats_in = ease_in_out_expo(time.phase(5.25, 6.35));
    let cluster_spin =
        ease_out_cubic(time.phase(6.35, 7.0)) * (time.seconds() - 6.35).max(0.0) * 0.18;

    VectorLayer::builder()
        .size(SCENE_SIZE)
        // Phase 1: contracting hero ring.
        .maybe_child((ring_r > 4.0).then(|| {
            Circle::builder()
                .center(Vec2(CX, CY))
                .radius(ring_r)
                .stroke(alpha(p.cyan, ring_alpha * 0.9))
                .stroke_width(3.5)
        }))
        // Phase 2: 8 satellites slide along their angular rays toward center.
        // They settle into two interleaved layers — cardinals further out, the
        // intercardinals closer in — so the final cluster reads as a deliberate
        // double-ring composition instead of a flat ring of dots.
        .children((0..8).map(move |i| {
            let cardinal = i % 2 == 0;
            let base_a = i as f32 / 8.0 * TAU - PI_HALF;
            let final_r = if cardinal { 46.0 } else { 22.0 };
            let target_r = lerp(300.0, final_r, sats_in);
            let a = base_a + cluster_spin;
            let pos = Vec2(CX + a.cos() * target_r, CY + a.sin() * target_r);
            let color = if cardinal { p.pink } else { p.cyan };
            let size = lerp(12.0, if cardinal { 6.0 } else { 4.5 }, sats_in);
            let sat_alpha = (1.0 - sats_in * 0.4) * life;
            Circle::builder()
                .center(pos)
                .radius(size)
                .fill(alpha(color, sat_alpha))
        }))
        // Phase 3: "memory" rings — ghosts of where the contracting ring used
        // to be. Three after-images spawn as the ring sweeps inward, each
        // fading slowly. Subtle texture for the contraction.
        .child(
            [(5.55_f32, 230.0_f32), (5.85, 160.0), (6.12, 95.0)]
                .into_iter()
                .filter_map(move |(start, r)| {
                    let ghost_in = ease_out_cubic(time.phase(start, start + 0.08));
                    let ghost_out = ease_in_out_expo(time.phase(start + 0.25, start + 1.3));
                    let ghost = (ghost_in * (1.0 - ghost_out)).clamp(0.0, 1.0);
                    (ghost > 0.0).then(|| {
                        Circle::builder()
                            .center(Vec2(CX, CY))
                            .radius(r)
                            .stroke(alpha(p.cyan, life * ghost * 0.32))
                            .stroke_width(1.8)
                    })
                })
                .collect::<Fragment>(),
        )
        // Phase 4: the singularity flash — a brief paper bloom at the moment
        // everything collapses to the center.
        .maybe_child({
            let sing = ease_out_quint(time.phase(6.2, 6.32))
                * (1.0 - ease_in_out_expo(time.phase(6.32, 6.62)));
            (sing > 0.0).then(|| {
                Circle::builder()
                    .center(Vec2(CX, CY))
                    .radius(46.0 + sing * 20.0)
                    .fill(alpha(p.paper, sing * life * 0.95))
            })
        })
        // 16 short radial shards — emit-then-fade exactly at the singularity.
        // Length peaks then collapses inward as they vanish.
        .maybe_child({
            let shard_kick = ease_out_quint(time.phase(6.22, 6.36));
            let shard_fade = ease_in_out_expo(time.phase(6.36, 6.6));
            let shard_life = (shard_kick * (1.0 - shard_fade)).clamp(0.0, 1.0);
            (shard_life > 0.0).then(|| {
                (0..16)
                    .map(move |i| {
                        let a = i as f32 / 16.0 * TAU;
                        let inner_r = 32.0 + shard_kick * 30.0;
                        let length = 26.0 + shard_kick * 50.0;
                        let mid_r = inner_r + length * 0.5;
                        let mid = Vec2(CX + a.cos() * mid_r, CY + a.sin() * mid_r);
                        let color = if i % 2 == 0 { p.pink } else { p.paper };
                        FxRect::builder()
                            .center(mid)
                            .size(Vec2(2.0, length))
                            .angle(a + PI_HALF)
                            .color(alpha(color, life * shard_life * 0.9))
                            .opacity(1.0)
                            .scale(Vec2(1.0, 1.0))
                    })
                    .collect::<Fragment>()
            })
        })
        // Phase 5: the outward ripple. The scene's one elastic moment — the
        // ring kicks out from center with an elastic-eased launch, then a
        // quint-eased growth carries it past the edge.
        .maybe_child({
            let pulse_kick = ease_out_elastic(time.phase(6.22, 6.55)).max(0.0);
            let pulse_grow = ease_out_quint(time.phase(6.3, 7.15));
            let pulse_fade = 1.0 - ease_in_out_expo(time.phase(6.7, 7.25));
            let pulse_r = pulse_kick * 600.0 * (0.05 + pulse_grow * 0.95);
            (pulse_r > 12.0 && pulse_fade > 0.0).then(|| {
                Circle::builder()
                    .center(Vec2(CX, CY))
                    .radius(pulse_r)
                    .stroke(alpha(p.cyan, life * pulse_fade * 0.9))
                    .stroke_width(3.5)
            })
        })
        // Secondary, smaller, delayed ripple in pink for rhythm.
        .maybe_child({
            let pulse2_grow = ease_out_quint(time.phase(6.6, 7.3));
            let pulse2_fade = 1.0 - ease_in_out_expo(time.phase(6.95, 7.4));
            let pulse2_r = pulse2_grow * 420.0;
            (pulse2_r > 12.0 && pulse2_fade > 0.0).then(|| {
                Circle::builder()
                    .center(Vec2(CX, CY))
                    .radius(pulse2_r)
                    .stroke(alpha(p.pink, life * pulse2_fade * 0.55))
                    .stroke_width(2.0)
            })
        })
        // Phase 6: the surviving central composition — a deliberate layered
        // mark. Hub + pink hairline collar + 4 axis rays + a dim outer field
        // ring with hash marks + an orbiting scan dot.
        .maybe_child({
            let comp_in = ease_out_cubic(time.phase(6.27, 6.7));
            (comp_in > 0.0).then(|| {
                CentralMark::builder()
                    .time(time)
                    .palette(p)
                    .life(life)
                    .comp_in(comp_in)
                    .cluster_spin(cluster_spin)
            })
        })
        // Four alignment ticks at the cardinals just outside the field ring,
        // making the central mark a fully four-way-symmetric axis indicator.
        .maybe_child({
            let tick_in = ease_out_cubic(time.phase(6.85, 7.15));
            (tick_in > 0.0).then(|| {
                let tick_alpha = alpha(p.paper, life * 0.5);
                let arm_len = 22.0 * tick_in;
                let offset = 96.0;
                Fragment::builder()
                    // Down + up vertical ticks.
                    .child(
                        Rect::builder()
                            .position(Vec2(CX - 1.0, CY + offset))
                            .size(Vec2(2.0, arm_len))
                            .color(tick_alpha),
                    )
                    .child(
                        Rect::builder()
                            .position(Vec2(CX - 1.0, CY - offset - arm_len))
                            .size(Vec2(2.0, arm_len))
                            .color(tick_alpha),
                    )
                    // Right + left horizontal ticks.
                    .child(
                        Rect::builder()
                            .position(Vec2(CX + offset, CY - 1.0))
                            .size(Vec2(arm_len, 2.0))
                            .color(tick_alpha),
                    )
                    .child(
                        Rect::builder()
                            .position(Vec2(CX - offset - arm_len, CY - 1.0))
                            .size(Vec2(arm_len, 2.0))
                            .color(tick_alpha),
                    )
                    .build()
            })
        })
        .build()
}

const PI_HALF: f32 = PI * 0.5;

// RESOLVE's surviving central mark: the hub, the pink collar, four axis rays,
// the outer field ring with hash marks, and the orbiting scan dot with its
// trailing whisker — built in that paint order.
#[tellur_core::component(vector)]
fn CentralMark(
    time: TimelineTime,
    palette: Palette,
    life: f32,
    comp_in: f32,
    cluster_spin: f32,
) -> impl VectorComponent {
    let p = palette;
    let breath = 1.0 + wave(time, 1.4, 0.0) * 0.06;
    let ray_in = ease_out_cubic(time.phase(6.5, 6.95));
    let field_in = ease_out_cubic(time.phase(6.65, 7.1));

    Fragment::builder()
        // (a) the hub: a filled cyan core.
        .child(
            Circle::builder()
                .center(Vec2(CX, CY))
                .radius(7.5 * comp_in * breath)
                .fill(alpha(p.cyan, life)),
        )
        // (b) pink hairline collar one step out from the hub.
        .child(
            Circle::builder()
                .center(Vec2(CX, CY))
                .radius(14.0 * comp_in)
                .stroke(alpha(p.pink, life * 0.9))
                .stroke_width(1.6),
        )
        // (c) four axis rays at the cardinals, rotating with the cluster.
        .maybe_child((ray_in > 0.0).then(|| {
            (0..4)
                .map(move |k| {
                    let a = k as f32 * PI_HALF + cluster_spin;
                    let inner = 19.0;
                    let outer = 38.0 + ray_in * 22.0;
                    let mid_r = (inner + outer) * 0.5;
                    let length = outer - inner;
                    let mid = Vec2(CX + a.cos() * mid_r, CY + a.sin() * mid_r);
                    FxRect::builder()
                        .center(mid)
                        .size(Vec2(1.5, length))
                        .angle(a + PI_HALF)
                        .color(alpha(p.paper, life * 0.55))
                        .opacity(1.0)
                        .scale(Vec2(1.0, 1.0))
                })
                .collect::<Fragment>()
        }))
        // (d) outer "field" ring with 12 micro-hash-marks + (e) orbiting dot.
        .maybe_child((field_in > 0.0).then(|| {
            let field_r = 78.0 + (1.0 - breath) * 4.0;
            let orbit_in = ease_out_cubic(time.phase(6.8, 7.2));
            Fragment::builder()
                .child(
                    Circle::builder()
                        .center(Vec2(CX, CY))
                        .radius(field_r * field_in)
                        .stroke(alpha(p.paper, life * 0.25))
                        .stroke_width(1.0),
                )
                .children((0..12).map(move |k| {
                    let a = k as f32 / 12.0 * TAU - PI_HALF + cluster_spin * 0.5;
                    let inner_r = field_r - 4.0;
                    let outer_r = field_r + 4.0;
                    let mid_r = (inner_r + outer_r) * 0.5;
                    let mid = Vec2(CX + a.cos() * mid_r, CY + a.sin() * mid_r);
                    let major = k % 3 == 0;
                    FxRect::builder()
                        .center(mid)
                        .size(Vec2(if major { 2.0 } else { 1.2 }, 8.0 * field_in))
                        .angle(a + PI_HALF)
                        .color(alpha(p.paper, life * if major { 0.6 } else { 0.35 }))
                        .opacity(1.0)
                        .scale(Vec2(1.0, 1.0))
                }))
                // (e) a single small "scan dot" orbiting the field ring.
                .maybe_child((orbit_in > 0.0).then(|| {
                    let orbit_period = 3.6;
                    let orbit_a = (time.seconds() - 6.8).max(0.0) * (TAU / orbit_period) - PI_HALF;
                    let orbit_pos =
                        Vec2(CX + orbit_a.cos() * field_r, CY + orbit_a.sin() * field_r);
                    let trail_count = 4_i32;
                    let trail_span = PI * 0.04;
                    Fragment::builder()
                        .child(
                            Circle::builder()
                                .center(orbit_pos)
                                .radius(3.0 * orbit_in)
                                .fill(alpha(p.cyan, life * orbit_in)),
                        )
                        // Tiny leading whisker — a 1px hairline behind the dot.
                        .children((1..=trail_count).map(move |j| {
                            let frac = j as f32 / trail_count as f32;
                            let a_t = orbit_a - frac * trail_span;
                            let pos = Vec2(CX + a_t.cos() * field_r, CY + a_t.sin() * field_r);
                            let fade = (1.0 - frac).powi(2);
                            Circle::builder()
                                .center(pos)
                                .radius(1.5 * orbit_in)
                                .fill(alpha(p.cyan, life * orbit_in * fade * 0.5))
                        }))
                        .build()
                }))
                .build()
        }))
        .build()
}
