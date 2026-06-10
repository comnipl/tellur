//! Unshadowed overlay pass: the pre-OVERTURE boot flash, the crisp white
//! SCAN→RESOLVE transition flash, and the gentle exit fade into the bg color.
//! Lives above the foreground shadow pass so the flashes stay crisp.
//!
//! Each of the three effects is a one-shot gate confined to a short time
//! window; for the long spans between them the overlay paints nothing. To let
//! `CachingRenderContext` reuse the (empty or saturated) raster across those
//! spans instead of re-rasterizing every frame, the component takes a clamped
//! `Window` per gate rather than raw time — the same trick `Hud` and
//! `Backdrop` use. Outside a gate the clamped snapshot is frozen at the gate's
//! start or end, so the component hashes equal frame to frame and caches.

use tellur_core::fragment::Fragment;
use tellur_core::geometry::{Anchor, Vec2};
use tellur_core::layer::VectorLayer;
use tellur_core::phase::Phase;
use tellur_core::text::Weight;
use tellur_core::window::Window;

use super::common::*;

// Gate windows, each spanning a one-shot effect from its first sub-event start
// to its last sub-event end. The matching value is `time.window(START, END)`
// clamped, so it freezes once the effect is over and before it begins —
// stable, and therefore cacheable, on every frame outside the window.
pub const OVERLAY_BOOT_START: f32 = 0.05;
pub const OVERLAY_BOOT_END: f32 = 0.55;
pub const OVERLAY_FLASH_START: f32 = 4.9;
pub const OVERLAY_FLASH_END: f32 = 5.35;
pub const OVERLAY_FADE_START: f32 = 7.25;
// The fade runs to the end of the timeline, so its window end is `DURATION`.

#[tellur_core::component(vector)]
pub fn Overlay(boot: Window, flash: Window, fade: Phase, palette: Palette) -> impl VectorComponent {
    let p = palette;
    VectorLayer::builder()
        .size(SCENE_SIZE)
        // Pre-OVERTURE "boot screen" — a big monospace timecode + tiny init
        // subtitle briefly appears at center then fades, before the HUD has
        // finished assembling. Reads as a system startup flash.
        .maybe_child({
            // Sub-events are addressed in window-local seconds via
            // `boot.sub_secs(...)`, so the original absolute starts are shifted
            // into the window's frame here.
            let boot_in = boot
                .sub_secs((0.05 - OVERLAY_BOOT_START)..(0.18 - OVERLAY_BOOT_START))
                .ease_in_out_expo(0.0, 1.0);
            let boot_out = boot
                .sub_secs((0.32 - OVERLAY_BOOT_START)..(0.55 - OVERLAY_BOOT_START))
                .ease_in_out_expo(1.0, 0.0);
            let boot_life = boot_in * boot_out;
            (boot_life > 0.0).then(|| {
                Fragment::builder()
                    .child(
                        Text::builder()
                            .font(MONOSPACE.clone())
                            .size(42.0)
                            .weight(Weight::BOLD)
                            .fill(p.paper.with_alpha(boot_life * 0.95))
                            .span(TextSpan::plain("TELLUR"))
                            .anchored(Anchor::BOTTOM_CENTER)
                            .snap_to(Vec2(CX, CY - 18.0)),
                    )
                    .child(
                        Text::builder()
                            .font(MONOSPACE.clone())
                            .size(13.0)
                            .weight(Weight::NORMAL)
                            .fill(p.paper.with_alpha(boot_life * 0.7))
                            .span(TextSpan::plain("00:00:00.000 · INIT"))
                            .anchored(Anchor::TOP_CENTER)
                            .snap_to(Vec2(CX, CY + 4.0)),
                    )
                    // A tiny pink underline dash to the right of "INIT".
                    .child(
                        Rectangle::builder()
                            .size(Vec2(20.0 * boot_in, 2.0))
                            .fill(p.pink.with_alpha(boot_life))
                            .place_at(Vec2(CX + 88.0, CY + 18.0)),
                    )
                    .build()
            })
        })
        // Crisp white flash at the SCAN → RESOLVE transition. Lives in the
        // unshadowed overlay so it doesn't smear into a grey haze through the
        // foreground shadow pass.
        .maybe_child({
            let flash_in = flash
                .sub_secs((4.9 - OVERLAY_FLASH_START)..(5.05 - OVERLAY_FLASH_START))
                .ease_out_quint(0.0, 1.0);
            let flash_out = flash
                .sub_secs((5.05 - OVERLAY_FLASH_START)..(5.35 - OVERLAY_FLASH_START))
                .ease_in_out_expo(1.0, 0.0);
            let flash = flash_in * flash_out;
            (flash > 0.0).then(|| {
                Rectangle::builder()
                    .size(SCENE_SIZE)
                    .fill(p.paper.with_alpha(flash * 0.22))
                    .place_at(Vec2::ZERO)
            })
        })
        // Exit fade — gentle quint ease into the bg color. The `fade` phase
        // already spans the whole window, so it drives the ease directly.
        .maybe_child({
            let fade = fade.ease_in_out_quint(0.0, 1.0);
            (fade > 0.0).then(|| {
                Rectangle::builder()
                    .size(SCENE_SIZE)
                    .fill(p.bg.with_alpha(fade))
                    .place_at(Vec2::ZERO)
            })
        })
        .build()
}
