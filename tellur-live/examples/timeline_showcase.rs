//! "Anatomy of a Timeline" — a short, self-referential explainer plugin for the
//! timeline subsystem.
//!
//! The piece teaches the five timeline primitives by *demonstrating* each one in
//! its own chapter, so the content of the explainer IS the mechanism rendering
//! it. A `Sequence` lays the five chapters end-to-end; each chapter is a windowed
//! overlay `Timeline` of a `#[clock]`-driven diagram, a heading, an on-screen
//! `Telop`, and a `Subtitle` sidecar. A persistent `Ruler` (a tiny live timeline
//! with a playhead) spans the whole piece, and a named `Event` fired inside the
//! "Event" chapter ripples out to make the ruler's marker pulse — a genuine
//! cross-component reaction, not a coincidence of timing.
//!
//! Everything is DECLARATIVE and media-free: every visual is a component tree of
//! vector shapes + text whose parameters are expressions of `clock.local()` /
//! `clock.global()`, rebuilt each frame.
//!
//! Chapters (a `Sequence`):
//!   01 Frame    — a single still image, held still.
//!   02 Clock    — give it a clock and it moves (an orbiting hand + live readout).
//!   03 Sequence — lay clips end-to-end.
//!   04 Event    — a playhead crosses a marker and fires the `mark` Event.
//!   05 Timeline — stack lanes in time (a mini multi-track arrangement).

use std::f32::consts::{PI, TAU};

use tellur_core::builder::{RasterEffect, VectorBuilderPlacement};
use tellur_core::color::Color;
use tellur_core::easing;
use tellur_core::geometry::{Anchor, Vec2};
use tellur_core::layer::{Layer, VectorLayer};
use tellur_core::phase::Phase;
use tellur_core::placement::raster::Positioned as RasterPositioned;
use tellur_core::placement::{Positioned, RasterPlacement};
use tellur_core::shapes::{Circle, Rectangle};
use tellur_core::text::{Text, Weight, SANS_SERIF};
use tellur_core::time::Time;
use tellur_core::timeline_component::{
    Clock, Event, TimedBuilder, TimelineComponent, TriggersBuilder,
};
use tellur_core::timeline_container::{Sequence, Subtitle, Timeline};
use tellur_core::vector::{Paint, Stroke};

use tellur_renderer::rasterize::{Rasterizable, RasterizableBuilder};
use tellur_renderer::DropShadow;

// A 16:9 logical canvas; the whole piece is authored against it and renders 1:1
// at any 16:9 target (1280x720, 1920x1080, …).
const CANVAS_W: f32 = 1920.0;
const CANVAS_H: f32 = 1080.0;
const CX: f32 = CANVAS_W * 0.5;

// Five equal chapters laid end-to-end ⇒ the whole piece is `5 * SEGMENT`.
const SEGMENT: f32 = 2.8;
const TOTAL: f32 = SEGMENT * 5.0;

// The "Event" chapter is the 4th of the five (3 chapters precede it), so it
// starts at `3*SEGMENT`; `mark` fires `EVENT_LOCAL` into it. The ruler draws —
// and pulses — its event marker at that same absolute position.
const EVENT_LOCAL: f32 = 1.35;
const EVENT_AT_GLOBAL: f32 = SEGMENT * 3.0 + EVENT_LOCAL;

// Shared persistent-ruler geometry — also read by the Clock chapter's lead line.
const RULER_X0: f32 = 190.0;
const RULER_X1: f32 = 1730.0;
const RULER_Y: f32 = 980.0;

// ── Palette: a curated dark-slate set; chosen by hand, never hue-swept ────────
const INK: Color = Color::rgb_u8(236, 240, 248); // primary text
const MUTED: Color = Color::rgb_u8(124, 136, 159); // secondary marks/labels
const AMBER: Color = Color::rgb_u8(244, 188, 96); // warm accent (events, frame)
const CYAN: Color = Color::rgb_u8(98, 206, 209); // cool accent (playheads)
const TRACK: Color = Color::rgb_u8(54, 62, 82); // track / baseline lines
const SLATE: Color = Color::rgb_u8(64, 84, 124); // neutral block fill
const TEAL: Color = Color::rgb_u8(70, 142, 144); // block fill 2
const SAND: Color = Color::rgb_u8(150, 120, 72); // block fill 3
const FRAME_BG: Color = Color::rgb_u8(24, 28, 40); // film-frame interior
const HOLE: Color = Color::rgb_u8(15, 17, 25); // sprocket holes

// ── Tiny authoring helpers ───────────────────────────────────────────────────

/// Multiplies a color's alpha (used to thread an envelope's opacity through a
/// whole shape tree).
fn fade(c: Color, a: f32) -> Color {
    c.multiply_alpha(a)
}

/// Smoothstep easing for `[0,1]` progress (eases in AND out).
fn ease(p: f32) -> f32 {
    easing::smoothstep(Phase::saturating(p)).get()
}

/// Cubic ease-out: fast start, gentle settle — a satisfying "snap into place".
fn ease_out(p: f32) -> f32 {
    easing::out_cubic(Phase::saturating(p)).get()
}

/// The per-chapter enter/exit transition, shared by every windowed element so
/// chapters cross-dissolve consistently instead of dipping to an empty frame.
///
/// Returns `(alpha, slide)`: `alpha` ramps up over the first 0.32s and down over
/// the last 0.24s of the element's window; `slide` runs `+1 → 0 → -1` (below at
/// entry, settled, above at exit), so callers can drift content upward through
/// the cut. Entry is slightly faster than exit, keeping the empty instant at the
/// boundary down to a single frame.
fn transition(clock: &Clock<'_>) -> (f32, f32) {
    let l = clock.local().seconds();
    let w = clock.window().unwrap_or(SEGMENT);
    let tin = ease((l / 0.32).clamp(0.0, 1.0));
    let tout = ease(((w - l) / 0.24).clamp(0.0, 1.0));
    (tin * tout, (1.0 - tin) - (1.0 - tout))
}

/// A filled rectangle centered on `(cx, cy)`.
fn fill_rect(cx: f32, cy: f32, w: f32, h: f32, c: Color) -> Positioned {
    Rectangle::builder()
        .size(Vec2(w, h))
        .fill(Paint::Solid(c))
        .place_at(Vec2(cx - w * 0.5, cy - h * 0.5))
}

/// A stroked (outline-only) rectangle centered on `(cx, cy)`.
fn stroke_rect(cx: f32, cy: f32, w: f32, h: f32, c: Color, width: f32) -> Positioned {
    Rectangle::builder()
        .size(Vec2(w, h))
        .stroke(Stroke {
            paint: Paint::Solid(c),
            width,
        })
        .place_at(Vec2(cx - w * 0.5, cy - h * 0.5))
}

/// A filled circle centered on `(cx, cy)`.
fn fill_circle(cx: f32, cy: f32, r: f32, c: Color) -> Positioned {
    Circle::builder()
        .radius(r)
        .fill(Paint::Solid(c))
        .place_at(Vec2(cx - r, cy - r))
}

/// A stroked (ring) circle centered on `(cx, cy)`.
fn stroke_circle(cx: f32, cy: f32, r: f32, c: Color, width: f32) -> Positioned {
    Circle::builder()
        .radius(r)
        .stroke(Stroke {
            paint: Paint::Solid(c),
            width,
        })
        .place_at(Vec2(cx - r, cy - r))
}

/// Rasterizes a canvas-sized vector layer of `shapes`, offset vertically by `dy`
/// (the transition drift), so it can be stacked under text in a raster [`Layer`].
fn shapes_layer(shapes: Vec<Positioned>, dy: f32) -> RasterPositioned {
    VectorLayer::builder()
        .size(Vec2(CANVAS_W, CANVAS_H))
        .children(shapes)
        .build()
        .rasterize()
        .place_at(Vec2(0.0, dy))
}

/// Like [`shapes_layer`] but lifts the content off the stage with a soft drop
/// shadow for depth (used by the solid-block diagrams). The shadow fades with
/// the chapter via `a`.
fn shapes_layer_lift(shapes: Vec<Positioned>, dy: f32, a: f32) -> RasterPositioned {
    VectorLayer::builder()
        .size(Vec2(CANVAS_W, CANVAS_H))
        .children(shapes)
        .build()
        .rasterize()
        .effect(
            DropShadow::builder()
                .offset(Vec2(0.0, 12.0))
                .blur(26.0)
                .color(Color::rgba_u8(0, 0, 0, (115.0 * a.clamp(0.0, 1.0)) as u8)),
        )
        .place_at(Vec2(0.0, dy))
}

/// A line of text centered on `(cx, cy)` (anchor-snapped from its glyph box).
fn text_at(s: &str, cx: f32, cy: f32, size: f32, weight: Weight, c: Color) -> RasterPositioned {
    Text::builder()
        .font(SANS_SERIF.clone())
        .size(size)
        .weight(weight)
        .fill(Paint::Solid(c))
        .span(s.to_string())
        .rasterize()
        .anchored(Anchor::CENTER)
        .snap_to(Vec2(cx, cy))
}

/// Maps an absolute timeline second onto its x position on the persistent ruler.
fn ruler_x(secs: f32) -> f32 {
    RULER_X0 + (RULER_X1 - RULER_X0) * (secs / TOTAL).clamp(0.0, 1.0)
}

/// Absolute second boundaries of the five chapters.
fn section_bounds() -> [f32; 6] {
    [
        0.0,
        SEGMENT,
        SEGMENT * 2.0,
        SEGMENT * 3.0,
        SEGMENT * 4.0,
        TOTAL,
    ]
}

// ── Backdrop: a clean, near-static slate stage (persistent) ──────────────────
//
// A vertical gradient faked with many thin bands so there is no visible banding,
// at low contrast so it reads as a calm stage rather than a test pattern. The
// value peaks as a soft spotlight over the upper-center content band and falls
// off toward the top and bottom edges; a barely-there drift keeps it alive.
// Fixed hue — NO sweep.
#[tellur_core::component(timeline, name = "Backdrop")]
fn Backdrop(#[clock] clock: Clock) -> impl TimelineComponent {
    let secs = clock.global().seconds();
    const BANDS: usize = 72;
    let band_h = CANVAS_H / BANDS as f32;
    let bands = (0..BANDS)
        .map(|i| {
            let t = i as f32 / (BANDS - 1) as f32; // 0 at top, 1 at bottom
            let spot = (-((t - 0.30).powi(2)) / 0.11).exp(); // soft vertical spotlight
            let drift = 0.008 * ((secs * 0.16 + t * 1.3) * TAU).sin();
            let value = (0.042 + 0.095 * spot + drift).max(0.0);
            fill_rect(
                CX,
                i as f32 * band_h + band_h * 0.5,
                CANVAS_W,
                band_h + 1.0,
                Color::hsv(224.0, 0.42, value),
            )
        })
        .collect();
    shapes_layer(bands, 0.0)
}

// ── Heading: the chapter number + the primitive's name, near the top ─────────
#[tellur_core::component(timeline)]
fn Heading(
    #[clock] clock: Clock,
    #[builder(into)] label: String,
    index: u32,
) -> impl TimelineComponent {
    let (a, slide) = transition(&clock);
    let dy = slide * 22.0;
    Layer::builder()
        .size(Vec2(CANVAS_W, CANVAS_H))
        .child(text_at(
            &format!("STEP {index:02} / 05"),
            CX,
            150.0 + dy,
            30.0,
            Weight::BOLD,
            fade(AMBER, a),
        ))
        .child(text_at(
            &label,
            CX,
            232.0 + dy,
            104.0,
            Weight::BLACK,
            fade(INK, a),
        ))
        .build()
}

// ── Telop: the on-screen narration line, lower third (above the ruler) ───────
#[tellur_core::component(timeline)]
fn Telop(#[clock] clock: Clock, #[builder(into)] line: String) -> impl TimelineComponent {
    let (a, slide) = transition(&clock);
    let text = Text::builder()
        .font(SANS_SERIF.clone())
        .size(50.0)
        .weight(Weight::MEDIUM)
        .fill(Paint::Solid(fade(INK, a)))
        .span(line)
        .rasterize()
        .effect(
            DropShadow::builder()
                .offset(Vec2(0.0, 3.0))
                .blur(11.0)
                .color(Color::rgba_u8(0, 0, 0, (170.0 * a) as u8)),
        )
        .anchored(Anchor::CENTER)
        .snap_to(Vec2(CX, 838.0 + slide * 16.0));
    Layer::builder()
        .size(Vec2(CANVAS_W, CANVAS_H))
        .child(text)
        .build()
}

// ── Ruler: a persistent live timeline of the whole piece (GLOBAL axis) ───────
//
// A baseline track with chapter ticks and a playhead that travels left→right as
// `clock.global()` advances; the current chapter's segment is tinted so it reads
// as "you are here". It also draws the `mark` event's marker at its absolute
// position and PULSES it when the event fires — the cross-component reaction
// that proves the event in chapter 04 reaches a different, persistent layer.
#[tellur_core::component(timeline, name = "Ruler")]
fn Ruler(#[clock] clock: Clock, event: Event) -> impl TimelineComponent {
    let g = clock.global().seconds();
    let y = RULER_Y;
    let bounds = section_bounds();
    let mut s: Vec<Positioned> = Vec::new();

    // Baseline.
    s.push(fill_rect(
        (RULER_X0 + RULER_X1) * 0.5,
        y,
        RULER_X1 - RULER_X0,
        4.0,
        TRACK,
    ));
    // Tint the section the playhead is currently inside ("you are here").
    for i in 0..bounds.len() - 1 {
        let (b0, b1) = (bounds[i], bounds[i + 1]);
        if g >= b0 && g < b1 {
            s.push(fill_rect(
                (ruler_x(b0) + ruler_x(b1)) * 0.5,
                y,
                ruler_x(b1) - ruler_x(b0),
                5.0,
                fade(CYAN, 0.45),
            ));
        }
    }
    // Section boundary ticks (intro · 5 chapters · outro).
    for &b in &bounds {
        s.push(fill_rect(ruler_x(b), y, 3.0, 22.0, MUTED));
    }

    // The event marker, and a ring that expands+fades on fire.
    let ex = ruler_x(EVENT_AT_GLOBAL);
    let ph = event.phase(&clock, 0.0, 0.6).get();
    let fired = event.is_after(&clock);
    s.push(fill_circle(
        ex,
        y,
        9.0,
        if fired {
            AMBER
        } else {
            Color::rgb_u8(92, 82, 58)
        },
    ));
    if ph > 0.0 && ph < 1.0 {
        s.push(stroke_circle(
            ex,
            y,
            9.0 + 36.0 * ease(ph),
            fade(AMBER, 1.0 - ph),
            3.0,
        ));
    }

    // The playhead.
    let px = ruler_x(g);
    s.push(fill_rect(px, y, 3.0, 30.0, CYAN));
    s.push(fill_circle(px, y - 22.0, 8.0, CYAN));

    Layer::builder()
        .size(Vec2(CANVAS_W, CANVAS_H))
        .child(shapes_layer(s, 0.0))
        .child(text_at(
            &format!("{g:>4.1}s"),
            RULER_X1 + 60.0,
            y,
            30.0,
            Weight::BOLD,
            MUTED,
        ))
        .build()
}

// ── 01 Frame: a single still image, held perfectly still ─────────────────────
//
// A "film frame" (border + sprocket holes + a tiny still picture). It only fades
// in via the transition — it never moves, which is the point of the chapter.
#[tellur_core::component(timeline, name = "FrameDiagram")]
fn FrameDiagram(#[clock] clock: Clock) -> impl TimelineComponent {
    let (a, slide) = transition(&clock);
    let dy = slide * 14.0;
    let cy = 520.0;
    // Frame body + amber border, then the "picture" (ground, horizon, low sun).
    let mut s: Vec<Positioned> = vec![
        fill_rect(CX, cy, 600.0, 360.0, fade(FRAME_BG, a)),
        stroke_rect(CX, cy, 600.0, 360.0, fade(AMBER, a), 5.0),
        fill_rect(CX, cy, 440.0, 200.0, fade(SLATE, a)),
        fill_rect(CX, cy + 24.0, 440.0, 6.0, fade(CYAN, a)),
        fill_circle(CX + 120.0, cy - 30.0, 30.0, fade(AMBER, a)),
    ];
    // Sprocket holes top + bottom, riding the border margins.
    for k in 0..6 {
        let hx = CX - 250.0 + k as f32 * 100.0;
        s.push(fill_rect(hx, cy - 152.0, 40.0, 24.0, fade(HOLE, a)));
        s.push(fill_rect(hx, cy + 152.0, 40.0, 24.0, fade(HOLE, a)));
    }
    // Corner registration brackets just outside the frame (a framing detail):
    // an L at each corner whose arms point inward.
    let (hw, hh) = (314.0, 194.0);
    for &(sx, sy) in &[(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
        let (xc, yc) = (CX + sx * hw, cy + sy * hh);
        s.push(fill_rect(
            xc - sx * 17.0,
            yc,
            34.0,
            4.0,
            fade(AMBER, a * 0.7),
        ));
        s.push(fill_rect(
            xc,
            yc - sy * 17.0,
            4.0,
            34.0,
            fade(AMBER, a * 0.7),
        ));
    }

    Layer::builder()
        .size(Vec2(CANVAS_W, CANVAS_H))
        .child(shapes_layer_lift(s, dy, a))
        .build()
}

// ── 02 Clock: one Clock, two times (local + timeline) ────────────────────────
//
// A `Clock` injected by `#[clock]` carries BOTH axes, so the face shows TWO
// hands off the same hub: a fast cyan one driven by `clock.local()` (which
// resets to 0 at this chapter's start) and a slow amber one driven by
// `clock.global()` (the absolute position on the whole 14s timeline). The two
// color-matched readouts make the relationship `timeline = local + start`
// observable — same instant, two scales.
#[tellur_core::component(timeline, name = "ClockDiagram")]
fn ClockDiagram(#[clock] clock: Clock) -> impl TimelineComponent {
    let (a, slide) = transition(&clock);
    let dy = slide * 14.0;
    let local = clock.local().seconds();
    let global = clock.global().seconds();
    let (cx, cy, r) = (CX, 466.0, 140.0);
    let mut s: Vec<Positioned> = Vec::new();

    // Face + 12 hour ticks.
    s.push(stroke_circle(cx, cy, r, fade(INK, a), 6.0));
    for k in 0..12 {
        let ang = k as f32 / 12.0 * TAU - PI * 0.5;
        s.push(fill_circle(
            cx + ang.cos() * r * 0.86,
            cy + ang.sin() * r * 0.86,
            5.0,
            fade(MUTED, a),
        ));
    }
    // A faint arc from 12 o'clock to the timeline hand, tracing how much of the
    // whole piece has elapsed (it ends exactly where the amber hand points).
    let prog = (global / TOTAL).clamp(0.0, 1.0);
    let arc_dots = (prog * 40.0).ceil() as usize;
    for k in 0..arc_dots {
        let ang = -PI * 0.5 + (k as f32 / 40.0) * TAU;
        s.push(fill_circle(
            cx + ang.cos() * r * 0.94,
            cy + ang.sin() * r * 0.94,
            3.0,
            fade(AMBER, a * 0.32),
        ));
    }

    // Two hands off the hub. The closure borrows `s`, so it lives in its own
    // block — when it ends the borrow is released and the hub can be added.
    {
        // A tapering string of dots from the hub to `(hx, hy)` + a fatter end cap.
        let mut hand = |hx: f32, hy: f32, dots: usize, base: f32, c: Color| {
            for j in 1..=dots {
                let f = j as f32 / dots as f32;
                s.push(fill_circle(
                    cx + (hx - cx) * f,
                    cy + (hy - cy) * f,
                    base - f * base * 0.45,
                    fade(c, a),
                ));
            }
            s.push(fill_circle(hx, hy, base * 1.35, fade(c, a)));
        };
        // Timeline (global) hand: slow + short, one revolution per whole piece.
        let ga = global * (TAU / TOTAL) - PI * 0.5;
        hand(
            cx + ga.cos() * r * 0.52,
            cy + ga.sin() * r * 0.52,
            5,
            8.5,
            AMBER,
        );
        // Local hand: fast + long, one revolution / 2s, drawn on top.
        let la = local * (TAU / 2.0) - PI * 0.5;
        hand(
            cx + la.cos() * r * 0.80,
            cy + la.sin() * r * 0.80,
            7,
            7.5,
            CYAN,
        );
    }
    s.push(fill_circle(cx, cy, 10.0, fade(INK, a)));

    // Lead line: tie the amber `timeline` hand to its spot on the persistent
    // ruler below (`ruler_x(global)`), ducking behind the telop. This makes the
    // "timeline = global = the ruler's playhead" correspondence explicit.
    let rx = ruler_x(global);
    let (sx, sy) = (cx, cy + r + 150.0);
    for j in 0..=9 {
        let f = j as f32 / 9.0;
        s.push(fill_circle(
            sx + (rx - sx) * f,
            sy + (RULER_Y - sy) * f,
            2.5,
            fade(AMBER, a * 0.45),
        ));
    }
    s.push(stroke_circle(rx, RULER_Y, 15.0, fade(AMBER, a * 0.75), 3.0));

    Layer::builder()
        .size(Vec2(CANVAS_W, CANVAS_H))
        .child(shapes_layer(s, dy))
        .child(text_at(
            &format!("local   {local:.1}s"),
            cx,
            cy + r + 68.0 + dy,
            40.0,
            Weight::BOLD,
            fade(CYAN, a),
        ))
        .child(text_at(
            &format!("timeline   {global:.1}s"),
            cx,
            cy + r + 116.0 + dy,
            40.0,
            Weight::BOLD,
            fade(AMBER, a),
        ))
        .build()
}

// ── 03 Sequence: lay clips end-to-end ────────────────────────────────────────
//
// Three labeled blocks slide in from the left, staggered, and snap (ease-out)
// into an end-to-end row over a baseline — exactly what a `Sequence` does to its
// children.
#[tellur_core::component(timeline, name = "SequenceDiagram")]
fn SequenceDiagram(#[clock] clock: Clock) -> impl TimelineComponent {
    let (a, slide) = transition(&clock);
    let dy = slide * 14.0;
    let l = clock.local().seconds();
    let (cy, bw, bh, gap) = (520.0, 270.0, 124.0, 14.0);
    let row_w = 3.0 * bw + 2.0 * gap;
    let left = CX - row_w * 0.5; // left edge of block A
    let labels = ["A", "B", "C"];
    let colors = [SLATE, TEAL, SAND];

    let mut shapes: Vec<Positioned> = Vec::new();
    let mut letters: Vec<RasterPositioned> = Vec::new();
    // Baseline grows in from the left as the blocks land on it.
    let base_w = row_w * ease_out(l / 1.7);
    shapes.push(fill_rect(
        left + base_w * 0.5,
        cy + bh * 0.5 + 24.0,
        base_w,
        4.0,
        fade(TRACK, a),
    ));
    for i in 0..3 {
        let slot_x = left + bw * 0.5 + i as f32 * (bw + gap);
        let local = l - i as f32 * 0.5; // staggered entry
        let e = ease_out(local / 0.55);
        let from_x = slot_x - 440.0; // slide in from the left
        let cx = from_x + (slot_x - from_x) * e;
        let alpha = a * (local / 0.22).clamp(0.0, 1.0);
        shapes.push(fill_rect(cx, cy, bw, bh, fade(colors[i], alpha)));
        shapes.push(stroke_rect(cx, cy, bw, bh, fade(INK, alpha * 0.7), 2.0));
        letters.push(text_at(
            labels[i],
            cx,
            cy + dy,
            58.0,
            Weight::BLACK,
            fade(INK, alpha),
        ));
    }

    let mut layer = Layer::builder()
        .size(Vec2(CANVAS_W, CANVAS_H))
        .child(shapes_layer_lift(shapes, dy, a));
    for letter in letters {
        layer = layer.child(letter);
    }
    layer.build()
}

// ── 04 Event: mark a moment, and watch it fire ───────────────────────────────
//
// A playhead crosses a track at a constant rate; the marker sits exactly where
// the playhead lands at `EVENT_LOCAL`. The ring is driven by `event.phase`, so
// the in-chapter pulse and the ruler's pulse are the SAME event — the chapter
// fires `mark` (see `build`), and both this diagram and the persistent ruler
// react to it.
#[tellur_core::component(timeline, name = "EventDiagram")]
fn EventDiagram(#[clock] clock: Clock, event: Event) -> impl TimelineComponent {
    let (a, slide) = transition(&clock);
    let dy = slide * 14.0;
    let l = clock.local().seconds();
    let (x0, x1, ty) = (540.0, 1380.0, 510.0);
    let span = x1 - x0;
    // The playhead sweeps the track linearly over [0.2, SEGMENT-0.2]; place the
    // marker at the fraction the playhead occupies at EVENT_LOCAL so they meet.
    let sweep = SEGMENT - 0.4;
    let mx = x0 + span * ((EVENT_LOCAL - 0.2) / sweep).clamp(0.0, 1.0);
    let px = x0 + span * ((l - 0.2) / sweep).clamp(0.0, 1.0);

    let ph = event.phase(&clock, 0.0, 0.55).get();
    let appear = event.phase(&clock, 0.0, 0.4).get(); // 0→1 once fired
    let fired = event.is_after(&clock);
    let mut s: Vec<Positioned> = Vec::new();

    s.push(fill_rect((x0 + x1) * 0.5, ty, span, 5.0, fade(TRACK, a)));
    // Marker (lit amber once fired) + expanding ring on fire.
    s.push(fill_circle(
        mx,
        ty,
        14.0,
        fade(if fired { AMBER } else { MUTED }, a),
    ));
    if ph > 0.0 && ph < 1.0 {
        s.push(stroke_circle(
            mx,
            ty,
            14.0 + 80.0 * ease(ph),
            fade(AMBER, a * (1.0 - ph)),
            4.0,
        ));
    }
    // Playhead: a tall thin bar with a knob.
    s.push(fill_rect(px, ty, 4.0, 150.0, fade(CYAN, a)));
    s.push(fill_circle(px, ty - 86.0, 10.0, fade(CYAN, a)));

    Layer::builder()
        .size(Vec2(CANVAS_W, CANVAS_H))
        .child(shapes_layer(s, dy))
        // "event" labels the marker; on fire the SAME twin-axis readout as the
        // Clock chapter fades in — an Event is one instant, named on both axes
        // (`timeline = local + the chapter's start`).
        .child(text_at(
            "event",
            mx,
            ty + 96.0 + dy,
            30.0,
            Weight::BOLD,
            fade(if fired { AMBER } else { MUTED }, a),
        ))
        .child(text_at(
            &format!("local   {EVENT_LOCAL:.2}s"),
            mx,
            ty + 142.0 + dy,
            32.0,
            Weight::BOLD,
            fade(CYAN, a * appear),
        ))
        .child(text_at(
            &format!("timeline   {EVENT_AT_GLOBAL:.2}s"),
            mx,
            ty + 184.0 + dy,
            32.0,
            Weight::BOLD,
            fade(AMBER, a * appear),
        ))
        .build()
}

// ── 05 Timeline: stack lanes in time ─────────────────────────────────────────
//
// Three lanes (a mini multi-track arrangement) wipe their clips in from the left,
// staggered by lane, with a single playhead crossing all of them — what a
// top-level `Timeline` overlay looks like once everything is composed.
#[tellur_core::component(timeline, name = "TimelineDiagram")]
fn TimelineDiagram(#[clock] clock: Clock) -> impl TimelineComponent {
    let (a, slide) = transition(&clock);
    let dy = slide * 14.0;
    let l = clock.local().seconds();
    let (lane_x, lane_w, lane_h, gap, top) = (560.0, 820.0, 70.0, 26.0, 440.0);
    let names = ["video", "audio", "title"];
    // (start_fraction, width_fraction) clips per lane.
    let lanes: [&[(f32, f32)]; 3] = [
        &[(0.0, 0.5), (0.54, 0.46)],
        &[(0.0, 1.0)],
        &[(0.12, 0.3), (0.6, 0.28)],
    ];
    let colors = [SLATE, TEAL, SAND];

    // The playhead sweeps at a constant rate; clips light up as it crosses them.
    let px = lane_x + lane_w * ((l - 0.3) / (SEGMENT - 0.6)).clamp(0.0, 1.0);
    let mut shapes: Vec<Positioned> = Vec::new();
    let mut labels: Vec<RasterPositioned> = Vec::new();
    for (li, clips) in lanes.iter().enumerate() {
        let ly = top + li as f32 * (lane_h + gap);
        shapes.push(fill_rect(
            lane_x + lane_w * 0.5,
            ly,
            lane_w,
            3.0,
            fade(TRACK, a * 0.7),
        ));
        labels.push(text_at(
            names[li],
            lane_x - 86.0,
            ly + dy,
            26.0,
            Weight::BOLD,
            fade(MUTED, a),
        ));
        let e = ease_out((l - li as f32 * 0.3) / 0.5); // wipe progress
        for (sf, wf) in clips.iter() {
            let full = (lane_w * wf - 6.0).max(0.0);
            let w_now = full * e;
            let x_left = lane_x + lane_w * sf;
            shapes.push(fill_rect(
                x_left + w_now * 0.5,
                ly,
                w_now,
                lane_h * 0.72,
                fade(colors[li], a),
            ));
            if w_now > 6.0 && px >= x_left && px <= x_left + w_now {
                shapes.push(stroke_rect(
                    x_left + w_now * 0.5,
                    ly,
                    w_now,
                    lane_h * 0.72,
                    fade(CYAN, a),
                    2.5,
                ));
            }
        }
    }
    // A playhead spanning all three lanes.
    let mid = top + (lane_h + gap);
    let height = 2.0 * (lane_h + gap) + 80.0;
    shapes.push(fill_rect(px, mid, 4.0, height, fade(CYAN, a)));

    let mut layer = Layer::builder()
        .size(Vec2(CANVAS_W, CANVAS_H))
        .child(shapes_layer_lift(shapes, dy, a));
    for label in labels {
        layer = layer.child(label);
    }
    layer.build()
}

// ── Chapters: each composes a heading + diagram + telop + subtitle, windowed ──
//
// The children are placed on an explicit `.at(0.0..SEGMENT)` WINDOW (not
// `.fill()`), so the chapter's overlay `Timeline` MEASURES `SEGMENT` — its
// intrinsic length. Each chapter is then placed BARE in the `Sequence`, which
// lays them end-to-end from those lengths. The `name` surfaces on the live UI's
// arrangement panel.

#[tellur_core::component(timeline, name = "01 · Frame")]
fn FrameChapter() -> impl TimelineComponent {
    const LINE: &str = "A frame is a single still image.";
    Timeline::builder()
        .child(Heading::builder().label("Frame").index(1).at(0.0..SEGMENT))
        .child(FrameDiagram::builder().at(0.0..SEGMENT))
        .child(Telop::builder().line(LINE).at(0.0..SEGMENT))
        .child(Subtitle::builder().text(LINE).at(0.0..SEGMENT))
        .build()
}

#[tellur_core::component(timeline, name = "02 · Clock")]
fn ClockChapter() -> impl TimelineComponent {
    const LINE: &str = "A clock carries two times: local and timeline.";
    Timeline::builder()
        .child(Heading::builder().label("Clock").index(2).at(0.0..SEGMENT))
        .child(ClockDiagram::builder().at(0.0..SEGMENT))
        .child(Telop::builder().line(LINE).at(0.0..SEGMENT))
        .child(Subtitle::builder().text(LINE).at(0.0..SEGMENT))
        .build()
}

#[tellur_core::component(timeline, name = "03 · Sequence")]
fn SequenceChapter() -> impl TimelineComponent {
    const LINE: &str = "Lay clips end to end — that's a Sequence.";
    Timeline::builder()
        .child(
            Heading::builder()
                .label("Sequence")
                .index(3)
                .at(0.0..SEGMENT),
        )
        .child(SequenceDiagram::builder().at(0.0..SEGMENT))
        .child(Telop::builder().line(LINE).at(0.0..SEGMENT))
        .child(Subtitle::builder().text(LINE).at(0.0..SEGMENT))
        .build()
}

#[tellur_core::component(timeline, name = "04 · Event")]
fn EventChapter(event: Event) -> impl TimelineComponent {
    const LINE: &str = "Mark a moment in time — that's an Event.";
    Timeline::builder()
        .child(Heading::builder().label("Event").index(4).at(0.0..SEGMENT))
        .child(EventDiagram::builder().event(event).at(0.0..SEGMENT))
        .child(Telop::builder().line(LINE).at(0.0..SEGMENT))
        .child(Subtitle::builder().text(LINE).at(0.0..SEGMENT))
        .build()
}

#[tellur_core::component(timeline, name = "05 · Timeline")]
fn TimelineChapter() -> impl TimelineComponent {
    const LINE: &str = "Stack them in layers — that's a Timeline.";
    Timeline::builder()
        .child(
            Heading::builder()
                .label("Timeline")
                .index(5)
                .at(0.0..SEGMENT),
        )
        .child(TimelineDiagram::builder().at(0.0..SEGMENT))
        .child(Telop::builder().line(LINE).at(0.0..SEGMENT))
        .child(Subtitle::builder().text(LINE).at(0.0..SEGMENT))
        .build()
}

/// The whole piece: a persistent backdrop + ruler overlaid with a `Sequence` of
/// the five chapters. The `mark` event is fired inside the Event chapter (via
/// `trigger_at`) and read by both that chapter's diagram and the persistent
/// ruler, so firing it ripples across components.
pub fn build() -> impl TimelineComponent + Send {
    let mark = Event::named("mark");

    Timeline::builder()
        .child(Backdrop::builder().fill())
        .child(Ruler::builder().event(mark).fill())
        .child(
            Sequence::builder()
                .child(FrameChapter::builder().build())
                .child(ClockChapter::builder().build())
                .child(SequenceChapter::builder().build())
                // Fire `mark` partway into the Event chapter; the Sequence hands
                // the chapter its resolved start, so `mark` lands at
                // `EVENT_AT_GLOBAL` on the global axis.
                .child(
                    EventChapter::builder()
                        .event(mark)
                        .trigger_at(EVENT_LOCAL, mark),
                )
                .child(TimelineChapter::builder().build())
                .build(),
        )
        .build()
}

tellur_live::export_timeline!(
    "main",
    "Anatomy of a Timeline",
    build,
    canvas = (CANVAS_W, CANVAS_H)
);
