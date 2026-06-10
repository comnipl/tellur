//! "Kinetic Motion" — the flagship demo scene, composed declaratively from a
//! handful of self-contained section components.
//!
//! Each section (`backdrop`, `overture`, `field`, `scan`, `resolve`,
//! `overlay`, plus the persistent `Hud`) is a `#[component(vector)]` that
//! computes its animation state and then builds a `VectorLayer` by streaming
//! shapes into the builder.
//!
//! ## Timeline-aware structure
//!
//! `build_timeline` returns a [`TimelineComponent`] (`Scene`) instead of the
//! old closure-based `Timeline`, so the live preview's timeline panel and the
//! arrangement JSON reflect WHEN each section plays rather than seeing one
//! opaque full-duration blob.
//!
//! The visual output is kept BYTE-equivalent to the original closure: the
//! foreground's four vector sections (`overture`/`field`/`scan`/`resolve`)
//! still rasterize together under ONE shared pair of drop shadows, so
//! [`Scene::frame`] builds exactly the original `DecoratedBox { Layer { … } }`
//! tree and routes it through `ctx.render` (the same pixels the old
//! `.render(SCENE_SIZE, …)` produced, modulo caching). The section/time
//! STRUCTURE is surfaced on [`Scene::arrangement`] only, built from a parallel
//! timeline tree of thin `#[component(timeline)]` section wrappers placed on
//! their absolute windows — a display-only arrangement, mirroring how
//! `timeline_showcase`'s `Dialogue` relabels without changing its frame path.

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
use tellur_core::layer::{Layer, VectorLayer};
use tellur_core::layout::raster::DecoratedBox;
use tellur_core::placement::RasterPlacement;
use tellur_core::raster::{RasterComponent, RasterImage, Resolution};
use tellur_core::render_context::RenderContext;
use tellur_core::time::{Time, TimelineTime};
use tellur_core::timeline_component::{Arrangement, Clock, TimedBuilder, TimelineComponent};
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

// The four foreground sections' absolute time windows — exactly the spans the
// section bodies used to self-gate on via `time.during(start, end)`. They
// OVERLAP at the crossfade seams (e.g. OVERTURE 0..2.2 and FIELD 1.7..3.6 both
// paint over 1.7..2.2), so the arrangement places them in an overlay `Timeline`
// rather than an end-to-end `Sequence`. Kept here so the arrangement windows
// stay in lock-step with each section's internal animation keys.
const OVERTURE_WINDOW: (f32, f32) = (0.0, 2.2);
const FIELD_WINDOW: (f32, f32) = (1.7, 3.6);
const SCAN_WINDOW: (f32, f32) = (3.4, 5.5);
const RESOLVE_WINDOW: (f32, f32) = (4.9, DURATION);

// The scene palette. Constant, so it is hoisted out of the per-frame body.
const PALETTE: Palette = Palette {
    bg: Color::rgb_u8(12, 11, 24),
    paper: Color::rgb_u8(247, 240, 224),
    pink: Color::rgb_u8(255, 79, 138),
    cyan: Color::rgb_u8(73, 222, 226),
};

/// The whole "Kinetic Motion" piece as a single [`TimelineComponent`].
///
/// `frame` reproduces the original closure's tree verbatim (one shared-shadow
/// rasterization); `arrangement` reports the timeline-aware section structure.
#[derive(PartialEq, Eq, Hash)]
struct Scene;

impl Scene {
    /// The original per-frame foreground+backdrop+HUD+overlay tree, unchanged.
    ///
    /// `DecoratedBox` paints the bg fill and pins paint_bounds to
    /// (0,0)..SCENE_SIZE, clipping the shadows' outward spill. Inside it, a
    /// raster `Layer` stacks four full-size children placed at the origin: the
    /// (cacheable) backdrop, the doubly-shadowed vector foreground, the HUD, and
    /// the overlay.
    fn frame_tree(t: TimelineTime) -> impl RasterComponent {
        let palette = PALETTE;

        DecoratedBox::builder()
            .background(palette.bg)
            .child(
                Layer::builder()
                    .size(SCENE_SIZE)
                    // Backdrop: shadow-free and time-stable after its reveal
                    // saturates (~0.6s), so it caches as its own raster child.
                    .child(
                        Backdrop::builder()
                            .reveal(
                                t.window(BACKDROP_REVEAL_START, BACKDROP_REVEAL_END)
                                    .clamped(),
                            )
                            .palette(palette)
                            .rasterize()
                            .place_at(Vec2::ZERO),
                    )
                    // Foreground: the vector sections rasterized, then wrapped
                    // by two stacked shadows via chained `.effect()`. Effects
                    // apply inside-out, so the first `.effect()` is the
                    // innermost soft paper-tinted halo with no offset, and the
                    // last `.effect()` is the outermost deeper dark drop further
                    // back. The sections are each a full-size child of one
                    // VectorLayer; `VectorLayer::render` wraps every child in an
                    // identity-transform, opacity-1.0 group — a transparent
                    // passthrough — so the nested grouping rasterizes
                    // identically to one flat layer while the global leaf
                    // z-order (overture before field before …) is kept.
                    .child(
                        VectorLayer::builder()
                            .size(SCENE_SIZE)
                            .child(
                                Overture::builder()
                                    .time(t)
                                    .palette(palette)
                                    .place_at(Vec2::ZERO),
                            )
                            .child(
                                Field::builder()
                                    .time(t)
                                    .palette(palette)
                                    .place_at(Vec2::ZERO),
                            )
                            .child(
                                Scan::builder()
                                    .time(t)
                                    .palette(palette)
                                    .place_at(Vec2::ZERO),
                            )
                            .child(
                                Resolve::builder()
                                    .time(t)
                                    .palette(palette)
                                    .place_at(Vec2::ZERO),
                            )
                            .rasterize()
                            .effect(
                                DropShadow::builder()
                                    // offset omitted → Vec2::ZERO (ambient halo)
                                    .blur(18.0)
                                    .color(palette.paper.with_alpha(0.26)),
                            )
                            .effect(
                                DropShadow::builder()
                                    .offset(Vec2(0.0, 22.0))
                                    .blur(26.0)
                                    .color(Color::rgba_u8(0, 0, 0, 170)),
                            )
                            .place_at(Vec2::ZERO),
                    )
                    // HUD: cacheable — its Hash/Eq is driven by a tiny set of
                    // inputs (palette + two phases + section index), so once the
                    // intro saturates and between section-marker switches the
                    // `Rasterize<Hud>` cache lookup reuses the previous raster
                    // instead of re-shaping all the text and brackets.
                    .child(
                        Hud {
                            palette,
                            intro: t.window(HUD_INTRO_START, HUD_INTRO_END).clamped(),
                            outro: t.phase(HUD_OUTRO_START, HUD_OUTRO_END),
                            section: hud::section_index_at(t.seconds()),
                        }
                        .rasterize()
                        .place_at(Vec2::ZERO),
                    )
                    .child(
                        Overlay::builder()
                            .boot(t.window(OVERLAY_BOOT_START, OVERLAY_BOOT_END).clamped())
                            .flash(t.window(OVERLAY_FLASH_START, OVERLAY_FLASH_END).clamped())
                            .fade(t.phase(OVERLAY_FADE_START, DURATION))
                            .palette(palette)
                            .rasterize()
                            .place_at(Vec2::ZERO),
                    )
                    .build(),
            )
            .build()
    }
}

impl TimelineComponent for Scene {
    fn duration(&self) -> Option<f32> {
        Some(DURATION)
    }

    fn frame(
        &self,
        clock: Clock<'_>,
        canvas: Vec2,
        target: Resolution,
        ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage> {
        // Byte-equivalent to the old closure: build the same tree at the global
        // (absolute) time and route it through `ctx.render` at the SCENE_SIZE
        // canvas, exactly as `.render(SCENE_SIZE, target, ctx)` did. `canvas` is
        // resolved to SCENE_SIZE by the plugin export's `canvas = (…)`.
        let _ = canvas;
        let tree = Self::frame_tree(clock.global());
        Some(ctx.render(&tree, SCENE_SIZE, target))
    }

    fn arrangement(&self, offset: f32) -> Arrangement {
        // Display-only structure: walk a parallel timeline tree whose section
        // windows the live panel can read. The frame path above does NOT use
        // this tree (it keeps the shared-shadow single rasterization), so the
        // arrangement is purely informational, like `Dialogue`'s relabel in
        // `timeline_showcase`.
        let mut node = arrangement_tree().arrangement(offset);
        node.name = Some(TITLE.to_owned());
        node
    }
}

/// Builds the timeline tree the arrangement is read from: an overlay
/// [`Timeline`] of the persistent layers (backdrop / HUD / overlay, full-span)
/// plus the foreground's four sections placed on their absolute windows. The
/// sections overlap at the crossfades, so they share a nested foreground
/// overlay rather than forming a `Sequence`.
fn arrangement_tree() -> Timeline {
    Timeline::builder()
        // Persistent full-span layers.
        .child(BackdropSection::builder().fill())
        // Foreground: the four windowed sections under one labeled node.
        // Placed on the explicit `0..DURATION` window (not `.fill()`) so it
        // ANCHORS the root overlay's length — every other child here is a
        // `.fill()`, and an all-fill overlay has no length anchor and would
        // collapse to 0 (`.sketch/02 §5`). With this window the root resolves to
        // DURATION and the fill siblings span the whole piece.
        .child(Foreground::builder().at(0.0..DURATION))
        .child(HudSection::builder().fill())
        .child(OverlaySection::builder().fill())
        .build()
}

// The foreground as one labeled `#[component(timeline)]` whose body is the
// overlay of the four windowed sections. The macro's `arrangement` delegates to
// the inner `Timeline` (preserving its four section children) and stamps the
// "Foreground" name, so the panel shows a single "Foreground" node containing
// the sections.
#[tellur_core::component(timeline, name = "Foreground")]
fn Foreground() -> impl TimelineComponent {
    Timeline::builder()
        .child(OvertureSection::builder().at(OVERTURE_WINDOW.0..OVERTURE_WINDOW.1))
        .child(FieldSection::builder().at(FIELD_WINDOW.0..FIELD_WINDOW.1))
        .child(ScanSection::builder().at(SCAN_WINDOW.0..SCAN_WINDOW.1))
        .child(ResolveSection::builder().at(RESOLVE_WINDOW.0..RESOLVE_WINDOW.1))
        .build()
}

// Thin `#[component(timeline)]` section wrappers. Each feeds `clock.global()`
// (the absolute time the vector bodies key their animation to) into the
// existing `#[component(vector)]` section, so they render identically to the
// frame-path sections. They exist so the arrangement tree above carries a real,
// named node per section; the byte-equivalent frame path uses the raw vector
// sections directly, not these.

#[tellur_core::component(timeline, name = "Backdrop")]
fn BackdropSection(#[clock] clock: Clock) -> impl TimelineComponent {
    let t = clock.global();
    Backdrop::builder()
        .reveal(
            t.window(BACKDROP_REVEAL_START, BACKDROP_REVEAL_END)
                .clamped(),
        )
        .palette(PALETTE)
        .rasterize()
}

#[tellur_core::component(timeline, name = "Overture")]
fn OvertureSection(#[clock] clock: Clock) -> impl TimelineComponent {
    Overture::builder()
        .time(clock.global())
        .palette(PALETTE)
        .rasterize()
}

#[tellur_core::component(timeline, name = "Field")]
fn FieldSection(#[clock] clock: Clock) -> impl TimelineComponent {
    Field::builder()
        .time(clock.global())
        .palette(PALETTE)
        .rasterize()
}

#[tellur_core::component(timeline, name = "Scan")]
fn ScanSection(#[clock] clock: Clock) -> impl TimelineComponent {
    Scan::builder()
        .time(clock.global())
        .palette(PALETTE)
        .rasterize()
}

#[tellur_core::component(timeline, name = "Resolve")]
fn ResolveSection(#[clock] clock: Clock) -> impl TimelineComponent {
    Resolve::builder()
        .time(clock.global())
        .palette(PALETTE)
        .rasterize()
}

#[tellur_core::component(timeline, name = "Hud")]
fn HudSection(#[clock] clock: Clock) -> impl TimelineComponent {
    let t = clock.global();
    Hud {
        palette: PALETTE,
        intro: t.window(HUD_INTRO_START, HUD_INTRO_END).clamped(),
        outro: t.phase(HUD_OUTRO_START, HUD_OUTRO_END),
        section: hud::section_index_at(t.seconds()),
    }
    .rasterize()
}

#[tellur_core::component(timeline, name = "Overlay")]
fn OverlaySection(#[clock] clock: Clock) -> impl TimelineComponent {
    let t = clock.global();
    Overlay::builder()
        .boot(t.window(OVERLAY_BOOT_START, OVERLAY_BOOT_END).clamped())
        .flash(t.window(OVERLAY_FLASH_START, OVERLAY_FLASH_END).clamped())
        .fade(t.phase(OVERLAY_FADE_START, DURATION))
        .palette(PALETTE)
        .rasterize()
}

/// The scene as a [`TimelineComponent`]. Resolve it against the [`SCENE_CANVAS`]
/// canvas (the plugin export passes `canvas = (1920, 1080)`; the mp4 encoder
/// resolves with the same canvas) so layout matches the original
/// `.render(SCENE_SIZE, …)`.
pub fn build_timeline() -> impl TimelineComponent + Send {
    Scene
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
