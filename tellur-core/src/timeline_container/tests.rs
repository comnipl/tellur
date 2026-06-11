use super::leaves::STUB_PROBE_SECONDS;
use super::*;
use crate::geometry::{Constraints, Vec2};
use crate::raster::{PixelFormat, RasterComponent, RasterImage, Resolution};
use crate::render_context::RenderContext;
use crate::timeline_component::{
    resolve, Arrangement, NodeKind, ResolveCtx, ResolveError, Timed, TimedBuilder,
    TimelineComponent,
};

// A trivial timeless visual (a stand-in "Caption") reaching the timeline
// world through the one-way `RasterComponent` blanket.
#[derive(PartialEq, Hash)]
struct Caption;

impl RasterComponent for Caption {
    fn layout(&self, _c: Constraints) -> Vec2 {
        Vec2(1.0, 1.0)
    }
    fn render(&self, _s: Vec2, _t: Resolution, _ctx: &mut dyn RenderContext) -> RasterImage {
        RasterImage::cpu(1, 1, PixelFormat::Rgba8, vec![0u8; 4])
    }
}

// (a) A Timeline with two visuals `.at(..)` resolves to the max end.
#[test]
fn timeline_resolves_to_max_non_fill_end() {
    let tl = Timeline::builder()
        .child(Caption.at(0.0..3.0))
        .child(Caption.at(2.0..5.0))
        .build();
    assert_eq!(tl.measure(), Some(5.0));
    let resolved = resolve(tl).expect("windowed, not timeless");
    assert_eq!(resolved.duration(), 5.0);
}

// (b) A Sequence of two stub clips resolves with the cursor (child 2 starts
// at child 1's end), and re-flows when a length changes.
#[test]
fn sequence_cursor_places_in_a_row_and_reflows() {
    let seq = Sequence::builder()
        .child(VideoFile::builder().path("a.mp4").duration(2.0))
        .child(VideoFile::builder().path("b.mp4").duration(3.0))
        .build();
    // Total = 2 + 3.
    assert_eq!(seq.measure(), Some(5.0));
    let resolved = resolve(seq).expect("media-backed, not timeless");
    assert_eq!(resolved.duration(), 5.0);

    // Re-flow: lengthening child 1 shifts the total without touching child 2.
    let seq = Sequence::builder()
        .child(VideoFile::builder().path("a.mp4").duration(4.0))
        .child(VideoFile::builder().path("b.mp4").duration(3.0))
        .build();
    assert_eq!(seq.measure(), Some(7.0));
}

// The cursor actually places child 2 at child 1's resolved end (via cues,
// which re-flow the same way `resolve` does).
#[test]
fn sequence_second_child_starts_at_first_end() {
    let seq = Sequence::builder()
        .child(Subtitle::builder().text("one").at(0.0..2.0))
        .child(Subtitle::builder().text("two").at(0.0..3.0))
        .build();
    let cues = seq.cues(0.0);
    assert_eq!(cues.len(), 2);
    assert_eq!(cues[0].start, 0.0);
    assert_eq!(cues[0].end, 2.0);
    // Child 2 starts at child 1's end (2.0) and runs its own 3.0 window.
    assert_eq!(cues[1].start, 2.0);
    assert_eq!(cues[1].end, 5.0);
}

#[test]
fn sequence_spacing_inserts_gaps() {
    let seq = Sequence::builder()
        .spacing(0.5)
        .child(VideoFile::builder().path("a.mp4").duration(2.0))
        .child(VideoFile::builder().path("b.mp4").duration(3.0))
        .build();
    // 2 + 0.5 gap + 3.
    assert_eq!(seq.measure(), Some(5.5));
}

// (c) `.fill()` inside a Sequence => resolve() returns Err.
#[test]
fn fill_inside_sequence_is_a_resolve_error() {
    let seq = Sequence::builder()
        .child(VideoFile::builder().path("a.mp4").duration(2.0))
        .child(Subtitle::builder().text("spanning").fill())
        .build();
    let err = resolve(seq).expect_err("a fill child in a Sequence is invalid");
    assert!(matches!(err, ResolveError::Invalid(_)));
}

// (d) A `.fill()` child in a Timeline takes the container length.
#[test]
fn fill_child_in_timeline_takes_container_length() {
    // The voice (3.0) sizes the timeline; the subtitle `.fill()` spans it.
    let tl = Timeline::builder()
        .child(AudioFile::builder().path("vo.wav").duration(3.0))
        .child(Subtitle::builder().text("spanning").fill())
        .build();
    // Fill children are excluded from measure; the voice sets 3.0.
    assert_eq!(tl.measure(), Some(3.0));
    let resolved = resolve(tl).expect("media-backed, not timeless");
    assert_eq!(resolved.duration(), 3.0);
    assert!(resolved.warnings().is_empty());

    // The fill subtitle's cue spans the whole container length.
    let cues = resolved.source().cues(0.0);
    let sub = cues
        .iter()
        .find(|c| c.text == "spanning")
        .expect("subtitle cue");
    assert_eq!(sub.start, 0.0);
    assert_eq!(sub.end, 3.0);
}

#[test]
fn all_fill_timeline_warns_and_collapses_to_zero() {
    let tl = Timeline::builder()
        .child(Subtitle::builder().text("a").fill())
        .child(Subtitle::builder().text("b").fill())
        .build();
    // All-fill interior: measure is None (no non-fill child), so the root is
    // a timeless tree (an error at the resolve entry per M4). Drive resolve
    // directly to observe the place-time warning.
    assert_eq!(tl.measure(), None);
    let mut ctx = ResolveCtx::new();
    let len = tl.resolve(0.0, &mut ctx);
    assert_eq!(len, 0.0);
    assert!(!ctx.warnings().is_empty());
}

// (e) Subtitle cues come out at the right absolute offset through
// Timeline→child nesting.
#[test]
fn subtitle_cue_absolute_offset_through_nesting() {
    // A Timeline whose subtitle is placed at 2.0..4.0, nested under an outer
    // Timeline that itself sits the inner one (bare → 0.0). The cue must be
    // absolute: 2.0..4.0.
    let inner = Timeline::builder()
        .child(VideoFile::builder().path("bg.mp4").duration(6.0))
        .child(Subtitle::builder().text("hello").at(2.0..4.0))
        .build();
    let outer = Timeline::builder().child(inner).build();
    let resolved = resolve(outer).expect("media-backed");
    let cues = resolved.source().cues(0.0);
    let hello = cues
        .iter()
        .find(|c| c.text == "hello")
        .expect("subtitle cue");
    assert_eq!(hello.start, 2.0);
    assert_eq!(hello.end, 4.0);
}

// (f) The `.sketch/01` Dialogue shape (Timeline of Caption.fill() +
// Subtitle.fill() + voice) type-checks and resolves.
#[test]
fn dialogue_shape_typechecks_and_resolves() {
    // Caption.fill() + Subtitle.fill() + bare voice; the voice sizes it.
    let dialogue = Timeline::builder()
        .child(Caption.fill())
        .child(Subtitle::builder().text("a line").fill())
        .child(AudioFile::builder().path("vo.wav").duration(4.5))
        .build();
    assert_eq!(dialogue.measure(), Some(4.5));
    let resolved = resolve(dialogue).expect("the voice gives it a length");
    assert_eq!(resolved.duration(), 4.5);
    assert!(resolved.warnings().is_empty());

    // The 字幕 spans the dialogue's whole resolved length.
    let cues = resolved.source().cues(0.0);
    let line = cues
        .iter()
        .find(|c| c.text == "a line")
        .expect("subtitle cue");
    assert_eq!(line.start, 0.0);
    assert_eq!(line.end, 4.5);
}

// (g) The `.sketch/01` B.4 arrangement shape: a Sequence of windowed
// segments (segment-2 `.trigger_at_start(e)`) under an overlay Timeline with
// a `.fill()` subtitle. The resolved arrangement must stamp non-zero
// start/end on every inner node, span the fill overlay across the whole, and
// surface segment-2's resolved start in its `triggers`.
#[test]
fn arrangement_stamps_resolved_starts_ends_and_triggers() {
    use crate::timeline_component::{Event, NodeKind, TriggerMark, Triggers};

    let e = Event::new();
    let root = Timeline::builder()
        .child(
            Sequence::builder()
                // Three 3s windowed segments → resolved at 0/3/6.
                .child(Caption.at(0.0..3.0))
                .child(Caption.at(0.0..3.0).trigger_at_start(e))
                .child(Caption.at(0.0..3.0))
                .build(),
        )
        // A timeless subtitle `.fill()` spans the whole 9s timeline.
        .child(Subtitle::builder().text("overlay").fill())
        .build();

    let resolved = resolve(root).expect("the sequence gives it a length");
    assert_eq!(resolved.duration(), 9.0);

    // Walk the resolved arrangement from root offset 0.
    let arr = resolved.source().arrangement(0.0);
    assert_eq!(arr.kind, NodeKind::Timeline);
    assert_eq!(arr.start, 0.0);
    assert_eq!(arr.end, 9.0);

    // Child 0 is the Sequence spanning the whole 9s.
    let seq = &arr.children[0];
    assert_eq!(seq.kind, NodeKind::Sequence);
    assert_eq!(seq.start, 0.0);
    assert_eq!(seq.end, 9.0);

    // The three segments are placed by the cursor at 0/3/6, each 3s long.
    assert_eq!(seq.children.len(), 3);
    assert_eq!((seq.children[0].start, seq.children[0].end), (0.0, 3.0));
    assert_eq!((seq.children[1].start, seq.children[1].end), (3.0, 6.0));
    assert_eq!((seq.children[2].start, seq.children[2].end), (6.0, 9.0));

    // Segment-2's trigger fires at its resolved start (3.0); the event is
    // unnamed (`Event::new`), so the mark carries `None`.
    assert_eq!(
        seq.children[1].triggers,
        vec![TriggerMark {
            time: 3.0,
            name: None
        }]
    );
    // The untriggered segments carry no triggers.
    assert!(seq.children[0].triggers.is_empty());
    assert!(seq.children[2].triggers.is_empty());

    // Child 1 is the `.fill()` subtitle overlay spanning the whole timeline.
    let overlay = &arr.children[1];
    assert_eq!(overlay.kind, NodeKind::Subtitle);
    assert_eq!(overlay.label, "overlay");
    assert_eq!(overlay.start, 0.0);
    assert_eq!(overlay.end, 9.0);
}

// (g.2) The generated `.child(...)` setter is `#[track_caller]` and wraps
// each child in a `Sourced`, so every arrangement node carries the `file:line`
// of its `.child(...)` call — for a bare placement, a placed segment under a
// nested container, and a `.fill()` overlay alike.
#[test]
// The assertions below check each `.child(...)` call's captured source line via offsets
// from `line!()`, so the builder's exact line layout is load-bearing. Pin it against
// rustfmt, which would otherwise collapse the Sequence builder onto one line — putting
// the nested `.child(` on the SAME line as the outer one and breaking the offsets.
#[rustfmt::skip]
fn arrangement_captures_child_call_site_source() {
    use crate::timeline_component::NodeKind;

    // Capture the exact lines of the three `.child(...)` calls below so the
    // assertion does not hard-code a brittle absolute line number.
    // `#[track_caller]` reports the line where `.child(` appears.
    let seq_line = line!() + 3; // the `.child(` of the Sequence
    let fill_line = line!() + 7; // the `.child(` of the `.fill()` subtitle
    let root = Timeline::builder()
        .child(
            Sequence::builder()
                .child(Caption.at(0.0..3.0))
                .build(),
        )
        .child(Subtitle::builder().text("overlay").fill())
        .build();

    let resolved = resolve(root).expect("the sequence gives it a length");
    let arr = resolved.source().arrangement(0.0);

    // The root itself has no enclosing `.child(...)` — no source.
    assert_eq!(arr.source, None);

    // Child 0 (the Sequence) is stamped with its `.child(...)` line.
    let seq = &arr.children[0];
    assert_eq!(seq.kind, NodeKind::Sequence);
    let seq_src = seq.source.as_ref().expect("sequence child has a source");
    assert!(
        seq_src.file.ends_with("timeline_container/tests.rs"),
        "{}",
        seq_src.file
    );
    assert_eq!(seq_src.line, seq_line);

    // The nested placed caption is stamped with ITS own `.child(...)` line
    // (inside the `Sequence::builder()` block), not the Sequence's.
    let inner = &seq.children[0];
    let inner_src = inner.source.as_ref().expect("placed caption has a source");
    assert_eq!(inner_src.line, seq_line + 2);

    // Child 1 (the `.fill()` subtitle) is stamped with its `.child(...)` line.
    let overlay = &arr.children[1];
    assert_eq!(overlay.kind, NodeKind::Subtitle);
    let overlay_src = overlay.source.as_ref().expect("fill overlay has a source");
    assert_eq!(overlay_src.line, fill_line);
}

// The leaf duration/probe seam: an injected `duration` overrides the stub.
#[test]
fn media_leaf_probe_seam_is_injectable() {
    assert_eq!(
        VideoFile::builder().path("x.mp4").build().duration(),
        Some(STUB_PROBE_SECONDS)
    );
    assert_eq!(
        VideoFile::builder()
            .path("x.mp4")
            .duration(7.0)
            .build()
            .duration(),
        Some(7.0)
    );
    assert_eq!(
        AudioFile::builder()
            .path("x.wav")
            .gain(0.5)
            .duration(9.0)
            .build()
            .duration(),
        Some(9.0)
    );
    // Subtitle is timeless.
    assert_eq!(Subtitle::builder().text("t").build().duration(), None);
}

// The complete builders satisfy `TimelineBuilder` (the marker bound), so the
// buildless `.child(..)` / `.at(..)` / `.fill()` paths work.
#[test]
fn containers_and_leaves_box_via_from() {
    let _boxed: Box<dyn TimelineComponent + Send> =
        Timeline::builder().child(Caption.at(0.0..1.0)).into();
    let _boxed2: Box<dyn TimelineComponent + Send> = VideoFile::builder().path("x.mp4").into();
}

// ── Per-frame sampling (step 5) ──────────────────────────────────────────
//
// These exercise the VIDEO-ONLY sampling path with SYNTHETIC colored
// visuals (no real media): container `frame` rebases each child's clock and
// composites the recursed frames source-over at the image layer.

use crate::time::{LocalTime, Time, TimelineTime};
use crate::timeline_component::{resolve as resolve_root, Clock, TriggerTable};
use std::sync::{Arc, Mutex};

/// A synthetic solid-color visual that fills the whole `target` with one
/// opaque RGBA color. A timeless `RasterComponent`, so it reaches the
/// timeline world through the one-way blanket and renders via `ctx.render`.
#[derive(PartialEq, Hash)]
struct SolidColor {
    rgba: [u8; 4],
}

impl RasterComponent for SolidColor {
    fn layout(&self, _c: Constraints) -> Vec2 {
        Vec2(1.0, 1.0)
    }
    fn render(&self, _s: Vec2, t: Resolution, _ctx: &mut dyn RenderContext) -> RasterImage {
        let count = (t.width as usize) * (t.height as usize);
        let mut pixels = Vec::with_capacity(count * 4);
        for _ in 0..count {
            pixels.extend_from_slice(&self.rgba);
        }
        RasterImage::cpu(t.width, t.height, PixelFormat::Rgba8, pixels)
    }
}

fn first_pixel(image: &RasterImage) -> [u8; 4] {
    let cpu = image.as_cpu().expect("cpu image");
    [cpu.pixels[0], cpu.pixels[1], cpu.pixels[2], cpu.pixels[3]]
}

// (a) A Timeline overlaying two solid-color visuals composites BOTH: the
// later child (added second) lands on top via source-over.
#[test]
fn timeline_overlays_two_solids_source_over() {
    // Bottom is fully opaque red; top is semi-transparent green over it.
    let tl = Timeline::builder()
        .child(
            SolidColor {
                rgba: [255, 0, 0, 255],
            }
            .fill(),
        )
        .child(
            SolidColor {
                rgba: [0, 255, 0, 128],
            }
            .fill(),
        )
        // A bare windowed solid gives the timeline a non-fill length so the
        // two fills have something to span.
        .child(
            SolidColor {
                rgba: [0, 0, 255, 0],
            }
            .at(0.0..2.0),
        )
        .build();
    let resolved = resolve_root(tl).expect("windowed, not timeless");

    let mut ctx = crate::render_context::PassThrough;
    let frame = resolved
        .frame(TimelineTime::new(0.5), Resolution::new(4, 4), &mut ctx)
        .expect("two visible solids contribute a frame");

    // Source-over of green(a=128) over opaque red:
    //   out_a = 128 + 255*(255-128)/255 ≈ 255 (opaque)
    //   out_r ≈ red * (1 - 128/255)  (red bleeds through ~half)
    //   out_g ≈ 255 * (128/255)      (green ~half)
    let px = first_pixel(&frame);
    assert!(px[3] >= 254, "result is opaque, got alpha {}", px[3]);
    assert!(px[0] > 100 && px[0] < 160, "red ~half, got {}", px[0]);
    assert!(px[1] > 100 && px[1] < 160, "green ~half, got {}", px[1]);
    assert_eq!(px[2], 0, "no blue contributes");
}

// A `#[component(timeline)]` with `#[clock]` that bakes `clock.local()`
// (seconds, quantized to an integer) into the red channel — proving the
// rebased local clock reaches the visual through `frame`.
#[crate::component(timeline)]
fn LocalProbe(#[clock] clock: Clock) -> impl TimelineComponent {
    let secs = clock.local().seconds().round().clamp(0.0, 255.0) as u8;
    SolidColor {
        rgba: [secs, 0, 0, 255],
    }
}

// (b) Clock rebasing: a visual placed `.at(2.0..)` sees `local ≈ 0` at
// global t = 2.0. The probe bakes its local seconds into red.
#[test]
fn placed_at_rebases_local_clock_to_zero_at_its_start() {
    let probe = LocalProbe::builder().build().at(2.0..5.0);
    let resolved = resolve_root(probe).expect("windowed, not timeless");

    let mut ctx = crate::render_context::PassThrough;
    // At global 2.0, the placed child's local is 0 → red channel 0.
    let f0 = resolved
        .frame(TimelineTime::new(2.0), Resolution::new(2, 2), &mut ctx)
        .expect("contributes");
    assert_eq!(first_pixel(&f0)[0], 0, "local ≈ 0 at its start");

    // `.at(2.0..5.0)` over a timeless child has speed 1.0, so at an INTERIOR
    // global 4.0 the child's local is ≈ 2.0 → red channel 2.
    let f2 = resolved
        .frame(TimelineTime::new(4.0), Resolution::new(2, 2), &mut ctx)
        .expect("contributes");
    assert_eq!(first_pixel(&f2)[0], 2, "local advances 1:1 with global");

    // The window is half-open `[2.0, 5.0)`: at the exclusive end the clip is
    // gated OFF and contributes no frame.
    assert!(
        resolved
            .frame(TimelineTime::new(5.0), Resolution::new(2, 2), &mut ctx)
            .is_none(),
        "the exclusive window end contributes nothing",
    );
}

// Temporal gate: a placed clip contributes ONLY within its half-open window
// `[start, end)`; outside it the frame is `None` — this is what makes a
// finished caption DISAPPEAR instead of staying painted.
#[test]
fn placed_frame_gates_outside_its_window() {
    let probe = LocalProbe::builder().build().at(1.0..3.0);
    let resolved = resolve_root(probe).expect("windowed, not timeless");
    let mut ctx = crate::render_context::PassThrough;

    assert!(
        resolved
            .frame(TimelineTime::new(0.5), Resolution::new(2, 2), &mut ctx)
            .is_none(),
        "before the window: nothing",
    );
    assert!(
        resolved
            .frame(TimelineTime::new(1.5), Resolution::new(2, 2), &mut ctx)
            .is_some(),
        "inside the window: contributes",
    );
    assert!(
        resolved
            .frame(TimelineTime::new(3.0), Resolution::new(2, 2), &mut ctx)
            .is_none(),
        "exclusive end (half-open): nothing",
    );
    assert!(
        resolved
            .frame(TimelineTime::new(3.5), Resolution::new(2, 2), &mut ctx)
            .is_none(),
        "past the window: nothing",
    );
}

// An overlay hard-cuts at its resolved length: a `.fill()` child does NOT
// render past the container length (fixed here by a non-fill sibling's
// window), even though a fill placement has no time-gate of its own.
#[test]
fn timeline_caps_fill_child_at_container_length() {
    let tl = Timeline::builder()
        // Non-fill sibling fixes the container length at 2.0s.
        .child(
            SolidColor {
                rgba: [0, 0, 0, 255],
            }
            .at(0.0..2.0),
        )
        // A fill visual takes the container length.
        .child(
            SolidColor {
                rgba: [255, 0, 0, 255],
            }
            .fill(),
        )
        .build();
    let resolved = resolve_root(tl).expect("windowed, not timeless");
    let mut ctx = crate::render_context::PassThrough;

    assert!(
        resolved
            .frame(TimelineTime::new(1.0), Resolution::new(2, 2), &mut ctx)
            .is_some(),
        "inside [0,2): the fill renders",
    );
    assert!(
        resolved
            .frame(TimelineTime::new(2.0), Resolution::new(2, 2), &mut ctx)
            .is_none(),
        "exclusive container end: hard cut (even for a fill)",
    );
    assert!(
        resolved
            .frame(TimelineTime::new(2.5), Resolution::new(2, 2), &mut ctx)
            .is_none(),
        "past the container length: nothing",
    );
}

// A Sequence composites only the ACTIVE slot; a child outside its slot is
// silent, and past the last slot the whole sequence contributes nothing.
#[test]
fn sequence_gates_each_child_to_its_slot() {
    let seq = Sequence::builder()
        .child(LocalProbe::builder().build().at(0.0..2.0)) // slot 1: [0,2)
        .child(LocalProbe::builder().build().at(0.0..2.0)) // slot 2: [2,4)
        .build();
    let resolved = resolve_root(seq).expect("windowed, not timeless");
    let mut ctx = crate::render_context::PassThrough;

    assert!(
        resolved
            .frame(TimelineTime::new(0.5), Resolution::new(2, 2), &mut ctx)
            .is_some(),
        "slot 1 active at 0.5",
    );
    // Slot 2 active at global 3.0 → local rebased to 1.0 → red round(1.0)=1.
    let f = resolved
        .frame(TimelineTime::new(3.0), Resolution::new(2, 2), &mut ctx)
        .expect("slot 2 active");
    assert_eq!(first_pixel(&f)[0], 1, "slot 2 local = 3.0 - 2.0 = 1.0");
    assert!(
        resolved
            .frame(TimelineTime::new(4.0), Resolution::new(2, 2), &mut ctx)
            .is_none(),
        "past both slots: nothing",
    );
}

// Records the clock window it is sampled with (post-stretch local seconds),
// proving a stretched `.at` surfaces the window in the child's OWN units.
#[derive(Clone)]
struct WindowProbe {
    log: Arc<Mutex<Vec<Option<f32>>>>,
    duration: f32,
}
impl PartialEq for WindowProbe {
    fn eq(&self, other: &Self) -> bool {
        self.duration == other.duration
    }
}
impl std::hash::Hash for WindowProbe {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.duration.to_bits().hash(state);
    }
}
impl TimelineComponent for WindowProbe {
    fn duration(&self) -> Option<f32> {
        Some(self.duration)
    }
    fn frame(
        &self,
        clock: Clock<'_>,
        canvas: Vec2,
        target: Resolution,
        _ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage> {
        let _ = canvas;
        self.log
            .lock()
            .unwrap()
            .push(clock.window().map(|w| w.width()));
        Some(RasterImage::cpu(
            target.width,
            target.height,
            PixelFormat::Rgba8,
            vec![255u8; (target.width as usize) * (target.height as usize) * 4],
        ))
    }
    fn arrangement(&self, offset: f32) -> Arrangement {
        Arrangement {
            kind: NodeKind::Video,
            label: String::new(),
            name: None,
            source: None,
            start: offset,
            end: offset + self.duration,
            trim: None,
            triggers: Vec::new(),
            children: Vec::new(),
        }
    }
}

// A 2s source stretched into a 1s window (speed 2.0) surfaces a window of 2.0
// (content seconds), matching the rebased local axis the child actually sees.
#[test]
fn placed_surfaces_post_stretch_window_to_the_clock() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let leaf = WindowProbe {
        log: Arc::clone(&log),
        duration: 2.0,
    };
    let resolved = resolve_root(leaf.at(0.0..1.0)).expect("windowed, not timeless");
    let mut ctx = crate::render_context::PassThrough;
    resolved.frame(TimelineTime::new(0.5), Resolution::new(1, 1), &mut ctx);
    assert_eq!(
        *log.lock().unwrap().last().expect("sampled"),
        Some(2.0),
        "(b - a) * speed = content seconds",
    );
}

// (b') Clock rebasing through a Sequence: a probe after a 2s clip sees
// `local ≈ 0` at global t = 2.0.
#[test]
fn sequence_rebases_second_child_local_clock() {
    let seq = Sequence::builder()
        // First slot: a 2s window (the probe is timeless, the window sizes it).
        .child(SolidColor { rgba: [0, 0, 0, 0] }.at(0.0..2.0))
        // Second slot: the probe, which starts at the cursor (2.0).
        .child(LocalProbe::builder().build().at(0.0..3.0))
        .build();
    let resolved = resolve_root(seq).expect("windowed, not timeless");

    let mut ctx = crate::render_context::PassThrough;
    // At global 2.0 the second child's local is ≈ 0 → red 0.
    let f = resolved
        .frame(TimelineTime::new(2.0), Resolution::new(2, 2), &mut ctx)
        .expect("contributes");
    assert_eq!(
        first_pixel(&f)[0],
        0,
        "second child's local ≈ 0 at the cursor"
    );

    // At global 4.0 the second child's local is ≈ 2.0 → red 2.
    let f2 = resolved
        .frame(TimelineTime::new(4.0), Resolution::new(2, 2), &mut ctx)
        .expect("contributes");
    assert_eq!(first_pixel(&f2)[0], 2, "local tracks the cursor offset");
}

/// A test leaf with a DIRECT `impl TimelineComponent` (not via the blanket)
/// so its `frame` receives the clock. It records every local time it is
/// sampled at into a shared log and emits a 1×1 opaque frame so the walk
/// treats it as a contributing visual.
#[derive(Clone)]
struct RecordingLeaf {
    // Excluded from eq/hash (interior, per-frame state — like `#[clock]`).
    log: Arc<Mutex<Vec<f32>>>,
    // An intrinsic length so it is a TIMED leaf (a window over it stretches).
    duration: f32,
}

impl PartialEq for RecordingLeaf {
    fn eq(&self, other: &Self) -> bool {
        self.duration == other.duration
    }
}
impl std::hash::Hash for RecordingLeaf {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.duration.to_bits().hash(state);
    }
}

impl TimelineComponent for RecordingLeaf {
    fn duration(&self) -> Option<f32> {
        Some(self.duration)
    }

    fn frame(
        &self,
        clock: Clock<'_>,
        canvas: Vec2,
        target: Resolution,
        _ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage> {
        let _ = canvas;
        self.log.lock().unwrap().push(clock.local().seconds());
        Some(RasterImage::cpu(
            target.width,
            target.height,
            PixelFormat::Rgba8,
            vec![255u8; (target.width as usize) * (target.height as usize) * 4],
        ))
    }

    fn arrangement(&self, offset: f32) -> Arrangement {
        Arrangement {
            kind: NodeKind::Video,
            label: String::new(),
            name: None,
            source: None,
            start: offset,
            end: offset + self.duration,
            trim: None,
            triggers: Vec::new(),
            children: Vec::new(),
        }
    }
}

// (c) `.at(window)` speed: a 2s source placed in a 1s window plays at 2×,
// so at parent-local `t` the source is sampled at ≈ `2 * t`.
#[test]
fn placement_window_stretch_reaches_the_leaf() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let leaf = RecordingLeaf {
        log: Arc::clone(&log),
        duration: 2.0,
    };
    // 2s source into a 1s window → speed = 2.0.
    let placed = leaf.at(0.0..1.0);
    assert_eq!(placed.speed(), 2.0);
    let resolved = resolve_root(placed).expect("windowed, not timeless");

    let mut ctx = crate::render_context::PassThrough;
    // At global 0.5 the leaf should be sampled at source-local ≈ 1.0.
    resolved.frame(TimelineTime::new(0.5), Resolution::new(1, 1), &mut ctx);
    let recorded = *log.lock().unwrap().last().expect("leaf was sampled");
    assert!(
        (recorded - 1.0).abs() < 1e-5,
        "2× stretch: parent-local 0.5 → source-local {recorded} (want 1.0)",
    );

    // And at global 0.25 → source-local ≈ 0.5.
    resolved.frame(TimelineTime::new(0.25), Resolution::new(1, 1), &mut ctx);
    let recorded = *log.lock().unwrap().last().expect("leaf was sampled");
    assert!(
        (recorded - 0.5).abs() < 1e-5,
        "source-local {recorded} (want 0.5)"
    );
}

// The timeless-visual path: a bare `RasterComponent` reached through the
// blanket renders via `ctx.render` (no clock dependence), and a
// `#[component(timeline)]` body that builds a timeless visual returns that
// visual's frame.
#[test]
fn timeless_visual_frame_routes_through_ctx_render() {
    let table = TriggerTable::new();
    let clock = Clock::new(TimelineTime::new(0.0), LocalTime::new(0.0), &table);
    let mut ctx = crate::render_context::PassThrough;

    // Bare RasterComponent via the blanket.
    let solid = SolidColor {
        rgba: [10, 20, 30, 255],
    };
    let f = solid
        .frame(clock, Vec2(2.0, 2.0), Resolution::new(2, 2), &mut ctx)
        .expect("renders");
    assert_eq!(first_pixel(&f), [10, 20, 30, 255]);

    // A timeline component whose body builds a timeless visual.
    let probe = LocalProbe::builder().build();
    let f = probe
        .frame(clock, Vec2(2.0, 2.0), Resolution::new(2, 2), &mut ctx)
        .expect("renders");
    assert_eq!(first_pixel(&f)[0], 0, "local 0 → red 0");
}

// ── Audio decode + eager mix-down (step 8) ───────────────────────────────
//
// These exercise the REAL `symphonia` decode path by synthesizing a tiny
// mono s16le WAV fixture to a temp file, then driving `render_audio` over
// timelines built from `AudioFile`s. The output layout is fixed (mono here
// for easy assertions; the encoder uses stereo @ 48 kHz).

use crate::timeline_component::resolve as resolve_audio;

/// Output rate the audio tests mix into (matches the encoder boundary).
const TEST_RATE: u32 = 48_000;

/// Writes a canonical 44-byte-header mono s16le WAV of `samples` at `rate`
/// to a unique temp path and returns it. The caller removes it when done.
fn write_wav_fixture(name: &str, rate: u32, samples: &[i16]) -> std::path::PathBuf {
    let mut path = std::env::temp_dir();
    // Disambiguate per-test + per-process so parallel tests don't collide.
    path.push(format!("tellur_audio_{}_{}.wav", name, std::process::id()));

    let channels: u16 = 1;
    let bits: u16 = 16;
    let byte_rate = rate * channels as u32 * (bits as u32 / 8);
    let block_align = channels * (bits / 8);
    let data_bytes = (samples.len() * 2) as u32;

    let mut bytes = Vec::with_capacity(44 + samples.len() * 2);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    bytes.extend_from_slice(&1u16.to_le_bytes()); // audio format = PCM
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&byte_rate.to_le_bytes());
    bytes.extend_from_slice(&block_align.to_le_bytes());
    bytes.extend_from_slice(&bits.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_bytes.to_le_bytes());
    for s in samples {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(&path, &bytes).expect("write wav fixture");
    path
}

/// A constant-amplitude mono fixture: `frames` samples all at `level`.
fn const_wav(name: &str, rate: u32, frames: usize, level: i16) -> std::path::PathBuf {
    write_wav_fixture(name, rate, &vec![level; frames])
}

// Decode reads the real source length: a 1.0s fixture probes to ~1.0s.
#[test]
fn audiofile_probes_real_decoded_duration() {
    let path = const_wav("probe", TEST_RATE, TEST_RATE as usize, 10_000);
    let af = AudioFile::builder().path(path.to_str().unwrap()).build();
    let d = af.duration().expect("audio has a duration");
    assert!((d - 1.0).abs() < 1e-3, "decoded ~1.0s, got {d}");
    let _ = std::fs::remove_file(&path);
}

// `.trim(a..b)` crops the SOURCE seconds and shortens the duration to b - a.
#[test]
fn audiofile_trim_crops_source_seconds() {
    // 2s of audio; trim to the middle 0.5s.
    let path = const_wav("trim", TEST_RATE, (TEST_RATE * 2) as usize, 8_000);
    let af = AudioFile::builder()
        .path(path.to_str().unwrap())
        .build()
        .trim(0.5..1.0);
    let d = af.duration().expect("trimmed duration");
    assert!((d - 0.5).abs() < 1e-3, "trim to 0.5s, got {d}");
    let _ = std::fs::remove_file(&path);
}

// (mix) Two AudioFiles overlapping in a Timeline SUM where they overlap.
#[test]
fn timeline_overlapping_audio_sums() {
    // Two 0.5s half-amplitude (+0.5) mono tones, both placed at start 0.0 in
    // a Timeline ⇒ they overlap fully and the mix is +1.0 in that region.
    let half = (0.5 * i16::MAX as f32) as i16;
    let frames = (TEST_RATE / 2) as usize; // 0.5s
    let a = const_wav("mix_a", TEST_RATE, frames, half);
    let b = const_wav("mix_b", TEST_RATE, frames, half);

    let tl = Timeline::builder()
        .child(AudioFile::builder().path(a.to_str().unwrap()))
        .child(AudioFile::builder().path(b.to_str().unwrap()))
        .build();
    let resolved = resolve_audio(tl).expect("media-backed");
    let mixed = resolved.render_audio(TEST_RATE, 1);

    // In the overlap, each source is ~+0.5, summed to ~+1.0.
    let mid = mixed.samples[frames / 2];
    assert!(
        (mid - 1.0).abs() < 0.02,
        "two +0.5 tones sum to ~1.0, got {mid}"
    );
    assert_eq!(mixed.rate, TEST_RATE);
    assert_eq!(mixed.channels, 1);
    let _ = std::fs::remove_file(&a);
    let _ = std::fs::remove_file(&b);
}

// A function-form `#[component(timeline)]` that wraps audio.
#[crate::component(timeline)]
fn Voiced(#[builder(into)] path: String) -> impl crate::timeline_component::TimelineComponent {
    AudioFile::builder().path(path).build()
}

// (mix) Regression: a fn-form `#[component(timeline)]` composing an
// `AudioFile` must forward `mix_into` to its body. Before the macro emitted
// a `mix_into` delegation it fell back to the silent trait default, so a
// `Dialogue(voice: AudioFile)`-style wrapper mixed to ZERO.
#[test]
fn fn_form_component_forwards_audio_mix() {
    let level = (0.6 * i16::MAX as f32) as i16;
    let frames = (TEST_RATE / 2) as usize; // 0.5s
    let src = const_wav("fnform", TEST_RATE, frames, level);

    let tl = Timeline::builder()
        .child(Voiced::builder().path(src.to_str().unwrap()))
        .build();
    let resolved = resolve_audio(tl).expect("media-backed");
    let mixed = resolved.render_audio(TEST_RATE, 1);

    let mid = mixed.samples[frames / 2];
    assert!(
        (mid - 0.6).abs() < 0.02,
        "wrapped audio must reach the mix (~0.6), got {mid}"
    );
    let _ = std::fs::remove_file(&src);
}

// (mix) A Sequence CONCATENATES audio along the cursor: child 2 starts at
// child 1's end, so the two regions are disjoint.
#[test]
fn sequence_concatenates_audio() {
    let frames = (TEST_RATE / 2) as usize; // 0.5s each
    let lo = (0.3 * i16::MAX as f32) as i16;
    let hi = (0.6 * i16::MAX as f32) as i16;
    let a = const_wav("seq_a", TEST_RATE, frames, lo);
    let b = const_wav("seq_b", TEST_RATE, frames, hi);

    let seq = Sequence::builder()
        .child(AudioFile::builder().path(a.to_str().unwrap()))
        .child(AudioFile::builder().path(b.to_str().unwrap()))
        .build();
    let resolved = resolve_audio(seq).expect("media-backed");
    let mixed = resolved.render_audio(TEST_RATE, 1);

    // First half ≈ 0.3 (child 1), second half ≈ 0.6 (child 2 at the cursor).
    let first = mixed.samples[frames / 2];
    let second = mixed.samples[frames + frames / 2];
    assert!(
        (first - 0.3).abs() < 0.02,
        "child 1 region ~0.3, got {first}"
    );
    assert!(
        (second - 0.6).abs() < 0.02,
        "child 2 region ~0.6, got {second}"
    );
    let _ = std::fs::remove_file(&a);
    let _ = std::fs::remove_file(&b);
}

// (mix) `gain` scales the decoded samples linearly.
#[test]
fn gain_scales_audio() {
    let frames = (TEST_RATE / 2) as usize;
    let level = (0.8 * i16::MAX as f32) as i16;
    let path = const_wav("gain", TEST_RATE, frames, level);

    let tl = Timeline::builder()
        .child(AudioFile::builder().path(path.to_str().unwrap()).gain(0.5))
        .build();
    let resolved = resolve_audio(tl).expect("media-backed");
    let mixed = resolved.render_audio(TEST_RATE, 1);

    // 0.8 source at gain 0.5 ⇒ ~0.4.
    let mid = mixed.samples[frames / 2];
    assert!(
        (mid - 0.4).abs() < 0.02,
        "gain 0.5 over 0.8 ⇒ ~0.4, got {mid}"
    );
    let _ = std::fs::remove_file(&path);
}

// (mix) `.at(window)` speed changes the SAMPLE COUNT: a 1.0s source placed
// into a 0.5s window plays at 2× (half as many output frames for that clip).
#[test]
fn placement_speed_changes_sample_count() {
    let src_frames = TEST_RATE as usize; // 1.0s source
    let level = (0.5 * i16::MAX as f32) as i16;
    let path = const_wav("speed", TEST_RATE, src_frames, level);

    // Native (1.0s): the mix is ~1.0s long.
    let native = AudioFile::builder().path(path.to_str().unwrap());
    let r_native = resolve_audio(Timeline::builder().child(native).build()).expect("media-backed");
    let mix_native = r_native.render_audio(TEST_RATE, 1);

    // Stretched into a 0.5s window ⇒ speed 2.0 ⇒ the resolved length is 0.5s,
    // so the mixed buffer holds about half the frames of the native one.
    let stretched = AudioFile::builder()
        .path(path.to_str().unwrap())
        .build()
        .at(0.0..0.5);
    let r_stretched = resolve_audio(stretched).expect("windowed, not timeless");
    let mix_stretched = r_stretched.render_audio(TEST_RATE, 1);

    assert!(
        mix_native.samples.len() > mix_stretched.samples.len() * 3 / 2,
        "2x speed ⇒ ~half the frames: native {} vs stretched {}",
        mix_native.samples.len(),
        mix_stretched.samples.len(),
    );
    // And the stretched buffer is ~0.5s.
    let stretched_secs = mix_stretched.samples.len() as f32 / TEST_RATE as f32;
    assert!(
        (stretched_secs - 0.5).abs() < 0.02,
        "stretched buffer ~0.5s, got {stretched_secs}",
    );
    let _ = std::fs::remove_file(&path);
}

// ── Real video decode via the ffmpeg child (step 9) ──────────────────────
//
// Behind `#[ignore]` (like the GPU + step-8 mux tests): synthesize a tiny
// mp4 fixture with `ffmpeg testsrc`, place it as a `VideoFile`, and decode a
// few frames asserting non-empty / plausible pixels. Runs for real here
// (the dev box has ffmpeg/ffprobe), giving byte-level validation under
// `cargo test -- --ignored`.

/// Writes a short `testsrc` mp4 (a moving color test pattern) to a unique
/// temp path via `ffmpeg`. `secs` long at 30 fps, `w`x`h`. Returns the path.
fn write_testsrc_mp4(name: &str, secs: u32, w: u32, h: u32) -> std::path::PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("tellur_video_{}_{}.mp4", name, std::process::id()));
    let size = format!("size={w}x{h}:rate=30:duration={secs}");
    let lavfi = format!("testsrc={size}");
    let status = std::process::Command::new("ffmpeg")
        .args(["-y", "-v", "error"])
        .args(["-f", "lavfi", "-i", &lavfi])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
        .arg(&path)
        .status()
        .expect("spawn ffmpeg to write testsrc fixture");
    assert!(status.success(), "ffmpeg testsrc fixture write failed");
    path
}

/// Whether an RGBA frame has at least one non-transparent, non-black pixel —
/// a "plausible decoded picture" check (testsrc is colorful).
fn frame_has_color(image: &RasterImage) -> bool {
    let cpu = image.as_cpu().expect("cpu frame");
    cpu.pixels
        .chunks_exact(4)
        .any(|px| px[3] > 0 && (px[0] > 0 || px[1] > 0 || px[2] > 0))
}

#[test]
#[ignore = "requires ffmpeg + ffprobe on PATH"]
fn videofile_probes_real_duration() {
    // A 2s fixture probes to ~2.0s via ffprobe.
    let path = write_testsrc_mp4("probe", 2, 64, 48);
    let vf = VideoFile::builder().path(path.to_str().unwrap()).build();
    let d = vf.duration().expect("video has a duration");
    assert!((d - 2.0).abs() < 0.2, "probed ~2.0s, got {d}");
    let _ = std::fs::remove_file(&path);
}

#[test]
#[ignore = "requires ffmpeg + ffprobe on PATH"]
fn videofile_decodes_plausible_frames() {
    // Decode a few frames at 64x48 and assert each is a real, colorful
    // picture scaled to the requested target. Exercises both the cold seek
    // (first request) and the forward advance (subsequent requests).
    let path = write_testsrc_mp4("decode", 2, 320, 240);
    let target = Resolution::new(64, 48);

    let tl = Timeline::builder()
        .child(
            VideoFile::builder()
                .path(path.to_str().unwrap())
                .at(0.0..2.0),
        )
        .build();
    let resolved = resolve_root(tl).expect("media-backed");
    let mut ctx = crate::render_context::PassThrough;

    for &t in &[0.0_f32, 0.1, 0.2, 1.0] {
        let frame = resolved
            .frame(TimelineTime::new(t), target, &mut ctx)
            .unwrap_or_else(|| panic!("decoded a frame at t={t}"));
        assert_eq!(frame.width(), 64, "scaled to target width");
        assert_eq!(frame.height(), 48, "scaled to target height");
        assert!(frame_has_color(&frame), "frame at t={t} has real pixels");
    }

    // A backward scrub (t=0.0 after t=1.0) re-seeks and still decodes.
    let back = resolved
        .frame(TimelineTime::new(0.0), target, &mut ctx)
        .expect("backward scrub decodes");
    assert!(
        frame_has_color(&back),
        "scrubbed-back frame has real pixels"
    );

    let _ = std::fs::remove_file(&path);
}
