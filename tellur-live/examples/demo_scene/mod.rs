//! "Kinetic Motion" — the flagship demo scene, authored as a real timeline
//! tree: one overlay [`Timeline`](tellur_core::timeline_container::Timeline)
//! whose children are the scene's layers, bottom to top.
//!
//! - `Backdrop` (`.fill()`) — bg fill + ambient texture, cacheable once its
//!   reveal saturates.
//! - `Foreground` (`.at(0..DURATION)`, which also anchors the overlay's
//!   length) — the four vector sections (`overture`/`field`/`scan`/
//!   `resolve`) composited into ONE rasterization so a single shared pair
//!   of drop shadows wraps their combined silhouette. The sections are
//!   absolute-time canvas-world pieces (each self-gates on its own span and
//!   keys its animation to the clock it is handed), so they stay vector
//!   children here rather than becoming separately-rasterized timeline
//!   clips — that would break the shared shadows.
//! - `Hud` / `Overlay` (`.fill()`) — the persistent instrument framing and
//!   the unshadowed flash/fade pass.
//!
//! The same tree drives BOTH the per-frame render and the live panel's
//! arrangement; there is no separate display-only structure.

mod backdrop;
mod common;
mod field;
mod hud;
mod overlay;
mod overture;
mod resolve;
mod scan;

use tellur_core::builder::{RasterEffect, VectorBuilderPlacement};
use tellur_core::color::Color;
use tellur_core::geometry::Vec2;
use tellur_core::layer::VectorLayer;
use tellur_core::time::Time;
use tellur_core::timeline_component::{Clock, TimedBuilder, TimelineComponent};
use tellur_core::timeline_container::Timeline;
use tellur_renderer::{DropShadow, Rasterizable, RasterizableBuilder};

use common::{Palette, DURATION, SCENE_SIZE};
use hud::{Hud, HUD_INTRO_END, HUD_INTRO_START, HUD_OUTRO_END, HUD_OUTRO_START};

use backdrop::{Backdrop, BACKDROP_REVEAL_END, BACKDROP_REVEAL_START};
use field::Field;
use overlay::{
    Overlay, OVERLAY_BOOT_END, OVERLAY_BOOT_START, OVERLAY_FADE_START, OVERLAY_FLASH_END,
    OVERLAY_FLASH_START,
};
use overture::Overture;
use resolve::Resolve;
use scan::Scan;

// The scene palette. Constant, so it is hoisted out of the per-frame body.
const PALETTE: Palette = Palette {
    bg: Color::rgb_u8(12, 11, 24),
    paper: Color::rgb_u8(247, 240, 224),
    pink: Color::rgb_u8(255, 79, 138),
    cyan: Color::rgb_u8(73, 222, 226),
};

/// The whole piece: the timeline tree below IS the scene — the resolve pass
/// reads the arrangement from it and `frame` renders it, layer by layer.
#[tellur_core::component(timeline, name = "Kinetic Motion")]
fn KineticMotion() -> impl TimelineComponent {
    Timeline::builder()
        .child(BackdropSection::builder().fill())
        // The explicit `0..DURATION` window (not `.fill()`) ANCHORS the
        // overlay's length — every other child is a `.fill()`, and an
        // all-fill overlay has no length anchor and would collapse to 0.
        .child(Foreground::builder().at(0.0..DURATION))
        .child(HudSection::builder().fill())
        .child(OverlaySection::builder().fill())
        .build()
}

/// The four vector sections composited into ONE rasterization, wrapped by the
/// two stacked shared shadows via chained `.effect()`. Effects apply
/// inside-out: the first `.effect()` is the innermost soft paper-tinted halo
/// with no offset, the last is the outermost deeper dark drop further back.
/// `VectorLayer::render` wraps every child in an identity-transform,
/// opacity-1.0 group — a transparent passthrough — so the nested grouping
/// rasterizes identically to one flat layer while the global leaf z-order
/// (overture before field before …) is kept.
#[tellur_core::component(timeline, name = "Foreground")]
fn Foreground(#[clock] clock: Clock) -> impl TimelineComponent {
    let t = clock.local();
    VectorLayer::builder()
        .size(SCENE_SIZE)
        .child(
            Overture::builder()
                .time(t)
                .palette(PALETTE)
                .place_at(Vec2::ZERO),
        )
        .child(
            Field::builder()
                .time(t)
                .palette(PALETTE)
                .place_at(Vec2::ZERO),
        )
        .child(
            Scan::builder()
                .time(t)
                .palette(PALETTE)
                .place_at(Vec2::ZERO),
        )
        .child(
            Resolve::builder()
                .time(t)
                .palette(PALETTE)
                .place_at(Vec2::ZERO),
        )
        .rasterize()
        .effect(
            DropShadow::builder()
                // offset omitted → Vec2::ZERO (ambient halo)
                .blur(18.0)
                .color(PALETTE.paper.with_alpha(0.26)),
        )
        .effect(
            DropShadow::builder()
                .offset(Vec2(0.0, 22.0))
                .blur(26.0)
                .color(Color::rgba_u8(0, 0, 0, 170)),
        )
}

/// Bg fill + ambient texture. Shadow-free and time-stable after its reveal
/// saturates (~1.33s), so it caches as its own raster layer.
#[tellur_core::component(timeline, name = "Backdrop")]
fn BackdropSection(#[clock] clock: Clock) -> impl TimelineComponent {
    let t = clock.local();
    Backdrop::builder()
        .reveal(
            t.window(BACKDROP_REVEAL_START, BACKDROP_REVEAL_END)
                .clamped(),
        )
        .palette(PALETTE)
        .rasterize()
}

/// The persistent instrument framing. Cacheable — its Hash/Eq is driven by a
/// tiny set of inputs (palette + clamped intro window + outro phase + section
/// index), so once the intro saturates and between section-marker switches
/// the `Rasterize<Hud>` cache lookup reuses the previous raster instead of
/// re-shaping all the text and brackets.
#[tellur_core::component(timeline, name = "Hud")]
fn HudSection(#[clock] clock: Clock) -> impl TimelineComponent {
    let t = clock.local();
    Hud {
        palette: PALETTE,
        intro: t.window(HUD_INTRO_START, HUD_INTRO_END).clamped(),
        outro: t.phase(HUD_OUTRO_START, HUD_OUTRO_END),
        section: hud::section_index_at(t.seconds()),
    }
    .rasterize()
}

/// The unshadowed overlay pass: boot flash, transition flash, exit fade.
#[tellur_core::component(timeline, name = "Overlay")]
fn OverlaySection(#[clock] clock: Clock) -> impl TimelineComponent {
    let t = clock.local();
    Overlay::builder()
        .boot(t.window(OVERLAY_BOOT_START, OVERLAY_BOOT_END).clamped())
        .flash(t.window(OVERLAY_FLASH_START, OVERLAY_FLASH_END).clamped())
        .fade(t.phase(OVERLAY_FADE_START, DURATION))
        .palette(PALETTE)
        .rasterize()
}

/// The scene as a [`TimelineComponent`]. Resolve it against the [`SCENE_CANVAS`]
/// canvas (the plugin export passes `canvas = (1920, 1080)`; the mp4 encoder
/// resolves with the same canvas) so layout matches the authored SCENE_SIZE.
pub fn build_timeline() -> impl TimelineComponent + Send {
    KineticMotion::builder().build()
}

pub const TITLE: &str = "Kinetic Motion";

/// The logical canvas the scene is authored against; the resolve pass lays the
/// tree out here and the pixel target scales it. Consumed by the mp4 encoder
/// (the plugin export spells the same dimensions inline).
#[allow(dead_code)]
pub const SCENE_CANVAS: Vec2 = SCENE_SIZE;

// Consumed by `demo_timeline_mp4` but not by the plugin entry; tell the
// per-binary dead-code lint to allow it.
#[allow(dead_code)]
pub const SCENE_RESOLUTION: (u32, u32) = (1920, 1080);
