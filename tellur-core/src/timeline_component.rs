//! The timeline subsystem's public surface.
//!
//! This module defines the time-varying analogue of the spatial component
//! system: the [`TimelineComponent`] trait and its builders/placement verbs,
//! the [`Placed`] / [`Triggered`] wrappers, the resolve pass, and the per-frame
//! [`Clock`]. The timeline macro arm and the container leaves
//! (`timeline_container.rs`) build on these. Every method below is implemented
//! and exercised by focused tests; the media-decode / ffmpeg integration paths
//! sit behind `#[ignore]`.
//!
//! The shape mirrors the spatial side of the library on purpose:
//!
//! | space (`raster.rs` / `builder.rs`)      | time (this module)                |
//! |-----------------------------------------|-----------------------------------|
//! | [`RasterComponent`]                     | [`TimelineComponent`]             |
//! | [`RasterBuilder`](crate::builder)       | [`TimelineBuilder`]               |
//! | `RasterBuilderPlacement` (`.place_at`)  | [`Timed`] / [`TimedBuilder`]      |
//! | `Positioned`                            | [`Placed`]                        |
//!
//! See `.sketch/01-timeline-api.rs` (ZONE A) for the target authoring API and
//! `.sketch/02-resolve-pass.md` for the resolve-pass architecture every method
//! here leans on (ownership model §3/§7, `Clock` §8, trigger table §11).

use std::collections::HashMap;
use std::hash::Hash;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use crate::dyn_compare::{DynEq, DynHash};
use crate::geometry::Vec2;
use crate::phase::Phase;
use crate::raster::{RasterComponent, RasterImage, Resolution};
use crate::render_context::RenderContext;
use crate::time::{LocalTime, Time, TimelineTime};

// ── The one unit: a thing placed on a timeline ──────────────────────────────

/// A "playable": over its own local clock `[0, duration)` it emits any of three
/// channels — video frames, audio samples, or subtitle cues. The temporal twin
/// of [`RasterComponent`].
///
/// Implemented by the timeline leaves/containers (later steps), by any
/// `#[component(timeline)]` (step 2), and — via the one-way blanket below — by
/// every [`RasterComponent`] (a *timeless* visual).
///
/// The trait must be usable as `Box<dyn TimelineComponent + Send>` (audit M2),
/// so it is object-safe and every implementor is expected to be `Send`.
///
/// The [`DynEq`] / [`DynHash`] super-traits mirror
/// [`RasterComponent`](crate::raster::RasterComponent): they give
/// `dyn TimelineComponent` an object-safe `PartialEq` / `Hash` (see the manual
/// impls below `Box<dyn TimelineComponent + Send>`), so a container that holds
/// `Vec<Box<dyn TimelineComponent + Send>>` (e.g. [`Timeline`] / [`Sequence`])
/// can satisfy the `TimelineBuilder::Output: PartialEq + Hash` marker bound the
/// same way the raster [`Flex`](crate::layout::raster::Flex) does — by
/// deriving over a comparable child vec. Timeline nodes are never memoized
/// through `ctx.render` (`.sketch/02 §11`), so this identity is purely the
/// builder-marker key, not a per-frame cache key.
pub trait TimelineComponent: DynEq + DynHash {
    /// Intrinsic length in seconds, or `None` for a *timeless* component (a
    /// visual / 字幕) whose length is given by the window it is placed into.
    ///
    /// This is the place-pass view of length; [`measure`](Self::measure) is the
    /// bottom-up measure-pass twin. Leaves usually define one and let the other
    /// default to it.
    fn duration(&self) -> Option<f32> {
        // Default: a component with no intrinsic length is timeless.
        None
    }

    /// Bottom-up intrinsic duration for the resolve pass's measure phase
    /// (`.sketch/02 §5`). Defaults to [`duration`](Self::duration); containers
    /// override to fold over their children.
    ///
    /// CONTRACT (the load-bearing invariant, `.sketch/02 §5/§9`): a container's
    /// `measure` is computed bottom-up from its children and **excludes every
    /// `.fill()` child**. `.fill()` takes the container's resolved length, so
    /// measuring a fill child *into* that length would close a cycle; excluding
    /// it keeps the dependency strictly one-directional (container length →
    /// fill-child length) and the two-pass solve acyclic and terminating.
    /// Consequently a non-fill container whose non-fill children are all `None`
    /// measures as `None`, and an empty / all-fill interior measures as a
    /// determinate `0.0` (`max(∅)` / `Σ(∅)`); the all-fill case should also
    /// [`warn`](ResolveCtx::warn) at place time. `None` means *timeless*: the
    /// length is supplied later by the placement window.
    fn measure(&self) -> Option<f32> {
        self.duration()
    }

    /// Top-down place hook (`.sketch/02 §6`). Walks `self` and, for a container,
    /// recurses over its OWN children via `&self` recursion — assigning each an
    /// absolute start and recording [`Event`] trigger times into `out` —
    /// returning this node's resolved length.
    ///
    /// CONTRACT: `abs_start` is the absolute start handed down by the parent;
    /// the return value is the node's resolved length folded back up. A
    /// container computes each child's absolute start as `abs_start + relative`
    /// and recurses with it (a `Sequence` advances a cursor; a `Timeline`
    /// overlays from a common base and resolves `.fill()` children against its
    /// own measured length in a second sub-pass). The all-fill / empty-interior
    /// case resolves to a determinate `0.0` and should emit a
    /// [`warn`](ResolveCtx::warn).
    ///
    /// The default is the leaf behaviour: record nothing and report own length
    /// (falling back to `0.0` for a timeless leaf, whose interval comes from the
    /// placement window instead).
    fn resolve(&self, abs_start: f32, out: &mut ResolveCtx) -> f32 {
        // Leaves record no triggers and report their own (possibly absent)
        // length; containers (step 4) override to recurse and place children.
        let _ = (abs_start, out);
        self.duration().unwrap_or(0.0)
    }

    /// Visual channel for this frame. `clock` carries both time axes (see
    /// [`Clock`]); `canvas` is the composition's fixed LOGICAL layout space
    /// (resolution-independent), which the pixel `target` scales. `None` ⇒
    /// contributes nothing visually.
    fn frame(
        &self,
        clock: Clock<'_>,
        canvas: Vec2,
        target: Resolution,
        ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage> {
        // TODO(task 4): leaves/containers produce real frames.
        let _ = (clock, canvas, target, ctx);
        None
    }

    /// Audio channel for `[clock, clock + window)`. `None` ⇒ silent.
    fn samples(&self, clock: Clock<'_>, window: f32) -> Option<AudioBuffer> {
        // The eager mix-down (step 8) uses [`mix_into`](Self::mix_into) instead
        // of this per-window pull; this stays the (unused) per-window seam.
        let _ = (clock, window);
        None
    }

    /// Eager mix-down hook (step 8, B4 v1 — `.sketch/01` ZONE C / `.sketch/02
    /// §15`). Contributes this node's audio into `mix` (a fixed rate / channel
    /// layout buffer for `[0, duration]`), placing it at the absolute
    /// `start_secs` and accounting for the accumulated placement `speed`.
    ///
    /// CONTRACT: mirrors [`resolve`](Self::resolve) / [`frame`](Self::frame)
    /// placement — a container recurses over its own children, advancing
    /// `start_secs` exactly as it advances absolute starts (a [`Sequence`]
    /// cursor sums prior lengths; a [`Timeline`] overlays at the same base), and
    /// a [`Placed`] shifts by its relative start and folds its window
    /// `speed()` in. A leaf decodes / conforms / sums via
    /// [`AudioMix::add`](crate::audio::AudioMix::add). The default is the
    /// timeless / silent behaviour: contribute nothing.
    fn mix_into(&self, mix: &mut crate::audio::AudioMix, start_secs: f32, speed: f32) {
        let _ = (mix, start_secs, speed);
    }

    /// Subtitle channel: cues made absolute by adding `offset` (this
    /// component's resolved start). Collected once at export.
    fn cues(&self, offset: f32) -> Vec<Cue> {
        // TODO(task 5+): Subtitle leaves / containers contribute cues.
        let _ = offset;
        Vec::new()
    }

    /// What the live UI draws: kind + label, plus the node's RESOLVED absolute
    /// interval and any [`Event`] trigger times.
    ///
    /// `offset` is this node's resolved absolute start, threaded top-down exactly
    /// like [`cues`](Self::cues): a leaf stamps `start = offset`,
    /// `end = offset + self.duration().unwrap_or(0.0)`; a container places each
    /// child at `offset + child_relative_start`, mirroring its
    /// [`resolve`](Self::resolve) / [`cues`](Self::cues) cursor.
    ///
    /// Named `arrangement` (not `outline`) to avoid clashing with the existing
    /// `tellur_renderer::Outline` raster effect (audit hygiene note).
    fn arrangement(&self, offset: f32) -> Arrangement;

    /// The STRUCTURAL `&dyn Any` behind this component, peeling any transparent
    /// decorator. A container inspecting a child's concrete type (e.g.
    /// downcasting to [`Placed`] to detect a `.fill()`) must go through this, not
    /// [`DynEq::as_any`](crate::dyn_compare::DynEq::as_any), so a wrapping
    /// [`Sourced`] (which the generated `.child(...)` setter adds to capture the
    /// call site) does not hide the real child. Defaults to `self` — only a
    /// decorator like `Sourced` overrides it to forward to its inner component.
    fn structural_any(&self) -> &dyn std::any::Any {
        self.as_any()
    }
}

// Compile-time guarantee that `TimelineComponent` is object-safe *and*
// spellable with `+ Send` (audit M2).
const _: Option<&(dyn TimelineComponent + Send)> = None;

// `dyn TimelineComponent + Send` gets object-safe `PartialEq` / `Hash` through
// the `DynEq` / `DynHash` super-traits, exactly as `dyn RasterComponent` does
// (`raster.rs`). This is what makes `Box<dyn TimelineComponent + Send>` and a
// container's `Vec<Box<dyn TimelineComponent + Send>>` comparable, so the
// containers can derive the `PartialEq + Hash` the `TimelineBuilder` marker
// requires.
impl PartialEq for dyn TimelineComponent + Send {
    fn eq(&self, other: &Self) -> bool {
        DynEq::dyn_eq(self, other.as_any())
    }
}

impl Eq for dyn TimelineComponent + Send {}

impl Hash for dyn TimelineComponent + Send {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        DynHash::dyn_hash(self, state);
    }
}

/// Any [`RasterComponent`] IS a timeless visual [`TimelineComponent`] — ONE
/// direction only. This is what lets a styled `Text` (a "Caption") be placed in
/// time with `.at(0.0..dur)`; its [`duration`](TimelineComponent::duration) is
/// `None` until a placement window gives it one.
///
/// COHERENCE (audit, `.sketch/01` A.1): this blanket is the *only* impl after
/// step 1. A concrete `RasterComponent` must reach the timeline world through
/// this blanket — never via a second direct `impl TimelineComponent for Foo`,
/// or the pair becomes an `E0119`.
impl<C> TimelineComponent for C
where
    C: RasterComponent + 'static,
{
    // A timeless visual has no intrinsic length; the placement window sets it.
    fn duration(&self) -> Option<f32> {
        None
    }

    fn frame(
        &self,
        clock: Clock<'_>,
        canvas: Vec2,
        target: Resolution,
        ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage> {
        // Route through `ctx.render` so the visual memoizes one level down
        // (`.sketch/02 §11`): the framework caches `&dyn RasterComponent`, not
        // `&dyn TimelineComponent`, so this is the path that earns a cache slot.
        let _ = clock;
        // `canvas` is the composition's FIXED logical layout space, passed in
        // and decoupled from the pixel `target` (resolution-independent). Lay
        // the visual out against the canvas — NOT its intrinsic box. Sizing
        // against the intrinsic box (`layout(UNBOUNDED)`) would stretch a
        // wide-short text box to fill the frame (aspect distortion) and collapse
        // a `Fill` component against the unbounded axis. The canvas makes
        // anchored/`Fill` placement resolve against the full frame, and
        // `ctx.render` scales that logical canvas to the pixel target with no
        // distortion (mirrors the raster root, where `size` aspect matches
        // `target`).
        Some(ctx.render(self, canvas, target))
    }

    fn samples(&self, _clock: Clock<'_>, _window: f32) -> Option<AudioBuffer> {
        None
    }

    fn cues(&self, _offset: f32) -> Vec<Cue> {
        Vec::new()
    }

    fn arrangement(&self, offset: f32) -> Arrangement {
        // A timeless visual surfaces as a Video-kind node — every rasterized
        // visual (a backdrop, a caption telop, a reveal) lives on the video
        // (映像) track; there is no separate caption kind. Un-windowed it is
        // 0-length; the wrapping `Placed` stamps its real window end (mirrors the
        // `cues` zero-length-point + window-end-stamp pattern).
        Arrangement {
            kind: NodeKind::Video,
            // TODO(task 6): carry a real label (e.g. the concrete type name).
            label: String::new(),
            // A `#[component(raster)]` overrides `arrangement_name` to surface
            // its display name here; plain raster primitives leave it `None`.
            name: self.arrangement_name(),
            // The wrapping `Sourced` (from the container setter) stamps the
            // call site; a bare visual has none of its own.
            source: None,
            start: offset,
            end: offset + self.duration().unwrap_or(0.0),
            trim: None,
            triggers: Vec::new(),
            children: Vec::new(),
        }
    }
}

// ── Builder marker ──────────────────────────────────────────────────────────

/// Marker for a *complete* builder of a [`TimelineComponent`], mirroring
/// [`VectorBuilder`](crate::builder::VectorBuilder) /
/// [`RasterBuilder`](crate::builder::RasterBuilder).
///
/// The buildless placement/trigger extensions ([`TimedBuilder`],
/// [`TriggersBuilder`]) hang off THIS marker, not off [`TimelineComponent`] —
/// that disjointness is what keeps the two blanket families from overlapping
/// (audit B2).
pub trait TimelineBuilder: Sized {
    type Output: TimelineComponent + PartialEq + Hash + 'static;
    /// Finishes the builder. This is the `.build()` the caller never writes.
    fn build_component(self) -> Self::Output;
}

// ── Placement ───────────────────────────────────────────────────────────────

/// Where a component sits on the PARENT clock.
///
/// `From<f32>` = a start point (it plays for its own
/// [`duration`](TimelineComponent::duration) at native speed); `From<Range<f32>>`
/// = an explicit `start..end` window. For a TIMELESS visual/subtitle the window
/// just gives it that interval. For a TIMED component a window ≠ its length is a
/// STRETCH that time-scales the (trimmed) source to fill the window, so
/// `speed = content_duration / (b - a)` (decided semantics, `.sketch/01` A.3).
/// There is no separate `.speed()` — to merely truncate, [`Timed::trim`] the
/// source.
#[derive(Debug, Clone, Copy, crate::Keyable)]
pub struct Placement {
    /// Relative start on the parent clock.
    start: f32,
    /// Explicit window end (exclusive); `None` for a bare start point, whose
    /// end is the inner component's own duration.
    end: Option<f32>,
    /// Whether this placement is a [`fill`](Timed::fill): it takes the
    /// CONTAINER's resolved length (set by the overlay `Timeline` in its
    /// second sub-pass) and is EXCLUDED from the container's length measure
    /// (the load-bearing acyclicity invariant, `.sketch/02 §5/§9`). A fill
    /// child inside a `Sequence` is a resolve error (ZONE C #1).
    fill: bool,
}

impl Placement {
    /// Relative start on the parent clock.
    pub fn start(&self) -> f32 {
        self.start
    }

    /// Explicit window end, if this placement is a `start..end` window.
    pub fn end(&self) -> Option<f32> {
        self.end
    }

    /// Whether this is a [`fill`](Timed::fill) placement (stretches to the
    /// container's resolved length).
    pub fn is_fill(&self) -> bool {
        self.fill
    }

    /// A fill placement — takes the container's resolved length. Constructed by
    /// [`Timed::fill`]; not reachable through the `From` conversions (a window /
    /// point is never implicitly a fill).
    fn fill() -> Self {
        Self {
            start: 0.0,
            end: None,
            fill: true,
        }
    }
}

impl From<f32> for Placement {
    fn from(start: f32) -> Self {
        Self {
            start,
            end: None,
            fill: false,
        }
    }
}

impl From<Range<f32>> for Placement {
    fn from(window: Range<f32>) -> Self {
        Self {
            start: window.start,
            end: Some(window.end),
            fill: false,
        }
    }
}

/// A component placed at a parent time — the temporal twin of
/// [`Positioned`](crate::placement::Positioned). A [`TimelineComponent`] itself,
/// so it nests.
///
/// Per the DECIDED stretch semantics (`.sketch/01` A.3): `.at(a..b)` footprint
/// is the window; the implied speed factor is recorded for later sampling.
#[derive(crate::Keyable)]
pub struct Placed {
    /// Where on the parent clock this sits.
    placement: Placement,
    /// The placed component.
    child: Box<dyn TimelineComponent + Send>,
}

impl Placed {
    /// Constructs a placement of `child` at `placement` on the parent clock.
    pub fn new(placement: Placement, child: Box<dyn TimelineComponent + Send>) -> Self {
        Self { placement, child }
    }

    /// Where this sits on the parent clock.
    pub fn placement(&self) -> Placement {
        self.placement
    }

    /// Whether this is a [`fill`](Timed::fill) placement. Containers read this
    /// to exclude fill children from their length measure and resolve them
    /// against the container's resolved length (`.sketch/02 §5/§6`).
    pub fn is_fill(&self) -> bool {
        self.placement.fill
    }

    /// The placed child, borrowed. Lets a container drive the child's own
    /// `resolve` against the container's resolved length for a fill child.
    pub fn child(&self) -> &(dyn TimelineComponent + Send) {
        &*self.child
    }

    /// Speed factor implied by a stretch window over a timed child, i.e.
    /// `content_duration / (b - a)`; `1.0` for a bare start point or a window
    /// over a timeless child. Recorded for later sampling (`.sketch/01` A.3).
    pub fn speed(&self) -> f32 {
        match (self.placement.end, self.child.duration()) {
            (Some(end), Some(content)) => {
                let window = end - self.placement.start;
                if window > 0.0 {
                    content / window
                } else {
                    // TODO(task 3): a zero/negative window is a resolve error.
                    1.0
                }
            }
            _ => 1.0,
        }
    }
}

impl TimelineComponent for Placed {
    fn duration(&self) -> Option<f32> {
        match self.placement.end {
            // An explicit window fixes the length regardless of the child's.
            Some(end) => Some(end - self.placement.start),
            // A bare start point plays for the child's own duration.
            None => self.child.duration(),
        }
    }

    fn measure(&self) -> Option<f32> {
        // The measure-pass footprint is the relative start plus the resolved
        // length (`.sketch/02 §5`): `b` for a window, `start + inner` for a
        // point.
        match self.placement.end {
            Some(end) => Some(end),
            None => self
                .child
                .measure()
                .map(|inner| self.placement.start + inner),
        }
    }

    fn resolve(&self, abs_start: f32, out: &mut ResolveCtx) -> f32 {
        // A zero/negative explicit window has no determinate speed (it would
        // stretch the child by ∞); surface it as an authoring error instead of
        // silently degrading to `speed() == 1.0`.
        if let Some(end) = self.placement.end {
            if end - self.placement.start <= 0.0 {
                out.error(format!(
                    "a placement window must have positive length, got .at({}..{})",
                    self.placement.start, end
                ));
            }
        }
        // Recurse at the child's absolute start, folding the window stretch in
        // (`.sketch/01 §A.3`). The relative start is in THIS level's local
        // seconds, so scale it to absolute by the enclosing `local_scale`; the
        // child then runs at this window's `speed()`, so any interior offsets it
        // records shrink by that factor (a `.at(0..1)` over a 2s child has
        // `speed = 2`, so the child's own seconds are half a parent second each).
        let scale = out.local_scale;
        let child_abs = abs_start + self.placement.start * scale;
        let saved = out.local_scale;
        out.local_scale = scale / self.speed();
        let child_len = self.child.resolve(child_abs, out);
        out.local_scale = saved;
        self.duration().unwrap_or(self.placement.start + child_len)
    }

    fn frame(
        &self,
        clock: Clock<'_>,
        canvas: Vec2,
        target: Resolution,
        ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage> {
        let t = clock.local().seconds();
        // Temporal gate: a placed clip contributes ONLY within its resolved
        // parent-clock interval `[start, end)` (half-open, so abutting clips and
        // Sequence slots never double-draw at the seam). A `.fill()` spans its
        // container and is never self-gated; an open-ended timeless point (no
        // window, no child duration) can't know its end, so it stays active.
        if !self.is_fill() {
            let active = match self.placement.end {
                Some(end) => t >= self.placement.start && t < end,
                None => match self.child.duration() {
                    Some(d) => t >= self.placement.start && t < self.placement.start + d,
                    None => true,
                },
            };
            if !active {
                return None;
            }
        }
        // Rebase + stretch (`.sketch/02 §8`, `.sketch/01 §A.3`): shift the child's
        // local axis to its relative start, then time-scale by the window's
        // implied `speed()`. A `.at(0.0..1.0)` over a 2s source has `speed = 2.0`,
        // so source-local advances twice as fast — at parent local `0.5` the child
        // sees `1.0`. `global` / `triggers` are unchanged (the trigger axis is
        // global, never remapped).
        let rebased = (t - self.placement.start) * self.speed();
        // Surface the child's LOCAL window length so end-relative effects
        // (`clock.envelope` / `clock.remaining`) know where the clip closes. A
        // window is `(end - start) * speed` = the child's own post-stretch seconds
        // (content_dur for a timed stretch, `b - a` for a timeless one), matching
        // the units of the rebased local axis above. A bare point takes the
        // child's own duration; a fill is open-ended (`None`).
        let window = if self.is_fill() {
            None
        } else {
            match self.placement.end {
                Some(end) => Some((end - self.placement.start) * self.speed()),
                None => self.child.duration(),
            }
        };
        let child_clock = clock.with_local_window(LocalTime::new(rebased), window);
        self.child.frame(child_clock, canvas, target, ctx)
    }

    fn samples(&self, clock: Clock<'_>, window: f32) -> Option<AudioBuffer> {
        // The mix-down uses `mix_into`; this per-window seam just forwards.
        self.child.samples(clock, window)
    }

    fn mix_into(&self, mix: &mut crate::audio::AudioMix, start_secs: f32, speed: f32) {
        // Shift the child to its relative start and fold the window stretch into
        // the speed — exactly the rebase + time-scale `frame` applies, but on
        // the sample axis (`.sketch/02 §8`). A fill child has relative start 0.
        self.child
            .mix_into(mix, start_secs + self.placement.start, speed * self.speed());
    }

    fn cues(&self, offset: f32) -> Vec<Cue> {
        let child_offset = offset + self.placement.start;
        let mut cues = self.child.cues(child_offset);
        // A TIMELESS child (e.g. a `Subtitle`) has no intrinsic length, so its
        // cues come out as zero-length points; the placement WINDOW is what
        // gives the cue its interval (`.sketch/02 §10`). Stamp the window end
        // onto those cues. A timed child already carries its own ends, and a
        // fill child's length is supplied by the container's own `cues` walk.
        if self.child.duration().is_none() {
            if let Some(end) = self.placement.end {
                let abs_end = offset + end;
                for cue in &mut cues {
                    cue.end = abs_end;
                }
            }
        }
        cues
    }

    fn arrangement(&self, offset: f32) -> Arrangement {
        // Produce the child's node at its absolute start (`offset + relative`).
        let child_offset = offset + self.placement.start;
        let mut node = self.child.arrangement(child_offset);
        // A TIMELESS child (e.g. a `Subtitle` / a bare visual) comes out
        // 0-length, so the placement WINDOW is what gives it its interval —
        // stamp the window end (mirrors `Placed::cues`). A timed child already
        // carries its own end; a fill child's end is supplied by the container.
        if self.child.duration().is_none() {
            if let Some(end) = self.placement.end {
                node.end = offset + end;
            }
        }
        node
    }
}

/// A `.at(..)` / `.fill()` result drops straight into a container's
/// `child(impl Into<Box<dyn TimelineComponent + Send>>)` setter, mirroring
/// raster `Positioned`'s `From<Positioned> for Box<dyn RasterComponent>`.
impl From<Placed> for Box<dyn TimelineComponent + Send> {
    fn from(placed: Placed) -> Self {
        Box::new(placed)
    }
}

// ── Placement verbs (split: component-side + builder-side, audit B2) ─────────

/// Placement verbs on a built [`TimelineComponent`]. Blanket-implemented for
/// every `T: TimelineComponent`, mirroring `RasterBuilderPlacement`.
///
/// The two clocks are deliberately different (`.sketch/01` A.3):
/// - `.at(..)` — PARENT clock: where it sits (point or window).
/// - [`trim`](Self::trim) — SOURCE clock: which of its own seconds play.
///
/// SPEED is emergent, not a verb: `.at(a..b)` over a timed component stretches
/// its (trimmed) length to fill the window. [`fill`](Self::fill) is valid ONLY
/// in an overlay `Timeline` — inside a `Sequence` it is a compile/resolve error.
pub trait Timed: TimelineComponent + Sized + Send + 'static {
    /// Place on the parent clock. `.at(2.0)` plays at native speed for the
    /// component's own duration; `.at(0.0..3.0)` places it into an explicit
    /// window (a stretch for a timed component, an interval for a timeless one).
    fn at(self, placement: impl Into<Placement>) -> Placed {
        Placed::new(placement.into(), Box::new(self))
    }

    /// Stretch to the CONTAINER's resolved length — the one declarative length
    /// verb. Valid ONLY in an overlay `Timeline` (inside a `Sequence` it is a
    /// compile/resolve error). Fill children are excluded from the container's
    /// length measure (the load-bearing invariant), so this never forms a cycle.
    fn fill(self) -> Placed {
        // A fill placement: the overlay `Timeline` resolves it against its own
        // length in its second sub-pass, and excludes it from the length
        // measure (`.sketch/02 §5/§6`). A `Sequence` rejects it at resolve time.
        Placed::new(Placement::fill(), Box::new(self))
    }

    /// Use only SOURCE seconds `a..b` (the in/out crop). The way to truncate (a
    /// short `.at` window stretches, it does not cut).
    ///
    /// Honoured ONLY by the media leaves (`VideoFile` / `AudioFile`), which shadow
    /// this blanket with an inherent `trim` that records the crop and reports
    /// `b - a` from [`duration`](TimelineComponent::duration). A source-time crop
    /// is meaningless for a synthetic (timeless / clock-driven) component, so on
    /// everything else this is intentionally a no-op returning `self` unchanged.
    fn trim(self, _r: Range<f32>) -> Self {
        self
    }
}

impl<T: TimelineComponent + Send + 'static> Timed for T {}

/// Buildless twin of [`Timed`], over complete builders. Same method names; lets
/// `Caption::builder().line(..).fill()` work with no `.build()`. Returns
/// built/placed types exactly as `VectorBuilderPlacement::place_at` returns
/// `Positioned`.
pub trait TimedBuilder: TimelineBuilder
where
    Self::Output: Send,
{
    fn at(self, placement: impl Into<Placement>) -> Placed {
        self.build_component().at(placement)
    }

    fn fill(self) -> Placed {
        self.build_component().fill()
    }

    fn trim(self, r: Range<f32>) -> Self::Output {
        self.build_component().trim(r)
    }
}

impl<B> TimedBuilder for B
where
    B: TimelineBuilder,
    B::Output: Send,
{
}

// ── Events — a structural moment shared across the tree ──────────────────────

/// Process-wide counter minting [`Event`] ids. A plain monotonic counter is
/// enough: ids only need to be distinct within a session (`.sketch/01` A.6).
static EVENT_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The typed handle the user binds once (`let e = Event::new()`). It is a small
/// `Copy` identity token — NOT a cell.
///
/// A clip declares the moment with a `.trigger_*` verb (see [`Triggers`]); the
/// resolve pass records the resolved time in a side table (`id → TimelineTime`),
/// and components read it through the [`Clock`]. Being a plain id, it is a sound
/// cache-key term (audit B5); the trigger *time* lives outside the component.
///
/// An optional `name` ([`Event::named`]) is a `&'static str` so the token stays
/// `Copy`. It is carried purely for display — it surfaces on the arrangement's
/// [`TriggerMark`]s so the live UI can label event markers — and is NOT part of
/// the event identity (only [`id`](Self::id) is).
#[derive(Debug, Clone, Copy)]
pub struct Event {
    id: u64,
    name: Option<&'static str>,
}

// Identity is the minted `id` ALONE: the `name` is a display annotation, never
// part of equality/hash (so it is not a cache-key term either, audit B5). Two
// `Event`s are equal iff their ids match — and ids are process-unique per mint.
impl PartialEq for Event {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for Event {}

impl Hash for Event {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl Event {
    /// Mints a fresh, unnamed process-unique id.
    pub fn new() -> Self {
        Self {
            id: EVENT_COUNTER.fetch_add(1, Ordering::Relaxed),
            name: None,
        }
    }

    /// Mints a fresh process-unique id carrying a display `name`. The name is
    /// for UI labelling only; the identity is still the freshly minted id.
    pub fn named(name: &'static str) -> Self {
        Self {
            id: EVENT_COUNTER.fetch_add(1, Ordering::Relaxed),
            name: Some(name),
        }
    }

    /// The raw id, used by the resolve pass / [`TriggerTable`] as the key.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// The optional display name bound at construction (`None` for [`Event::new`]).
    pub fn name(&self) -> Option<&'static str> {
        self.name
    }

    /// `now < trigger` (unfired ⇒ trigger is `+∞`, so always true).
    pub fn is_before(&self, clock: &Clock<'_>) -> bool {
        clock.global().seconds() < clock.trigger_of(*self).seconds()
    }

    /// `now >= trigger` (unfired ⇒ always false).
    pub fn is_after(&self, clock: &Clock<'_>) -> bool {
        clock.global().seconds() >= clock.trigger_of(*self).seconds()
    }

    /// Phase over `[trigger + a, trigger + b]`; clamps 0 before, 1 after.
    pub fn phase(&self, clock: &Clock<'_>, a: f32, b: f32) -> Phase {
        let trigger = clock.trigger_of(*self).seconds();
        // `.sketch/02 §11`: an unfired (+∞) trigger must short-circuit to 0,
        // otherwise the naive `(now - ∞)/(∞ - ∞)` is `NaN`.
        if trigger.is_infinite() {
            return Phase::ZERO;
        }
        clock.global().phase(trigger + a, trigger + b)
    }
}

impl Default for Event {
    fn default() -> Self {
        Self::new()
    }
}

/// A transparent wrapper that registers an [`Event`]'s time during the resolve
/// pass and otherwise plays its child unchanged. A [`TimelineComponent`].
pub struct Triggered<T> {
    child: T,
    event: Event,
    kind: TriggerKind,
}

// Hand-written so the `T: PartialEq + Hash` bound is attached (the `Keyable`
// derive copies the declared generics verbatim and would not add it). Backs the
// `DynEq` / `DynHash` super-traits a `Triggered<T>` needs as a
// `TimelineComponent`.
impl<T: PartialEq> PartialEq for Triggered<T> {
    fn eq(&self, other: &Self) -> bool {
        self.child == other.child && self.event == other.event && self.kind == other.kind
    }
}

impl<T: Eq> Eq for Triggered<T> {}

impl<T: Hash> Hash for Triggered<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.child.hash(state);
        self.event.hash(state);
        self.kind.hash(state);
    }
}

/// Where in the child's interval a [`Triggered`] fires its [`Event`].
#[derive(Debug, Clone, Copy, crate::Keyable)]
enum TriggerKind {
    /// At the child's resolved start (`abs_start`).
    Start,
    /// At the child's resolved end (`abs_start + len`).
    End,
    /// At a local offset into the child (`abs_start + local`).
    At(f32),
}

impl<T> Triggered<T> {
    /// The wrapped child.
    pub fn child(&self) -> &T {
        &self.child
    }

    /// The event this wrapper fires.
    pub fn event(&self) -> Event {
        self.event
    }
}

impl<T: TimelineComponent + PartialEq + Hash + 'static> TimelineComponent for Triggered<T> {
    fn duration(&self) -> Option<f32> {
        self.child.duration()
    }

    fn measure(&self) -> Option<f32> {
        self.child.measure()
    }

    fn resolve(&self, abs_start: f32, out: &mut ResolveCtx) -> f32 {
        let len = self.child.resolve(abs_start, out);
        // `Start` is already the absolute parent-clock start. `End` / `At` are in
        // the child's LOCAL seconds, which run faster than the global clock by any
        // enclosing window stretch, so scale them back via the cumulative
        // `local_scale` (1.0 unless inside a stretched `.at(a..b)`).
        let scale = out.local_scale;
        let at = match self.kind {
            TriggerKind::Start => abs_start,
            TriggerKind::End => abs_start + len * scale,
            TriggerKind::At(local) => abs_start + local * scale,
        };
        out.triggers_mut().record(self.event, at);
        len
    }

    fn frame(
        &self,
        clock: Clock<'_>,
        canvas: Vec2,
        target: Resolution,
        ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage> {
        self.child.frame(clock, canvas, target, ctx)
    }

    fn samples(&self, clock: Clock<'_>, window: f32) -> Option<AudioBuffer> {
        self.child.samples(clock, window)
    }

    fn mix_into(&self, mix: &mut crate::audio::AudioMix, start_secs: f32, speed: f32) {
        // A trigger wrapper is transparent to the mix-down: contribute the
        // child unchanged at the same offset / speed.
        self.child.mix_into(mix, start_secs, speed);
    }

    fn cues(&self, offset: f32) -> Vec<Cue> {
        self.child.cues(offset)
    }

    fn arrangement(&self, offset: f32) -> Arrangement {
        // Transparent to the structure: build the child's node, then push this
        // trigger's resolved time onto it (mirrors `resolve`'s trigger table).
        // The child's resolved interval is `[offset, node.end]`, so its length
        // is `node.end - offset` — `Start → offset`, `End → offset + len`,
        // `At(local) → offset + local`. Multiple triggers accumulate.
        let mut node = self.child.arrangement(offset);
        let at = match self.kind {
            TriggerKind::Start => offset,
            TriggerKind::End => node.end,
            TriggerKind::At(local) => offset + local,
        };
        node.triggers.push(TriggerMark {
            time: at,
            name: self.event().name().map(::std::string::String::from),
        });
        node
    }
}

/// A `.trigger_*` result drops straight into a container's child setter, like
/// [`Placed`] does.
impl<T> From<Triggered<T>> for Box<dyn TimelineComponent + Send>
where
    T: TimelineComponent + PartialEq + Hash + Send + 'static,
{
    fn from(triggered: Triggered<T>) -> Self {
        Box::new(triggered)
    }
}

/// A transparent decorator that stamps a CALL-SITE source location onto its
/// child's arrangement node and otherwise plays the child unchanged.
///
/// The generated container `.child(...)` setter (a `#[track_caller]` method)
/// wraps every child in a `Sourced` carrying `Location::caller()`, so each node
/// in the arrangement tree can be traced back to the authoring `.child(...)`
/// line. Every query EXCEPT [`arrangement`](TimelineComponent::arrangement) is
/// forwarded verbatim to `inner`; `arrangement` stamps the location onto the
/// returned node (only if it is not already set — the innermost wrapper wins,
/// which is the most specific call site).
pub struct Sourced {
    source: &'static ::core::panic::Location<'static>,
    inner: Box<dyn TimelineComponent + Send>,
}

impl Sourced {
    /// Wraps `inner`, recording the `source` call site for its arrangement node.
    pub fn new(
        source: &'static ::core::panic::Location<'static>,
        inner: Box<dyn TimelineComponent + Send>,
    ) -> Self {
        Self { source, inner }
    }

    /// The captured call site as a [`SourceLoc`].
    pub fn source_loc(&self) -> SourceLoc {
        SourceLoc {
            file: self.source.file().to_owned(),
            line: self.source.line(),
        }
    }
}

/// Peels any wrapping [`Sourced`] decorators off a boxed child, returning the
/// INNERMOST call site (matching [`Sourced::arrangement`], where the innermost
/// stamp wins) and the structural inner component.
///
/// A container that bypasses a child's own `arrangement` (e.g. [`Timeline`],
/// which builds the node from the peeled [`Placed`]) uses this to re-stamp the
/// source the wrapper would otherwise have applied.
pub fn peel_source(
    child: &(dyn TimelineComponent + Send),
) -> (Option<SourceLoc>, &(dyn TimelineComponent + Send)) {
    let mut source = None;
    let mut cur = child;
    // Walk inward, keeping the LAST (innermost) source seen.
    while let Some(sourced) = cur.as_any().downcast_ref::<Sourced>() {
        source = Some(sourced.source_loc());
        cur = sourced.inner.as_ref();
    }
    (source, cur)
}

// IDENTITY DELEGATES TO `inner`, IGNORING `source`: the call-site location is a
// display annotation, NOT part of component identity / a cache-key term (same
// contract as `Event::name`). These hand-written impls back the `DynEq` /
// `DynHash` super-traits a `TimelineComponent` needs, and let `Sourced` be a
// comparable `Box<dyn TimelineComponent + Send>` child like any other.
impl PartialEq for Sourced {
    fn eq(&self, other: &Self) -> bool {
        // Compare the boxed children through `dyn TimelineComponent + Send`'s
        // own `PartialEq` (the `DynEq` downcast), ignoring `source`.
        *self.inner == *other.inner
    }
}

impl Eq for Sourced {}

impl Hash for Sourced {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // Hash through the boxed child's `dyn TimelineComponent + Send` `Hash`
        // (the `DynHash` path), ignoring `source`.
        self.inner.hash(state);
    }
}

impl TimelineComponent for Sourced {
    fn duration(&self) -> Option<f32> {
        self.inner.duration()
    }

    fn measure(&self) -> Option<f32> {
        self.inner.measure()
    }

    fn resolve(&self, abs_start: f32, out: &mut ResolveCtx) -> f32 {
        self.inner.resolve(abs_start, out)
    }

    fn frame(
        &self,
        clock: Clock<'_>,
        canvas: Vec2,
        target: Resolution,
        ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage> {
        self.inner.frame(clock, canvas, target, ctx)
    }

    fn samples(&self, clock: Clock<'_>, window: f32) -> Option<AudioBuffer> {
        self.inner.samples(clock, window)
    }

    fn mix_into(&self, mix: &mut crate::audio::AudioMix, start_secs: f32, speed: f32) {
        self.inner.mix_into(mix, start_secs, speed);
    }

    fn cues(&self, offset: f32) -> Vec<Cue> {
        self.inner.cues(offset)
    }

    fn arrangement(&self, offset: f32) -> Arrangement {
        let mut node = self.inner.arrangement(offset);
        // Only the innermost (most specific) call site stamps the node; an outer
        // wrapper leaves an already-set `source` untouched.
        if node.source.is_none() {
            node.source = Some(SourceLoc {
                file: self.source.file().to_owned(),
                line: self.source.line(),
            });
        }
        node
    }

    fn structural_any(&self) -> &dyn std::any::Any {
        // Transparent to structural inspection: a container downcasting a child
        // must see the real component, not this decorator. Recurses so nested
        // wrappers peel fully.
        self.inner.structural_any()
    }
}

/// A `Sourced` drops straight into a container's child setter as a boxed
/// `TimelineComponent`, like [`Placed`] / [`Triggered`] do.
impl From<Sourced> for Box<dyn TimelineComponent + Send> {
    fn from(sourced: Sourced) -> Self {
        Box::new(sourced)
    }
}

/// Trigger verbs on a built [`TimelineComponent`]. Blanket-implemented for every
/// `T: TimelineComponent`. Multiple writers to one [`Event`] ⇒ EARLIEST wins
/// (resolved in the place pass).
///
/// VERB ORDERING (`.sketch/01` A.6): a `.trigger_*` records its time against the
/// wrapped node's OWN resolved interval (`abs_start` / `abs_start + len` /
/// `abs_start + local`), where `abs_start` is whatever the parent hands the
/// [`Triggered`] wrapper. So put the trigger OUTERMOST: either trigger a child
/// the container positions (e.g. a `Sequence`/`Timeline` child —
/// `Dialogue::builder()…​.trigger_at_start(e)`, the canonical usage; the
/// container hands it its resolved start), or trigger THEN place
/// (`x.trigger_at_start(e).at(5.0)`).
///
/// The inverted order `x.at(5.0).trigger_at_start(e)` wraps a `Placed` whose
/// inner relative `5.0` is the CHILD's offset, not the wrapper's: the wrapper
/// still receives its own `abs_start` and records the event THERE, ignoring the
/// inner `5.0`. This is intentional — a `Triggered` fires at the interval IT is
/// handed, and "fold a wrapped child's leading offset into my own trigger" would
/// only handle a single immediate `Placed` and silently misbehave for any other
/// nesting. See `triggered_over_placed_ignores_inner_offset` for the documented
/// behaviour and `placed_over_triggered_keeps_offset` for the correct order.
pub trait Triggers: TimelineComponent + Sized {
    /// Records the [`Event`] at the wrapped node's resolved START (the
    /// `abs_start` the parent hands this wrapper). See the trait-level VERB
    /// ORDERING note: trigger outermost (or trigger a container-positioned
    /// child), not `x.at(off).trigger_at_start(e)` — that records at the
    /// wrapper's start, NOT accounting for the inner placement's `off`.
    fn trigger_at_start(self, e: Event) -> Triggered<Self> {
        Triggered {
            child: self,
            event: e,
            kind: TriggerKind::Start,
        }
    }

    fn trigger_at_end(self, e: Event) -> Triggered<Self> {
        Triggered {
            child: self,
            event: e,
            kind: TriggerKind::End,
        }
    }

    /// At a local offset into the clip (an interior beat).
    fn trigger_at(self, local: f32, e: Event) -> Triggered<Self> {
        Triggered {
            child: self,
            event: e,
            kind: TriggerKind::At(local),
        }
    }
}

impl<T: TimelineComponent + Sized> Triggers for T {}

/// Buildless twin of [`Triggers`], over complete builders.
pub trait TriggersBuilder: TimelineBuilder {
    fn trigger_at_start(self, e: Event) -> Triggered<Self::Output> {
        self.build_component().trigger_at_start(e)
    }

    fn trigger_at_end(self, e: Event) -> Triggered<Self::Output> {
        self.build_component().trigger_at_end(e)
    }

    fn trigger_at(self, local: f32, e: Event) -> Triggered<Self::Output> {
        self.build_component().trigger_at(local, e)
    }
}

impl<B: TimelineBuilder> TriggersBuilder for B {}

// ── The clock a component is sampled with ────────────────────────────────────

/// What `#[clock]` injects — BOTH time axes for this frame, plus a borrowed,
/// read-only handle to the resolved [`TriggerTable`] so [`Event`] queries can
/// resolve their id.
///
/// `Clock<'a>` borrows the trigger table (`.sketch/02 §8`): the resolved tree
/// owns the one [`TriggerTable`] by value and lends a `&` to each frame's
/// clock, which keeps `Clock: Copy` (both time types are `Copy`).
#[derive(Debug, Clone, Copy)]
pub struct Clock<'a> {
    global: TimelineTime,
    local: LocalTime,
    triggers: &'a TriggerTable,
    /// Resolved LOCAL window length (this component's own post-stretch seconds),
    /// or `None` for an open-ended placement (`.fill()`, a bare timeless point,
    /// the root). Carried for FRAME only — structure is never window-aware.
    window: Option<f32>,
}

impl<'a> Clock<'a> {
    /// Constructs a clock for one frame from both axes and the resolved table.
    pub fn new(global: TimelineTime, local: LocalTime, triggers: &'a TriggerTable) -> Self {
        Self {
            global,
            local,
            triggers,
            window: None,
        }
    }

    /// A neutral, time-zero clock over a shared empty [`TriggerTable`].
    ///
    /// Used by the `#[component(timeline)]` macro's clock-less delegators
    /// (`duration`/`measure`/`resolve`/`cues`/`arrangement`) to build the body
    /// when there is no per-frame clock to forward. This is sound because a
    /// component's STRUCTURE must be clock-independent by design (the audit
    /// model: `frame`/`samples` bake per-frame values into a stable structure,
    /// so the resolved shape never varies with the clock value). A body that
    /// branches its structure on `clock` violates that contract.
    pub fn structural() -> Clock<'static> {
        static EMPTY: OnceLock<TriggerTable> = OnceLock::new();
        let triggers = EMPTY.get_or_init(TriggerTable::new);
        Clock {
            global: TimelineTime::new(0.0),
            local: LocalTime::new(0.0),
            triggers,
            window: None,
        }
    }

    /// 0 at THIS component's resolved start; survives `Sequence` re-flow.
    /// Self-animation: `clock.local().phase(0.0, 0.4)`.
    pub fn local(&self) -> LocalTime {
        self.local
    }

    /// Pure rebase: shifts the child's local axis but PRESERVES the window (used
    /// where the rebase does not change which window the child lives in).
    pub fn with_local(&self, local: LocalTime) -> Clock<'a> {
        Clock {
            global: self.global,
            local,
            triggers: self.triggers,
            window: self.window,
        }
    }

    /// Rebase AND set the child's resolved local window length in one step. The
    /// soundness rule (`.sketch/02 §8`): the window is set ONLY by the node that
    /// owns it (a `Placed` / `Sequence` slot) at the same site it rebases, never
    /// carried-then-cleared. A pure-rebase node uses [`with_local`] instead.
    pub fn with_local_window(&self, local: LocalTime, window: Option<f32>) -> Clock<'a> {
        Clock {
            global: self.global,
            local,
            triggers: self.triggers,
            window,
        }
    }

    /// Absolute frame time — the SAME axis as [`Event`] triggers.
    pub fn global(&self) -> TimelineTime {
        self.global
    }

    /// The resolved LOCAL window length (this component's own slot, in its own
    /// post-stretch seconds), or `None` for an open-ended placement (`.fill()`,
    /// a bare timeless point, the root). End-relative effects read this.
    pub fn window(&self) -> Option<f32> {
        self.window
    }

    /// Time remaining until the window closes (`window - local`, clamped at 0),
    /// or `None` if the placement is open-ended. The end-relative twin of
    /// [`local`](Self::local): `clock.remaining().map(|r| r.phase(0.0, 0.4))`
    /// ramps a fade-OUT over the last 0.4s.
    pub fn remaining(&self) -> Option<LocalTime> {
        self.window
            .map(|w| LocalTime::new((w - self.local.seconds()).max(0.0)))
    }

    /// A fade envelope over this component's window: opacity ramps 0→1 over the
    /// first `fade_in` seconds, holds 1, then ramps 1→0 over the last `fade_out`
    /// seconds, and stays 0 past the window. An open-ended window (`None`) has no
    /// end, so it fades IN only (the pre-window behavior). Because the fade-out
    /// term is exactly 0 once `local >= window` (`phase` clamps via
    /// `Phase::saturating`), an envelope doubles as a self-contained
    /// appear/disappear for a caption.
    pub fn envelope(&self, fade_in: f32, fade_out: f32) -> Phase {
        let rise = if fade_in <= 0.0 {
            1.0
        } else {
            self.local().phase(0.0, fade_in).get()
        };
        let fall = match self.window {
            Some(w) if fade_out > 0.0 => 1.0 - self.local().phase(w - fade_out, w).get(),
            _ => 1.0,
        };
        Phase::saturating(rise * fall)
    }

    /// Resolved trigger time of `e`, or `+∞` if unfired. Used by [`Event`]'s
    /// queries; not called directly by authors.
    pub(crate) fn trigger_of(&self, e: Event) -> TimelineTime {
        self.triggers.get(e.id())
    }
}

// ── Resolve-pass state (`.sketch/02 §7`) ─────────────────────────────────────

/// `Event` id → absolute trigger time. One per resolved tree, built by the
/// place pass. Earliest-wins on insert; an absent id reads as `+∞`.
#[derive(Debug, Clone, Default)]
pub struct TriggerTable {
    map: HashMap<u64, f32>,
}

impl TriggerTable {
    /// An empty table — every id reads as `+∞`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records `time` for `event`, keeping the EARLIEST write (`.sketch/02 §6`).
    pub fn record(&mut self, event: Event, time: f32) {
        self.record_id(event.id(), time);
    }

    /// `record` by raw id (the resolve pass works in ids).
    pub fn record_id(&mut self, id: u64, time: f32) {
        self.map
            .entry(id)
            .and_modify(|t| {
                if time < *t {
                    *t = time;
                }
            })
            .or_insert(time);
    }

    /// Resolved trigger time for `id`, or `+∞` if absent.
    pub fn get(&self, id: u64) -> TimelineTime {
        TimelineTime::new(self.map.get(&id).copied().unwrap_or(f32::INFINITY))
    }

    /// Whether `id` has a recorded trigger time.
    pub fn contains(&self, id: u64) -> bool {
        self.map.contains_key(&id)
    }
}

/// Mutable state threaded through the place pass (`.sketch/02 §6/§7`). Holds the
/// trigger table under construction plus the absolute-start accounting the walk
/// needs.
#[derive(Debug)]
pub struct ResolveCtx {
    triggers: TriggerTable,
    /// Absolute start of the node currently being resolved. Top-down bookkeeping
    /// the container walk maintains; surfaced for leaves that need it.
    abs_start: f32,
    /// Non-fatal diagnostics collected during the walk (`.sketch/02 §9`). A
    /// container (step 4) emits here when, e.g., all of a non-fill container's
    /// children are `.fill()` — a determinate `0.0` that is almost always an
    /// authoring mistake, so it warns rather than fails.
    warnings: Vec<String>,
    /// FATAL diagnostics collected during the walk (`.sketch/02 §6`, ZONE C #1).
    /// A `Sequence` (in `timeline_container`) pushes here when it sees a
    /// `.fill()` child — illegal in an in-a-row container (a `.fill()` needs a
    /// container length, which a `Sequence` does not impose). The entry
    /// [`resolve`] turns any collected error into a [`ResolveError::Invalid`].
    errors: Vec<String>,
    /// Cumulative absolute-seconds-per-local-second of the enclosing stretched
    /// `.at(a..b)` windows: `1.0` at the top, smaller once inside a window that
    /// compresses its child. [`Placed::resolve`] folds each window's `speed()`
    /// in around its recursion; `Triggered` / `Sequence` multiply their LOCAL
    /// offsets by it so interior trigger times land on the global axis
    /// (`.sketch/01 §A.3`).
    local_scale: f32,
}

impl Default for ResolveCtx {
    fn default() -> Self {
        Self {
            triggers: TriggerTable::default(),
            abs_start: 0.0,
            warnings: Vec::new(),
            errors: Vec::new(),
            local_scale: 1.0,
        }
    }
}

impl ResolveCtx {
    /// A fresh context with an empty trigger table.
    pub fn new() -> Self {
        Self::default()
    }

    /// The trigger table under construction.
    pub fn triggers(&self) -> &TriggerTable {
        &self.triggers
    }

    /// Mutable access to the trigger table under construction.
    pub fn triggers_mut(&mut self) -> &mut TriggerTable {
        &mut self.triggers
    }

    /// Absolute start of the node currently being resolved.
    pub fn abs_start(&self) -> f32 {
        self.abs_start
    }

    /// Cumulative window-stretch scale (absolute seconds per current-level local
    /// second). Containers in other modules multiply their local cursor offsets
    /// by this so children land at the right absolute start under a stretch.
    pub fn local_scale(&self) -> f32 {
        self.local_scale
    }

    /// Sets the absolute start for the node currently being resolved.
    pub fn set_abs_start(&mut self, abs_start: f32) {
        self.abs_start = abs_start;
    }

    /// Records a non-fatal resolve diagnostic (`.sketch/02 §9`). Containers
    /// (step 4) call this for the all-fill / empty-interior `0.0` cases.
    pub fn warn(&mut self, message: impl Into<String>) {
        self.warnings.push(message.into());
    }

    /// The diagnostics collected so far.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Records a FATAL resolve diagnostic (`.sketch/02 §6`, ZONE C #1). A
    /// `Sequence` calls this when it sees a `.fill()` child; the entry
    /// [`resolve`] fails with a [`ResolveError::Invalid`] if any error landed.
    pub fn error(&mut self, message: impl Into<String>) {
        self.errors.push(message.into());
    }

    /// The fatal diagnostics collected so far.
    pub fn errors(&self) -> &[String] {
        &self.errors
    }

    /// Consumes the context, yielding the finished trigger table.
    pub fn into_triggers(self) -> TriggerTable {
        self.triggers
    }

    /// Consumes the context, yielding the finished trigger table, the collected
    /// warnings, and the collected fatal errors — the form [`resolve`] uses to
    /// assemble a [`ResolvedTimeline`] (or to fail on the first error).
    pub fn into_parts(self) -> (TriggerTable, Vec<String>, Vec<String>) {
        (self.triggers, self.warnings, self.errors)
    }
}

/// Why a resolve pass failed (`.sketch/02 §5 M4`, §12 M5).
#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    /// A media probe (duration read) failed; carries a human-readable reason.
    #[error("media probe failed: {0}")]
    Probe(String),
    /// The root has no intrinsic duration (a purely timeless tree). Collapsing
    /// it to `0.0` would silently emit a zero-frame export, so it is an error.
    #[error("root timeline is timeless: place media or an explicit window to give it a length")]
    Timeless,
    /// An invalid arrangement caught during the place pass (`.sketch/02 §6`),
    /// e.g. a `.fill()` child inside a `Sequence` (ZONE C #1). Carries the
    /// human-readable reason collected via [`ResolveCtx::error`].
    #[error("invalid timeline arrangement: {0}")]
    Invalid(String),
}

// ── Resolve entry + resolved output (`.sketch/02 §3/§7`) ─────────────────────

/// The output of the resolve pass (`.sketch/02 §3/§7`). Owns the built source
/// tree WHOLE (audit M1 — no borrow, no `Rc`, no per-child decomposition) and
/// the only global metadata the pass produces: the flat [`TriggerTable`] plus
/// the root's resolved length.
///
/// Per-frame SAMPLING (step 5) runs `&self` recursion over [`source`] using the
/// borrowed [`triggers`] — this type just holds the resolved state.
///
/// `Send` because it is stored in the plugin collection later (audit M2); the
/// guarantee is asserted just below.
pub struct ResolvedTimeline {
    /// The built source tree, owned intact. The place pass walks it via `&self`
    /// recursion; sampling (step 5) walks the same `Box` per frame.
    source: Box<dyn TimelineComponent + Send>,
    /// `Event` id → absolute trigger time, built by the place pass. One per
    /// resolved tree; earliest-wins, absent reads as `+∞`.
    triggers: TriggerTable,
    /// The root's resolved length in seconds. Never `None`: a timeless root is
    /// a [`ResolveError::Timeless`] instead (audit M4), not a coerced `0.0`.
    duration: f32,
    /// Non-fatal diagnostics surfaced from the walk (`.sketch/02 §9`), e.g. an
    /// all-fill container's determinate `0.0`.
    warnings: Vec<String>,
    /// The composition's fixed LOGICAL layout space (resolution-independent).
    /// The pixel `target` passed to `frame` scales this canvas; layout never
    /// depends on the target's magnitude.
    canvas: Vec2,
}

// Compile-time guarantee that `ResolvedTimeline` is `Send` (audit M2): it is
// stored in the plugin collection and moved across threads (`server.rs`).
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<ResolvedTimeline>();
};

impl ResolvedTimeline {
    /// The owned source tree. Sampling (step 5) drives its channels through this
    /// borrow.
    pub fn source(&self) -> &(dyn TimelineComponent + Send) {
        &*self.source
    }

    /// The resolved trigger table. Lent to each frame's [`Clock`] so [`Event`]
    /// queries can resolve their id (`.sketch/02 §8`).
    pub fn triggers(&self) -> &TriggerTable {
        &self.triggers
    }

    /// The root's resolved length in seconds.
    pub fn duration(&self) -> f32 {
        self.duration
    }

    /// The composition's fixed LOGICAL layout space (resolution-independent).
    /// The pixel `target` passed to [`frame`](Self::frame) scales this canvas.
    pub fn canvas(&self) -> Vec2 {
        self.canvas
    }

    /// Non-fatal diagnostics collected during the resolve walk (`.sketch/02
    /// §9`).
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Samples the visual channel at global time `t` (`.sketch/02 §8`).
    ///
    /// Builds the ROOT clock — `global = t`, `local = t` (the root's resolved
    /// start is `0.0`, so its local axis coincides with the global one) — over
    /// the resolved [`triggers`](Self::triggers), then drives `&self` recursion
    /// through [`source`](Self::source). `None` ⇒ the timeline contributes
    /// nothing at `t` (a fully transparent / empty frame).
    pub fn frame(
        &self,
        t: TimelineTime,
        target: Resolution,
        ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage> {
        let clock = Clock::new(t, LocalTime::new(t.seconds()), self.triggers());
        self.source().frame(clock, self.canvas, target, ctx)
    }

    /// Samples the audio channel for `[t, t + window)`.
    ///
    /// The eager mix-down ([`render_audio`](Self::render_audio)) is the path the
    /// encoder uses; this per-window pull is the (unused) `samples(Clock,
    /// window)` seam, which forwards to the source's still-`None` `samples`.
    pub fn samples(&self, t: TimelineTime, window: f32) -> Option<AudioBuffer> {
        let clock = Clock::new(t, LocalTime::new(t.seconds()), self.triggers());
        self.source().samples(clock, window)
    }

    /// EAGER MIX-DOWN of the whole audio track (step 8, B4 v1 — `.sketch/01`
    /// ZONE C / `.sketch/02 §15`).
    ///
    /// Allocates one interleaved f32 buffer of the resolved
    /// [`duration`](Self::duration) at the encoder's fixed `rate` / `channels`,
    /// then walks the source tree via [`mix_into`](TimelineComponent::mix_into):
    /// each [`AudioFile`](crate::timeline_container::AudioFile) decodes (honoring
    /// its `.trim`), resamples / re-channels / gain-scales into the fixed layout
    /// at the placement speed, and SUMS into the mix at its resolved start
    /// (clamping on overflow). The encoder feeds the result to ffmpeg as a temp
    /// WAV second input.
    pub fn render_audio(&self, rate: u32, channels: u16) -> AudioBuffer {
        let mut mix = crate::audio::AudioMix::new(self.duration, rate, channels);
        self.source().mix_into(&mut mix, 0.0, 1.0);
        mix.into_buffer()
    }
}

impl std::fmt::Debug for ResolvedTimeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `dyn TimelineComponent` is not `Debug`; show the resolved metadata.
        f.debug_struct("ResolvedTimeline")
            .field("duration", &self.duration)
            .field("triggers", &self.triggers)
            .field("warnings", &self.warnings)
            .finish_non_exhaustive()
    }
}

/// Runs the resolve pass over a built timeline tree (`.sketch/02 §2/§3/§7`),
/// CONSUMING it (audit M1) and producing a [`ResolvedTimeline`] that owns the
/// tree whole.
///
/// The pass is two phases, both pure over the built tree (no pixel resolution):
///
/// 1. MEASURE (bottom-up): `root.measure()` folds intrinsic durations, with
///    every `.fill()` child EXCLUDED (the load-bearing acyclicity invariant,
///    `.sketch/02 §5`). A `None` here means the root is purely timeless — no
///    media, no explicit window. Coercing that to `0.0` would silently emit a
///    zero-frame export, so it is a [`ResolveError::Timeless`] (audit M4), NOT
///    a `0.0`. (The internal all-fill / empty case stays a determinate `0.0`;
///    only the ROOT being `None` is fatal.)
/// 2. PLACE (top-down): `root.resolve(0.0, &mut ResolveCtx)` walks the tree,
///    assigning absolute starts and recording every [`Triggered`] node's
///    absolute time into the [`TriggerTable`] (earliest-wins). Containers
///    (step 4) recurse over their own children via `&self` recursion and may
///    [`warn`](ResolveCtx::warn) for the all-fill `0.0` case.
///
/// Media-leaf duration PROBING (`.sketch/02 §12`, [`ResolveError::Probe`])
/// happens through the leaves during the measure fold above: `VideoFile` /
/// `AudioFile` report their probed source length as their `duration`.
pub fn resolve(
    root: impl TimelineComponent + Send + 'static,
) -> Result<ResolvedTimeline, ResolveError> {
    resolve_with_canvas(root, DEFAULT_CANVAS)
}

/// The default authoring canvas (1080p logical space) used when a composition
/// does not declare one. The pixel target scales this; layout is resolution-
/// independent.
pub const DEFAULT_CANVAS: Vec2 = Vec2(1920.0, 1080.0);

/// Resolve `root` against an explicit logical `canvas` (the composition's
/// authored layout space). [`resolve`] is this with [`DEFAULT_CANVAS`].
pub fn resolve_with_canvas(
    root: impl TimelineComponent + Send + 'static,
    canvas: Vec2,
) -> Result<ResolvedTimeline, ResolveError> {
    // Own the source root intact (audit M1): box it once and keep it whole.
    let source: Box<dyn TimelineComponent + Send> = Box::new(root);

    // Phase 1 — measure. A `None` root is a timeless tree: error, not 0.0 (M4).
    let duration = source.measure().ok_or(ResolveError::Timeless)?;

    // Phase 2 — place: drive the top-down walk so every `Triggered` records its
    // absolute time into the table (earliest-wins) and containers can warn /
    // error. A `Sequence` with a `.fill()` child records a fatal error here.
    let mut ctx = ResolveCtx::new();
    source.resolve(0.0, &mut ctx);
    let (triggers, warnings, errors) = ctx.into_parts();

    // A collected error (e.g. `.fill()` in a `Sequence`) fails the whole pass;
    // report the first, mirroring how `measure` fails fast on a timeless root.
    if let Some(first) = errors.into_iter().next() {
        return Err(ResolveError::Invalid(first));
    }

    Ok(ResolvedTimeline {
        source,
        triggers,
        duration,
        warnings,
        canvas,
    })
}

// ── Channels / output types (`.sketch/01` A.7) ──────────────────────────────

/// Interleaved f32 samples + rate. A minimal skeleton; the encoder fixes one
/// output rate + channel layout and leaves resample into it (`.sketch/01` A.7).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioBuffer {
    /// Interleaved f32 PCM samples.
    pub samples: Vec<f32>,
    /// Sample rate in Hz.
    pub rate: u32,
    /// Number of interleaved channels.
    pub channels: u16,
}

impl AudioBuffer {
    /// An empty buffer at the given rate / channel layout.
    pub fn empty(rate: u32, channels: u16) -> Self {
        Self {
            samples: Vec::new(),
            rate,
            channels,
        }
    }
}

/// One subtitle interval, absolute on the timeline (after [`cues`](TimelineComponent::cues)).
#[derive(Debug, Clone, PartialEq)]
pub struct Cue {
    pub start: f32,
    pub end: f32,
    pub text: String,
}

/// One resolved [`Event`] marker on a node: the absolute time it fires, plus the
/// event's optional display name (from [`Event::named`]) so the live UI can
/// label the marker. The name is owned (`String`) so the arrangement is a
/// self-contained, serializable snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct TriggerMark {
    pub time: f32,
    pub name: Option<String>,
}

/// The CALL-SITE source location of a node: the `file:line` of the `.child(...)`
/// call that placed the component into its container. Surfaced so the live UI
/// can jump a clicked node back to its authoring line. Owned (`String`) so the
/// arrangement is a self-contained, serializable snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceLoc {
    pub file: String,
    pub line: u32,
}

/// What the live UI draws — the resolved arrangement of a node and its
/// children. Built by walking the RESOLVED tree (`.sketch/01` A.7 / B.4).
///
/// `trim` carries the source crop separately so the UI can show both the placed
/// bar and the source crop; `triggers` surfaces where [`Event`]s fire (each a
/// [`TriggerMark`] carrying the time and the event's optional name); `source` is
/// the `.child(...)` call site that placed the node (see [`SourceLoc`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Arrangement {
    pub kind: NodeKind,
    pub label: String,
    /// Human-readable DISPLAY NAME for a `#[component(...)]` node, distinct from
    /// [`label`](Self::label) (which carries body-specific content such as a
    /// caption's text or a clip's source path). Auto-derived from the component's
    /// PascalCase name, or set from an explicit `name = "..."` template; `None`
    /// for nodes that have no enclosing named component.
    pub name: Option<String>,
    /// The `.child(...)` CALL SITE that placed this node, captured by the
    /// generated container setter via `#[track_caller]`; `None` for the root and
    /// for nodes built outside a tracked setter.
    pub source: Option<SourceLoc>,
    pub start: f32,
    pub end: f32,
    pub trim: Option<(f32, f32)>,
    pub triggers: Vec<TriggerMark>,
    pub children: Vec<Arrangement>,
}

/// The kind of node the live UI renders.
///
/// The display side collapses to three TRACK kinds — `Video` (映像: every
/// rasterized/timeless visual, including backdrops, captions, and reveals),
/// `Audio` (音声), and `Subtitle` (字幕) — plus the two structural containers
/// (`Timeline` / `Sequence`). There is intentionally no separate caption kind:
/// a styled `Text` telop is a visual and lives on the video track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeKind {
    Video,
    Audio,
    Subtitle,
    Timeline,
    Sequence,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{Constraints, Vec2};
    use crate::raster::PixelFormat;
    use crate::render_context::PassThrough;

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
        let frame = dot.frame(clock, Vec2(4.0, 4.0), Resolution::new(4, 4), &mut ctx);
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
            .frame(clock, canvas, target, &mut ctx)
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

    use crate::time::Time;

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
        let frame = pulse.frame(clock, Vec2(4.0, 4.0), Resolution::new(4, 4), &mut ctx);
        assert!(frame.is_some());

        // Clock-less queries still resolve via the structural clock.
        assert_eq!(pulse.duration(), Some(1.0));

        let builder = Pulse::builder().start(1.0);
        assert_timeline_builder(&builder);
        let _boxed: Box<dyn TimelineComponent + Send> = assert_boxable(pulse);
    }

    #[test]
    fn clock_window_surfaces_and_drives_envelope_and_remaining() {
        let table = TriggerTable::new();
        let base = Clock::new(TimelineTime::new(0.0), LocalTime::new(0.0), &table);

        // No window ⇒ open-ended: remaining is None and envelope fades IN only.
        let open = base.with_local_window(LocalTime::new(5.0), None);
        assert_eq!(open.window(), None);
        assert!(open.remaining().is_none());
        assert_eq!(
            open.envelope(0.4, 0.4).get(),
            1.0,
            "open-ended holds after fade-in"
        );

        // A 3s window: remaining counts down and clamps at 0; envelope rises over
        // the first 0.5s, holds, falls over the last 0.5s, 0 at/after the end.
        let at = |t: f32| base.with_local_window(LocalTime::new(t), Some(3.0));
        assert!((at(1.0).remaining().unwrap().seconds() - 2.0).abs() < 1e-6);
        assert_eq!(
            at(4.0).remaining().unwrap().seconds(),
            0.0,
            "remaining clamps at 0"
        );
        assert_eq!(at(0.0).envelope(0.5, 0.5).get(), 0.0, "0 at start");
        assert!(
            (at(0.5).envelope(0.5, 0.5).get() - 1.0).abs() < 1e-6,
            "full after fade-in"
        );
        assert!(
            (at(1.5).envelope(0.5, 0.5).get() - 1.0).abs() < 1e-6,
            "held at full"
        );
        assert_eq!(at(3.0).envelope(0.5, 0.5).get(), 0.0, "0 at the window end");
        assert_eq!(
            at(4.0).envelope(0.5, 0.5).get(),
            0.0,
            "stays 0 past the window"
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
        let resolved =
            resolve(Dot.at(5.0..8.0).trigger_at_end(e)).expect("windowed, so not timeless");
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
}

// ── Step 6: the Event path, end-to-end (`.sketch/01` B.1b/B.2) ───────────────
//
// These integration tests build the `.sketch/01` shape — a `Sequence` of
// `Dialogue`s whose line-2 fires an `Event` at its RESOLVED start, plus an
// overlay `Reveal` that animates off that event — and prove the WHOLE path:
// resolve glues the trigger to the resolved start, re-flow re-glues it, and the
// per-frame `Clock` reads the resolved table so `event.phase(&clock)` ramps the
// reveal's opacity. Media decode is stubbed, so voice lengths are injected on
// `AudioFile::duration`.
#[cfg(test)]
mod event_path_tests {
    use super::*;
    use crate::geometry::{Constraints, Vec2};
    use crate::raster::{PixelFormat, RasterComponent};
    use crate::render_context::PassThrough;
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
        fn render(&self, _s: Vec2, t: Resolution, _ctx: &mut dyn RenderContext) -> RasterImage {
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
            .frame(TimelineTime::new(t), Resolution::new(2, 2), &mut ctx)
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
}
