//! [`Placement`] / [`Placed`] and the `.at(..)` / `.fill()` / `.trim(..)`
//! placement verbs — where a component sits on the parent clock.

use std::ops::Range;

use crate::geometry::Vec2;
use crate::raster::{RasterImage, RasterResidency, Resolution};
use crate::render_context::RenderContext;
use crate::time::{LocalTime, Time};

use super::*;

// ── Placement ───────────────────────────────────────────────────────────────

/// Where a component sits on the PARENT clock.
///
/// `From<f64>` = a start point (it plays for its own
/// [`duration`](TimelineComponent::duration) at native speed); `From<Range<f64>>`
/// = an explicit `start..end` window. For a TIMELESS visual/subtitle the window
/// just gives it that interval. For a TIMED component a window ≠ its length is a
/// STRETCH that time-scales the (trimmed) source to fill the window, so
/// `speed = content_duration / (b - a)` (decided semantics, `.sketch/01` A.3).
/// There is no separate `.speed()` — to merely truncate, [`Timed::trim`] the
/// source.
#[derive(Debug, Clone, Copy, crate::Keyable)]
pub struct Placement {
    /// Relative start on the parent clock.
    start: f64,
    /// Explicit window end (exclusive); `None` for a bare start point, whose
    /// end is the inner component's own duration.
    end: Option<f64>,
    /// Whether this placement is a [`fill`](Timed::fill): it takes the
    /// CONTAINER's resolved length (set by the overlay `Timeline` in its
    /// second sub-pass) and is EXCLUDED from the container's length measure
    /// (the load-bearing acyclicity invariant, `.sketch/02 §5/§9`). A fill
    /// child inside a `Sequence` is a resolve error (ZONE C #1).
    fill: bool,
}

impl Placement {
    /// Relative start on the parent clock.
    pub fn start(&self) -> f64 {
        self.start
    }

    /// Explicit window end, if this placement is a `start..end` window.
    pub fn end(&self) -> Option<f64> {
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

impl From<f64> for Placement {
    fn from(start: f64) -> Self {
        Self {
            start,
            end: None,
            fill: false,
        }
    }
}

impl From<Range<f64>> for Placement {
    fn from(window: Range<f64>) -> Self {
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
    pub fn speed(&self) -> f64 {
        self.speed_with_fill_length(None)
    }

    /// Parent-clock window length for this placement. A fill's window only
    /// becomes known when its owning [`Timeline`](crate::timeline_container::Timeline)
    /// supplies the resolved container length.
    fn parent_window_length(&self, fill_length: Option<f64>) -> Option<f64> {
        if self.is_fill() {
            fill_length
        } else {
            self.placement.end.map(|end| end - self.placement.start)
        }
    }

    /// The ordinary placement speed, extended with the owning container's
    /// resolved length for a fill. Timeless fills deliberately remain at
    /// native speed, matching their pre-stretch behaviour.
    fn speed_with_fill_length(&self, fill_length: Option<f64>) -> f64 {
        match (self.parent_window_length(fill_length), self.child.measure()) {
            (Some(window), Some(content)) if window > 0.0 => content / window,
            _ => 1.0,
        }
    }

    fn resolve_with_fill_length(
        &self,
        abs_start: f64,
        fill_length: Option<f64>,
        out: &mut ResolveCtx,
    ) -> f64 {
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

        // Recurse at the child's absolute start, folding the window stretch in.
        // A fill uses the same affine rule once its owning Timeline supplies the
        // container length. The relative start is in THIS level's local seconds,
        // so scale it to absolute by the enclosing `local_scale`.
        let scale = out.local_scale;
        let child_abs = abs_start + self.placement.start * scale;
        let saved = out.local_scale;
        out.local_scale = scale / self.speed_with_fill_length(fill_length);
        let child_len = self.child.resolve(child_abs, out);
        out.local_scale = saved;

        // Preserve Placed's established resolve return: its playable duration,
        // excluding a leading relative offset. Triggered-over-Placed relies on
        // that interval contract. Containers use `measure()` as the authoritative
        // footprint when advancing/folding their cursors, so an outer delegating
        // wrapper still retains the leading offset without changing trigger order.
        fill_length.unwrap_or_else(|| self.duration().unwrap_or(self.placement.start + child_len))
    }

    /// Resolves a fill against its owning Timeline's already measured length.
    pub(crate) fn resolve_fill(
        &self,
        abs_start: f64,
        fill_length: f64,
        out: &mut ResolveCtx,
    ) -> f64 {
        debug_assert!(self.is_fill());
        self.resolve_with_fill_length(abs_start, Some(fill_length), out)
    }

    fn frame_with_fill_length(
        &self,
        clock: Clock<'_>,
        fill_length: Option<f64>,
        canvas: Vec2,
        target: Resolution,
        residency: RasterResidency,
        ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage> {
        let t = clock.local().seconds();
        let speed = self.speed_with_fill_length(fill_length);
        let parent_window = self.parent_window_length(fill_length);

        // Temporal gate: a placed clip contributes only within its resolved
        // parent-clock interval. A fill without a known owner length and an
        // open-ended timeless point remain ungated, preserving the old fallback.
        let active = match parent_window {
            Some(window) => t >= self.placement.start && t < self.placement.start + window,
            None if self.is_fill() => true,
            None => match self.child.measure() {
                Some(duration) => {
                    t >= self.placement.start && t < self.placement.start + duration / speed
                }
                None => true,
            },
        };
        if !active {
            return None;
        }

        let rebased = (t - self.placement.start) * speed;
        // A timed fill exposes the child's own post-stretch duration, just like
        // an explicit stretch window. A timeless fill remains open-ended so its
        // established clock behaviour does not change.
        let window = if self.is_fill() {
            match (fill_length, self.child.measure()) {
                (Some(length), Some(_)) => Some(length * speed),
                _ => None,
            }
        } else {
            match self.placement.end {
                Some(end) => Some((end - self.placement.start) * speed),
                None => self.child.measure(),
            }
        };
        let child_clock = clock.with_local_window(LocalTime::new(rebased), window);
        self.child
            .frame(child_clock, canvas, target, residency, ctx)
    }

    /// Samples a fill using its owning Timeline's resolved length.
    pub(crate) fn frame_fill(
        &self,
        clock: Clock<'_>,
        fill_length: f64,
        canvas: Vec2,
        target: Resolution,
        residency: RasterResidency,
        ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage> {
        debug_assert!(self.is_fill());
        self.frame_with_fill_length(clock, Some(fill_length), canvas, target, residency, ctx)
    }

    fn render_audio_block_with_fill_length(
        &self,
        mut block: AudioBlockMut<'_>,
        fill_length: Option<f64>,
        ctx: &mut AudioRenderContext,
    ) {
        let request = block.request();
        let speed = self.speed_with_fill_length(fill_length);
        let parent_window = self.parent_window_length(fill_length);
        let active_end = match parent_window {
            Some(window) => Some(self.placement.start + window),
            None if self.is_fill() => None,
            None => self
                .child
                .measure()
                .map(|duration| self.placement.start + duration / speed),
        };
        if active_end.is_some_and(|end| !request.may_overlap_local(self.placement.start, end)) {
            block.clear();
            return;
        }

        let child_request = request.with_local_timing(
            (request.local_start() - self.placement.start) * speed,
            request.local_step() * speed,
        );
        let mut scratch = ctx.take_scratch(request.sample_len());
        self.child
            .render_audio_block(AudioBlockMut::new(child_request, &mut scratch), ctx);

        block.clear();
        let channels = request.channels() as usize;
        for frame in 0..request.frame_count() {
            let t = request.time_at(frame);
            let active = active_end.is_none_or(|end| t >= self.placement.start && t < end);
            if active {
                let base = frame * channels;
                block.samples_mut()[base..base + channels]
                    .copy_from_slice(&scratch[base..base + channels]);
            }
        }
        ctx.recycle_scratch(scratch);
    }

    /// Renders a fill using its owning Timeline's resolved length.
    pub(crate) fn render_audio_fill(
        &self,
        block: AudioBlockMut<'_>,
        fill_length: f64,
        ctx: &mut AudioRenderContext,
    ) {
        debug_assert!(self.is_fill());
        self.render_audio_block_with_fill_length(block, Some(fill_length), ctx);
    }

    fn cues_with_fill_length(&self, offset: f64, fill_length: Option<f64>) -> Vec<Cue> {
        let child_offset = offset + self.placement.start;
        let inverse_speed = 1.0 / self.speed_with_fill_length(fill_length);
        let mut cues = self.child.cues(0.0);
        for cue in &mut cues {
            cue.start = child_offset + cue.start * inverse_speed;
            cue.end = child_offset + cue.end * inverse_speed;
        }
        if self.child.measure().is_none() {
            let parent_end = if self.is_fill() {
                fill_length.map(|length| self.placement.start + length)
            } else {
                self.placement.end
            };
            if let Some(end) = parent_end {
                let abs_end = offset + end;
                for cue in &mut cues {
                    cue.end = abs_end;
                }
            }
        }
        cues
    }

    /// Collects fill cues using its owning Timeline's resolved length.
    pub(crate) fn cues_fill(&self, offset: f64, fill_length: f64) -> Vec<Cue> {
        debug_assert!(self.is_fill());
        self.cues_with_fill_length(offset, Some(fill_length))
    }

    fn arrangement_with_fill_length(&self, offset: f64, fill_length: Option<f64>) -> Arrangement {
        let child_offset = offset + self.placement.start;
        let mut node = self.child.arrangement(0.0);
        map_arrangement_time(
            &mut node,
            child_offset,
            1.0 / self.speed_with_fill_length(fill_length),
        );
        if self.child.measure().is_none() {
            let parent_end = if self.is_fill() {
                fill_length.map(|length| self.placement.start + length)
            } else {
                self.placement.end
            };
            if let Some(end) = parent_end {
                node.end = offset + end;
            }
        }
        node
    }

    /// Builds a fill arrangement using its owning Timeline's resolved length.
    pub(crate) fn arrangement_fill(&self, offset: f64, fill_length: f64) -> Arrangement {
        debug_assert!(self.is_fill());
        self.arrangement_with_fill_length(offset, Some(fill_length))
    }
}

impl TimelineComponent for Placed {
    fn duration(&self) -> Option<f64> {
        match self.placement.end {
            // An explicit window fixes the length regardless of the child's.
            Some(end) => Some(end - self.placement.start),
            // A bare start point plays for the child's own duration.
            None => self.child.measure(),
        }
    }

    fn measure(&self) -> Option<f64> {
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

    fn resolve(&self, abs_start: f64, out: &mut ResolveCtx) -> f64 {
        self.resolve_with_fill_length(abs_start, None, out)
    }

    fn frame(
        &self,
        clock: Clock<'_>,
        canvas: Vec2,
        target: Resolution,
        residency: RasterResidency,
        ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage> {
        self.frame_with_fill_length(clock, None, canvas, target, residency, ctx)
    }

    fn render_audio_block(&self, block: AudioBlockMut<'_>, ctx: &mut AudioRenderContext) {
        self.render_audio_block_with_fill_length(block, None, ctx);
    }

    fn cues(&self, offset: f64) -> Vec<Cue> {
        self.cues_with_fill_length(offset, None)
    }

    fn arrangement(&self, offset: f64) -> Arrangement {
        self.arrangement_with_fill_length(offset, None)
    }
}

fn map_arrangement_time(node: &mut Arrangement, offset: f64, scale: f64) {
    node.start = offset + node.start * scale;
    node.end = offset + node.end * scale;
    for trigger in &mut node.triggers {
        trigger.time = offset + trigger.time * scale;
    }
    for child in &mut node.children {
        map_arrangement_time(child, offset, scale);
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

    /// Keeps a half-open interval of this component and rebases its start to
    /// local time zero. Negative endpoints count backwards from the immediate
    /// child's end; an open end means that exact end.
    fn trim<R>(self, bounds: R) -> Trim<Self>
    where
        R: TrimBounds,
        Self: PartialEq + std::hash::Hash,
    {
        Trim::new(self, bounds)
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

    fn trim<R>(self, bounds: R) -> Trim<Self::Output>
    where
        R: TrimBounds,
    {
        self.build_component().trim(bounds)
    }
}

impl<B> TimedBuilder for B
where
    B: TimelineBuilder,
    B::Output: Send,
{
}
