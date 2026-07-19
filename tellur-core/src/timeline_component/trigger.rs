//! [`Event`]s and the wrappers that decorate a component transparently:
//! [`Triggered`] (records an event time during resolve) and [`Sourced`]
//! (stamps the authoring call site onto the arrangement).

use std::hash::Hash;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::geometry::Vec2;
use crate::phase::Phase;
use crate::raster::{RasterImage, RasterResidency, Resolution};
use crate::render_context::RenderContext;
use crate::time::Time;
use crate::window::Window;

use super::*;

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
    pub fn phase(&self, clock: &Clock<'_>, a: f64, b: f64) -> Phase {
        let trigger = clock.trigger_of(*self).seconds();
        // `.sketch/02 §11`: an unfired (+∞) trigger must short-circuit to 0,
        // otherwise the naive `(now - ∞)/(∞ - ∞)` is `NaN`.
        if trigger.is_infinite() {
            return Phase::ZERO;
        }
        clock.global().phase(trigger + a, trigger + b)
    }

    /// Seconds elapsed since the trigger on the global timeline.
    ///
    /// Unfired events report `0.0`, and times before the trigger are clamped to
    /// `0.0`. Unlike [`Event::phase`], this keeps counting after the event, so
    /// components can run at a natural speed without choosing an end time up
    /// front.
    pub fn elapsed(&self, clock: &Clock<'_>) -> f64 {
        let trigger = clock.trigger_of(*self).seconds();
        if trigger.is_infinite() {
            return 0.0;
        }
        (clock.global().seconds() - trigger).max(0.0)
    }

    /// A [`Window`] over `[trigger + range.start, trigger + range.end)`,
    /// already [`clamped`](Window::clamped) — the event-relative twin of
    /// [`crate::time::Time::window`], for staggering sub-events
    /// ([`Window::sub_secs`]) or reading elapsed/remaining seconds off a
    /// single trigger-anchored interval instead of composing them from
    /// [`Event::phase`] / [`Event::elapsed`] by hand.
    ///
    /// Returning the clamped snapshot (not the live cursor) directly is what
    /// makes this safe to store in a component field: like
    /// [`Window::clamped`], the snapshot is constant at `(start, end, start)`
    /// before the window opens and constant at `(start, end, end)` once it
    /// closes, so it is a frame-stable cache-key term the same way a
    /// saturating [`Phase`] is (`.sketch/02 §11`).
    ///
    /// An unfired event (trigger at `+∞`) cannot be shifted into absolute
    /// seconds, so it reports the "before the window" snapshot directly —
    /// `range.start` doubling as its own anchor, cursor pinned to
    /// `range.start` (phase `0`, matching [`Event::phase`]'s `+∞` short
    /// circuit) — rather than propagating `+∞`/`NaN` or panicking. That
    /// snapshot does not depend on the current time, so it stays stable
    /// across every frame the event remains unfired, exactly like the
    /// post-close snapshot stays stable across every frame after firing.
    pub fn window(&self, clock: &Clock<'_>, range: Range<f64>) -> Window {
        assert!(
            range.start.is_finite() && range.end.is_finite() && range.end > range.start,
            "Event::window requires a finite range with end > start"
        );
        let trigger = clock.trigger_of(*self).seconds();
        if trigger.is_infinite() {
            return Window::new(range.start, range.end, range.start);
        }
        let start = trigger + range.start;
        let end = trigger + range.end;
        Window::new(start, end, clock.global().seconds()).clamped()
    }
}

impl Default for Event {
    fn default() -> Self {
        Self::new()
    }
}

/// A transparent wrapper that registers an [`Event`]'s time during the resolve
/// pass and otherwise plays its child unchanged. A [`TimelineComponent`].
#[derive(Clone)]
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
    At(f64),
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

impl<T: TimelineComponent + Clone + PartialEq + Hash + 'static> TimelineComponent for Triggered<T> {
    fn duration(&self) -> Option<f64> {
        self.child.duration()
    }

    fn measure(&self) -> Option<f64> {
        self.child.measure()
    }

    fn resolve(&self, abs_start: f64, out: &mut ResolveCtx) -> f64 {
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
        residency: RasterResidency,
        ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage> {
        self.child.frame(clock, canvas, target, residency, ctx)
    }

    fn render_audio_block(&self, block: AudioBlockMut<'_>, ctx: &mut AudioRenderContext) {
        self.child.render_audio_block(block, ctx);
    }

    fn cues(&self, offset: f64) -> Vec<Cue> {
        self.child.cues(offset)
    }

    fn arrangement(&self, offset: f64) -> Arrangement {
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
    T: TimelineComponent + Clone + PartialEq + Hash + Send + 'static,
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
#[derive(Clone)]
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
/// A container that bypasses a child's own `arrangement` (e.g.
/// [`Timeline`](crate::timeline_container::Timeline),
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
    fn duration(&self) -> Option<f64> {
        self.inner.duration()
    }

    fn measure(&self) -> Option<f64> {
        self.inner.measure()
    }

    fn resolve(&self, abs_start: f64, out: &mut ResolveCtx) -> f64 {
        self.inner.resolve(abs_start, out)
    }

    fn frame(
        &self,
        clock: Clock<'_>,
        canvas: Vec2,
        target: Resolution,
        residency: RasterResidency,
        ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage> {
        self.inner.frame(clock, canvas, target, residency, ctx)
    }

    fn render_audio_block(&self, block: AudioBlockMut<'_>, ctx: &mut AudioRenderContext) {
        self.inner.render_audio_block(block, ctx);
    }

    fn cues(&self, offset: f64) -> Vec<Cue> {
        self.inner.cues(offset)
    }

    fn arrangement(&self, offset: f64) -> Arrangement {
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
    fn trigger_at(self, local: f64, e: Event) -> Triggered<Self> {
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

    fn trigger_at(self, local: f64, e: Event) -> Triggered<Self::Output> {
        self.build_component().trigger_at(local, e)
    }
}

impl<B: TimelineBuilder> TriggersBuilder for B {}
