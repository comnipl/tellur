//! The timeline containers and leaves — STEP 4.
//!
//! This module lands the authoring surface sketched in `.sketch/01-timeline-api.rs`
//! (A.2 leaves, A.4 containers) on top of the [`TimelineComponent`] contract and
//! the [`resolve`](crate::timeline_component::resolve) pass committed in steps
//! 1–3. It mirrors the SPATIAL side of the library on purpose:
//!
//! | space (`layout.rs` / `layer.rs`)        | time (this module)                |
//! |-----------------------------------------|-----------------------------------|
//! | `Layer` (overlay children)              | [`Timeline`] (overlay in time)    |
//! | `Stack` (lay along an axis, cursor)     | [`Sequence`] (lay one-after-another) |
//!
//! Both containers are struct-form `#[component(timeline)]` (builder + glue
//! only, NO trait impl from the macro) plus a hand-written
//! `impl TimelineComponent`, exactly as raster `Stack` is a
//! `#[component(raster)] struct` + hand-written `impl RasterComponent`.
//!
//! The leaves ([`VideoFile`], [`AudioFile`], [`Subtitle`]) are buildless
//! builders. Media DECODE is steps 8/9; here their length comes from a stubbed
//! [`probe`](VideoFile::probe) seam (a caller-injectable `duration`), and
//! `frame` / `samples` stay `None` placeholders.

use crate::composite::composite_frame_over;
use crate::raster::{RasterImage, Resolution};
use crate::render_context::RenderContext;
use crate::time::{LocalTime, Time};
use crate::timeline_component::{
    Arrangement, AudioBuffer, Clock, Cue, NodeKind, Placed, ResolveCtx, TimelineComponent,
};

// ── Containers ───────────────────────────────────────────────────────────────

/// Overlay container — the temporal twin of [`Layer`](crate::layer::Layer).
///
/// Children combine (visuals composite source-over, audio mixes, cues merge),
/// each placed by an absolute `.at(..)` (default `0.0`) or `.fill()`. There is
/// NO cursor: every child shares the same base start. The resolved length is
/// the latest NON-fill child end; `.fill()` children then take that length in a
/// second sub-pass (`.sketch/02 §5/§6`).
///
/// Struct-form `#[component(timeline)]`: the macro emits the buildless `bon`
/// builder, the `From<..> for Box<dyn TimelineComponent + Send>` glue, and the
/// `TimelineBuilder` marker — but NO trait impl. The behaviour is the
/// hand-written `impl TimelineComponent` below.
#[crate::component(timeline)]
#[derive(crate::Keyable)]
pub struct Timeline {
    // `#[builder(field)]` members (the streamed children) must precede the
    // setter members, per bon's member-ordering rule (mirrors raster `Stack`).
    #[children(each = child)]
    pub children: Vec<Box<dyn TimelineComponent + Send>>,
}

impl Timeline {
    /// The non-fill children, as `Placed` views when the child is a placement.
    /// A child that is not a `Placed` (e.g. a bare visual / a fn-form timeline
    /// component) is treated as a non-fill, start-0.0 child.
    fn classify(&self) -> impl Iterator<Item = ChildView<'_>> {
        self.children.iter().map(|child| {
            // Downcast THROUGH the trait object (`child.as_ref()`), not the
            // `Box` itself: `Box<dyn TimelineComponent + Send>` is *itself*
            // `DynEq` (it is `PartialEq + Any`), so `box.as_any()` would erase
            // the box, not the inner `Placed`. Dispatching `as_any` on the
            // `dyn` object routes through the vtable to the concrete leaf.
            let obj: &(dyn TimelineComponent + Send) = child.as_ref();
            match obj.as_any().downcast_ref::<Placed>() {
                Some(placed) if placed.is_fill() => ChildView::Fill(placed),
                Some(placed) => ChildView::PlacedAt(placed),
                None => ChildView::Bare(obj),
            }
        })
    }
}

/// How a [`Timeline`] sees one child during measure / place.
enum ChildView<'a> {
    /// A `.fill()` placement — excluded from the length measure; resolved to the
    /// container length in the second sub-pass.
    Fill(&'a Placed),
    /// A `.at(..)` placement — measured / placed at its relative start.
    PlacedAt(&'a Placed),
    /// A non-placement child (a bare visual etc.) — start 0.0, its own measure.
    Bare(&'a (dyn TimelineComponent + Send)),
}

impl TimelineComponent for Timeline {
    fn measure(&self) -> Option<f32> {
        // max over the NON-fill children of each child's measure footprint
        // (relative start folded in by `Placed::measure`). Fill children are
        // EXCLUDED (the load-bearing acyclicity invariant, `.sketch/02 §5`). If
        // no non-fill child has a length, the container is timeless (`None`).
        let mut acc: Option<f32> = None;
        for view in self.classify() {
            let m = match view {
                ChildView::Fill(_) => continue,
                ChildView::PlacedAt(placed) => placed.measure(),
                ChildView::Bare(child) => child.measure(),
            };
            if let Some(end) = m {
                acc = Some(acc.map_or(end, |cur: f32| cur.max(end)));
            }
        }
        acc
    }

    fn resolve(&self, abs_start: f32, out: &mut ResolveCtx) -> f32 {
        // Sub-pass 1 — non-fill children: place each at its relative start and
        // fold the max end into the container length.
        let mut length = 0.0_f32;
        let mut saw_non_fill = false;
        let mut saw_fill = false;
        for view in self.classify() {
            match view {
                ChildView::Fill(_) => {
                    saw_fill = true;
                }
                ChildView::PlacedAt(placed) => {
                    saw_non_fill = true;
                    // `Placed::resolve` recurses at the relative start and
                    // returns the placed length; its footprint end is
                    // `relative_start + placed_len`.
                    let len = placed.resolve(abs_start, out);
                    length = length.max(placed.placement().start() + len);
                }
                ChildView::Bare(child) => {
                    saw_non_fill = true;
                    let len = child.resolve(abs_start, out);
                    length = length.max(len);
                }
            }
        }

        // An all-fill / empty interior collapses to a determinate `0.0` — almost
        // always an authoring mistake, so warn (`.sketch/02 §9`).
        if saw_fill && !saw_non_fill {
            out.warn(
                "Timeline has no non-fill child to set its length; \
                 it collapses to 0.0 (place media or an explicit window)",
            );
        }

        // Sub-pass 2 — fill children take the resolved container length. Recurse
        // into the INNER component (the fill `Placed` is a start-0.0 wrapper) so
        // its triggers / cues compose at the container base.
        for view in self.classify() {
            if let ChildView::Fill(placed) = view {
                placed.child().resolve(abs_start, out);
            }
        }

        length
    }

    fn cues(&self, offset: f32) -> Vec<Cue> {
        // Concat children cues at `offset + child relative start`. `Placed::cues`
        // already adds its own relative start and stamps a windowed timeless
        // child's interval. A FILL child has no window, so its interval is the
        // container's resolved length — stamp that here (`.sketch/02 §10`).
        let fill_len = self.measure().unwrap_or(0.0);
        let mut cues = Vec::new();
        for view in self.classify() {
            match view {
                ChildView::Fill(placed) => {
                    let mut child_cues = placed.child().cues(offset);
                    if placed.child().duration().is_none() {
                        let abs_end = offset + fill_len;
                        for cue in &mut child_cues {
                            cue.end = abs_end;
                        }
                    }
                    cues.extend(child_cues);
                }
                ChildView::PlacedAt(placed) => cues.extend(placed.cues(offset)),
                ChildView::Bare(child) => cues.extend(child.cues(offset)),
            }
        }
        cues
    }

    fn frame(
        &self,
        clock: Clock<'_>,
        target: Resolution,
        ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage> {
        // Overlay: every child shares the container's base, so each sees the
        // SAME clock (relative offset 0). A `.at(start..)` child is a `Placed`
        // whose own `frame` rebases by its relative start; a `.fill()` child
        // spans the whole container (relative start 0, local unchanged); a bare
        // child is at 0 too. Recurse each child and source-over composite the
        // resulting frames at the IMAGE layer (`.sketch/02 §8`), in child order
        // so later children land on top. `None` frames are dropped.
        let mut acc: Option<RasterImage> = None;
        for child in &self.children {
            if let Some(img) = child.frame(clock, target, ctx) {
                acc = Some(composite_frame_over(acc, img, target, ctx));
            }
        }
        acc
    }

    fn samples(&self, clock: Clock<'_>, window: f32) -> Option<AudioBuffer> {
        // TODO(task 7): mix children. Placeholder: no audio yet.
        let _ = (clock, window);
        None
    }

    fn arrangement(&self) -> Arrangement {
        // TODO(task 6): stamp resolved start/end via a resolve walk; the start /
        // end here are placeholders the live UI fills from the resolved tree.
        let children = self.children.iter().map(|c| c.arrangement()).collect();
        Arrangement {
            kind: NodeKind::Timeline,
            label: String::new(),
            start: 0.0,
            end: self.measure().unwrap_or(0.0),
            trim: None,
            triggers: Vec::new(),
            children,
        }
    }
}

/// In-a-row container — the temporal twin of [`Stack`](crate::layout::raster::Stack).
///
/// Lays children one after another: child N starts at child N-1's resolved end.
/// RE-FLOW falls out for free — the cursor is recomputed from the children's
/// current lengths every resolve, so a length change shifts everything after it
/// (`.sketch/02 §6`). Mirrors `compute_stack_pass`'s `Start` branch.
///
/// A `.fill()` child here is a RESOLVE error (ZONE C #1): a `Sequence` imposes
/// no container length for the fill to take, the same reason the spatial `Stack`
/// has no main-axis `Fill`. Span the row from the parent overlay `Timeline`
/// instead. `.fill()` is a runtime `Placed` value, so this is caught at resolve
/// time (via [`ResolveCtx::error`]), not by the type system.
#[crate::component(timeline)]
#[derive(crate::Keyable)]
pub struct Sequence {
    #[children(each = child)]
    pub children: Vec<Box<dyn TimelineComponent + Send>>,
    /// Gap inserted between consecutive children (seconds). `0.0` by default.
    #[builder(default)]
    pub spacing: f32,
}

impl Sequence {
    /// Whether `child` is a `.fill()` placement (rejected inside a `Sequence`).
    fn is_fill(child: &(dyn TimelineComponent + Send)) -> bool {
        child
            .as_any()
            .downcast_ref::<Placed>()
            .is_some_and(Placed::is_fill)
    }
}

impl TimelineComponent for Sequence {
    fn measure(&self) -> Option<f32> {
        // Σ over the NON-fill children of each child's length, plus spacing
        // between them. A fill child contributes nothing (it is an error at
        // place time). All-`None` children ⇒ the sequence is timeless.
        let mut total = 0.0_f32;
        let mut any = false;
        let mut count = 0usize;
        for child in &self.children {
            if Self::is_fill(child.as_ref()) {
                continue;
            }
            count += 1;
            if let Some(len) = child.measure() {
                total += len;
                any = true;
            }
        }
        if !any {
            return None;
        }
        let gaps = count.saturating_sub(1) as f32;
        Some(total + self.spacing * gaps)
    }

    fn resolve(&self, abs_start: f32, out: &mut ResolveCtx) -> f32 {
        // Cursor from 0: child N starts at `abs_start + Σ prior lengths (+gaps)`,
        // the time version of `compute_stack_pass`'s Start branch. A `.fill()`
        // child is a fatal error — record it and skip its length contribution.
        let mut cursor = 0.0_f32;
        let mut placed_any = false;
        for child in &self.children {
            if Self::is_fill(child.as_ref()) {
                out.error(
                    ".fill() is not allowed inside a Sequence (it has no container \
                     length to take); use .at(..) here, or .fill() in a parent Timeline",
                );
                continue;
            }
            if placed_any {
                cursor += self.spacing;
            }
            let len = child.resolve(abs_start + cursor, out);
            cursor += len;
            placed_any = true;
        }
        cursor
    }

    fn cues(&self, offset: f32) -> Vec<Cue> {
        // Re-flow the cursor the same way `resolve` does so cues land at the
        // child's absolute start. Fill children are skipped (an error).
        let mut cues = Vec::new();
        let mut cursor = 0.0_f32;
        let mut placed_any = false;
        for child in &self.children {
            if Self::is_fill(child.as_ref()) {
                continue;
            }
            if placed_any {
                cursor += self.spacing;
            }
            cues.extend(child.cues(offset + cursor));
            cursor += child.measure().unwrap_or(0.0);
            placed_any = true;
        }
        cues
    }

    fn frame(
        &self,
        clock: Clock<'_>,
        target: Resolution,
        ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage> {
        // Re-flow the cursor exactly as `resolve` / `cues` do, then hand child N
        // a clock rebased to its slot: `local = clock.local() - cursorN`
        // (`.sketch/02 §8`). Each child's own `local` starts at 0 at the moment
        // it begins, so a clip after a 2s clip sees `local ≈ 0` at global t=2.0.
        // Composite the recursed frames source-over at the image layer (in a row
        // usually only the active child contributes, but composite generally).
        let mut cursor = 0.0_f32;
        let mut placed_any = false;
        let mut acc: Option<RasterImage> = None;
        for child in &self.children {
            if Self::is_fill(child.as_ref()) {
                // `.fill()` inside a Sequence is a resolve error; a valid
                // sampled tree never reaches here, but skip it defensively so
                // it neither shifts the cursor nor draws.
                continue;
            }
            if placed_any {
                cursor += self.spacing;
            }
            let child_clock = clock.with_local(LocalTime::new(clock.local().seconds() - cursor));
            if let Some(img) = child.frame(child_clock, target, ctx) {
                acc = Some(composite_frame_over(acc, img, target, ctx));
            }
            cursor += child.measure().unwrap_or(0.0);
            placed_any = true;
        }
        acc
    }

    fn samples(&self, clock: Clock<'_>, window: f32) -> Option<AudioBuffer> {
        // TODO(task 7): concat children audio along the cursor.
        let _ = (clock, window);
        None
    }

    fn arrangement(&self) -> Arrangement {
        let children = self.children.iter().map(|c| c.arrangement()).collect();
        Arrangement {
            kind: NodeKind::Sequence,
            label: String::new(),
            start: 0.0,
            end: self.measure().unwrap_or(0.0),
            trim: None,
            triggers: Vec::new(),
            children,
        }
    }
}

// ── Leaves ───────────────────────────────────────────────────────────────────

/// A placeholder media duration for the stubbed [`probe`](VideoFile::probe)
/// seam. Real decode (steps 8/9) replaces this with a header read
/// (`.sketch/02 §12`): video via an `ffmpeg`/`ffprobe` child process, audio via
/// `symphonia`. Until then both media leaves report this fixed length unless a
/// test injects one via `duration`.
const STUB_PROBE_SECONDS: f32 = 1.0;

/// Decoded video — the visual channel. Built with
/// `VideoFile::builder().path("x.mp4")`.
///
/// Its intrinsic length is the file's, read once by the resolve pass. DECODE IS
/// STUBBED here (step 9): [`probe`](Self::probe) returns the injected
/// `duration` if set, else [`STUB_PROBE_SECONDS`]. `frame` is a `None`
/// placeholder until the decode backend lands.
#[crate::component(timeline)]
// `Clone` so the leaf can be a field of a `#[component(timeline)]` fn (e.g. the
// `.sketch/01` `Dialogue(voice: AudioFile)`): the macro clones `self` to
// destructure the body's fields, so every component field type must be `Clone`.
#[derive(Clone, crate::Keyable)]
pub struct VideoFile {
    #[builder(into)]
    pub path: String,
    /// TODO(task 9): real probe reads the file header. Until then a test can
    /// inject the duration so resolve has a determinate length to fold; `None`
    /// falls back to the stub.
    #[builder(into)]
    pub duration: Option<f32>,
}

impl VideoFile {
    /// Stubbed duration probe (`.sketch/02 §12`).
    ///
    /// TODO(task 9): replace with an `ffprobe`/`ffmpeg` header read that can
    /// fail with [`ResolveError::Probe`](crate::timeline_component::ResolveError::Probe);
    /// the trim/speed remap a window implies (`.sketch/01 §A.3`) is the leaf's
    /// `frame`/`samples` concern at the same step. For now it never fails.
    fn probe(&self) -> f32 {
        self.duration.unwrap_or(STUB_PROBE_SECONDS)
    }
}

impl TimelineComponent for VideoFile {
    fn duration(&self) -> Option<f32> {
        Some(self.probe())
    }

    fn frame(
        &self,
        clock: Clock<'_>,
        target: Resolution,
        ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage> {
        // TODO(task 9): decode + return the frame at `clock`.
        let _ = (clock, target, ctx);
        None
    }

    fn arrangement(&self) -> Arrangement {
        Arrangement {
            kind: NodeKind::Video,
            label: self.path.clone(),
            start: 0.0,
            end: self.probe(),
            // TODO(task 9): carry the source crop once `.trim` is wired.
            trim: None,
            triggers: Vec::new(),
            children: Vec::new(),
        }
    }
}

/// Decoded audio — the audio channel. Built with
/// `AudioFile::builder().path("v.wav").gain(0.25)`.
///
/// Its intrinsic length is the file's, read once by the resolve pass. DECODE IS
/// STUBBED here (step 8): [`probe`](Self::probe) returns the injected
/// `duration` if set, else [`STUB_PROBE_SECONDS`]. `samples` is a `None`
/// placeholder until `symphonia` decode lands.
#[crate::component(timeline)]
// `Clone`: see `VideoFile` — a media leaf may be a `#[component(timeline)]`
// field (the `.sketch/01` `Dialogue(voice: AudioFile)`), which the macro clones.
#[derive(Clone, crate::Keyable)]
pub struct AudioFile {
    #[builder(into)]
    pub path: String,
    /// Linear gain applied to the decoded samples (`1.0` = unity).
    #[builder(default = 1.0)]
    pub gain: f32,
    /// TODO(task 8): real probe reads the file header. Test-injectable; `None`
    /// falls back to the stub.
    #[builder(into)]
    pub duration: Option<f32>,
}

impl AudioFile {
    /// Stubbed duration probe (`.sketch/02 §12`).
    ///
    /// TODO(task 8): replace with a `symphonia` header read that can fail with
    /// [`ResolveError::Probe`](crate::timeline_component::ResolveError::Probe).
    /// For now it never fails.
    fn probe(&self) -> f32 {
        self.duration.unwrap_or(STUB_PROBE_SECONDS)
    }
}

impl TimelineComponent for AudioFile {
    fn duration(&self) -> Option<f32> {
        Some(self.probe())
    }

    fn samples(&self, clock: Clock<'_>, window: f32) -> Option<AudioBuffer> {
        // TODO(task 8): decode `[clock, clock + window)` and apply `gain`.
        let _ = (clock, window);
        None
    }

    fn arrangement(&self) -> Arrangement {
        Arrangement {
            kind: NodeKind::Audio,
            label: self.path.clone(),
            start: 0.0,
            end: self.probe(),
            trim: None,
            triggers: Vec::new(),
            children: Vec::new(),
        }
    }
}

/// 字幕 — the subtitle channel only (written to .srt/.vtt, NOT a burned-in
/// telop, which is a visual). Built with `Subtitle::builder().text("…")`.
///
/// TIMELESS (`measure()` = `None`): its interval comes from the placement window
/// (`.at(0.0..dur)`) or a `.fill()` taking the container's resolved length. Its
/// [`cues`](TimelineComponent::cues) emit `Cue { start: offset, end: offset +
/// resolved_len, text }`. `frame` / `samples` are `None`.
#[crate::component(timeline)]
// `Clone`: see `VideoFile` — a leaf may be a `#[component(timeline)]` field that
// the macro clones to build the body.
#[derive(Clone, crate::Keyable)]
pub struct Subtitle {
    #[builder(into)]
    pub text: String,
}

impl TimelineComponent for Subtitle {
    // `duration` defaults to `None` (timeless): the placement window supplies the
    // length, so `measure` (which defaults to `duration`) is `None` too.

    fn cues(&self, offset: f32) -> Vec<Cue> {
        // The resolved length comes from the placement window / `.fill()`, which
        // wraps this leaf in a `Placed`. When called directly (no window) the
        // leaf is timeless, so the cue is a zero-length point at `offset`; the
        // wrapping `Placed` (or container, for `.fill()`) re-stamps the real end.
        let end = offset + self.duration().unwrap_or(0.0);
        vec![Cue {
            start: offset,
            end,
            text: self.text.clone(),
        }]
    }

    fn arrangement(&self) -> Arrangement {
        Arrangement {
            kind: NodeKind::Subtitle,
            label: self.text.clone(),
            start: 0.0,
            end: 0.0,
            trim: None,
            triggers: Vec::new(),
            children: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{Constraints, Vec2};
    use crate::raster::{PixelFormat, RasterComponent, RasterImage, Resolution};
    use crate::render_context::RenderContext;
    use crate::timeline_component::{resolve, ResolveError, Timed, TimedBuilder};

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
        let sub = cues.iter().find(|c| c.text == "spanning").expect("subtitle cue");
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
        let hello = cues.iter().find(|c| c.text == "hello").expect("subtitle cue");
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
        let line = cues.iter().find(|c| c.text == "a line").expect("subtitle cue");
        assert_eq!(line.start, 0.0);
        assert_eq!(line.end, 4.5);
    }

    // The leaf duration/probe seam: an injected `duration` overrides the stub.
    #[test]
    fn media_leaf_probe_seam_is_injectable() {
        assert_eq!(VideoFile::builder().path("x.mp4").build().duration(), Some(STUB_PROBE_SECONDS));
        assert_eq!(
            VideoFile::builder().path("x.mp4").duration(7.0).build().duration(),
            Some(7.0)
        );
        assert_eq!(
            AudioFile::builder().path("x.wav").gain(0.5).duration(9.0).build().duration(),
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
        let _boxed2: Box<dyn TimelineComponent + Send> =
            VideoFile::builder().path("x.mp4").into();
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
            .child(SolidColor { rgba: [255, 0, 0, 255] }.fill())
            .child(SolidColor { rgba: [0, 255, 0, 128] }.fill())
            // A bare windowed solid gives the timeline a non-fill length so the
            // two fills have something to span.
            .child(SolidColor { rgba: [0, 0, 255, 0] }.at(0.0..2.0))
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
        SolidColor { rgba: [secs, 0, 0, 255] }
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

        // `.at(2.0..5.0)` over a timeless child has speed 1.0, so at global 5.0
        // the child's local is ≈ 3.0 → red channel 3.
        let f3 = resolved
            .frame(TimelineTime::new(5.0), Resolution::new(2, 2), &mut ctx)
            .expect("contributes");
        assert_eq!(first_pixel(&f3)[0], 3, "local advances 1:1 with global");
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
        assert_eq!(first_pixel(&f)[0], 0, "second child's local ≈ 0 at the cursor");

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
            target: Resolution,
            _ctx: &mut dyn RenderContext,
        ) -> Option<RasterImage> {
            self.log.lock().unwrap().push(clock.local().seconds());
            Some(RasterImage::cpu(
                target.width,
                target.height,
                PixelFormat::Rgba8,
                vec![255u8; (target.width as usize) * (target.height as usize) * 4],
            ))
        }

        fn arrangement(&self) -> Arrangement {
            Arrangement {
                kind: NodeKind::Video,
                label: String::new(),
                start: 0.0,
                end: self.duration,
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
        assert!((recorded - 0.5).abs() < 1e-5, "source-local {recorded} (want 0.5)");
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
        let solid = SolidColor { rgba: [10, 20, 30, 255] };
        let f = solid.frame(clock, Resolution::new(2, 2), &mut ctx).expect("renders");
        assert_eq!(first_pixel(&f), [10, 20, 30, 255]);

        // A timeline component whose body builds a timeless visual.
        let probe = LocalProbe::builder().build();
        let f = probe.frame(clock, Resolution::new(2, 2), &mut ctx).expect("renders");
        assert_eq!(first_pixel(&f)[0], 0, "local 0 → red 0");
    }
}

