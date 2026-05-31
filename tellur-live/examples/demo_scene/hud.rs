//! Persistent UI scaffolding — the "instrument panel" framing that keeps the
//! piece reading as a deliberate system rather than free-floating shapes.
//!
//! Implemented as a `VectorComponent` whose state is reduced to three small,
//! stable values: a `Palette`, two `Phase`s, and a `section` discriminator.
//! Once the intro phase saturates to 1.0 (after ~1.24s), and as long as
//! `section` hasn't ticked over, the struct hashes and compares equal across
//! frames — so wrapping `Hud` in `.rasterize()` lets `CachingRenderContext`
//! reuse the rasterized image for the full steady-state span instead of
//! re-rendering every frame.

use std::hash::{Hash, Hasher};

use tellur_core::color::Color;
use tellur_core::dyn_compare::hash_f32;
use tellur_core::fragment::Fragment;
use tellur_core::geometry::{Anchor, Constraints, Vec2};
use tellur_core::layer::VectorLayer;
use tellur_core::phase::Phase;
use tellur_core::text::Weight;
use tellur_core::vector::{VectorComponent, VectorGraphic};

use super::common::*;

// HUD intro/outro time windows. All bracket/tick/label staggers fit inside
// `INTRO`; everything saturates by `INTRO_END` and the component becomes
// byte-identical between frames.
pub const HUD_INTRO_START: f32 = 0.15;
pub const HUD_INTRO_END: f32 = 1.24;
pub const HUD_OUTRO_START: f32 = 7.1;
pub const HUD_OUTRO_END: f32 = 7.55;
const HUD_INTRO_WIDTH: f32 = HUD_INTRO_END - HUD_INTRO_START;
const HUD_OUTRO_WIDTH: f32 = HUD_OUTRO_END - HUD_OUTRO_START;

fn section_marker(section: u8, p: Palette) -> (&'static str, Color) {
    match section {
        0 => ("01 / OVERTURE", p.pink),
        1 => ("02 / FIELD", p.cyan),
        2 => ("03 / SCAN", p.pink),
        _ => ("04 / RESOLVE", p.cyan),
    }
}

pub fn section_index_at(t: f32) -> u8 {
    if t < 1.85 {
        0
    } else if t < 3.4 {
        1
    } else if t < 5.0 {
        2
    } else {
        3
    }
}

// Phase-local helper: returns the sub-phase reached at virtual time
// `virtual_t` (measured from the intro/outro start) for an event spanning
// `[start, end]` in that same virtual frame.
fn local_phase(virtual_t: f32, start: f32, end: f32) -> Phase {
    Phase::saturating((virtual_t - start) / (end - start))
}

#[derive(Clone, Copy)]
pub struct Hud {
    pub palette: Palette,
    pub intro: Phase,
    pub outro: Phase,
    pub section: u8,
}

impl PartialEq for Hud {
    fn eq(&self, other: &Self) -> bool {
        self.section == other.section
            && self.intro == other.intro
            && self.outro == other.outro
            && self.palette == other.palette
    }
}

impl Hash for Hud {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.section.hash(state);
        hash_f32(self.intro.get(), state);
        hash_f32(self.outro.get(), state);
        self.palette.hash(state);
    }
}

impl VectorComponent for Hud {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        constraints.constrain(SCENE_SIZE)
    }

    // paint_bounds defaults to (0, 0)..size — the HUD doesn't paint outside
    // its own scene rect, so the default is exactly right.

    fn render(&self, size: Vec2) -> VectorGraphic {
        self.layer(size).render(size)
    }
}

impl Hud {
    fn layer(&self, size: Vec2) -> VectorLayer {
        // `intro_t` / `outro_t` are virtual elapsed seconds inside the intro
        // / outro window. Each sub-event below is expressed relative to that
        // virtual frame — so when `intro = 1.0` (saturated), every sub-phase
        // is also fully saturated, and the resulting `VectorLayer` is
        // structurally identical to last frame's.
        let intro_t = self.intro.get() * HUD_INTRO_WIDTH;
        let outro_t = self.outro.get() * HUD_OUTRO_WIDTH;

        let alpha_in = ease_in_out_expo(local_phase(intro_t, 0.0, 0.4));
        let alpha_out = ease_in_out_expo(local_phase(outro_t, 0.0, HUD_OUTRO_WIDTH));
        let life = (alpha_in * (1.0 - alpha_out)).clamp(0.0, 1.0);
        if life <= 0.0 {
            return VectorLayer::builder().size(size).build();
        }

        let p = self.palette;
        let stroke_w = 3.0_f32;
        let inset = 96.0_f32;
        let bracket_len = 92.0_f32;

        let label_in = ease_in_out_expo(local_phase(intro_t, 0.4, 0.8));
        let label_alpha = 0.95 * life * label_in;
        let (idx_text, idx_color) = section_marker(self.section, p);
        let marker_x = SCENE_SIZE.0 - inset;

        let corners = [
            (inset, inset, 1.0_f32, 1.0_f32),
            (SCENE_SIZE.0 - inset, inset, -1.0, 1.0),
            (inset, SCENE_SIZE.1 - inset, 1.0, -1.0),
            (SCENE_SIZE.0 - inset, SCENE_SIZE.1 - inset, -1.0, -1.0),
        ];

        VectorLayer::builder()
            .size(size)
            // Four corner brackets that pop in staggered.
            .children(
                corners
                    .into_iter()
                    .enumerate()
                    .map(move |(i, (ax, ay, dx, dy))| {
                        let stagger = i as f32 * 0.05;
                        let pop =
                            ease_out_cubic(local_phase(intro_t, 0.1 + stagger, 0.6 + stagger));
                        let len = bracket_len * pop;
                        let color = alpha(p.paper, 0.55 * life);
                        let hx = if dx > 0.0 { ax } else { ax - len };
                        let vy = if dy > 0.0 { ay } else { ay - len };
                        Fragment::builder()
                            .child(
                                Rect::builder()
                                    .position(Vec2(hx, ay - stroke_w * 0.5))
                                    .size(Vec2(len, stroke_w))
                                    .color(color),
                            )
                            .child(
                                Rect::builder()
                                    .position(Vec2(ax - stroke_w * 0.5, vy))
                                    .size(Vec2(stroke_w, len))
                                    .color(color),
                            )
                            .build()
                    }),
            )
            // Top-left wordmark + tagline.
            .child(
                Label::builder()
                    .position(Vec2(inset, inset - 22.0))
                    .anchor(Anchor::BOTTOM_LEFT)
                    .text("TELLUR")
                    .size(20.0)
                    .color(alpha(p.paper, label_alpha))
                    .weight(Weight::BOLD),
            )
            .child(
                Label::builder()
                    .position(Vec2(inset + 96.0, inset - 22.0))
                    .anchor(Anchor::BOTTOM_LEFT)
                    .text("kinetic-motion · 7.6s")
                    .size(13.0)
                    .color(alpha(p.paper, 0.5 * life * label_in))
                    .weight(Weight::NORMAL),
            )
            // Top-right section marker + accent dot.
            .child(
                Label::builder()
                    .position(Vec2(marker_x, inset - 22.0))
                    .anchor(Anchor::BOTTOM_RIGHT)
                    .text(idx_text)
                    .size(14.0)
                    .color(alpha(p.paper, 0.75 * life * label_in))
                    .weight(Weight::NORMAL),
            )
            .child(
                Circle::builder()
                    .center(Vec2(marker_x - 128.0, inset - 28.0))
                    .radius(4.5 * label_in)
                    .fill(alpha(idx_color, life * label_in)),
            )
            // Static "OBS" badge below the section marker — reads as a "live
            // observation" tag without animating per frame (so it stays inside
            // the cached HUD raster).
            .child(
                Label::builder()
                    .position(Vec2(marker_x, inset + 4.0))
                    .anchor(Anchor::TOP_RIGHT)
                    .text("OBS · TELLUR-04")
                    .size(11.0)
                    .color(alpha(p.paper, 0.4 * life * label_in))
                    .weight(Weight::NORMAL),
            )
            // Bottom edge tick ruler — every 4th tick is taller.
            .children((0..17).map(move |i| {
                let stagger = i as f32 * 0.018;
                let pop = ease_out_cubic(local_phase(intro_t, 0.3 + stagger, 0.8 + stagger));
                let bar_left = inset + 24.0;
                let bar_right = SCENE_SIZE.0 - inset - 24.0;
                let tick_y_top = SCENE_SIZE.1 - inset + 28.0;
                let frac = i as f32 / 16.0;
                let x = lerp(bar_left, bar_right, frac);
                let major = i % 4 == 0;
                let height = if major { 18.0 } else { 8.0 };
                let color = alpha(p.paper, if major { 0.55 } else { 0.35 } * life);
                Rect::builder()
                    .position(Vec2(x - 1.0, tick_y_top))
                    .size(Vec2(2.0, height * pop))
                    .color(color)
            }))
            // Left + right edge tick rulers — completes the four-sided
            // instrument frame so the scaffold reads as a full HUD.
            .children((0..11).map(move |i| {
                let stagger = i as f32 * 0.02;
                let pop = ease_out_cubic(local_phase(intro_t, 0.55 + stagger, 1.0 + stagger));
                let v_bar_top = inset + 60.0;
                let v_bar_bottom = SCENE_SIZE.1 - inset - 60.0;
                let frac = i as f32 / 10.0;
                let y = lerp(v_bar_top, v_bar_bottom, frac);
                let major = i % 5 == 0;
                let width = if major { 16.0 } else { 7.0 };
                let color = alpha(p.paper, if major { 0.5 } else { 0.3 } * life);
                Fragment::builder()
                    // Left side ticks point inward.
                    .maybe_child((pop > 0.0).then(|| {
                        Rect::builder()
                            .position(Vec2(inset - 28.0, y - 1.0))
                            .size(Vec2(width * pop, 2.0))
                            .color(color)
                    }))
                    // Right side ticks point inward.
                    .maybe_child((pop > 0.0).then(|| {
                        Rect::builder()
                            .position(Vec2(SCENE_SIZE.0 - inset + 28.0 - width * pop, y - 1.0))
                            .size(Vec2(width * pop, 2.0))
                            .color(color)
                    }))
                    .build()
            }))
            // Bottom-corner runtime + resolution readouts.
            .child(
                Label::builder()
                    .position(Vec2(inset, SCENE_SIZE.1 - inset + 20.0))
                    .anchor(Anchor::TOP_LEFT)
                    .text("RUNTIME 7600MS · 60FPS")
                    .size(12.0)
                    .color(alpha(p.paper, 0.45 * life * label_in))
                    .weight(Weight::NORMAL),
            )
            .child(
                Label::builder()
                    .position(Vec2(SCENE_SIZE.0 - inset, SCENE_SIZE.1 - inset + 20.0))
                    .anchor(Anchor::TOP_RIGHT)
                    .text("1920 × 1080 · RGBA")
                    .size(12.0)
                    .color(alpha(p.paper, 0.45 * life * label_in))
                    .weight(Weight::NORMAL),
            )
            .build()
    }
}
