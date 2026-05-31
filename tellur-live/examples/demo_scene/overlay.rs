//! Unshadowed overlay pass: the pre-OVERTURE boot flash, the crisp white
//! SCAN→RESOLVE transition flash, and the gentle exit fade into the bg color.
//! Lives above the foreground shadow pass so the flashes stay crisp.

use tellur_core::fragment::Fragment;
use tellur_core::geometry::{Anchor, Vec2};
use tellur_core::layer::VectorLayer;
use tellur_core::text::Weight;
use tellur_core::time::{Time, TimelineTime};

use super::common::*;

#[tellur_core::component(vector)]
pub fn Overlay(time: TimelineTime, palette: Palette) -> impl VectorComponent {
    let p = palette;
    VectorLayer::builder()
        .size(SCENE_SIZE)
        // Pre-OVERTURE "boot screen" — a big monospace timecode + tiny init
        // subtitle briefly appears at center then fades, before the HUD has
        // finished assembling. Reads as a system startup flash.
        .maybe_child({
            let boot_in = ease_in_out_expo(time.phase(0.05, 0.18));
            let boot_out = ease_in_out_expo(time.phase(0.32, 0.55));
            let boot_life = (boot_in * (1.0 - boot_out)).clamp(0.0, 1.0);
            (boot_life > 0.0).then(|| {
                Fragment::builder()
                    .child(
                        Label::builder()
                            .position(Vec2(CX, CY - 18.0))
                            .anchor(Anchor::BOTTOM_CENTER)
                            .text("TELLUR")
                            .size(42.0)
                            .color(alpha(p.paper, boot_life * 0.95))
                            .weight(Weight::BOLD),
                    )
                    .child(
                        Label::builder()
                            .position(Vec2(CX, CY + 4.0))
                            .anchor(Anchor::TOP_CENTER)
                            .text("00:00:00.000 · INIT")
                            .size(13.0)
                            .color(alpha(p.paper, boot_life * 0.7))
                            .weight(Weight::NORMAL),
                    )
                    // A tiny pink underline dash to the right of "INIT".
                    .child(
                        Rect::builder()
                            .position(Vec2(CX + 88.0, CY + 18.0))
                            .size(Vec2(20.0 * boot_in, 2.0))
                            .color(alpha(p.pink, boot_life)),
                    )
                    .build()
            })
        })
        // Crisp white flash at the SCAN → RESOLVE transition. Lives in the
        // unshadowed overlay so it doesn't smear into a grey haze through the
        // foreground shadow pass.
        .maybe_child({
            let flash = ease_out_quint(time.phase(4.9, 5.05))
                * (1.0 - ease_in_out_expo(time.phase(5.05, 5.35)));
            (flash > 0.0).then(|| {
                Rect::builder()
                    .position(Vec2::ZERO)
                    .size(SCENE_SIZE)
                    .color(alpha(p.paper, flash * 0.22))
            })
        })
        // Exit fade — gentle quint ease into the bg color.
        .maybe_child({
            let fade = ease_in_out_quint(time.phase(7.25, DURATION));
            (fade > 0.0).then(|| {
                Rect::builder()
                    .position(Vec2::ZERO)
                    .size(SCENE_SIZE)
                    .color(alpha(p.bg, fade))
            })
        })
        .build()
}
