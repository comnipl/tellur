//! Showcase plugin for the NEW timeline subsystem.
//!
//! A self-contained, media-free `tellur-live` cdylib that exercises the new
//! timeline authoring API end-to-end (`TimelineComponent` / `Timeline` /
//! `Sequence` / `Subtitle` / `Event` / `#[clock]`), so the live UI's
//! arrangement panel shows a real multi-node tree and every frame is produced
//! by the new sampling path. It mirrors the authoring patterns in
//! `.sketch/01-timeline-api.rs` (ZONE B): a `Caption` raster component, a
//! `#[component(timeline)]` with `#[clock]` whose visual parameters are
//! expressions of the clock, and a top-level `Timeline` overlaying an animated
//! backdrop, a `Sequence` of dialogue segments, and an event-driven reveal.
//!
//! Everything is DECLARATIVE: there is no imperative drawing. A time-varying
//! visual is a component tree whose parameters (opacity, color) are read off
//! `clock.local()` / `clock.global()`, and the framework re-builds the body
//! each frame, so the declarative tree animates.

use std::f32::consts::TAU;

use tellur_core::builder::{RasterEffect, VectorBuilderPlacement};
use tellur_core::color::Color;
use tellur_core::geometry::{Anchor, Vec2};
use tellur_core::layer::VectorLayer;
use tellur_core::layout::raster::Place;
use tellur_core::shapes::Rectangle;
use tellur_core::text::{Text, Weight, SANS_SERIF};
use tellur_core::time::Time;
use tellur_core::timeline_component::{
    Clock, Event, TimedBuilder, TimelineComponent, TriggersBuilder,
};
use tellur_core::timeline_container::{Sequence, Subtitle, Timeline};
use tellur_core::vector::Paint;

use tellur_renderer::rasterize::{Rasterizable, RasterizableBuilder};
use tellur_renderer::DropShadow;

// The backdrop is authored against a 16:9 logical canvas. It is resolution-
// independent: the canvas aspect matches every 16:9 target (1280x720,
// 1920x1080, …), so the rebuilt-per-frame band stack renders 1:1 with no
// distortion regardless of the live preview's actual pixel size.
const CANVAS_W: f32 = 1920.0;
const CANVAS_H: f32 = 1080.0;

// Each dialogue segment occupies a 3s slot, so the `Sequence` is 9s and the
// whole piece resolves to ~9s.
const SEGMENT_SECS: f32 = 3.0;

// ── Caption: a styled lower-third telop (Text + DropShadow) ──────────────────
//
// A `#[component(raster)]` like `.sketch/01`'s `Caption`: a bold, warm-white
// line pinned to the lower third with a soft drop shadow. RESOLUTION-AWARE: the
// Text + DropShadow are wrapped in a `Place` that FILLS the canvas and anchors
// the line's bottom-center onto a lower-third fraction (`Anchor::new(0.5,
// 0.9)`), so the telop sits in the lower third at 1280x720, 1920x1080, or any
// 16:9 target — no hardcoded pixel coordinates. The text renders at its natural
// glyph aspect because the canvas the blanket `frame` hands this visual matches
// the target's aspect (FIX 1). The `opacity` parameter is what the time-varying
// components animate — a complete `Caption::builder()` is a timeless visual
// `TimelineComponent` via the one-way `RasterComponent` blanket, so the
// per-frame opacity is baked into the returned value and memoizes one level
// down.
#[tellur_core::component(raster)]
fn Caption(#[builder(into)] line: String, #[builder(default = 1.0)] opacity: f32) -> impl RasterComponent {
    let telop = Text::builder()
        .font(SANS_SERIF.clone())
        .size(72.0)
        .weight(Weight::BOLD)
        .fill(Paint::Solid(Color { r: 0.97, g: 0.94, b: 0.88, a: opacity }))
        .span(line)
        .rasterize()
        .effect(
            DropShadow::builder()
                .offset(Vec2(0.0, 4.0))
                .blur(10.0)
                .color(Color::rgba_u8(0, 0, 0, (200.0 * opacity) as u8)),
        );
    Place::builder()
        // Snap the line's bottom-center onto a lower-third anchor of the canvas.
        .child_anchor(Anchor::new(0.5, 1.0))
        .at(Anchor::new(0.5, 0.9))
        .child(telop)
        .build()
}

// ── Backdrop: a declaratively-composed, clock-driven background ──────────────
//
// A `#[component(timeline)]` with `#[clock]`. The body declaratively composes
// a full-frame stack of horizontal color bands whose HUE and VALUE are
// expressions of `clock.global().seconds()` — a slow drift that sweeps a
// gradient down the frame and rotates the palette over the whole piece. The
// vector layer is `.rasterize()`d into a timeless visual, so the rebuilt-per-
// frame tree is what animates. `.fill()`ed into the root so it spans the piece.
#[tellur_core::component(timeline)]
fn Backdrop(#[clock] clock: Clock) -> impl TimelineComponent {
    // Global seconds drive the animation: a slow hue rotation plus a vertical
    // gradient that breathes over time.
    let secs = clock.global().seconds();
    const BANDS: usize = 24;
    const BAND_H: f32 = CANVAS_H / BANDS as f32;

    VectorLayer::builder()
        .size(Vec2(CANVAS_W, CANVAS_H))
        .children((0..BANDS).map(move |i| {
            let t = i as f32 / (BANDS - 1) as f32; // 0..1 down the frame
            // Hue rotates ~40 deg/s around a deep indigo→teal base; each band
            // is offset so the gradient visibly travels downward over time.
            let hue = (220.0 + secs * 40.0 + t * 60.0) % 360.0;
            // Value dips toward the bottom and pulses gently with time.
            let pulse = 0.5 + 0.5 * ((secs * 0.6 + t) * TAU).sin();
            let value = 0.14 + 0.20 * (1.0 - t) + 0.06 * pulse;
            Rectangle::builder()
                .size(Vec2(CANVAS_W, BAND_H + 1.0))
                .fill(Paint::Solid(Color::hsv(hue, 0.55, value)))
                .place_at(Vec2(0.0, i as f32 * BAND_H))
        }))
        .build()
        .rasterize()
}

// ── FadingCaption: a self-animated caption that eases IN and OUT ─────────────
//
// `clock.local()` is 0 at this component's resolved start; `clock.window()` (the
// length of the `.at(0.0..secs)` slot it is placed into) is its end. `envelope`
// ramps opacity 0→1 over the first 0.4s and 1→0 over the last 0.4s, so the telop
// appears and then DISAPPEARS within its slot instead of staying painted through
// later segments — the fade-out term is exactly 0 once local ≥ window (`phase`
// clamps), and the frame path additionally gates the clip off past its window.
#[tellur_core::component(timeline)]
fn FadingCaption(#[clock] clock: Clock, #[builder(into)] line: String) -> impl TimelineComponent {
    let appear = clock.envelope(0.4, 0.4);
    Caption::builder().line(line).opacity(appear.get()).build()
}

// ── Dialogue: a component composing telop + 字幕 declaratively ────────────────
//
// Overlays the self-fading lower-third caption and a `Subtitle` carrying the
// same line. Both are placed into an explicit `.at(0.0..secs)` WINDOW (NOT
// `.fill()`), so the overlay `Timeline`'s measured length IS `secs` — that is
// the segment's INTRINSIC length. Without it (an all-fill body) the segment
// would measure 0 and its inner caption/subtitle arrangement bars would
// collapse to `[offset, offset]`. With a window, the segment is `[0, secs]` and
// its inner bars span the slot. The `Dialogue` is then placed BARE in the
// `Sequence`, which gives it its own slot from this intrinsic length.
#[tellur_core::component(timeline, name = "Dialogue · {line}")]
fn Dialogue(#[builder(into)] line: String, secs: f32) -> impl TimelineComponent {
    Timeline::builder()
        .child(FadingCaption::builder().line(line.clone()).at(0.0..secs)) // テロップ
        .child(Subtitle::builder().text(line).at(0.0..secs)) // 字幕
        .build()
}

// ── Reveal: an event-driven overlay (GLOBAL axis) ───────────────────────────
//
// `Event::phase` takes the whole `&clock`, so its opacity rises over 0.5s
// starting WHEN the bound event fires — wherever segment 2 lands — with no
// chance of crossing in a `LocalTime`. `.fill()`ed so it overlays the piece.
#[tellur_core::component(timeline)]
fn Reveal(#[clock] clock: Clock, #[builder(into)] line: String, event: Event) -> impl TimelineComponent {
    let appear = event.phase(&clock, 0.0, 0.5);
    // Pin the reveal a little higher than the lower-third captions so it reads
    // as a separate "chapter" beat rather than overlapping the telop. Placed via
    // a canvas-filling `Place` anchored at an upper-third fraction, so it is
    // resolution-aware (works at any 16:9 target, no hardcoded coordinates).
    let title = Text::builder()
        .font(SANS_SERIF.clone())
        .size(96.0)
        .weight(Weight::BLACK)
        .fill(Paint::Solid(Color { r: 0.99, g: 0.82, b: 0.40, a: appear.get() }))
        .span(line)
        .rasterize()
        .effect(
            DropShadow::builder()
                .offset(Vec2(0.0, 6.0))
                .blur(16.0)
                .color(Color::rgba_u8(0, 0, 0, (210.0 * appear.get()) as u8)),
        );
    Place::builder()
        .child_anchor(Anchor::new(0.5, 0.5))
        .at(Anchor::new(0.5, 0.38))
        .child(title)
        .build()
}

/// The whole piece: an overlay [`Timeline`] of an animated backdrop, a
/// [`Sequence`] of three dialogue segments laid in a row, and an event-driven
/// reveal that fades in when the second segment starts.
pub fn build() -> impl TimelineComponent + Send {
    // The one explicit handle — a structural moment bound to segment 2's start.
    // Named so the live UI can label the reveal marker in the arrangement.
    let reveal = Event::named("reveal");

    Timeline::builder()
        // Full-frame animated background, spanning the whole piece.
        .child(Backdrop::builder().fill())
        // Three dialogue segments in a row; the Sequence re-flows if any slot
        // length changes. Each `Dialogue` carries its own intrinsic length
        // (`secs`), so it is placed BARE — the Sequence lays them end-to-end
        // from those lengths. Segment 2 fires `reveal` at its resolved start.
        .child(
            Sequence::builder()
                .child(
                    Dialogue::builder()
                        .line("A frame is just a moment, held still.")
                        .secs(SEGMENT_SECS)
                        .build(),
                )
                .child(
                    // Canonical order: trigger the complete builder. The Sequence
                    // hands the resulting `Triggered` its resolved start (3.0s),
                    // so `reveal` fires exactly when segment 2 begins.
                    Dialogue::builder()
                        .line("Give it a clock, and it begins to move.")
                        .secs(SEGMENT_SECS)
                        .trigger_at_start(reveal),
                )
                .child(
                    Dialogue::builder()
                        .line("Compose them in time — that is a timeline.")
                        .secs(SEGMENT_SECS)
                        .build(),
                )
                .build(),
        )
        // Event-driven reveal, glued to segment 2's start no matter how the
        // slots re-flow. `reveal` is used twice; `Event` is `Copy`.
        .child(Reveal::builder().line("II. Kinetic").event(reveal).fill())
        .build()
}

tellur_live::export_timeline!("main", "Timeline Showcase", build, canvas = (CANVAS_W, CANVAS_H));
