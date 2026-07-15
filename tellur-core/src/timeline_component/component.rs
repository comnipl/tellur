//! The [`TimelineComponent`] trait, its builder marker, and the one-way
//! blanket that lifts every [`RasterComponent`] into the timeline world.

use std::hash::Hash;

use crate::dyn_compare::{DynEq, DynHash};
use crate::geometry::{Rect, Vec2};
use crate::layer::composite_children;
use crate::raster::{RasterComponent, RasterImage, RasterResidency, Resolution};
use crate::render_context::RenderContext;

use super::*;

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
/// `Vec<Box<dyn TimelineComponent + Send>>` (e.g.
/// [`Timeline`](crate::timeline_container::Timeline) /
/// [`Sequence`](crate::timeline_container::Sequence))
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

    /// Visual channel for this frame with the representation requested by its
    /// consumer. `clock` carries both time axes (see [`Clock`]); `canvas` is
    /// the composition's fixed LOGICAL layout space (resolution-independent),
    /// which the pixel `target` scales. `None` ⇒ contributes nothing visually.
    fn frame(
        &self,
        clock: Clock<'_>,
        canvas: Vec2,
        target: Resolution,
        residency: RasterResidency,
        ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage> {
        // TODO(task 4): leaves/containers produce real frames.
        let _ = (clock, canvas, target, residency, ctx);
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
    /// `start_secs` exactly as it advances absolute starts (a
    /// [`Sequence`](crate::timeline_container::Sequence)
    /// cursor sums prior lengths; a
    /// [`Timeline`](crate::timeline_container::Timeline) overlays at the same
    /// base), and
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
        residency: RasterResidency,
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
        let canvas_rect = Rect {
            origin: Vec2::ZERO,
            size: canvas,
        };
        if self.paint_bounds(canvas) == canvas_rect {
            return Some(ctx.render(self, canvas, target, residency));
        }
        // A visual painting outside its layout box (a drop shadow, an
        // outline) expects its pixel `target` to span `paint_bounds`, not the
        // canvas (`composite_children`'s contract) — handing it the frame's
        // `target` directly would squeeze the wider paint bounds into the
        // canvas-sized pixel grid, shrinking and distorting the content.
        // Composite it like a `Layer` child instead: render at paint-bounds
        // resolution, blit at the painted origin, clip spill at the frame edge.
        Some(composite_children(
            canvas_rect,
            target,
            &[(Vec2::ZERO, canvas, self as &dyn RasterComponent)],
            residency,
            ctx,
        ))
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
