use super::*;
use crate::geometry::{Constraints, Rect, Vec2};
use crate::phase::Phase;
use crate::placement::RasterPlacement;
use crate::raster::{PixelFormat, RasterComponent, RasterImage, RasterResidency, Resolution};
use crate::render_context::{CachePolicy, PassThrough, RenderContext};
use crate::time::{LocalTime, Time, TimelineTime};
use std::sync::atomic::{AtomicU8, Ordering};

// A trivial RasterComponent so we can exercise the blanket impl.
#[derive(PartialEq, Hash)]
struct Dot;

impl RasterComponent for Dot {
    fn layout(&self, _constraints: Constraints) -> Vec2 {
        Vec2(1.0, 1.0)
    }

    fn render(
        &self,
        _size: Vec2,
        _target: Resolution,
        _residency: RasterResidency,
        _ctx: &mut dyn RenderContext,
    ) -> RasterImage {
        RasterImage::cpu(1, 1, PixelFormat::Rgba8, vec![0u8, 0, 0, 0])
    }
}

// A macro-generated raster component reaches the timeline via the one-way
// blanket; its overridden `arrangement_name` surfaces on the node's `name`.
#[crate::component(raster)]
fn Badge(#[builder(into)] tag: String) -> impl RasterComponent {
    let _ = &tag;
    Dot
}

#[crate::component(raster)]
fn PositionedBadge() -> impl RasterComponent {
    Dot.place_at(Vec2::ZERO)
}

#[crate::component(raster)]
fn AvailablePositionedBadge(#[available] _available: Vec2) -> impl RasterComponent {
    Dot.place_at(Vec2::ZERO)
}

#[test]
fn raster_component_macro_name_flows_through_the_blanket() {
    let badge = Badge::builder().tag("hello").build();
    // Direct hook: the generated override returns the templated/derived name.
    assert_eq!(badge.arrangement_name().as_deref(), Some("Badge"));
    // And it lands on the arrangement node through the blanket impl.
    let boxed: Box<dyn TimelineComponent + Send> = Box::new(badge);
    assert_eq!(boxed.arrangement(0.0).name.as_deref(), Some("Badge"));
}

#[test]
fn raster_component_macro_routes_cache_policy_by_build_mode() {
    assert_eq!(
        PositionedBadge::builder().build().cache_policy(),
        CachePolicy::Transparent,
    );
    assert_eq!(
        Badge::builder().tag("memoized").build().cache_policy(),
        CachePolicy::Memoize,
    );

    // An `#[available]` component builds its root at render time and routes that
    // root back through the context, so the generated outer remains transparent.
    assert_eq!(
        AvailablePositionedBadge::builder().build().cache_policy(),
        CachePolicy::Transparent,
    );
}

static LAST_TIMELINE_RESIDENCY: AtomicU8 = AtomicU8::new(0);

#[derive(PartialEq, Hash)]
struct ResidencyProbe;

impl TimelineComponent for ResidencyProbe {
    fn frame(
        &self,
        _clock: Clock<'_>,
        _canvas: Vec2,
        _target: Resolution,
        residency: RasterResidency,
        _ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage> {
        let value = match residency {
            RasterResidency::Cpu => 1,
            RasterResidency::Gpu => 2,
        };
        LAST_TIMELINE_RESIDENCY.store(value, Ordering::Relaxed);
        None
    }

    fn arrangement(&self, offset: f32) -> Arrangement {
        Arrangement {
            kind: NodeKind::Video,
            label: String::new(),
            name: None,
            source: None,
            start: offset,
            end: offset,
            trim: None,
            triggers: Vec::new(),
            children: Vec::new(),
        }
    }
}

#[crate::component(timeline)]
fn ResidencyForwarder() -> impl TimelineComponent {
    ResidencyProbe
}

#[test]
fn timeline_component_macro_forwards_requested_residency() {
    LAST_TIMELINE_RESIDENCY.store(0, Ordering::Relaxed);
    let component = ResidencyForwarder::builder().build();
    let table = TriggerTable::new();
    let clock = Clock::new(TimelineTime::new(0.0), LocalTime::new(0.0), &table);
    let mut ctx = PassThrough;

    let _ = component.frame(
        clock,
        Vec2(1.0, 1.0),
        Resolution::new(1, 1),
        RasterResidency::Gpu,
        &mut ctx,
    );

    assert_eq!(LAST_TIMELINE_RESIDENCY.load(Ordering::Relaxed), 2);
}

#[test]
fn raster_component_is_a_timeline_component_via_blanket() {
    // The whole point of the one-way blanket: a RasterComponent can be
    // boxed as `Box<dyn TimelineComponent + Send>` (audit M2).
    let boxed: Box<dyn TimelineComponent + Send> = Box::new(Dot);
    assert_eq!(boxed.duration(), None);
    assert_eq!(boxed.measure(), None);
    assert_eq!(boxed.cues(0.0), Vec::new());
    assert_eq!(boxed.arrangement(0.0).kind, NodeKind::Video);
    // A plain raster primitive has no display name; only a
    // `#[component(...)]` overrides `arrangement_name`.
    assert_eq!(boxed.arrangement(0.0).name, None);
}

#[test]
fn blanket_frame_routes_through_ctx_render() {
    // A timeless visual produces a frame via `ctx.render` (memoization path).
    let dot = Dot;
    let table = TriggerTable::new();
    let clock = Clock::new(TimelineTime::new(0.0), LocalTime::new(0.0), &table);
    let mut ctx = PassThrough;
    let frame = dot.frame(
        clock,
        Vec2(4.0, 4.0),
        Resolution::new(4, 4),
        RasterResidency::Cpu,
        &mut ctx,
    );
    assert!(frame.is_some());
}

// A FIXED-size visual: its intrinsic box is a WIDE-SHORT rectangle (4:1),
// but `render` paints a centered logical SQUARE into whatever `size` /
// `target` it is handed, mapping logical→pixels by the per-axis `target /
// size` scale (exactly how real components — `Layer`, anchored `Positioned`
// — turn a logical box into pixels). The painted square is detectable as a
// run of opaque-alpha pixels; its pixel WIDTH vs HEIGHT reveals aspect
// distortion. If `frame` lays this out against its intrinsic 4:1 box rather
// than the canvas, the square is stretched 4:1 in the 16:9 frame.
#[derive(PartialEq, Hash)]
struct Square;

impl Square {
    // The logical box is 4:1 (deliberately NOT the 16:9 target aspect).
    const BOX: Vec2 = Vec2(400.0, 100.0);
    // The painted square's logical side (in `size` units).
    const SIDE: f32 = 50.0;
}

impl RasterComponent for Square {
    fn layout(&self, _constraints: Constraints) -> Vec2 {
        Self::BOX
    }

    fn render(
        &self,
        size: Vec2,
        target: Resolution,
        _residency: RasterResidency,
        _ctx: &mut dyn RenderContext,
    ) -> RasterImage {
        let scale_x = target.width as f32 / size.0;
        let scale_y = target.height as f32 / size.1;
        // A `SIDE`-by-`SIDE` logical square, centered in the logical box.
        let half = Self::SIDE * 0.5;
        let cx = size.0 * 0.5;
        let cy = size.1 * 0.5;
        let px0 = ((cx - half) * scale_x).round() as i64;
        let px1 = ((cx + half) * scale_x).round() as i64;
        let py0 = ((cy - half) * scale_y).round() as i64;
        let py1 = ((cy + half) * scale_y).round() as i64;
        let w = target.width as usize;
        let h = target.height as usize;
        let mut pixels = vec![0u8; w * h * 4];
        for y in 0..h as i64 {
            for x in 0..w as i64 {
                if (px0..px1).contains(&x) && (py0..py1).contains(&y) {
                    let i = ((y as usize) * w + (x as usize)) * 4;
                    pixels[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
                }
            }
        }
        RasterImage::cpu(target.width, target.height, PixelFormat::Rgba8, pixels)
    }
}

// Width / height (in pixels) of the opaque square painted into `image`.
fn opaque_span(image: &crate::raster::CpuRasterImage) -> (u32, u32) {
    let w = image.width as usize;
    let h = image.height as usize;
    let mut min_x = w;
    let mut max_x = 0usize;
    let mut min_y = h;
    let mut max_y = 0usize;
    for y in 0..h {
        for x in 0..w {
            if image.pixels[(y * w + x) * 4 + 3] != 0 {
                min_x = min_x.min(x);
                max_x = max_x.max(x);
                min_y = min_y.min(y);
                max_y = max_y.max(y);
            }
        }
    }
    ((max_x + 1 - min_x) as u32, (max_y + 1 - min_y) as u32)
}

#[test]
fn blanket_frame_lays_out_against_the_canvas_not_the_intrinsic_box() {
    // FIX 1: `frame` must lay a timeless visual out against the CANVAS (the
    // target's logical size, 1:1 with the pixel resolution), NOT its
    // intrinsic box. A 16:9 target with a 1:1 logical→pixel mapping keeps a
    // logical square square; sizing against the 4:1 intrinsic box would
    // stretch it horizontally ~7.1x (16:9 ÷ 4:1) in pixels.
    let table = TriggerTable::new();
    let clock = Clock::new(TimelineTime::new(0.0), LocalTime::new(0.0), &table);
    let mut ctx = PassThrough;
    let target = Resolution::new(1280, 720); // 16:9
    let canvas = Vec2(target.width as f32, target.height as f32);
    let frame = Square
        .frame(clock, canvas, target, RasterResidency::Cpu, &mut ctx)
        .expect("a visual contributes a frame");
    let cpu = frame.as_cpu().expect("cpu image");
    let (span_w, span_h) = opaque_span(cpu);
    // The square is rendered 1:1 (canvas == target), so its pixel span is
    // square within rounding. A stretch would make `span_w` ~7x `span_h`.
    let diff = (span_w as i32 - span_h as i32).abs();
    assert!(
        diff <= 2,
        "square must stay square (no aspect stretch): {span_w}x{span_h}",
    );
}

// A visual that paints OUTSIDE its layout box, like a drop shadow: its
// `paint_bounds` carry a `MARGIN` on every side, and `render` — following
// `composite_children`'s contract, where `target` pixels span
// `paint_bounds(size)` — paints one opaque marker pixel at the logical
// canvas origin (0,0).
#[derive(PartialEq, Hash)]
struct Margined;

impl Margined {
    const MARGIN: f32 = 4.0;
}

impl RasterComponent for Margined {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        constraints.constrain(Vec2(8.0, 8.0))
    }

    fn paint_bounds(&self, size: Vec2) -> Rect {
        Rect {
            origin: Vec2(-Self::MARGIN, -Self::MARGIN),
            size: Vec2(size.0 + 2.0 * Self::MARGIN, size.1 + 2.0 * Self::MARGIN),
        }
    }

    fn render(
        &self,
        size: Vec2,
        target: Resolution,
        _residency: RasterResidency,
        _ctx: &mut dyn RenderContext,
    ) -> RasterImage {
        let bounds = self.paint_bounds(size);
        let sx = target.width as f32 / bounds.size.0;
        let sy = target.height as f32 / bounds.size.1;
        let px = ((0.0 - bounds.origin.0) * sx).round() as usize;
        let py = ((0.0 - bounds.origin.1) * sy).round() as usize;
        let w = target.width as usize;
        let h = target.height as usize;
        let mut pixels = vec![0u8; w * h * 4];
        let i = (py.min(h - 1) * w + px.min(w - 1)) * 4;
        pixels[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
        RasterImage::cpu(target.width, target.height, PixelFormat::Rgba8, pixels)
    }
}

#[test]
fn blanket_frame_gives_paint_bounds_pixels_and_composites_at_the_painted_origin() {
    // A shadow-like visual paints beyond its layout box. `frame` must hand it
    // a pixel target spanning its `paint_bounds` and composite the result at
    // the painted origin, clipping spill at the frame edge — handing it the
    // frame's own `target` would squeeze the wider paint bounds into the
    // canvas-sized pixel grid, shrinking and shifting everything it painted
    // toward the center.
    let table = TriggerTable::new();
    let clock = Clock::new(TimelineTime::new(0.0), LocalTime::new(0.0), &table);
    let mut ctx = PassThrough;
    let target = Resolution::new(8, 8);
    let canvas = Vec2(8.0, 8.0);
    let frame = Margined
        .frame(clock, canvas, target, RasterResidency::Cpu, &mut ctx)
        .expect("a visual contributes a frame");
    let cpu = frame.as_cpu().expect("cpu image");
    assert_eq!(
        (cpu.width, cpu.height),
        (8, 8),
        "the frame stays target-sized regardless of the visual's paint bounds",
    );
    // The marker painted at logical (0,0) must land at frame pixel (0,0).
    // The squeezed path maps it to ((0 + MARGIN) * 8/16, …) = (2, 2).
    assert_eq!(cpu.pixels[3], 255, "logical (0,0) lands at frame (0,0)");
    let squeezed = (2 * 8 + 2) * 4 + 3;
    assert_eq!(
        cpu.pixels[squeezed], 0,
        "content must not be squeezed toward the center",
    );
}

#[test]
fn timed_and_triggers_blankets_apply_to_a_visual() {
    // `.at(..)` and `.trigger_*` must be reachable on a RasterComponent.
    let placed = Dot.at(0.0..2.0);
    assert_eq!(placed.duration(), Some(2.0));
    assert_eq!(placed.measure(), Some(2.0));

    let e = Event::new();
    let triggered = Dot.trigger_at_start(e);
    assert_eq!(triggered.event(), e);
}

#[test]
fn event_new_gives_distinct_ids() {
    let a = Event::new();
    let b = Event::new();
    let c = Event::new();
    assert_ne!(a, b);
    assert_ne!(b, c);
    assert_ne!(a, c);
}

#[test]
fn trigger_table_earliest_wins_and_absent_is_infinity() {
    let e = Event::new();
    let mut table = TriggerTable::new();

    // Absent → +∞.
    assert!(table.get(e.id()).seconds().is_infinite());

    // First write registers; a later, larger write does not override.
    table.record(e, 5.0);
    table.record(e, 8.0);
    assert_eq!(table.get(e.id()).seconds(), 5.0);

    // An earlier write does override (earliest-wins).
    table.record(e, 2.0);
    assert_eq!(table.get(e.id()).seconds(), 2.0);
}

#[test]
fn unfired_event_phase_is_zero_not_nan() {
    // `.sketch/02 §11`: an unfired (+∞) trigger short-circuits to Phase::ZERO.
    let e = Event::new();
    let table = TriggerTable::new();
    let clock = Clock::new(TimelineTime::new(3.0), LocalTime::new(3.0), &table);
    assert_eq!(e.phase(&clock, 0.0, 0.5), Phase::ZERO);
    assert!(e.is_before(&clock));
    assert!(!e.is_after(&clock));
}

#[test]
fn fired_event_phase_tracks_the_global_clock() {
    let e = Event::new();
    let mut table = TriggerTable::new();
    table.record(e, 2.0);
    // At t = 2.25, phase over [2.0, 2.5] is halfway.
    let clock = Clock::new(TimelineTime::new(2.25), LocalTime::new(0.0), &table);
    assert_eq!(e.phase(&clock, 0.0, 0.5).get(), 0.5);
    assert!(e.is_after(&clock));
}

#[test]
fn event_elapsed_tracks_the_global_clock() {
    let e = Event::new();
    let mut table = TriggerTable::new();
    table.record(e, 2.0);

    let before = Clock::new(TimelineTime::new(1.5), LocalTime::new(99.0), &table);
    assert_eq!(e.elapsed(&before), 0.0);

    let after = Clock::new(TimelineTime::new(2.25), LocalTime::new(0.0), &table);
    assert_eq!(e.elapsed(&after), 0.25);
}

#[test]
fn unfired_event_elapsed_is_zero() {
    let e = Event::new();
    let table = TriggerTable::new();
    let clock = Clock::new(TimelineTime::new(3.0), LocalTime::new(3.0), &table);
    assert_eq!(e.elapsed(&clock), 0.0);
}

#[test]
fn event_window_unfired_reports_the_before_state_with_a_stable_key() {
    let e = Event::new();
    let table = TriggerTable::new();
    let early = Clock::new(TimelineTime::new(3.0), LocalTime::new(3.0), &table);
    let late = Clock::new(TimelineTime::new(50.0), LocalTime::new(50.0), &table);

    let w_early = e.window(&early, 0.2..0.9);
    let w_late = e.window(&late, 0.2..0.9);

    // An unfired (+∞) trigger cannot be shifted into absolute seconds, so the
    // window reports the same "before it opens" snapshot regardless of `now`
    // — otherwise it could not be a stable cache-key term (`.sketch/02 §11`).
    assert_eq!(w_early, w_late);
    assert_eq!(w_early.phase().get(), 0.0);
    assert_eq!(w_early.envelope(0.1, 0.1).get(), 0.0);
}

#[test]
fn event_window_before_the_trigger_is_clamped_to_start() {
    let e = Event::new();
    let mut table = TriggerTable::new();
    table.record(e, 5.0);
    // Trigger at 5.0 ⇒ window [5.5, 6.5); `now` = 4.0 is before it opens.
    let clock = Clock::new(TimelineTime::new(4.0), LocalTime::new(0.0), &table);

    let w = e.window(&clock, 0.5..1.5);
    assert_eq!(w.phase().get(), 0.0);
    assert_eq!(w.envelope(0.2, 0.2).get(), 0.0);
}

#[test]
fn event_window_inside_tracks_the_trigger_relative_cursor() {
    let e = Event::new();
    let mut table = TriggerTable::new();
    table.record(e, 5.0);
    // Window [trigger + 0.5, trigger + 1.5) = [5.5, 6.5); `now` = 6.0 is
    // halfway through.
    let clock = Clock::new(TimelineTime::new(6.0), LocalTime::new(0.0), &table);

    let w = e.window(&clock, 0.5..1.5);
    assert!((w.phase().get() - 0.5).abs() < 1e-6);
}

#[test]
fn event_window_after_is_clamped_to_end_with_a_stable_key() {
    let e = Event::new();
    let mut table = TriggerTable::new();
    table.record(e, 5.0);

    let just_after = Clock::new(TimelineTime::new(7.0), LocalTime::new(0.0), &table);
    let long_after = Clock::new(TimelineTime::new(500.0), LocalTime::new(0.0), &table);

    let w_just_after = e.window(&just_after, 0.5..1.5);
    let w_long_after = e.window(&long_after, 0.5..1.5);

    // Both are past the window's close (6.5); the clamped snapshot is
    // identical however far past, matching `Window::clamped`'s own guarantee
    // (`clamped_freezes_outside_the_window` in `window.rs`).
    assert_eq!(w_just_after, w_long_after);
    assert_eq!(w_just_after.phase().get(), 1.0);
}

#[test]
#[should_panic(expected = "Event::window requires a finite range with end > start")]
fn event_window_rejects_empty_range() {
    let e = Event::new();
    let table = TriggerTable::new();
    let clock = Clock::new(TimelineTime::new(0.0), LocalTime::new(0.0), &table);
    let _ = e.window(&clock, 1.0..1.0);
}

#[test]
fn triggered_resolve_records_start_time() {
    let e = Event::new();
    let triggered = Dot.trigger_at_start(e);
    let mut ctx = ResolveCtx::new();
    triggered.resolve(4.0, &mut ctx);
    let table = ctx.into_triggers();
    assert_eq!(table.get(e.id()).seconds(), 4.0);
}

// ── `#[component(timeline)]` macro arm (step 2) ──────────────────────────
//
// COMPILE-FAIL (no trybuild in this repo — verified manually): `#[clock]` on
// a non-timeline arm is rejected by the macro. Uncomment to reproduce; it
// fails with "#[clock] is only valid on a #[component(timeline)]":
//
//     #[crate::component(raster)]
//     fn BadClock(#[clock] clock: Clock, x: f32) -> impl RasterComponent {
//         let _ = (clock, x);
//         unimplemented!()
//     }

// A timeline component WITHOUT `#[clock]`: builds a `Placed` and delegates
// the full query set to it (audit M3). `start` becomes a builder field.
#[crate::component(timeline)]
fn Beat(start: f32) -> impl TimelineComponent {
    Dot.at(start..(start + 2.0))
}

// A timeline component WITH `#[clock]`: the clock is injected (not a field /
// cache-key term) and forwarded into the body. The structure (a placed
// `Dot`) is clock-independent; only the read value would vary per frame.
#[crate::component(timeline)]
fn Pulse(#[clock] clock: Clock, start: f32) -> impl TimelineComponent {
    // Read both axes to prove the real clock threads through `frame`.
    let _ = clock.local().seconds() + clock.global().seconds();
    Dot.at(start..(start + 1.0))
}

// A timeline component with an explicit `name = "..."` template: the
// `{label}`/`{take}` placeholders interpolate the STORED builder fields at
// arrangement-time, so two instances surface distinct display names.
#[crate::component(timeline, name = "Shot {take}: {label}")]
fn NamedBeat(#[builder(into)] label: String, take: u32, start: f32) -> impl TimelineComponent {
    // The body destructures every stored field; `label`/`take` feed only the
    // name template, so silence the body-side "unused" lint for them.
    let _ = (&label, take);
    Dot.at(start..(start + 2.0))
}

// Generic acceptors that only hold if the bounds are met — these are the
// load-bearing assertions: the generated types implement the right traits.
fn assert_timeline_component<T: TimelineComponent>(_: &T) {}
fn assert_timeline_builder<B: TimelineBuilder>(_: &B) {}
fn assert_boxable<T: TimelineComponent + Send + 'static>(
    value: T,
) -> Box<dyn TimelineComponent + Send> {
    Box::new(value)
}

#[test]
fn timeline_component_without_clock_delegates_full_query_set() {
    let beat = Beat::builder().start(3.0).build();
    assert_timeline_component(&beat);

    // The clock-less queries build with a structural clock and delegate to
    // the inner `Placed` (window `3.0..5.0`) — so the wrapper is transparent
    // to resolve (M3): `duration` = window length, `measure` = window end.
    assert_eq!(beat.duration(), Some(2.0));
    assert_eq!(beat.measure(), Some(5.0));
    assert_eq!(beat.cues(0.0), Vec::new());
    let node = beat.arrangement(0.0);
    assert_eq!(node.kind, NodeKind::Video);
    // The delegated node is relabeled with the auto-derived component name
    // (the PascalCase ident), not the inner body's empty caption label.
    assert_eq!(node.name.as_deref(), Some("Beat"));
    assert_eq!(node.label, String::new());

    let mut ctx = ResolveCtx::new();
    // resolve recurses into the placed child at its relative start.
    assert_eq!(beat.resolve(0.0, &mut ctx), 2.0);

    // The complete builder is a `TimelineBuilder` and boxes with `+ Send`.
    let builder = Beat::builder().start(3.0);
    assert_timeline_builder(&builder);
    let _boxed: Box<dyn TimelineComponent + Send> = assert_boxable(beat);
}

#[test]
fn timeline_component_name_template_interpolates_stored_fields() {
    // Two instances with distinct stored fields surface distinct names.
    let a = NamedBeat::builder()
        .label("intro")
        .take(1)
        .start(0.0)
        .build();
    let b = NamedBeat::builder()
        .label("outro")
        .take(7)
        .start(0.0)
        .build();
    assert_eq!(a.arrangement(0.0).name.as_deref(), Some("Shot 1: intro"));
    assert_eq!(b.arrangement(0.0).name.as_deref(), Some("Shot 7: outro"));
    // The template only relabels — it adds no tree level and preserves kind.
    assert_eq!(a.arrangement(0.0).kind, NodeKind::Video);
    assert!(a.arrangement(0.0).children.is_empty());
}

#[test]
fn timeline_component_with_clock_forwards_the_real_clock() {
    let pulse = Pulse::builder().start(1.0).build();
    assert_timeline_component(&pulse);

    // `frame` forwards the framework-supplied clock into the body.
    let table = TriggerTable::new();
    // Sample INSIDE the body's window `[1.0, 2.0)` — the placed `Dot` is now
    // temporally gated, so a pre-window local would (correctly) contribute
    // nothing; an interior local still proves the real clock threads through.
    let clock = Clock::new(TimelineTime::new(1.25), LocalTime::new(1.25), &table);
    let mut ctx = PassThrough;
    let frame = pulse.frame(
        clock,
        Vec2(4.0, 4.0),
        Resolution::new(4, 4),
        RasterResidency::Cpu,
        &mut ctx,
    );
    assert!(frame.is_some());

    // Clock-less queries still resolve via the structural clock.
    assert_eq!(pulse.duration(), Some(1.0));

    let builder = Pulse::builder().start(1.0);
    assert_timeline_builder(&builder);
    let _boxed: Box<dyn TimelineComponent + Send> = assert_boxable(pulse);
}

#[test]
fn clock_window_surfaces_the_local_interval() {
    let table = TriggerTable::new();
    let base = Clock::new(TimelineTime::new(0.0), LocalTime::new(0.0), &table);

    // No window ⇒ open-ended.
    let open = base.with_local_window(LocalTime::new(5.0), None);
    assert!(open.window().is_none());

    // A 3s window surfaces as [0, 3) over the local axis; Window's own
    // vocabulary takes over from there (remaining / envelope / phase —
    // behaviour tested in window.rs).
    let at = |t: f32| base.with_local_window(LocalTime::new(t), Some(3.0));
    let w = at(1.0).window().expect("windowed");
    assert_eq!(w.start(), 0.0);
    assert_eq!(w.end(), 3.0);
    assert_eq!(w.current(), 1.0);
    assert!((w.remaining() - 2.0).abs() < 1e-6);
    assert_eq!(at(4.0).window().unwrap().remaining(), 0.0);
    assert_eq!(at(0.0).window().unwrap().envelope(0.5, 0.5).get(), 0.0);
    assert!(
        (at(1.5).window().unwrap().envelope(0.5, 0.5).get() - 1.0).abs() < 1e-6,
        "held at full between the fades"
    );
}

#[test]
fn timeline_clock_is_excluded_from_the_cache_key() {
    // `#[clock]` is stripped, so two `Pulse`s with equal fields are equal
    // and hash identically regardless of any clock (mirrors `#[available]`).
    let a = Pulse::builder().start(1.0).build();
    let b = Pulse::builder().start(1.0).build();
    assert!(a == b);

    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut ha = DefaultHasher::new();
    let mut hb = DefaultHasher::new();
    a.hash(&mut ha);
    b.hash(&mut hb);
    assert_eq!(ha.finish(), hb.finish());
}

// A complete builder also boxes into `Box<dyn TimelineComponent + Send>`
// via the per-builder `From` glue (no explicit `.build()`).
#[test]
fn timeline_complete_builder_boxes_via_from() {
    let boxed: Box<dyn TimelineComponent + Send> = Beat::builder().start(0.0).into();
    assert_eq!(boxed.duration(), Some(2.0));
}

// ── Resolve entry (step 3) ───────────────────────────────────────────────

#[test]
fn resolve_visual_in_a_window_gives_window_duration() {
    // A timeless visual placed in an explicit window resolves to that
    // window's length, and the resolved tree owns the source whole.
    let resolved = resolve(Dot.at(0.0..3.0)).expect("a windowed visual is not timeless");
    assert_eq!(resolved.duration(), 3.0);
    // The source is owned intact and still queryable through the accessor.
    assert_eq!(resolved.source().duration(), Some(3.0));
    assert!(resolved.warnings().is_empty());
}

#[test]
fn resolve_bare_timeless_visual_is_an_error() {
    // A bare visual (no placement window) is purely timeless: `measure()`
    // is `None`, so the root resolves to `Err(Timeless)` rather than 0.0
    // (audit M4).
    let err = resolve(Dot).expect_err("a bare visual has no intrinsic length");
    assert!(matches!(err, ResolveError::Timeless));
}

#[test]
fn resolve_records_trigger_absolute_start() {
    // A `Triggered` visual fires `trigger_at_start` at the wrapped node's
    // absolute start. As the root the start is 0.0, recorded in the
    // resolved table; the resolved root length is the placement window.
    let e = Event::new();
    let resolved =
        resolve(Dot.at(5.0..8.0).trigger_at_start(e)).expect("windowed, so not timeless");
    assert_eq!(resolved.triggers().get(e.id()).seconds(), 0.0);
    assert_eq!(resolved.duration(), 8.0);
}

#[test]
fn resolve_scales_interior_trigger_under_window_stretch() {
    // A 2s child placed into a 1s window plays at speed 2, so an INTERIOR
    // trigger at the child's local 1.0s fires at parent-clock 0.5s — not the
    // un-stretched 1.0s. (`trigger_at` first, so the `Triggered` is INSIDE
    // the stretched `Placed`.)
    let e = Event::new();
    let resolved = resolve(
        Beat::builder()
            .start(0.0)
            .build()
            .trigger_at(1.0, e)
            .at(0.0..1.0),
    )
    .expect("windowed timed child is not timeless");
    assert_eq!(resolved.duration(), 1.0);
    assert!(
        (resolved.triggers().get(e.id()).seconds() - 0.5).abs() < 1e-6,
        "interior trigger should compress to 0.5s, got {}",
        resolved.triggers().get(e.id()).seconds()
    );
}

#[test]
fn resolve_zero_length_window_is_an_error() {
    // A zero-length `.at(a..a)` window has no determinate speed; resolve
    // rejects it rather than silently degrading to speed 1.0.
    let err = resolve(Dot.at(2.0..2.0)).expect_err("zero-length window is invalid");
    assert!(matches!(err, ResolveError::Invalid(_)));
}

#[test]
fn resolve_records_trigger_at_end_uses_resolved_length() {
    // `trigger_at_end` fires at the wrapped node's absolute start + its
    // resolved length. A 3-second window (5.0..8.0) at root start 0.0 fires
    // at 3.0 — proving the absolute time is folded from the place walk.
    let e = Event::new();
    let resolved = resolve(Dot.at(5.0..8.0).trigger_at_end(e)).expect("windowed, so not timeless");
    assert_eq!(resolved.triggers().get(e.id()).seconds(), 3.0);
}

#[test]
fn resolve_trigger_earliest_wins_across_two_writers() {
    // Two `Triggered` wrappers over the SAME event at different absolute
    // times: the EARLIEST recorded time wins. A `Sequence` would place
    // these in a row (step 4); with no containers yet we drive the place
    // walk directly with distinct `abs_start`s to exercise earliest-wins.
    let e = Event::new();
    let mut ctx = ResolveCtx::new();
    // Later writer first (abs_start 7.0), then the earlier one (abs_start 2.0).
    Dot.at(0.0..2.0).trigger_at_start(e).resolve(7.0, &mut ctx);
    Dot.at(0.0..2.0).trigger_at_start(e).resolve(2.0, &mut ctx);
    let table = ctx.into_triggers();
    assert_eq!(table.get(e.id()).seconds(), 2.0);
}

#[test]
fn resolved_timeline_is_send() {
    // `ResolvedTimeline` is stored in the plugin collection and moved across
    // threads (audit M2) — assert it at the value level too.
    fn assert_send<T: Send>(_: &T) {}
    let resolved = resolve(Dot.at(0.0..1.0)).unwrap();
    assert_send(&resolved);
}
