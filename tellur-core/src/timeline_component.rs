//! The timeline subsystem's public surface — STEP 1 skeleton.
//!
//! This module lands the *types and trait* the timeline macro arm (step 2)
//! and the resolve pass (step 3+) will fill in. Everything here compiles and
//! is exercised by focused tests, but the time-varying behaviour is still a
//! set of empty/default returns marked `// TODO(task N):`.
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

use crate::geometry::Constraints;
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
pub trait TimelineComponent {
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
    fn measure(&self) -> Option<f32> {
        self.duration()
    }

    /// Top-down place hook (`.sketch/02 §6`). Walks `self` (and, for a
    /// container, its children via `&self` recursion) assigning absolute starts
    /// and recording [`Event`] trigger times into `out`. Returns this node's
    /// resolved length.
    ///
    /// The default is the leaf behaviour: record nothing and report own length
    /// (falling back to `0.0` for a timeless leaf, whose interval comes from the
    /// placement window instead).
    fn resolve(&self, abs_start: f32, out: &mut ResolveCtx) -> f32 {
        // TODO(task 2+): containers override to recurse and place children.
        let _ = (abs_start, out);
        self.duration().unwrap_or(0.0)
    }

    /// Visual channel for this frame. `clock` carries both time axes (see
    /// [`Clock`]). `None` ⇒ contributes nothing visually.
    fn frame(
        &self,
        clock: Clock<'_>,
        target: Resolution,
        ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage> {
        // TODO(task 4): leaves/containers produce real frames.
        let _ = (clock, target, ctx);
        None
    }

    /// Audio channel for `[clock, clock + window)`. `None` ⇒ silent.
    fn samples(&self, clock: Clock<'_>, window: f32) -> Option<AudioBuffer> {
        // TODO(task 7): audio leaves produce real sample windows.
        let _ = (clock, window);
        None
    }

    /// Subtitle channel: cues made absolute by adding `offset` (this
    /// component's resolved start). Collected once at export.
    fn cues(&self, offset: f32) -> Vec<Cue> {
        // TODO(task 5+): Subtitle leaves / containers contribute cues.
        let _ = offset;
        Vec::new()
    }

    /// What the live UI draws: kind + label (+ trim marker for media).
    ///
    /// Named `arrangement` (not `outline`) to avoid clashing with the existing
    /// `tellur_renderer::Outline` raster effect (audit hygiene note).
    fn arrangement(&self) -> Arrangement;
}

// Compile-time guarantee that `TimelineComponent` is object-safe *and*
// spellable with `+ Send` (audit M2).
const _: Option<&(dyn TimelineComponent + Send)> = None;

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
        target: Resolution,
        ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage> {
        // Route through `ctx.render` so the visual memoizes one level down
        // (`.sketch/02 §11`): the framework caches `&dyn RasterComponent`, not
        // `&dyn TimelineComponent`, so this is the path that earns a cache slot.
        let _ = clock;
        let size = self.layout(Constraints::UNBOUNDED);
        Some(ctx.render(self, size, target))
    }

    fn samples(&self, _clock: Clock<'_>, _window: f32) -> Option<AudioBuffer> {
        None
    }

    fn cues(&self, _offset: f32) -> Vec<Cue> {
        Vec::new()
    }

    fn arrangement(&self) -> Arrangement {
        // A timeless visual surfaces as a Caption-kind node; the resolved
        // start/end are filled by the placement that wraps it.
        Arrangement {
            kind: NodeKind::Caption,
            // TODO(task 6): carry a real label (e.g. the concrete type name).
            label: String::new(),
            start: 0.0,
            end: 0.0,
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
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    /// Relative start on the parent clock.
    start: f32,
    /// Explicit window end (exclusive); `None` for a bare start point, whose
    /// end is the inner component's own duration.
    end: Option<f32>,
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
}

impl From<f32> for Placement {
    fn from(start: f32) -> Self {
        Self { start, end: None }
    }
}

impl From<Range<f32>> for Placement {
    fn from(window: Range<f32>) -> Self {
        Self {
            start: window.start,
            end: Some(window.end),
        }
    }
}

/// A component placed at a parent time — the temporal twin of
/// [`Positioned`](crate::placement::Positioned). A [`TimelineComponent`] itself,
/// so it nests.
///
/// Per the DECIDED stretch semantics (`.sketch/01` A.3): `.at(a..b)` footprint
/// is the window; the implied speed factor is recorded for later sampling.
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
            None => self.child.measure().map(|inner| self.placement.start + inner),
        }
    }

    fn resolve(&self, abs_start: f32, out: &mut ResolveCtx) -> f32 {
        // TODO(task 3): apply the window/stretch rules; for now recurse into the
        // child at its relative start and report the placed length.
        let child_len = self.child.resolve(abs_start + self.placement.start, out);
        self.duration()
            .unwrap_or(self.placement.start + child_len)
    }

    fn frame(
        &self,
        clock: Clock<'_>,
        target: Resolution,
        ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage> {
        // TODO(task 4): rebase the clock by the resolved start and apply speed.
        self.child.frame(clock, target, ctx)
    }

    fn samples(&self, clock: Clock<'_>, window: f32) -> Option<AudioBuffer> {
        // TODO(task 7): rebase + resample for the placement speed.
        self.child.samples(clock, window)
    }

    fn cues(&self, offset: f32) -> Vec<Cue> {
        self.child.cues(offset + self.placement.start)
    }

    fn arrangement(&self) -> Arrangement {
        // TODO(task 6): stamp the resolved start/end onto the child's node.
        self.child.arrangement()
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
        // TODO(task 3): mark this placement as fill so the container resolves it
        // against its own length; for now it is a bare start at 0.0.
        Placed::new(Placement::from(0.0), Box::new(self))
    }

    /// Use only SOURCE seconds `a..b` (the in/out crop). Shortens
    /// [`duration`](TimelineComponent::duration) to `b - a`. The way to truncate
    /// (a short `.at` window stretches, it does not cut). Returns a component.
    fn trim(self, r: Range<f32>) -> Self {
        // TODO(task 3): record the trim on the leaf so `duration()` reports
        // `b - a` and the leaf remaps its own sampling. No-op skeleton for now.
        let _ = r;
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Event {
    id: u64,
}

impl Event {
    /// Mints a fresh process-unique id.
    pub fn new() -> Self {
        Self {
            id: EVENT_COUNTER.fetch_add(1, Ordering::Relaxed),
        }
    }

    /// The raw id, used by the resolve pass / [`TriggerTable`] as the key.
    pub fn id(&self) -> u64 {
        self.id
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

/// Where in the child's interval a [`Triggered`] fires its [`Event`].
#[derive(Debug, Clone, Copy, PartialEq)]
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

impl<T: TimelineComponent> TimelineComponent for Triggered<T> {
    fn duration(&self) -> Option<f32> {
        self.child.duration()
    }

    fn measure(&self) -> Option<f32> {
        self.child.measure()
    }

    fn resolve(&self, abs_start: f32, out: &mut ResolveCtx) -> f32 {
        let len = self.child.resolve(abs_start, out);
        // Register the trigger time (earliest-wins) before/while recursing.
        let at = match self.kind {
            TriggerKind::Start => abs_start,
            TriggerKind::End => abs_start + len,
            TriggerKind::At(local) => abs_start + local,
        };
        out.triggers_mut().record(self.event, at);
        len
    }

    fn frame(
        &self,
        clock: Clock<'_>,
        target: Resolution,
        ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage> {
        self.child.frame(clock, target, ctx)
    }

    fn samples(&self, clock: Clock<'_>, window: f32) -> Option<AudioBuffer> {
        self.child.samples(clock, window)
    }

    fn cues(&self, offset: f32) -> Vec<Cue> {
        self.child.cues(offset)
    }

    fn arrangement(&self) -> Arrangement {
        self.child.arrangement()
    }
}

/// Trigger verbs on a built [`TimelineComponent`]. Blanket-implemented for every
/// `T: TimelineComponent`. Multiple writers to one [`Event`] ⇒ EARLIEST wins
/// (resolved in the place pass).
pub trait Triggers: TimelineComponent + Sized {
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
}

impl<'a> Clock<'a> {
    /// Constructs a clock for one frame from both axes and the resolved table.
    pub fn new(global: TimelineTime, local: LocalTime, triggers: &'a TriggerTable) -> Self {
        Self {
            global,
            local,
            triggers,
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
        }
    }

    /// 0 at THIS component's resolved start; survives `Sequence` re-flow.
    /// Self-animation: `clock.local().phase(0.0, 0.4)`.
    pub fn local(&self) -> LocalTime {
        self.local
    }

    /// Absolute frame time — the SAME axis as [`Event`] triggers.
    pub fn global(&self) -> TimelineTime {
        self.global
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
#[derive(Debug, Default)]
pub struct ResolveCtx {
    triggers: TriggerTable,
    /// Absolute start of the node currently being resolved. Top-down bookkeeping
    /// the container walk maintains; surfaced for leaves that need it.
    abs_start: f32,
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

    /// Sets the absolute start for the node currently being resolved.
    pub fn set_abs_start(&mut self, abs_start: f32) {
        self.abs_start = abs_start;
    }

    /// Consumes the context, yielding the finished trigger table.
    pub fn into_triggers(self) -> TriggerTable {
        self.triggers
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

/// What the live UI draws — the resolved arrangement of a node and its
/// children. Built by walking the RESOLVED tree (`.sketch/01` A.7 / B.4).
///
/// `trim` carries the source crop separately so the UI can show both the placed
/// bar and the source crop; `triggers` surfaces where [`Event`]s fire.
#[derive(Debug, Clone, PartialEq)]
pub struct Arrangement {
    pub kind: NodeKind,
    pub label: String,
    pub start: f32,
    pub end: f32,
    pub trim: Option<(f32, f32)>,
    pub triggers: Vec<f32>,
    pub children: Vec<Arrangement>,
}

/// The kind of node the live UI renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeKind {
    Video,
    Audio,
    Caption,
    Subtitle,
    Timeline,
    Sequence,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Vec2;
    use crate::raster::PixelFormat;
    use crate::render_context::PassThrough;

    // A trivial RasterComponent so we can exercise the blanket impl.
    #[derive(PartialEq, Hash)]
    struct Dot;

    impl RasterComponent for Dot {
        fn layout(&self, _constraints: Constraints) -> Vec2 {
            Vec2(1.0, 1.0)
        }

        fn render(&self, _size: Vec2, _target: Resolution, _ctx: &mut dyn RenderContext) -> RasterImage {
            RasterImage::cpu(1, 1, PixelFormat::Rgba8, vec![0u8, 0, 0, 0])
        }
    }

    #[test]
    fn raster_component_is_a_timeline_component_via_blanket() {
        // The whole point of the one-way blanket: a RasterComponent can be
        // boxed as `Box<dyn TimelineComponent + Send>` (audit M2).
        let boxed: Box<dyn TimelineComponent + Send> = Box::new(Dot);
        assert_eq!(boxed.duration(), None);
        assert_eq!(boxed.measure(), None);
        assert_eq!(boxed.cues(0.0), Vec::new());
        assert_eq!(boxed.arrangement().kind, NodeKind::Caption);
    }

    #[test]
    fn blanket_frame_routes_through_ctx_render() {
        // A timeless visual produces a frame via `ctx.render` (memoization path).
        let dot = Dot;
        let table = TriggerTable::new();
        let clock = Clock::new(TimelineTime::new(0.0), LocalTime::new(0.0), &table);
        let mut ctx = PassThrough;
        let frame = dot.frame(clock, Resolution::new(4, 4), &mut ctx);
        assert!(frame.is_some());
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

    // Generic acceptors that only hold if the bounds are met — these are the
    // load-bearing assertions: the generated types implement the right traits.
    fn assert_timeline_component<T: TimelineComponent>(_: &T) {}
    fn assert_timeline_builder<B: TimelineBuilder>(_: &B) {}
    fn assert_boxable<T: TimelineComponent + Send + 'static>(value: T) -> Box<dyn TimelineComponent + Send> {
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
        assert_eq!(beat.arrangement().kind, NodeKind::Caption);

        let mut ctx = ResolveCtx::new();
        // resolve recurses into the placed child at its relative start.
        assert_eq!(beat.resolve(0.0, &mut ctx), 2.0);

        // The complete builder is a `TimelineBuilder` and boxes with `+ Send`.
        let builder = Beat::builder().start(3.0);
        assert_timeline_builder(&builder);
        let _boxed: Box<dyn TimelineComponent + Send> = assert_boxable(beat);
    }

    #[test]
    fn timeline_component_with_clock_forwards_the_real_clock() {
        let pulse = Pulse::builder().start(1.0).build();
        assert_timeline_component(&pulse);

        // `frame` forwards the framework-supplied clock into the body.
        let table = TriggerTable::new();
        let clock = Clock::new(TimelineTime::new(0.5), LocalTime::new(0.25), &table);
        let mut ctx = PassThrough;
        let frame = pulse.frame(clock, Resolution::new(4, 4), &mut ctx);
        assert!(frame.is_some());

        // Clock-less queries still resolve via the structural clock.
        assert_eq!(pulse.duration(), Some(1.0));

        let builder = Pulse::builder().start(1.0);
        assert_timeline_builder(&builder);
        let _boxed: Box<dyn TimelineComponent + Send> = assert_boxable(pulse);
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
}
