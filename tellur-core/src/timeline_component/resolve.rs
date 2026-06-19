//! The resolve pass: [`TriggerTable`], [`ResolveCtx`], the [`resolve`]
//! entry point, and its [`ResolvedTimeline`] output.

use std::collections::HashMap;

use crate::geometry::Vec2;
use crate::raster::{RasterImage, Resolution};
use crate::render_context::RenderContext;
use crate::time::{LocalTime, Time, TimelineTime};

use super::*;

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
    /// (`.sketch/01 §A.3`). `pub(super)` so the sibling [`Placed`] /
    /// [`Triggered`] impls can save/restore it around their recursion;
    /// other modules read it through [`Self::local_scale`].
    pub(super) local_scale: f32,
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
/// Per-frame SAMPLING (step 5) runs `&self` recursion over [`source`](ResolvedTimeline::source)
/// using the borrowed [`triggers`](ResolvedTimeline::triggers) — this type
/// just holds the resolved state.
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
        let clock = Clock::new(t, LocalTime::new(t.seconds()), self.triggers())
            .with_local_window(LocalTime::new(t.seconds()), Some(self.duration));
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

    /// Eager mix-down for a timeline window.
    ///
    /// This uses the same tree walk as [`render_audio`](Self::render_audio), but
    /// the mix target is only `duration` seconds long and the global timeline is
    /// shifted left by `start`, so live-preview cache segments do not allocate or
    /// write a full-timeline WAV for every requested segment.
    pub fn render_audio_window(
        &self,
        start: f32,
        duration: f32,
        rate: u32,
        channels: u16,
    ) -> AudioBuffer {
        let start = start.max(0.0);
        let duration = duration.max(0.0);
        let mut mix = crate::audio::AudioMix::new(duration, rate, channels);
        self.source().mix_into(&mut mix, -start, 1.0);
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
