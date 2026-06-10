//! 04 / RESOLVE — the aperture-close finale: the SCAN ring contracts, the 8
//! satellites slide inward into a double-ring cluster, a singularity flash and
//! shard burst punctuate the collapse, twin ripples wash outward, and a
//! layered central mark settles the piece.

use std::f32::consts::{PI, TAU};

use tellur_core::fragment::Fragment;
use tellur_core::geometry::{Anchor, Vec2};
use tellur_core::layer::VectorLayer;
use tellur_core::time::{LocalTime, Time};

use super::common::*;

#[tellur_core::component(vector)]
pub fn Resolve(time: LocalTime, palette: Palette) -> impl VectorComponent {
    let p = palette;
    if time.during(4.9, DURATION).is_none() {
        return VectorLayer::builder().size(SCENE_SIZE).build();
    }

    let life = time.phase(5.05, 5.5).ease_in_out_expo(0.0, 1.0);

    // Phase 1: contracting hero ring (same R=300 that SCAN ended on).
    let contract = time.phase(5.2, 6.3).eased(Easing::InOutExpo);
    let ring_r = contract.linear(300.0, 22.0);
    // Fade ring from full to 45% as it contracts.
    let ring_alpha = contract.linear(1.0, 0.45) * life;

    // Phase 2 inputs. `cluster_spin` is shared by the satellites and the
    // surviving central mark's rays / field hash marks. The cluster `Window`
    // ties the ease-in envelope (`phase()`) and the unbounded post-anchor
    // angular sweep (`elapsed()`) to one declared `[6.35, 7.0)` interval.
    let sats_in = time.phase(5.25, 6.35).eased(Easing::InOutExpo);
    let cluster = time.window(6.35, 7.0);
    let cluster_spin = cluster.phase().ease_out_cubic(0.0, 1.0) * cluster.elapsed() * 0.18;

    VectorLayer::builder()
        .size(SCENE_SIZE)
        // Phase 1: contracting hero ring.
        .maybe_child((ring_r > 4.0).then(|| {
            Circle::builder()
                .radius(ring_r)
                .stroke(Stroke::new(p.cyan.with_alpha(ring_alpha * 0.9), 3.5))
                .anchored(Anchor::CENTER)
                .snap_to(Vec2(CX, CY))
        }))
        // Phase 2: 8 satellites slide along their angular rays toward center.
        // They settle into two interleaved layers — cardinals further out, the
        // intercardinals closer in — so the final cluster reads as a deliberate
        // double-ring composition instead of a flat ring of dots.
        .children((0..8).map(move |i| {
            let cardinal = i % 2 == 0;
            let base_a = i as f32 / 8.0 * TAU - PI_HALF;
            let final_r = if cardinal { 46.0 } else { 22.0 };
            let target_r = sats_in.linear(300.0, final_r);
            let a = base_a + cluster_spin;
            let pos = Vec2(CX + a.cos() * target_r, CY + a.sin() * target_r);
            let color = if cardinal { p.pink } else { p.cyan };
            let size = sats_in.linear(12.0, if cardinal { 6.0 } else { 4.5 });
            let sat_alpha = sats_in.linear(1.0, 0.6) * life;
            Circle::builder()
                .radius(size)
                .fill(color.with_alpha(sat_alpha))
                .anchored(Anchor::CENTER)
                .snap_to(pos)
        }))
        // Phase 3: "memory" rings — ghosts of where the contracting ring used
        // to be. Three after-images spawn as the ring sweeps inward, each
        // fading slowly. Subtle texture for the contraction.
        .child(
            [(5.55_f32, 230.0_f32), (5.85, 160.0), (6.12, 95.0)]
                .into_iter()
                .filter_map(move |(start, r)| {
                    let ghost_in = time.phase(start, start + 0.08).ease_out_cubic(0.0, 1.0);
                    let ghost_remain = time
                        .phase(start + 0.25, start + 1.3)
                        .ease_in_out_expo(1.0, 0.0);
                    let ghost = (ghost_in * ghost_remain).clamp(0.0, 1.0);
                    (ghost > 0.0).then(|| {
                        Circle::builder()
                            .radius(r)
                            .stroke(Stroke::new(p.cyan.with_alpha(life * ghost * 0.32), 1.8))
                            .anchored(Anchor::CENTER)
                            .snap_to(Vec2(CX, CY))
                    })
                })
                .collect::<Fragment>(),
        )
        // Phase 4: the singularity flash — a brief paper bloom at the moment
        // everything collapses to the center.
        .maybe_child({
            let sing = time.phase(6.2, 6.32).ease_out_quint(0.0, 1.0)
                * time.phase(6.32, 6.62).ease_in_out_expo(1.0, 0.0);
            (sing > 0.0).then(|| {
                Circle::builder()
                    .radius(46.0 + sing * 20.0)
                    .fill(p.paper.with_alpha(sing * life * 0.95))
                    .anchored(Anchor::CENTER)
                    .snap_to(Vec2(CX, CY))
            })
        })
        // 16 short radial shards — emit-then-fade exactly at the singularity.
        // Length peaks then collapses inward as they vanish.
        .maybe_child({
            let shard_kick = time.phase(6.22, 6.36).ease_out_quint(0.0, 1.0);
            let shard_fade = time.phase(6.36, 6.6).ease_in_out_expo(1.0, 0.0);
            let shard_life = (shard_kick * shard_fade).clamp(0.0, 1.0);
            (shard_life > 0.0).then(|| {
                (0..16)
                    .map(move |i| {
                        let a = i as f32 / 16.0 * TAU;
                        let inner_r = 32.0 + shard_kick * 30.0;
                        let length = 26.0 + shard_kick * 50.0;
                        let mid_r = inner_r + length * 0.5;
                        let mid = Vec2(CX + a.cos() * mid_r, CY + a.sin() * mid_r);
                        let color = if i % 2 == 0 { p.pink } else { p.paper };
                        Rectangle::builder()
                            .size(Vec2(2.0, length))
                            .fill(color.with_alpha(life * shard_life * 0.9))
                            .transform_around(Anchor::CENTER, Transform::rotate(a + PI_HALF))
                            .anchored(Anchor::CENTER)
                            .snap_to(mid)
                    })
                    .collect::<Fragment>()
            })
        })
        // Phase 5: the outward ripple. The scene's one elastic moment — the
        // ring kicks out from center with an elastic-eased launch, then a
        // quint-eased growth carries it past the edge.
        .maybe_child({
            let pulse_kick = time.phase(6.22, 6.55).ease_out_elastic(0.0, 600.0).max(0.0);
            let pulse_grow = time.phase(6.3, 7.15).ease_out_quint(0.0, 1.0);
            let pulse_fade = time.phase(6.7, 7.25).ease_in_out_expo(1.0, 0.0);
            let pulse_r = pulse_kick * (0.05 + pulse_grow * 0.95);
            (pulse_r > 12.0 && pulse_fade > 0.0).then(|| {
                Circle::builder()
                    .radius(pulse_r)
                    .stroke(Stroke::new(p.cyan.with_alpha(life * pulse_fade * 0.9), 3.5))
                    .anchored(Anchor::CENTER)
                    .snap_to(Vec2(CX, CY))
            })
        })
        // Secondary, smaller, delayed ripple in pink for rhythm.
        .maybe_child({
            let pulse2_grow = time.phase(6.6, 7.3).ease_out_quint(0.0, 1.0);
            let pulse2_fade = time.phase(6.95, 7.4).ease_in_out_expo(1.0, 0.0);
            let pulse2_r = pulse2_grow * 420.0;
            (pulse2_r > 12.0 && pulse2_fade > 0.0).then(|| {
                Circle::builder()
                    .radius(pulse2_r)
                    .stroke(Stroke::new(
                        p.pink.with_alpha(life * pulse2_fade * 0.55),
                        2.0,
                    ))
                    .anchored(Anchor::CENTER)
                    .snap_to(Vec2(CX, CY))
            })
        })
        // Phase 6: the surviving central composition — a deliberate layered
        // mark. Hub + pink hairline collar + 4 axis rays + a dim outer field
        // ring with hash marks + an orbiting scan dot.
        .maybe_child({
            let comp_in = time.phase(6.27, 6.7).ease_out_cubic(0.0, 1.0);
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
            let tick_in = time.phase(6.85, 7.15).ease_out_cubic(0.0, 1.0);
            (tick_in > 0.0).then(|| {
                let tick_alpha = p.paper.with_alpha(life * 0.5);
                let arm_len = 22.0 * tick_in;
                let offset = 96.0;
                Fragment::builder()
                    // Down + up vertical ticks.
                    .child(
                        Rectangle::builder()
                            .size(Vec2(2.0, arm_len))
                            .fill(tick_alpha)
                            .place_at(Vec2(CX - 1.0, CY + offset)),
                    )
                    .child(
                        Rectangle::builder()
                            .size(Vec2(2.0, arm_len))
                            .fill(tick_alpha)
                            .place_at(Vec2(CX - 1.0, CY - offset - arm_len)),
                    )
                    // Right + left horizontal ticks.
                    .child(
                        Rectangle::builder()
                            .size(Vec2(arm_len, 2.0))
                            .fill(tick_alpha)
                            .place_at(Vec2(CX + offset, CY - 1.0)),
                    )
                    .child(
                        Rectangle::builder()
                            .size(Vec2(arm_len, 2.0))
                            .fill(tick_alpha)
                            .place_at(Vec2(CX - offset - arm_len, CY - 1.0)),
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
    time: LocalTime,
    palette: Palette,
    life: f32,
    comp_in: f32,
    cluster_spin: f32,
) -> impl VectorComponent {
    let p = palette;
    // `wave` rises from its trough (1 - cos); the breathing was authored on a
    // sine that starts mid-swing rising, so lead by a quarter period.
    let breath = LocalTime::new(time.seconds() + 0.25 * 1.4)
        .wave(1.4)
        .linear(0.94, 1.06);
    let ray_in = time.phase(6.5, 6.95).ease_out_cubic(0.0, 1.0);
    let field_in = time.phase(6.65, 7.1).ease_out_cubic(0.0, 1.0);

    Fragment::builder()
        // (a) the hub: a filled cyan core.
        .child(
            Circle::builder()
                .radius(7.5 * comp_in * breath)
                .fill(p.cyan.with_alpha(life))
                .anchored(Anchor::CENTER)
                .snap_to(Vec2(CX, CY)),
        )
        // (b) pink hairline collar one step out from the hub.
        .child(
            Circle::builder()
                .radius(14.0 * comp_in)
                .stroke(Stroke::new(p.pink.with_alpha(life * 0.9), 1.6))
                .anchored(Anchor::CENTER)
                .snap_to(Vec2(CX, CY)),
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
                    Rectangle::builder()
                        .size(Vec2(1.5, length))
                        .fill(p.paper.with_alpha(life * 0.55))
                        .transform_around(Anchor::CENTER, Transform::rotate(a + PI_HALF))
                        .anchored(Anchor::CENTER)
                        .snap_to(mid)
                })
                .collect::<Fragment>()
        }))
        // (d) outer "field" ring with 12 micro-hash-marks + (e) orbiting dot.
        .maybe_child((field_in > 0.0).then(|| {
            let field_r = 78.0 + (1.0 - breath) * 4.0;
            // The orbit `Window` pairs the dot's ease-in (`phase()`) with the
            // continuously-advancing orbital angle (`elapsed()`) — both
            // anchored at 6.8s, so the dot fades in while the orbit is
            // already accruing angle.
            let orbit = time.window(6.8, 7.2);
            let orbit_in = orbit.phase().ease_out_cubic(0.0, 1.0);
            Fragment::builder()
                .child(
                    Circle::builder()
                        .radius(field_r * field_in)
                        .stroke(Stroke::new(p.paper.with_alpha(life * 0.25), 1.0))
                        .anchored(Anchor::CENTER)
                        .snap_to(Vec2(CX, CY)),
                )
                .children((0..12).map(move |k| {
                    let a = k as f32 / 12.0 * TAU - PI_HALF + cluster_spin * 0.5;
                    let inner_r = field_r - 4.0;
                    let outer_r = field_r + 4.0;
                    let mid_r = (inner_r + outer_r) * 0.5;
                    let mid = Vec2(CX + a.cos() * mid_r, CY + a.sin() * mid_r);
                    let major = k % 3 == 0;
                    Rectangle::builder()
                        .size(Vec2(if major { 2.0 } else { 1.2 }, 8.0 * field_in))
                        .fill(p.paper.with_alpha(life * if major { 0.6 } else { 0.35 }))
                        .transform_around(Anchor::CENTER, Transform::rotate(a + PI_HALF))
                        .anchored(Anchor::CENTER)
                        .snap_to(mid)
                }))
                // (e) a single small "scan dot" orbiting the field ring.
                .maybe_child((orbit_in > 0.0).then(|| {
                    let orbit_period = 3.6;
                    let orbit_a = orbit.elapsed() * (TAU / orbit_period) - PI_HALF;
                    let orbit_pos =
                        Vec2(CX + orbit_a.cos() * field_r, CY + orbit_a.sin() * field_r);
                    let trail_count = 4_i32;
                    let trail_span = PI * 0.04;
                    Fragment::builder()
                        .child(
                            Circle::builder()
                                .radius(3.0 * orbit_in)
                                .fill(p.cyan.with_alpha(life * orbit_in))
                                .anchored(Anchor::CENTER)
                                .snap_to(orbit_pos),
                        )
                        // Tiny leading whisker — a 1px hairline behind the dot.
                        .children((1..=trail_count).map(move |j| {
                            let frac = j as f32 / trail_count as f32;
                            let a_t = orbit_a - frac * trail_span;
                            let pos = Vec2(CX + a_t.cos() * field_r, CY + a_t.sin() * field_r);
                            let fade = (1.0 - frac).powi(2);
                            Circle::builder()
                                .radius(1.5 * orbit_in)
                                .fill(p.cyan.with_alpha(life * orbit_in * fade * 0.5))
                                .anchored(Anchor::CENTER)
                                .snap_to(pos)
                        }))
                        .build()
                }))
                .build()
        }))
        .build()
}
