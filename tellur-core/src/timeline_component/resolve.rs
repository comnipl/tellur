//! The resolve pass: [`TriggerTable`], [`ResolveCtx`], the [`resolve`]
//! entry point, and its [`ResolvedTimeline`] output.

use std::collections::HashMap;

use crate::geometry::Vec2;
use crate::raster::{RasterImage, RasterResidency, Resolution};
use crate::render_context::RenderContext;
use crate::time::{LocalTime, Time, TimelineTime};

use super::*;

// ── Resolve-pass state (`.sketch/02 §7`) ─────────────────────────────────────

/// `Event` id → absolute trigger time. One per resolved tree, built by the
/// place pass. Earliest-wins on insert; an absent id reads as `+∞`.
#[derive(Debug, Clone, Default)]
pub struct TriggerTable {
    map: HashMap<u64, f64>,
}

impl TriggerTable {
    /// An empty table — every id reads as `+∞`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records `time` for `event`, keeping the EARLIEST write (`.sketch/02 §6`).
    pub fn record(&mut self, event: Event, time: f64) {
        self.record_id(event.id(), time);
    }

    /// `record` by raw id (the resolve pass works in ids).
    pub fn record_id(&mut self, id: u64, time: f64) {
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
        TimelineTime::new(self.map.get(&id).copied().unwrap_or(f64::INFINITY))
    }

    /// Whether `id` has a recorded trigger time.
    pub fn contains(&self, id: u64) -> bool {
        self.map.contains_key(&id)
    }

    fn merge_in_range(&mut self, other: Self, range: std::ops::Range<f64>) {
        for (id, time) in other.map {
            // Audio/video samples use half-open intervals, but an Event is a
            // boundary marker: `trigger_at_end` lives exactly at a component's
            // resolved end. Preserve both trim boundaries so even a no-op
            // `.trim(..)` cannot silently erase an end trigger.
            if time >= range.start && time <= range.end {
                self.record_id(id, time);
            }
        }
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
    abs_start: f64,
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
    pub(super) local_scale: f64,
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
    pub fn abs_start(&self) -> f64 {
        self.abs_start
    }

    /// Cumulative window-stretch scale (absolute seconds per current-level local
    /// second). Containers in other modules multiply their local cursor offsets
    /// by this so children land at the right absolute start under a stretch.
    pub fn local_scale(&self) -> f64 {
        self.local_scale
    }

    /// Sets the absolute start for the node currently being resolved.
    pub fn set_abs_start(&mut self, abs_start: f64) {
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

    /// Resolves a trimmed child while retaining only triggers that survive the
    /// wrapper's half-open output interval. Warnings/errors and local-scale
    /// bookkeeping still flow through this context normally; only trigger
    /// writes are captured locally and filtered before merging.
    pub(crate) fn resolve_trimmed(
        &mut self,
        child: &dyn TimelineComponent,
        child_abs_start: f64,
        output_range: std::ops::Range<f64>,
    ) -> f64 {
        let parent_triggers = std::mem::take(&mut self.triggers);
        let child_len = child.resolve(child_abs_start, self);
        let child_triggers = std::mem::replace(&mut self.triggers, parent_triggers);
        self.triggers.merge_in_range(child_triggers, output_range);
        child_len
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
    duration: f64,
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
    pub fn duration(&self) -> f64 {
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
        residency: RasterResidency,
        ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage> {
        let clock = Clock::new(t, LocalTime::new(t.seconds()), self.triggers())
            .with_local_window(LocalTime::new(t.seconds()), Some(self.duration));
        self.source()
            .frame(clock, self.canvas, target, residency, ctx)
    }

    /// Renders the complete audio track as interleaved f32 PCM.
    ///
    /// The root advances in integer output frames and recursively asks the
    /// component tree for bounded blocks. Components may remap or expand a
    /// child request, but there is no separately compiled audio graph.
    pub fn render_audio(&self, rate: u32, channels: u16) -> AudioBuffer {
        let frame_count = frames_for_duration(self.duration, rate);
        self.render_audio_frames(0, frame_count, rate, channels)
    }

    /// Block-rendered mix-down for a timeline window.
    ///
    /// `start` is snapped once to the nearest output frame; recursive components
    /// then retain that integer root-frame identity. This uses the same tree walk
    /// as [`render_audio`](Self::render_audio), but the target is only `duration`
    /// seconds long, so live-preview segments do not allocate a full-timeline WAV.
    pub fn render_audio_window(
        &self,
        start: f64,
        duration: f64,
        rate: u32,
        channels: u16,
    ) -> AudioBuffer {
        let start_frame = seconds_to_frame(start.max(0.0), rate);
        let frame_count = frames_for_duration(duration, rate);
        self.render_audio_frames(start_frame, frame_count, rate, channels)
    }

    fn render_audio_frames(
        &self,
        start_frame: i64,
        frame_count: usize,
        rate: u32,
        channels: u16,
    ) -> AudioBuffer {
        const BLOCK_FRAMES: usize = 4_096;

        let rate = rate.max(1);
        let channels = channels.max(1);
        let mut samples = vec![0.0; frame_count.saturating_mul(channels as usize)];
        let mut ctx = AudioRenderContext::default();
        let root_request = AudioRenderRequest::new(start_frame, frame_count, rate, channels);
        let mut rendered = 0usize;
        while rendered < frame_count {
            let block_frames = (frame_count - rendered).min(BLOCK_FRAMES);
            let request = root_request.subrange(rendered, block_frames);
            let sample_start = rendered * channels as usize;
            let sample_end = sample_start + request.sample_len();
            self.source().render_audio_block(
                AudioBlockMut::new(request, &mut samples[sample_start..sample_end]),
                &mut ctx,
            );
            rendered += block_frames;
        }

        // The resolved root interval is authoritative even for a custom or
        // timeless child that does not gate itself.
        for frame in 0..frame_count {
            let absolute_frame = start_frame.saturating_add(frame as i64);
            let t = absolute_frame as f64 / rate as f64;
            if t < 0.0 || t >= self.duration {
                let base = frame * channels as usize;
                samples[base..base + channels as usize].fill(0.0);
            }
        }

        AudioBuffer {
            samples,
            rate,
            channels,
        }
    }
}

fn seconds_to_frame(seconds: f64, rate: u32) -> i64 {
    if !seconds.is_finite() {
        return 0;
    }
    (seconds * rate.max(1) as f64).round() as i64
}

fn frames_for_duration(duration: f64, rate: u32) -> usize {
    if !duration.is_finite() || duration <= 0.0 {
        return 0;
    }
    (duration * rate.max(1) as f64).ceil() as usize
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
