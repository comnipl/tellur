//! Unshadowed overlay pass: the pre-OVERTURE boot flash, the crisp white
//! SCAN→RESOLVE transition flash, and the gentle exit fade into the bg color.
//! Lives above the foreground shadow pass so the flashes stay crisp.
//!
//! Each of the three effects is a one-shot gate confined to a short time
//! window; for the long spans between them the overlay paints nothing. To let
//! `CachingRenderContext` reuse the (empty or saturated) raster across those
//! spans instead of re-rasterizing every frame, the component takes one
//! `Phase` per gate window rather than raw time — the same trick `Hud` and
//! `Backdrop` use. Outside a window its `Phase` is a stable 0.0 (before) or
//! 1.0 (after), so the component hashes equal frame to frame and caches.

use tellur_core::fragment::Fragment;
use tellur_core::geometry::{Anchor, Vec2};
use tellur_core::layer::VectorLayer;
use tellur_core::phase::Phase;
use tellur_core::text::Weight;

use super::common::*;

// Gate windows, each spanning a one-shot effect from its first sub-event start
// to its last sub-event end. The matching `Phase` is `time.phase(START, END)`,
// so it saturates to 1.0 once the effect is over and sits at 0.0 before it
// begins — stable, and therefore cacheable, on every frame outside the window.
pub const OVERLAY_BOOT_START: f32 = 0.05;
pub const OVERLAY_BOOT_END: f32 = 0.55;
pub const OVERLAY_FLASH_START: f32 = 4.9;
pub const OVERLAY_FLASH_END: f32 = 5.35;
pub const OVERLAY_FADE_START: f32 = 7.25;
// The fade runs to the end of the timeline, so its window end is `DURATION`.

const OVERLAY_BOOT_WIDTH: f32 = OVERLAY_BOOT_END - OVERLAY_BOOT_START;
const OVERLAY_FLASH_WIDTH: f32 = OVERLAY_FLASH_END - OVERLAY_FLASH_START;

// Sub-phase of an event spanning `[start, end]` (both absolute, but shifted
// into the window-local frame by the caller) at virtual elapsed time `t`.
// Mirrors `hud::local_phase` / `backdrop::local_phase`.
fn local_phase(virtual_t: f32, start: f32, end: f32) -> Phase {
    Phase::saturating((virtual_t - start) / (end - start))
}

#[tellur_core::component(vector)]
pub fn Overlay(boot: Phase, flash: Phase, fade: Phase, palette: Palette) -> impl VectorComponent {
    let p = palette;
    VectorLayer::builder()
        .size(SCENE_SIZE)
        // Pre-OVERTURE "boot screen" — a big monospace timecode + tiny init
        // subtitle briefly appears at center then fades, before the HUD has
        // finished assembling. Reads as a system startup flash.
        .maybe_child({
            // Virtual elapsed seconds inside the boot window. The window width
            // is exactly 0.5, so this round-trips the original `time` bit-for-
            // bit; sub-events are then expressed in window-local coordinates.
            let t = boot.get() * OVERLAY_BOOT_WIDTH;
            let boot_in = local_phase(t, 0.05 - OVERLAY_BOOT_START, 0.18 - OVERLAY_BOOT_START)
                .ease_in_out_expo()
                .get();
            let boot_out = local_phase(t, 0.32 - OVERLAY_BOOT_START, 0.55 - OVERLAY_BOOT_START)
                .ease_in_out_expo()
                .get();
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
            let t = flash.get() * OVERLAY_FLASH_WIDTH;
            let flash = local_phase(t, 4.9 - OVERLAY_FLASH_START, 5.05 - OVERLAY_FLASH_START)
                .ease_out_quint()
                .get()
                * (1.0
                    - local_phase(t, 5.05 - OVERLAY_FLASH_START, 5.35 - OVERLAY_FLASH_START)
                        .ease_in_out_expo()
                        .get());
            (flash > 0.0).then(|| {
                Rect::builder()
                    .position(Vec2::ZERO)
                    .size(SCENE_SIZE)
                    .color(alpha(p.paper, flash * 0.22))
            })
        })
        // Exit fade — gentle quint ease into the bg color. The `fade` phase
        // already spans the whole window, so it drives the ease directly.
        .maybe_child({
            let fade = fade.ease_in_out_quint().get();
            (fade > 0.0).then(|| {
                Rect::builder()
                    .position(Vec2::ZERO)
                    .size(SCENE_SIZE)
                    .color(alpha(p.bg, fade))
            })
        })
        .build()
}
