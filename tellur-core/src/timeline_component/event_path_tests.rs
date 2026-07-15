// ── Step 6: the Event path, end-to-end (`.sketch/01` B.1b/B.2) ───────────────
//
// These integration tests build the `.sketch/01` shape — a `Sequence` of
// `Dialogue`s whose line-2 fires an `Event` at its RESOLVED start, plus an
// overlay `Reveal` that animates off that event — and prove the WHOLE path:
// resolve glues the trigger to the resolved start, re-flow re-glues it, and the
// per-frame `Clock` reads the resolved table so `event.phase(&clock)` ramps the
// reveal's opacity. Media decode is stubbed, so voice lengths are injected on
// `AudioFile::duration`.
use super::*;
use crate::geometry::{Constraints, Vec2};
use crate::phase::Phase;
use crate::raster::{PixelFormat, RasterComponent, RasterImage, RasterResidency, Resolution};
use crate::render_context::{PassThrough, RenderContext};
use crate::time::{LocalTime, Time, TimelineTime};
use crate::timeline_container::{AudioFile, Sequence, Timeline};

// A test visual whose RGBA alpha bakes a `[0, 1]` opacity into a recoverable
// byte (`round(opacity * 255)`). It fills the whole `target` so the test can
// read the opacity back out of any pixel after compositing. A timeless
// `RasterComponent`, so it reaches the timeline world through the one-way
// blanket and renders via `ctx.render`.
#[derive(PartialEq, Hash)]
struct OpacityProbe {
    // The opacity quantized to a byte, so it survives `PartialEq`/`Hash`
    // (an `f32` would need bit-twiddling; the byte is exact and recoverable).
    alpha: u8,
}

impl OpacityProbe {
    fn new(opacity: f32) -> Self {
        Self {
            alpha: (opacity.clamp(0.0, 1.0) * 255.0).round() as u8,
        }
    }
}

impl RasterComponent for OpacityProbe {
    fn layout(&self, _c: Constraints) -> Vec2 {
        Vec2(1.0, 1.0)
    }
    fn render(
        &self,
        _s: Vec2,
        t: Resolution,
        _residency: RasterResidency,
        _ctx: &mut dyn RenderContext,
    ) -> RasterImage {
        let count = (t.width as usize) * (t.height as usize);
        // Opaque-white RGB so a source-over composite leaves the alpha
        // recoverable; the alpha channel carries the baked opacity.
        let mut pixels = Vec::with_capacity(count * 4);
        for _ in 0..count {
            pixels.extend_from_slice(&[255, 255, 255, self.alpha]);
        }
        RasterImage::cpu(t.width, t.height, PixelFormat::Rgba8, pixels)
    }
}

// The `.sketch/01` `Reveal`: an event-driven caption whose opacity ramps
// 0→1 over the 0.5s after its event fires. The body bakes the per-frame
// phase into the visual's alpha so a frame sample can recover it.
#[crate::component(timeline)]
fn Reveal(
    #[clock] clock: Clock,
    #[builder(into)] line: String,
    event: Event,
) -> impl TimelineComponent {
    let _ = line; // carried for parity with `.sketch/01`; not asserted on.
    let appear = event.phase(&clock, 0.0, 0.5);
    OpacityProbe::new(appear.get())
}

// The `.sketch/01` `Dialogue`: a `Timeline` overlay whose voice (a stub
// `AudioFile` with an injected length) sizes it. Simplified to just the
// voice — the telop/字幕 fills are exercised in `timeline_container`'s
// tests; here the voice length is what re-flows the `Sequence`.
#[crate::component(timeline)]
fn Dialogue(#[builder(into)] voice: AudioFile) -> impl TimelineComponent {
    Timeline::builder().child(voice).build()
}

fn voice(seconds: f32) -> AudioFile {
    AudioFile::builder()
        .path("media/vo.wav")
        .duration(seconds)
        .build()
}

// The whole `.sketch/01` B.2 shape, parameterized on the line-1 voice length
// (the upstream length that re-flows line-2's start), with the reveal event
// threaded in so the test can read its resolved trigger time.
fn build_piece(reveal: Event, line1: f32, line2: f32, line3: f32) -> impl TimelineComponent {
    Timeline::builder()
        .child(
            Sequence::builder()
                .child(Dialogue::builder().voice(voice(line1)).build())
                .child(
                    Dialogue::builder()
                        .voice(voice(line2))
                        .build()
                        .trigger_at_start(reveal),
                )
                .child(Dialogue::builder().voice(voice(line3)).build())
                .build(),
        )
        .child(Reveal::builder().line("Chapter II").event(reveal).fill())
        .build()
}

fn alpha_at(resolved: &ResolvedTimeline, t: f32) -> u8 {
    let mut ctx = PassThrough;
    let frame = resolved
        .frame(
            TimelineTime::new(t),
            Resolution::new(2, 2),
            RasterResidency::Cpu,
            &mut ctx,
        )
        .expect("the reveal always contributes a frame");
    frame.as_cpu().expect("cpu image").pixels[3]
}

// (a) The trigger is glued to line-2's RESOLVED start (= the line-1 voice
// length), recorded in the resolved table after the place walk.
#[test]
fn trigger_glued_to_resolved_start() {
    let reveal = Event::new();
    let resolved =
        resolve(build_piece(reveal, 3.0, 4.0, 5.0)).expect("voices give the piece a length");
    // Line 2 starts at the line-1 voice length (3.0); the reveal fires there.
    assert_eq!(resolved.triggers().get(reveal.id()).seconds(), 3.0);
    // The whole piece is 3 + 4 + 5 = 12s (the Sequence sums the voices).
    assert_eq!(resolved.duration(), 12.0);
}

// (b) RE-FLOW gluing: lengthen line-1's voice and re-resolve; the trigger
// moves to the new line-2 start — the animation stays glued (`.sketch/01`
// B.2: "glued to line 2's start no matter how long line 1's voice is").
#[test]
fn reflow_reglues_trigger_to_new_start() {
    let reveal = Event::new();

    let resolved = resolve(build_piece(reveal, 3.0, 4.0, 5.0)).expect("not timeless");
    assert_eq!(resolved.triggers().get(reveal.id()).seconds(), 3.0);

    // Re-resolve with a LONGER line-1 voice: the trigger re-glues to 5.0.
    let reglued = resolve(build_piece(reveal, 5.0, 4.0, 5.0)).expect("not timeless");
    assert_eq!(reglued.triggers().get(reveal.id()).seconds(), 5.0);

    // And a SHORTER line-1 voice pulls it earlier.
    let earlier = resolve(build_piece(reveal, 1.5, 4.0, 5.0)).expect("not timeless");
    assert_eq!(earlier.triggers().get(reveal.id()).seconds(), 1.5);
}

// (c) Per-frame phase: sample `frame` just before / during / after the
// trigger and recover the reveal's opacity from the baked alpha. This proves
// `event.phase(&clock)` reads the resolved table through the real per-frame
// clock (the trigger lands at 3.0; the ramp spans [3.0, 3.5]).
#[test]
fn per_frame_phase_ramps_off_the_resolved_trigger() {
    let reveal = Event::new();
    let resolved = resolve(build_piece(reveal, 3.0, 4.0, 5.0)).expect("not timeless");
    assert_eq!(resolved.triggers().get(reveal.id()).seconds(), 3.0);

    // Before the trigger: opacity 0.
    assert_eq!(alpha_at(&resolved, 2.9), 0, "before the trigger ⇒ 0");
    // At the trigger boundary: still 0 (phase start).
    assert_eq!(alpha_at(&resolved, 3.0), 0, "at the trigger ⇒ phase 0");
    // Halfway through the 0.5s ramp (t = 3.25): ~0.5 ⇒ alpha ~128.
    let mid = alpha_at(&resolved, 3.25);
    assert!(
        (120..=136).contains(&mid),
        "mid-ramp opacity ~0.5, got {mid}"
    );
    // Past the ramp: fully opaque (1.0 ⇒ 255).
    assert_eq!(alpha_at(&resolved, 3.5), 255, "ramp end ⇒ 1.0");
    assert_eq!(alpha_at(&resolved, 9.0), 255, "well after ⇒ stays 1.0");
}

// (d) An UNFIRED event: a `Reveal` whose event is never triggered stays at
// opacity 0 at every t (the `+∞` short-circuit), with no NaN reaching the
// baked alpha (`.sketch/02 §11`).
#[test]
fn unfired_event_stays_zero_with_no_nan() {
    // A reveal driven by an event nothing triggers, given a length by a
    // bare windowed visual sibling so the root is not timeless.
    let never = Event::new();
    let root = Timeline::builder()
        .child(OpacityProbe { alpha: 0 }.at(0.0..6.0))
        .child(Reveal::builder().line("nope").event(never).fill())
        .build();
    let resolved = resolve(root).expect("the windowed sibling gives a length");

    // The event has no recorded time ⇒ reads as +∞.
    assert!(resolved.triggers().get(never.id()).seconds().is_infinite());

    for &t in &[0.0_f32, 1.0, 3.0, 5.9] {
        let a = alpha_at(&resolved, t);
        assert_eq!(a, 0, "unfired event ⇒ opacity 0 at t={t}");
    }

    // And the phase itself is exactly Phase::ZERO (no NaN) at an arbitrary t.
    let table = TriggerTable::new();
    let clock = Clock::new(TimelineTime::new(4.0), LocalTime::new(4.0), &table);
    let p = never.phase(&clock, 0.0, 0.5);
    assert_eq!(p, Phase::ZERO);
    assert!(!p.get().is_nan());
}

// (e) Structural-clock soundness: the clock-less queries on a `#[clock]`
// component build with `Clock::structural()` (an empty trigger table). They
// must not panic and must yield a sensible timeless node — the empty table
// makes `event.phase` read +∞ ⇒ Phase::ZERO (no NaN), and the baked opacity
// is 0, so the structural visual is well-formed.
#[test]
fn structural_clock_queries_do_not_panic() {
    let reveal = Reveal::builder()
        .line("Chapter II")
        .event(Event::new())
        .build();

    // Clock-less queries (built with the structural clock) are timeless and
    // never panic / NaN.
    assert_eq!(reveal.duration(), None, "the baked visual is timeless");
    assert_eq!(reveal.measure(), None);
    assert_eq!(reveal.arrangement(0.0).kind, NodeKind::Video);
    assert!(reveal.cues(0.0).is_empty());

    // `resolve` on the wrapper recurses into the structural visual without
    // recording any trigger (the structural clock's table is empty) and
    // reports a timeless `0.0` length.
    let mut ctx = ResolveCtx::new();
    let len = reveal.resolve(0.0, &mut ctx);
    assert_eq!(len, 0.0);
}

// ── Trigger-at-resolved-start SEMANTICS + ordering (scope item 3) ─────────

// The CANONICAL usage: a `Sequence`-positioned child carrying
// `.trigger_at_start(e)` records `e` at the child's resolved start (the
// container hands the wrapper its `abs_start`). Already holds — asserted on
// the line-2 boundary above; here directly through a minimal `Sequence`.
#[test]
fn container_positioned_child_triggers_at_its_resolved_start() {
    let e = Event::new();
    let seq = Sequence::builder()
        .child(voice(2.5)) // line 1: a 2.5s slot
        .child(voice(3.0).trigger_at_start(e)) // line 2: fires at the cursor
        .build();
    let resolved = resolve(seq).expect("voices give a length");
    // Child 2 starts at child 1's resolved end (2.5).
    assert_eq!(resolved.triggers().get(e.id()).seconds(), 2.5);
}

// DOCUMENTED edge case (verb-ordering decision (i)): `x.at(5.0)
// .trigger_at_start(e)` wraps a `Placed`. The `Triggered` records at the
// wrapper's own `abs_start` (0.0 as the root) and IGNORES the inner `5.0`.
// This is the documented behaviour on `Triggers` — trigger outermost
// instead. (No fix: folding a wrapped child's offset would only cover an
// immediate `Placed` and misbehave for any other nesting.)
#[test]
fn triggered_over_placed_ignores_inner_offset() {
    let e = Event::new();
    // `.at(5.0..7.0)` then `.trigger_at_start` ⇒ Triggered<Placed>.
    let resolved =
        resolve(OpacityProbe { alpha: 0 }.at(5.0..7.0).trigger_at_start(e)).expect("windowed");
    // Recorded at the wrapper's start (0.0), NOT the inner 5.0.
    assert_eq!(resolved.triggers().get(e.id()).seconds(), 0.0);
}

// The CORRECT order for an offset trigger: trigger THEN place. `x
// .trigger_at_start(e).at(5.0..7.0)` wraps the `Triggered` in the `Placed`,
// so the placement hands the trigger its offset start (5.0).
#[test]
fn placed_over_triggered_keeps_offset() {
    let e = Event::new();
    let resolved =
        resolve(OpacityProbe { alpha: 0 }.trigger_at_start(e).at(5.0..7.0)).expect("windowed");
    // The `Placed` recurses into the `Triggered` at the relative start 5.0,
    // so the event is recorded there.
    assert_eq!(resolved.triggers().get(e.id()).seconds(), 5.0);
}
