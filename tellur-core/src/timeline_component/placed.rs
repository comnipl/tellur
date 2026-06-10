//! [`Placement`] / [`Placed`] and the `.at(..)` / `.fill()` / `.trim(..)`
//! placement verbs — where a component sits on the parent clock.

use std::ops::Range;

use crate::geometry::Vec2;
use crate::raster::{RasterImage, Resolution};
use crate::render_context::RenderContext;
use crate::time::{LocalTime, Time};

use super::*;

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
