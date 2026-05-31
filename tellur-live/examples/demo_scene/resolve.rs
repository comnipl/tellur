//! 04 / RESOLVE — the aperture-close finale: the SCAN ring contracts, the 8
//! satellites slide inward into a double-ring cluster, a singularity flash and
//! shard burst punctuate the collapse, twin ripples wash outward, and a
//! layered central mark settles the piece.

use std::f32::consts::{PI, TAU};

use tellur_core::geometry::Vec2;
use tellur_core::layer::VectorLayer;
use tellur_core::placement::Placed;
use tellur_core::time::{Time, TimelineTime};
use tellur_core::vector::VectorComponent;

use super::common::*;

#[tellur_core::component(vector)]
pub fn resolve(time: TimelineTime, palette: Palette) -> impl VectorComponent {
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
        .maybe_child(if ring_r > 4.0 {
            circle(
                Vec2(CX, CY),
                ring_r,
                None,
                Some((alpha(p.cyan, ring_alpha * 0.9), 3.5)),
            )
        } else {
            None
        })
        // Phase 2: 8 satellites slide along their angular rays toward center.
        // They settle into two interleaved layers — cardinals further out, the
        // intercardinals closer in — so the final cluster reads as a deliberate
        // double-ring composition instead of a flat ring of dots.
        .children((0..8).filter_map(move |i| {
            let cardinal = i % 2 == 0;
            let base_a = i as f32 / 8.0 * TAU - PI_HALF;
            let final_r = if cardinal { 46.0 } else { 22.0 };
            let target_r = lerp(300.0, final_r, sats_in);
            let a = base_a + cluster_spin;
            let pos = Vec2(CX + a.cos() * target_r, CY + a.sin() * target_r);
            let color = if cardinal { p.pink } else { p.cyan };
            let size = lerp(12.0, if cardinal { 6.0 } else { 4.5 }, sats_in);
            let sat_alpha = (1.0 - sats_in * 0.4) * life;
            circle(pos, size, Some(alpha(color, sat_alpha)), None)
        }))
        // Phase 3: "memory" rings — ghosts of where the contracting ring used
        // to be. Three after-images spawn as the ring sweeps inward, each
        // fading slowly. Subtle texture for the contraction.
        .children(
            [(5.55_f32, 230.0_f32), (5.85, 160.0), (6.12, 95.0)]
                .into_iter()
                .filter_map(move |(start, r)| {
                    let ghost_in = ease_out_cubic(time.phase(start, start + 0.08));
                    let ghost_out = ease_in_out_expo(time.phase(start + 0.25, start + 1.3));
                    let ghost = (ghost_in * (1.0 - ghost_out)).clamp(0.0, 1.0);
                    (ghost > 0.0)
                        .then(|| {
                            circle(
                                Vec2(CX, CY),
                                r,
                                None,
                                Some((alpha(p.cyan, life * ghost * 0.32), 1.8)),
                            )
                        })
                        .flatten()
                }),
        )
        // Phase 4: the singularity flash — a brief paper bloom at the moment
        // everything collapses to the center.
        .maybe_child({
            let sing = ease_out_quint(time.phase(6.2, 6.32))
                * (1.0 - ease_in_out_expo(time.phase(6.32, 6.62)));
            if sing > 0.0 {
                circle(
                    Vec2(CX, CY),
                    46.0 + sing * 20.0,
                    Some(alpha(p.paper, sing * life * 0.95)),
                    None,
                )
            } else {
                None
            }
        })
        // 16 short radial shards — emit-then-fade exactly at the singularity.
        // Length peaks then collapses inward as they vanish.
        .maybe_children({
            let shard_kick = ease_out_quint(time.phase(6.22, 6.36));
            let shard_fade = ease_in_out_expo(time.phase(6.36, 6.6));
            let shard_life = (shard_kick * (1.0 - shard_fade)).clamp(0.0, 1.0);
            (shard_life > 0.0).then(|| {
                (0..16).filter_map(move |i| {
                    let a = i as f32 / 16.0 * TAU;
                    let inner_r = 32.0 + shard_kick * 30.0;
                    let length = 26.0 + shard_kick * 50.0;
                    let mid_r = inner_r + length * 0.5;
                    let mid = Vec2(CX + a.cos() * mid_r, CY + a.sin() * mid_r);
                    let color = if i % 2 == 0 { p.pink } else { p.paper };
                    fx_rect(
                        mid,
                        Vec2(2.0, length),
                        a + PI_HALF,
                        alpha(color, life * shard_life * 0.9),
                        1.0,
                        Vec2(1.0, 1.0),
                    )
                })
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
            if pulse_r > 12.0 && pulse_fade > 0.0 {
                circle(
                    Vec2(CX, CY),
                    pulse_r,
                    None,
                    Some((alpha(p.cyan, life * pulse_fade * 0.9), 3.5)),
                )
            } else {
                None
            }
        })
        // Secondary, smaller, delayed ripple in pink for rhythm.
        .maybe_child({
            let pulse2_grow = ease_out_quint(time.phase(6.6, 7.3));
            let pulse2_fade = 1.0 - ease_in_out_expo(time.phase(6.95, 7.4));
            let pulse2_r = pulse2_grow * 420.0;
            if pulse2_r > 12.0 && pulse2_fade > 0.0 {
                circle(
                    Vec2(CX, CY),
                    pulse2_r,
                    None,
                    Some((alpha(p.pink, life * pulse2_fade * 0.55), 2.0)),
                )
            } else {
                None
            }
        })
        // Phase 6: the surviving central composition — a deliberate layered
        // mark. Hub + pink hairline collar + 4 axis rays + a dim outer field
        // ring with hash marks + an orbiting scan dot.
        .maybe_children({
            let comp_in = ease_out_cubic(time.phase(6.27, 6.7));
            (comp_in > 0.0).then(|| central_mark(time, p, life, comp_in, cluster_spin))
        })
        // Four alignment ticks at the cardinals just outside the field ring,
        // making the central mark a fully four-way-symmetric axis indicator.
        .maybe_children({
            let tick_in = ease_out_cubic(time.phase(6.85, 7.15));
            (tick_in > 0.0).then(|| {
                let tick_alpha = alpha(p.paper, life * 0.5);
                let arm_len = 22.0 * tick_in;
                let offset = 96.0;
                [
                    // Down + up vertical ticks.
                    rect(Vec2(CX - 1.0, CY + offset), Vec2(2.0, arm_len), tick_alpha),
                    rect(
                        Vec2(CX - 1.0, CY - offset - arm_len),
                        Vec2(2.0, arm_len),
                        tick_alpha,
                    ),
                    // Right + left horizontal ticks.
                    rect(Vec2(CX + offset, CY - 1.0), Vec2(arm_len, 2.0), tick_alpha),
                    rect(
                        Vec2(CX - offset - arm_len, CY - 1.0),
                        Vec2(arm_len, 2.0),
                        tick_alpha,
                    ),
                ]
                .into_iter()
                .flatten()
            })
        })
        .build()
}

const PI_HALF: f32 = PI * 0.5;

// RESOLVE's surviving central mark: the hub, the pink collar, four axis rays,
// the outer field ring with hash marks, and the orbiting scan dot with its
// trailing whisker — built in that paint order.
fn central_mark(
    time: TimelineTime,
    p: Palette,
    life: f32,
    comp_in: f32,
    cluster_spin: f32,
) -> impl Iterator<Item = Placed<dyn VectorComponent>> {
    let breath = 1.0 + wave(time, 1.4, 0.0) * 0.06;

    // (a) the hub: a filled cyan core.
    let hub = circle(
        Vec2(CX, CY),
        7.5 * comp_in * breath,
        Some(alpha(p.cyan, life)),
        None,
    );

    // (b) pink hairline collar one step out from the hub.
    let collar = circle(
        Vec2(CX, CY),
        14.0 * comp_in,
        None,
        Some((alpha(p.pink, life * 0.9), 1.6)),
    );

    // (c) four axis rays at the cardinals, rotating with the cluster. They
    // start just outside the collar and end just inside the outer field ring.
    let ray_in = ease_out_cubic(time.phase(6.5, 6.95));
    let rays = (ray_in > 0.0)
        .then(|| {
            (0..4).filter_map(move |k| {
                let a = k as f32 * PI_HALF + cluster_spin;
                let inner = 19.0;
                let outer = 38.0 + ray_in * 22.0;
                let mid_r = (inner + outer) * 0.5;
                let length = outer - inner;
                let mid = Vec2(CX + a.cos() * mid_r, CY + a.sin() * mid_r);
                fx_rect(
                    mid,
                    Vec2(1.5, length),
                    a + PI_HALF,
                    alpha(p.paper, life * 0.55),
                    1.0,
                    Vec2(1.0, 1.0),
                )
            })
        })
        .into_iter()
        .flatten();

    // (d) outer "field" ring with 12 micro-hash-marks — echoes the SCAN
    // graduated dial at miniature scale. (e) a single orbiting scan dot.
    let field_in = ease_out_cubic(time.phase(6.65, 7.1));
    let field = (field_in > 0.0)
        .then(|| {
            let field_r = 78.0 + (1.0 - breath) * 4.0;
            let ring = circle(
                Vec2(CX, CY),
                field_r * field_in,
                None,
                Some((alpha(p.paper, life * 0.25), 1.0)),
            );
            let hashes = (0..12).filter_map(move |k| {
                let a = k as f32 / 12.0 * TAU - PI_HALF + cluster_spin * 0.5;
                let inner_r = field_r - 4.0;
                let outer_r = field_r + 4.0;
                let mid_r = (inner_r + outer_r) * 0.5;
                let mid = Vec2(CX + a.cos() * mid_r, CY + a.sin() * mid_r);
                let major = k % 3 == 0;
                fx_rect(
                    mid,
                    Vec2(if major { 2.0 } else { 1.2 }, 8.0 * field_in),
                    a + PI_HALF,
                    alpha(p.paper, life * if major { 0.6 } else { 0.35 }),
                    1.0,
                    Vec2(1.0, 1.0),
                )
            });

            // (e) a single small "scan dot" orbiting the field ring — like a
            // slow second-hand. Period ~3.6s.
            let orbit_in = ease_out_cubic(time.phase(6.8, 7.2));
            let orbit = (orbit_in > 0.0)
                .then(|| {
                    let orbit_period = 3.6;
                    let orbit_a = (time.seconds() - 6.8).max(0.0) * (TAU / orbit_period) - PI_HALF;
                    let orbit_pos =
                        Vec2(CX + orbit_a.cos() * field_r, CY + orbit_a.sin() * field_r);
                    let dot = circle(
                        orbit_pos,
                        3.0 * orbit_in,
                        Some(alpha(p.cyan, life * orbit_in)),
                        None,
                    );
                    // Tiny leading whisker — a 1px hairline behind the dot for
                    // motion sense.
                    let trail_count = 4_i32;
                    let trail_span = PI * 0.04;
                    let trail = (1..=trail_count).filter_map(move |j| {
                        let frac = j as f32 / trail_count as f32;
                        let a_t = orbit_a - frac * trail_span;
                        let pos = Vec2(CX + a_t.cos() * field_r, CY + a_t.sin() * field_r);
                        let fade = (1.0 - frac).powi(2);
                        circle(
                            pos,
                            1.5 * orbit_in,
                            Some(alpha(p.cyan, life * orbit_in * fade * 0.5)),
                            None,
                        )
                    });
                    dot.into_iter().chain(trail)
                })
                .into_iter()
                .flatten();

            ring.into_iter().chain(hashes).chain(orbit)
        })
        .into_iter()
        .flatten();

    hub.into_iter().chain(collar).chain(rays).chain(field)
}
