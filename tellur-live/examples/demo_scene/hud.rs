//! Persistent UI scaffolding — the "instrument panel" framing that keeps the
//! piece reading as a deliberate system rather than free-floating shapes.
//!
//! Implemented as a `VectorComponent` whose state is reduced to four small,
//! stable values: a `Palette`, a clamped intro `Window`, an outro `Phase`, and
//! a `section` discriminator.
//! Once the intro window saturates (after ~1.35s), and as long as
//! `section` hasn't ticked over, the struct hashes and compares equal across
//! frames — so wrapping `Hud` in `.rasterize()` lets `CachingRenderContext`
//! reuse the rasterized image for the full steady-state span instead of
//! re-rendering every frame.

use tellur_core::color::Color;
use tellur_core::fragment::Fragment;
use tellur_core::geometry::{Anchor, Constraints, Vec2};
use tellur_core::layer::VectorLayer;
use tellur_core::phase::Phase;
use tellur_core::text::Weight;
use tellur_core::vector::{VectorComponent, VectorGraphic};
use tellur_core::window::Window;

use super::common::*;

// HUD intro/outro time windows. All bracket/tick/label staggers fit inside
// the intro window expressed via `Window::sub_secs`; everything saturates by
// `INTRO_END` and the component becomes byte-identical between frames.
pub const HUD_INTRO_START: f64 = 0.15;
pub const HUD_INTRO_END: f64 = 1.35;
pub const HUD_OUTRO_START: f64 = 7.1;
pub const HUD_OUTRO_END: f64 = 7.55;

fn section_marker(section: u8, p: Palette) -> (&'static str, Color) {
    match section {
        0 => ("01 / OVERTURE", p.pink),
        1 => ("02 / FIELD", p.cyan),
        2 => ("03 / SCAN", p.pink),
        _ => ("04 / RESOLVE", p.cyan),
    }
}

pub fn section_index_at(t: f64) -> u8 {
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

#[derive(Clone, Copy, PartialEq, Hash)]
pub struct Hud {
    pub palette: Palette,
    pub intro: Window,
    pub outro: Phase,
    pub section: u8,
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
        // Each sub-event below carves a window-local seconds slice out of
        // `intro` via `Window::sub_secs`. The eased factors come out as f32
        // already interpolated into their target range (here `(0, 1)` for
        // alpha) via the uniform `ease_*(from, to)` shape.
        let intro = self.intro;
        let alpha_in = intro.sub_secs(0.0..0.4).ease_in_out_expo(0.0, 1.0);
        let alpha_remain = self.outro.ease_in_out_expo(1.0, 0.0);
        let life = alpha_in * alpha_remain;
        if life <= 0.0 {
            return VectorLayer::builder().size(size).build();
        }

        let p = self.palette;
        let stroke_w = 3.0_f32;
        let inset = 96.0_f32;
        let bracket_len = 92.0_f32;

        let label_in = intro.sub_secs(0.4..0.8).ease_in_out_expo(0.0, 1.0);
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
                        let stagger = i as f64 * 0.05;
                        let pop = intro
                            .sub_secs((0.1 + stagger)..(0.6 + stagger))
                            .ease_out_cubic(0.0, 1.0);
                        let len = pop * bracket_len;
                        let color = p.paper.with_alpha(life * 0.55);
                        let hx = if dx > 0.0 { ax } else { ax - len };
                        let vy = if dy > 0.0 { ay } else { ay - len };
                        Fragment::builder()
                            .child(
                                Rectangle::builder()
                                    .size(Vec2(len, stroke_w))
                                    .fill(color)
                                    .place_at(Vec2(hx, ay - stroke_w * 0.5)),
                            )
                            .child(
                                Rectangle::builder()
                                    .size(Vec2(stroke_w, len))
                                    .fill(color)
                                    .place_at(Vec2(ax - stroke_w * 0.5, vy)),
                            )
                            .build()
                    }),
            )
            // Top-left wordmark + tagline.
            .child(
                Text::builder()
                    .font(MONOSPACE.clone())
                    .size(20.0)
                    .weight(Weight::BOLD)
                    .fill(p.paper.with_alpha(label_alpha))
                    .span(TextSpan::plain("TELLUR"))
                    .anchored(Anchor::BOTTOM_LEFT)
                    .snap_to(Vec2(inset, inset - 22.0)),
            )
            .child(
                Text::builder()
                    .font(MONOSPACE.clone())
                    .size(13.0)
                    .weight(Weight::NORMAL)
                    .fill(p.paper.with_alpha(0.5 * life * label_in))
                    .span(TextSpan::plain("kinetic-motion · 7.6s"))
                    .anchored(Anchor::BOTTOM_LEFT)
                    .snap_to(Vec2(inset + 96.0, inset - 22.0)),
            )
            // Top-right section marker + accent dot.
            .child(
                Text::builder()
                    .font(MONOSPACE.clone())
                    .size(14.0)
                    .weight(Weight::NORMAL)
                    .fill(p.paper.with_alpha(0.75 * life * label_in))
                    .span(TextSpan::plain(idx_text))
                    .anchored(Anchor::BOTTOM_RIGHT)
                    .snap_to(Vec2(marker_x, inset - 22.0)),
            )
            .child(
                Circle::builder()
                    .radius(4.5 * label_in)
                    .fill(idx_color.with_alpha(life * label_in))
                    .anchored(Anchor::CENTER)
                    .snap_to(Vec2(marker_x - 128.0, inset - 28.0)),
            )
            // Static "OBS" badge below the section marker — reads as a "live
            // observation" tag without animating per frame (so it stays inside
            // the cached HUD raster).
            .child(
                Text::builder()
                    .font(MONOSPACE.clone())
                    .size(11.0)
                    .weight(Weight::NORMAL)
                    .fill(p.paper.with_alpha(0.4 * life * label_in))
                    .span(TextSpan::plain("OBS · TELLUR-04"))
                    .anchored(Anchor::TOP_RIGHT)
                    .snap_to(Vec2(marker_x, inset + 4.0)),
            )
            // Bottom edge tick ruler — every 4th tick is taller.
            .children((0..17).map(move |i| {
                let stagger = i as f64 * 0.018;
                let pop = intro
                    .sub_secs((0.3 + stagger)..(0.8 + stagger))
                    .ease_out_cubic(0.0, 1.0);
                let bar_left = inset + 24.0;
                let bar_right = SCENE_SIZE.0 - inset - 24.0;
                let tick_y_top = SCENE_SIZE.1 - inset + 28.0;
                let frac = i as f32 / 16.0;
                let x = bar_left + (bar_right - bar_left) * frac;
                let major = i % 4 == 0;
                let height = if major { 18.0 } else { 8.0 };
                let color = p.paper.with_alpha(if major { 0.55 } else { 0.35 } * life);
                Rectangle::builder()
                    .size(Vec2(2.0, height * pop))
                    .fill(color)
                    .place_at(Vec2(x - 1.0, tick_y_top))
            }))
            // Left + right edge tick rulers — completes the four-sided
            // instrument frame so the scaffold reads as a full HUD.
            .children((0..11).map(move |i| {
                let stagger = i as f64 * 0.02;
                let pop = intro
                    .sub_secs((0.55 + stagger)..(1.0 + stagger))
                    .ease_out_cubic(0.0, 1.0);
                let v_bar_top = inset + 60.0;
                let v_bar_bottom = SCENE_SIZE.1 - inset - 60.0;
                let frac = i as f32 / 10.0;
                let y = v_bar_top + (v_bar_bottom - v_bar_top) * frac;
                let major = i % 5 == 0;
                let width = if major { 16.0 } else { 7.0 };
                let color = p.paper.with_alpha(if major { 0.5 } else { 0.3 } * life);
                Fragment::builder()
                    // Left side ticks point inward.
                    .maybe_child((pop > 0.0).then(|| {
                        Rectangle::builder()
                            .size(Vec2(width * pop, 2.0))
                            .fill(color)
                            .place_at(Vec2(inset - 28.0, y - 1.0))
                    }))
                    // Right side ticks point inward.
                    .maybe_child((pop > 0.0).then(|| {
                        Rectangle::builder()
                            .size(Vec2(width * pop, 2.0))
                            .fill(color)
                            .place_at(Vec2(SCENE_SIZE.0 - inset + 28.0 - width * pop, y - 1.0))
                    }))
                    .build()
            }))
            // Bottom-corner runtime + resolution readouts.
            .child(
                Text::builder()
                    .font(MONOSPACE.clone())
                    .size(12.0)
                    .weight(Weight::NORMAL)
                    .fill(p.paper.with_alpha(0.45 * life * label_in))
                    .span(TextSpan::plain("RUNTIME 7600MS · 60FPS"))
                    .anchored(Anchor::TOP_LEFT)
                    .snap_to(Vec2(inset, SCENE_SIZE.1 - inset + 20.0)),
            )
            .child(
                Text::builder()
                    .font(MONOSPACE.clone())
                    .size(12.0)
                    .weight(Weight::NORMAL)
                    .fill(p.paper.with_alpha(0.45 * life * label_in))
                    .span(TextSpan::plain("1920 × 1080 · RGBA"))
                    .anchored(Anchor::TOP_RIGHT)
                    .snap_to(Vec2(SCENE_SIZE.0 - inset, SCENE_SIZE.1 - inset + 20.0)),
            )
            .build()
    }
}
