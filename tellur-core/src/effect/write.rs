//! Manim-style "write-on" reveal for vector components.
//!
//! [`Write`] wraps any [`VectorComponent`](crate::vector::VectorComponent) and
//! turns its visible paths into partial strokes driven by a [`Phase`]. It is
//! especially useful with [`Text`](crate::text::Text) and, with the `latex`
//! feature enabled, `MathSpan`: both ultimately render as vector paths, so they
//! can be revealed with the same component.
//!
//! How write time is split across paths is a [`WritePacing`] policy: by
//! default every writable path gets an equal, staggered time slot, so text
//! advances at a steady per-character rhythm no matter how intricate each
//! glyph is. [`WritePacing::ByLength`] instead moves the pen at a constant
//! speed, making a path's share proportional to its outline length.

use crate::builder::VectorBuilder;
use crate::geometry::{Constraints, Rect, Transform, Vec2};
use crate::phase::Phase;
use crate::scalar::clamp_unit;
use crate::time::Time;
use crate::timeline_component::{Clock, Event};
use crate::vector::{
    ClipGroup, Group, Node, Path, PathCommand, SingleGroup, Stroke, VectorComponent, VectorGraphic,
};
use crate::Keyable;

const DEFAULT_STROKE_WIDTH: f32 = 1.2;
const DEFAULT_STROKE_END: f32 = 0.72;
const DEFAULT_FILL_DELAY: f32 = 0.0;
const DEFAULT_FILL_LEAD: f32 = 0.07;
const DEFAULT_FILL_DURATION: f32 = 0.24;
const DEFAULT_STROKE_SPEED: f32 = 2400.0;
const DEFAULT_MAX_STROKE_SPEED: f32 = 2400.0;
const DEFAULT_PER_PATH_SECS: f64 = 0.17;
const DEFAULT_LAG_RATIO: f32 = 0.5;
const DEFAULT_TIMED_FILL_LEAD: f64 = 0.08;
const DEFAULT_TIMED_FILL_DURATION: f64 = 0.18;
const DEFAULT_COMPLETED_STROKE_OPACITY: f32 = 0.35;
const QUAD_STEPS: usize = 16;
const CUBIC_STEPS: usize = 24;

/// How write time is distributed across the child's writable paths.
///
/// [`Text`](crate::text::Text) and `MathSpan` render one path per glyph, so
/// for text these policies read as "per character".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WritePacing {
    /// The pen moves at a constant speed: a path's share of the write time is
    /// proportional to its outline length, so intricate glyphs take visibly
    /// longer than simple ones.
    ByLength,
    /// Every writable path gets an equal time slot regardless of its length,
    /// giving text a steady per-character rhythm. Neighbouring slots overlap
    /// according to the wrapper's `lag_ratio`.
    PerPath,
}

/// Reveals a vector component by drawing its paths over time.
///
/// Each writable path is assigned a slot of the write time according to
/// `pacing` (equal per-path slots by default, staggered by `lag_ratio`).
/// Within its slot a path draws an inset stroke along its outline. As each
/// filled path finishes being written, that path's fill fades in behind the
/// completed temporary stroke while later paths keep writing. At `progress == 1`,
/// the original child graphic is returned, so filled text and math settle into
/// their normal final rendering.
#[crate::component(vector)]
#[derive(Clone, Keyable)]
pub struct Write {
    pub progress: Phase,
    #[builder(default = WritePacing::PerPath)]
    pub pacing: WritePacing,
    #[builder(default = DEFAULT_LAG_RATIO)]
    pub lag_ratio: f32,
    #[builder(default = DEFAULT_STROKE_WIDTH)]
    pub stroke_width: f32,
    #[builder(default = DEFAULT_STROKE_END)]
    pub stroke_end: f32,
    #[builder(default = DEFAULT_FILL_DELAY)]
    pub fill_delay: f32,
    #[builder(default = DEFAULT_FILL_LEAD)]
    pub fill_lead: f32,
    #[builder(default = DEFAULT_FILL_DURATION)]
    pub fill_duration: f32,
    #[builder(default = DEFAULT_COMPLETED_STROKE_OPACITY)]
    pub completed_stroke_opacity: f32,
    #[builder(into)]
    pub child: Box<dyn VectorComponent>,
}

impl Write {
    pub fn new<C: VectorComponent + 'static>(progress: Phase, child: C) -> Self {
        Self::from_box(progress, Box::new(child))
    }

    pub fn from_box(progress: Phase, child: Box<dyn VectorComponent>) -> Self {
        Self {
            progress,
            pacing: WritePacing::PerPath,
            lag_ratio: DEFAULT_LAG_RATIO,
            stroke_width: DEFAULT_STROKE_WIDTH,
            stroke_end: DEFAULT_STROKE_END,
            fill_delay: DEFAULT_FILL_DELAY,
            fill_lead: DEFAULT_FILL_LEAD,
            fill_duration: DEFAULT_FILL_DURATION,
            completed_stroke_opacity: DEFAULT_COMPLETED_STROKE_OPACITY,
            child,
        }
    }

    /// Gives every writable path an equal time slot (the default).
    pub fn per_path(mut self) -> Self {
        self.pacing = WritePacing::PerPath;
        self
    }

    /// Allocates write time proportionally to path length.
    pub fn by_length(mut self) -> Self {
        self.pacing = WritePacing::ByLength;
        self
    }

    /// Sets how far into a path's slot the next path starts, as a fraction of
    /// the slot (1.0 = strictly sequential, 0.5 = halfway through, 0.0 = all
    /// paths write simultaneously). Only meaningful with
    /// [`WritePacing::PerPath`].
    pub fn lag_ratio(mut self, lag_ratio: f32) -> Self {
        self.lag_ratio = lag_ratio;
        self
    }

    pub fn stroke_width(mut self, stroke_width: f32) -> Self {
        self.stroke_width = stroke_width;
        self
    }

    pub fn stroke_end(mut self, stroke_end: Phase) -> Self {
        self.stroke_end = stroke_end.get();
        self
    }

    pub fn fill_start(mut self, fill_start: Phase) -> Self {
        self.stroke_end = fill_start.get();
        self
    }

    pub fn fill_delay(mut self, fill_delay: Phase) -> Self {
        self.fill_delay = fill_delay.get();
        self
    }

    pub fn fill_lead(mut self, fill_lead: Phase) -> Self {
        self.fill_lead = fill_lead.get();
        self
    }

    pub fn fill_duration(mut self, fill_duration: Phase) -> Self {
        self.fill_duration = fill_duration.get();
        self
    }

    pub fn completed_stroke_opacity(mut self, opacity: f32) -> Self {
        self.completed_stroke_opacity = opacity;
        self
    }
}

impl VectorComponent for Write {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        self.child.layout(constraints)
    }

    fn paint_bounds(&self, size: Vec2) -> Rect {
        if self.progress == Phase::ZERO || self.progress == Phase::ONE {
            return self.child.paint_bounds(size);
        }
        let inner = self.child.render(size);
        write_bounds_for_node(
            inner.view_box,
            &inner.root,
            self.progress,
            self.stroke_width,
        )
    }

    fn render(&self, size: Vec2) -> VectorGraphic {
        let inner = self.child.render(size);
        if self.progress == Phase::ONE {
            return inner;
        }

        let view_box = write_bounds_for_node(
            inner.view_box,
            &inner.root,
            self.progress,
            self.stroke_width,
        );
        let progress = self.progress.get();
        if progress <= 0.0 {
            return VectorGraphic {
                view_box,
                root: Node::empty(),
            };
        }

        let lengths = collect_write_lengths(&inner.root, self.stroke_width);
        let total: f32 = lengths.iter().sum();
        if total <= 0.0 {
            return VectorGraphic {
                view_box,
                root: Node::empty(),
            };
        }

        // Slots are laid out so that the last outline completes exactly at
        // `stroke_end`, leaving the tail of `progress` for fills to finish.
        let stroke_end = stroke_end_point(self.stroke_end);
        let (mut slots, span) = write_slots(&lengths, self.pacing, self.lag_ratio);
        scale_slots(&mut slots, stroke_end as f64 / span);

        let mut children = Vec::new();
        let mut fill_walk = FillWalk {
            slots: &slots,
            next: 0,
            alpha_at: |completion: f64| {
                fill_alpha(
                    progress,
                    completion as f32,
                    self.fill_lead,
                    self.fill_delay,
                    self.fill_duration,
                )
            },
        };
        let fill = fill_node(inner.root.clone(), &mut fill_walk);
        if !fill.is_empty() {
            children.push(fill);
        }

        let mut stroke_walk = StrokeWalk {
            slots: &slots,
            next: 0,
            cursor: progress as f64,
            completed_stroke_opacity: self.completed_stroke_opacity,
            fallback_stroke_width: self.stroke_width,
        };
        let stroke = stroke_node(inner.root, &mut stroke_walk);
        if !stroke.is_empty() {
            children.push(stroke);
        }

        VectorGraphic {
            view_box,
            root: Node::Group(Group {
                transform: Transform::IDENTITY,
                opacity: 1.0,
                children,
            }),
        }
    }
}

/// Reveals a vector component from a start time, pacing paths in seconds.
///
/// Unlike [`Write`], which consumes an already-normalized [`Phase`],
/// `TimedWrite` schedules the child's paths on a clock of elapsed seconds
/// after layout. By default every writable path gets the same `per_path_secs`
/// slot ([`WritePacing::PerPath`]), so text advances at a steady
/// per-character rhythm and longer text takes longer. Call
/// [`TimedWrite::stroke_speed`] (or [`TimedWrite::by_length`]) to move the
/// pen at a constant speed instead, making intricate glyphs take longer.
#[crate::component(vector)]
#[derive(Clone, Keyable)]
pub struct TimedWrite {
    pub time: f64,
    pub start: f64,
    #[builder(default = WritePacing::PerPath)]
    pub pacing: WritePacing,
    /// Seconds each writable path takes under [`WritePacing::PerPath`].
    #[builder(default = DEFAULT_PER_PATH_SECS)]
    pub per_path_secs: f64,
    #[builder(default = DEFAULT_LAG_RATIO)]
    pub lag_ratio: f32,
    /// Pen speed in child units per second under [`WritePacing::ByLength`].
    #[builder(default = DEFAULT_STROKE_SPEED)]
    pub stroke_speed: f32,
    /// Upper bound on pen speed (child units per second). Slots whose
    /// nominal pace would exceed this get a longer stroke_duration while
    /// their fill timing stays on the nominal slot. `f32::INFINITY` disables.
    #[builder(default = DEFAULT_MAX_STROKE_SPEED)]
    pub max_stroke_speed: f32,
    #[builder(default = DEFAULT_STROKE_WIDTH)]
    pub stroke_width: f32,
    #[builder(default = DEFAULT_TIMED_FILL_LEAD)]
    pub fill_lead: f64,
    #[builder(default = DEFAULT_TIMED_FILL_DURATION)]
    pub fill_duration: f64,
    #[builder(default = DEFAULT_COMPLETED_STROKE_OPACITY)]
    pub completed_stroke_opacity: f32,
    #[builder(into)]
    pub child: Box<dyn VectorComponent>,
}

impl TimedWrite {
    pub fn new<T: Time, C: VectorComponent + 'static>(time: T, start: f64, child: C) -> Self {
        Self::from_box(time, start, Box::new(child))
    }

    pub fn from_box<T: Time>(time: T, start: f64, child: Box<dyn VectorComponent>) -> Self {
        Self::with_defaults(time.seconds(), start, child)
    }

    pub fn from_elapsed<C: VectorComponent + 'static>(elapsed: f64, child: C) -> Self {
        Self::from_elapsed_box(elapsed, Box::new(child))
    }

    pub fn from_elapsed_box(elapsed: f64, child: Box<dyn VectorComponent>) -> Self {
        Self::with_defaults(elapsed, 0.0, child)
    }

    fn with_defaults(time: f64, start: f64, child: Box<dyn VectorComponent>) -> Self {
        Self {
            time,
            start,
            pacing: WritePacing::PerPath,
            per_path_secs: DEFAULT_PER_PATH_SECS,
            lag_ratio: DEFAULT_LAG_RATIO,
            stroke_speed: DEFAULT_STROKE_SPEED,
            max_stroke_speed: DEFAULT_MAX_STROKE_SPEED,
            stroke_width: DEFAULT_STROKE_WIDTH,
            fill_lead: DEFAULT_TIMED_FILL_LEAD,
            fill_duration: DEFAULT_TIMED_FILL_DURATION,
            completed_stroke_opacity: DEFAULT_COMPLETED_STROKE_OPACITY,
            child,
        }
    }

    /// Gives every writable path an equal `secs`-second slot and switches
    /// pacing to [`WritePacing::PerPath`].
    pub fn per_path_secs(mut self, secs: f64) -> Self {
        self.pacing = WritePacing::PerPath;
        self.per_path_secs = secs;
        self
    }

    /// Switches pacing to equal per-path slots (the default).
    pub fn per_path(mut self) -> Self {
        self.pacing = WritePacing::PerPath;
        self
    }

    /// Moves the pen at a constant `stroke_speed` (child units per second)
    /// and switches pacing to [`WritePacing::ByLength`].
    pub fn stroke_speed(mut self, stroke_speed: f32) -> Self {
        self.pacing = WritePacing::ByLength;
        self.stroke_speed = stroke_speed;
        self
    }

    /// Caps how fast the pen may travel (child units per second). Paths whose
    /// nominal per-path pace would exceed this take longer to stroke, while
    /// fill timing stays on the nominal slot. Pass `f32::INFINITY` to disable.
    pub fn max_stroke_speed(mut self, max_stroke_speed: f32) -> Self {
        self.max_stroke_speed = max_stroke_speed;
        self
    }

    /// Switches pacing to length-proportional at the current `stroke_speed`.
    pub fn by_length(mut self) -> Self {
        self.pacing = WritePacing::ByLength;
        self
    }

    /// Sets how far into a path's slot the next path starts, as a fraction of
    /// the slot (1.0 = strictly sequential, 0.5 = halfway through, 0.0 = all
    /// paths write simultaneously). Only meaningful with
    /// [`WritePacing::PerPath`].
    pub fn lag_ratio(mut self, lag_ratio: f32) -> Self {
        self.lag_ratio = lag_ratio;
        self
    }

    pub fn stroke_width(mut self, stroke_width: f32) -> Self {
        self.stroke_width = stroke_width;
        self
    }

    pub fn fill_lead_secs(mut self, fill_lead: f64) -> Self {
        self.fill_lead = fill_lead;
        self
    }

    pub fn fill_duration_secs(mut self, fill_duration: f64) -> Self {
        self.fill_duration = fill_duration;
        self
    }

    pub fn completed_stroke_opacity(mut self, opacity: f32) -> Self {
        self.completed_stroke_opacity = opacity;
        self
    }

    fn elapsed(&self) -> f64 {
        self.time - self.start
    }

    /// Seconds per abstract schedule unit (see [`write_slots`]).
    fn seconds_per_unit(&self) -> f64 {
        match self.pacing {
            WritePacing::ByLength => 1.0 / timed_stroke_speed(self.stroke_speed) as f64,
            WritePacing::PerPath => self.per_path_secs.max(0.000_1),
        }
    }
}

impl VectorComponent for TimedWrite {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        self.child.layout(constraints)
    }

    fn paint_bounds(&self, size: Vec2) -> Rect {
        let inner = self.child.render(size);
        let lengths = collect_write_lengths(&inner.root, self.stroke_width);
        let total: f32 = lengths.iter().sum();
        let (mut slots, span) = write_slots(&lengths, self.pacing, self.lag_ratio);
        let scale = self.seconds_per_unit();
        scale_slots(&mut slots, scale);
        cap_stroke_speed(&mut slots, self.max_stroke_speed);
        let span = span * scale;
        let elapsed = self.elapsed();
        if elapsed <= 0.0
            || total <= 0.0
            || elapsed
                >= timed_done_at(
                    stroke_span(&slots),
                    span,
                    self.fill_lead,
                    self.fill_duration,
                )
        {
            return self.child.paint_bounds(size);
        }

        write_bounds_for_node(inner.view_box, &inner.root, Phase::HALF, self.stroke_width)
    }

    fn render(&self, size: Vec2) -> VectorGraphic {
        let inner = self.child.render(size);
        let lengths = collect_write_lengths(&inner.root, self.stroke_width);
        let total: f32 = lengths.iter().sum();
        let (mut slots, span) = write_slots(&lengths, self.pacing, self.lag_ratio);
        let scale = self.seconds_per_unit();
        scale_slots(&mut slots, scale);
        cap_stroke_speed(&mut slots, self.max_stroke_speed);
        let span = span * scale;
        let elapsed = self.elapsed();

        if total > 0.0
            && elapsed
                >= timed_done_at(
                    stroke_span(&slots),
                    span,
                    self.fill_lead,
                    self.fill_duration,
                )
        {
            return inner;
        }

        let view_box =
            write_bounds_for_node(inner.view_box, &inner.root, Phase::HALF, self.stroke_width);
        if elapsed <= 0.0 || total <= 0.0 {
            return VectorGraphic {
                view_box,
                root: Node::empty(),
            };
        }

        let mut children = Vec::new();
        let mut fill_walk = FillWalk {
            slots: &slots,
            next: 0,
            alpha_at: |completion: f64| {
                timed_fill_alpha(elapsed, completion, self.fill_lead, self.fill_duration)
            },
        };
        let fill = fill_node(inner.root.clone(), &mut fill_walk);
        if !fill.is_empty() {
            children.push(fill);
        }

        let mut stroke_walk = StrokeWalk {
            slots: &slots,
            next: 0,
            cursor: elapsed,
            completed_stroke_opacity: self.completed_stroke_opacity,
            fallback_stroke_width: self.stroke_width,
        };
        let stroke = stroke_node(inner.root, &mut stroke_walk);
        if !stroke.is_empty() {
            children.push(stroke);
        }

        VectorGraphic {
            view_box,
            root: Node::Group(Group {
                transform: Transform::IDENTITY,
                opacity: 1.0,
                children,
            }),
        }
    }
}

/// Extension trait adding write-on wrappers to built vector components.
pub trait VectorWrite: VectorComponent + Sized + 'static {
    fn write_on(self, progress: Phase) -> Write {
        Write::new(progress, self)
    }

    fn write_on_with_width(self, progress: Phase, stroke_width: f32) -> Write {
        Write::new(progress, self).stroke_width(stroke_width)
    }

    fn write_from<T: Time>(self, time: T, start: f64) -> TimedWrite {
        TimedWrite::new(time, start, self)
    }

    fn write_from_with_speed<T: Time>(self, time: T, start: f64, stroke_speed: f32) -> TimedWrite {
        TimedWrite::new(time, start, self).stroke_speed(stroke_speed)
    }

    fn write_elapsed(self, elapsed: f64) -> TimedWrite {
        TimedWrite::from_elapsed(elapsed, self)
    }

    fn write_elapsed_with_speed(self, elapsed: f64, stroke_speed: f32) -> TimedWrite {
        TimedWrite::from_elapsed(elapsed, self).stroke_speed(stroke_speed)
    }

    fn write_since(self, event: Event, clock: &Clock<'_>) -> TimedWrite {
        self.write_elapsed(event.elapsed(clock))
    }

    fn write_since_with_speed(
        self,
        event: Event,
        clock: &Clock<'_>,
        stroke_speed: f32,
    ) -> TimedWrite {
        self.write_elapsed_with_speed(event.elapsed(clock), stroke_speed)
    }
}

impl<T: VectorComponent + 'static> VectorWrite for T {}

/// Builder-side write-on wrappers, so complete builders do not need `.build()`.
pub trait VectorBuilderWrite: VectorBuilder {
    fn write_on(self, progress: Phase) -> Write {
        Write::new(progress, self.build_component())
    }

    fn write_on_with_width(self, progress: Phase, stroke_width: f32) -> Write {
        Write::new(progress, self.build_component()).stroke_width(stroke_width)
    }

    fn write_from<T: Time>(self, time: T, start: f64) -> TimedWrite {
        TimedWrite::new(time, start, self.build_component())
    }

    fn write_from_with_speed<T: Time>(self, time: T, start: f64, stroke_speed: f32) -> TimedWrite {
        TimedWrite::new(time, start, self.build_component()).stroke_speed(stroke_speed)
    }

    fn write_elapsed(self, elapsed: f64) -> TimedWrite {
        TimedWrite::from_elapsed(elapsed, self.build_component())
    }

    fn write_elapsed_with_speed(self, elapsed: f64, stroke_speed: f32) -> TimedWrite {
        TimedWrite::from_elapsed(elapsed, self.build_component()).stroke_speed(stroke_speed)
    }

    fn write_since(self, event: Event, clock: &Clock<'_>) -> TimedWrite {
        self.write_elapsed(event.elapsed(clock))
    }

    fn write_since_with_speed(
        self,
        event: Event,
        clock: &Clock<'_>,
        stroke_speed: f32,
    ) -> TimedWrite {
        self.write_elapsed_with_speed(event.elapsed(clock), stroke_speed)
    }
}

impl<B: VectorBuilder> VectorBuilderWrite for B {}

fn write_bounds_for_node(bounds: Rect, node: &Node, progress: Phase, stroke_width: f32) -> Rect {
    if progress == Phase::ZERO || progress == Phase::ONE {
        return bounds;
    }
    let outset = node_write_outset(node, stroke_width, Transform::IDENTITY);
    if outset <= 0.0 {
        return bounds;
    }
    Rect {
        origin: Vec2(bounds.origin.0 - outset, bounds.origin.1 - outset),
        size: Vec2(bounds.size.0 + outset * 2.0, bounds.size.1 + outset * 2.0),
    }
}

fn node_write_outset(node: &Node, fallback_stroke_width: f32, parent: Transform) -> f32 {
    match node {
        Node::Group(group) => {
            let transform = parent.concat(group.transform);
            group
                .children
                .iter()
                .map(|child| node_write_outset(child, fallback_stroke_width, transform))
                .fold(0.0, f32::max)
        }
        Node::SingleGroup(group) => node_write_outset(
            &group.child,
            fallback_stroke_width,
            parent.concat(group.transform),
        ),
        Node::ClipGroup(group) => node_write_outset(&group.child, fallback_stroke_width, parent),
        Node::Path(path) => {
            if path.fill.as_ref().is_some_and(|fill| fill.is_visible()) {
                return 0.0;
            }
            write_stroke(path, fallback_stroke_width)
                .map(|stroke| {
                    let transform = parent.concat(path.transform);
                    stroke.width.max(0.0) * max_scale(transform) * 0.5
                })
                .unwrap_or(0.0)
        }
    }
}

/// One entry per `Node::Path` in traversal order: the length the write pen
/// spends on it, or `0.0` when the path has nothing to stroke. Both render
/// walks visit paths in this same order, so indices line up with the slots
/// produced by [`write_slots`].
fn collect_write_lengths(node: &Node, fallback_stroke_width: f32) -> Vec<f32> {
    fn walk(node: &Node, fallback_stroke_width: f32, out: &mut Vec<f32>) {
        match node {
            Node::Group(group) => {
                for child in &group.children {
                    walk(child, fallback_stroke_width, out);
                }
            }
            Node::SingleGroup(group) => walk(&group.child, fallback_stroke_width, out),
            Node::ClipGroup(group) => walk(&group.child, fallback_stroke_width, out),
            Node::Path(path) => {
                let length = if write_stroke(path, fallback_stroke_width).is_some() {
                    path_length(&path.commands)
                } else {
                    0.0
                };
                out.push(length);
            }
        }
    }

    let mut out = Vec::new();
    walk(node, fallback_stroke_width, &mut out);
    out
}

/// A path's slot on the write clock, plus its pen length.
#[derive(Clone, Copy)]
struct Slot {
    start: f64,
    /// Fill scheduling slot: fill starts fading at `start + duration`.
    duration: f64,
    /// How long the pen actually takes; >= duration when speed-capped.
    stroke_duration: f64,
    length: f32,
}

/// Lays the collected path lengths out on an abstract write clock according
/// to `pacing`, returning one slot per path plus the total span. The caller
/// scales the result into its own time units with [`scale_slots`]: [`Write`]
/// normalizes the span onto the progress interval, [`TimedWrite`] converts
/// schedule units into seconds.
fn write_slots(lengths: &[f32], pacing: WritePacing, lag_ratio: f32) -> (Vec<Slot>, f64) {
    match pacing {
        WritePacing::ByLength => {
            let mut walked = 0.0_f64;
            let slots = lengths
                .iter()
                .map(|&length| {
                    let duration = length.max(0.0) as f64;
                    let slot = Slot {
                        start: walked,
                        duration,
                        stroke_duration: duration,
                        length,
                    };
                    walked += duration;
                    slot
                })
                .collect();
            (slots, walked)
        }
        WritePacing::PerPath => {
            let lag = lag_ratio.max(0.0) as f64;
            let mut start = 0.0_f64;
            let mut span = 0.0_f64;
            let slots = lengths
                .iter()
                .map(|&length| {
                    if length <= 0.0 {
                        // Nothing to stroke: complete instantly where the pen is.
                        Slot {
                            start,
                            duration: 0.0,
                            stroke_duration: 0.0,
                            length: 0.0,
                        }
                    } else {
                        let slot = Slot {
                            start,
                            duration: 1.0,
                            stroke_duration: 1.0,
                            length,
                        };
                        span = span.max(start + 1.0);
                        start += lag;
                        slot
                    }
                })
                .collect();
            (slots, span)
        }
    }
}

fn scale_slots(slots: &mut [Slot], scale: f64) {
    for slot in slots {
        slot.start *= scale;
        slot.duration *= scale;
        slot.stroke_duration *= scale;
    }
}

/// Extends each slot's stroke_duration so the pen never exceeds
/// `max_speed` (units/sec), leaving fill scheduling (`duration`) intact.
fn cap_stroke_speed(slots: &mut [Slot], max_speed: f32) {
    if !max_speed.is_finite() || max_speed <= 0.0 {
        return;
    }
    for slot in slots {
        if slot.length > 0.0 {
            slot.stroke_duration = slot.stroke_duration.max((slot.length / max_speed) as f64);
        }
    }
}

/// Latest moment any pen is still drawing, accounting for speed caps.
fn stroke_span(slots: &[Slot]) -> f64 {
    slots
        .iter()
        .map(|slot| slot.start + slot.stroke_duration)
        .fold(0.0, f64::max)
}

/// Fraction of `slot` the write cursor has covered, saturating at both ends.
fn slot_progress(cursor: f64, slot: Slot) -> f32 {
    if slot.stroke_duration <= 0.0 {
        if cursor >= slot.start {
            1.0
        } else {
            0.0
        }
    } else {
        clamp_unit(((cursor - slot.start) / slot.stroke_duration) as f32)
    }
}

struct StrokeWalk<'a> {
    slots: &'a [Slot],
    next: usize,
    cursor: f64,
    completed_stroke_opacity: f32,
    fallback_stroke_width: f32,
}

fn stroke_node(node: Node, walk: &mut StrokeWalk<'_>) -> Node {
    match node {
        Node::Group(group) => Node::Group(Group {
            transform: group.transform,
            opacity: group.opacity,
            children: group
                .children
                .into_iter()
                .map(|child| stroke_node(child, walk))
                .collect(),
        }),
        Node::SingleGroup(group) => Node::SingleGroup(SingleGroup {
            transform: group.transform,
            opacity: group.opacity,
            child: Box::new(stroke_node(*group.child, walk)),
        }),
        Node::ClipGroup(group) => Node::ClipGroup(ClipGroup {
            commands: group.commands,
            transform: group.transform,
            child: Box::new(stroke_node(*group.child, walk)),
        }),
        Node::Path(path) => stroke_path_node(path, walk),
    }
}

fn stroke_path_node(path: Path, walk: &mut StrokeWalk<'_>) -> Node {
    let slot = walk.slots[walk.next];
    walk.next += 1;

    let Some(stroke) = write_stroke(&path, walk.fallback_stroke_width) else {
        return Node::empty();
    };
    if slot.length <= 0.0 {
        return Node::empty();
    }

    let take = slot.length * slot_progress(walk.cursor, slot);
    if take <= 0.0 {
        return Node::empty();
    }
    let commands = trim_path(&path.commands, take);
    if commands.len() < 2 {
        return Node::empty();
    }

    let stroke_node = Node::Path(Path {
        commands,
        fill: None,
        stroke: Some(stroke),
        transform: path.transform,
    });

    let opacity = if temporary_stroke_from_fill(&path) && take >= slot.length {
        clamp_unit(walk.completed_stroke_opacity)
    } else {
        1.0
    };
    if opacity <= 0.0 {
        return Node::empty();
    }

    if path.fill.as_ref().is_some_and(|fill| fill.is_visible()) {
        let clipped = Node::ClipGroup(ClipGroup {
            commands: path.commands,
            transform: path.transform,
            child: Box::new(stroke_node),
        });
        if opacity >= 1.0 {
            clipped
        } else {
            Node::single_group(Transform::IDENTITY, opacity, clipped)
        }
    } else {
        stroke_node
    }
}

fn write_stroke(path: &Path, fallback_stroke_width: f32) -> Option<Stroke> {
    if let Some(stroke) = path.stroke.as_ref().filter(|stroke| stroke.is_visible()) {
        return Some(stroke.clone());
    }
    path.fill
        .as_ref()
        .filter(|fill| fill.is_visible())
        .map(|fill| Stroke {
            paint: fill.paint.clone(),
            width: fallback_stroke_width,
            dash: None,
        })
        .filter(|stroke| stroke.is_visible())
}

fn temporary_stroke_from_fill(path: &Path) -> bool {
    path.stroke
        .as_ref()
        .is_none_or(|stroke| !stroke.is_visible())
        && path.fill.as_ref().is_some_and(|fill| fill.is_visible())
}

fn path_length(commands: &[PathCommand]) -> f32 {
    let mut current = None;
    let mut start = None;
    let mut total = 0.0;

    for &cmd in commands {
        match cmd {
            PathCommand::MoveTo(p) => {
                current = Some(p);
                start = Some(p);
            }
            PathCommand::LineTo(to) => {
                if let Some(from) = current {
                    total += distance(from, to);
                }
                current = Some(to);
            }
            PathCommand::QuadTo { control, to } => {
                if let Some(from) = current {
                    total += quad_length(from, control, to);
                }
                current = Some(to);
            }
            PathCommand::CubicTo { c1, c2, to } => {
                if let Some(from) = current {
                    total += cubic_length(from, c1, c2, to);
                }
                current = Some(to);
            }
            PathCommand::Close => {
                if let (Some(from), Some(to)) = (current, start) {
                    total += distance(from, to);
                    current = Some(to);
                }
            }
        }
    }

    total
}

fn trim_path(commands: &[PathCommand], target_len: f32) -> Vec<PathCommand> {
    let mut out = Vec::new();
    let mut current = None;
    let mut start = None;
    let mut remaining = target_len.max(0.0);

    for &cmd in commands {
        match cmd {
            PathCommand::MoveTo(p) => {
                if remaining <= 0.0 {
                    break;
                }
                out.push(cmd);
                current = Some(p);
                start = Some(p);
            }
            PathCommand::LineTo(to) => {
                let Some(from) = current else {
                    continue;
                };
                let len = distance(from, to);
                let t = segment_t(remaining, len);
                if !consume_segment(
                    &mut remaining,
                    len,
                    PathCommand::LineTo(to),
                    PathCommand::LineTo(lerp(from, to, t)),
                    &mut out,
                ) {
                    break;
                }
                current = Some(to);
            }
            PathCommand::QuadTo { control, to } => {
                let Some(from) = current else {
                    continue;
                };
                let len = quad_length(from, control, to);
                let full = PathCommand::QuadTo { control, to };
                let t = quad_t_at_length(from, control, to, remaining);
                let (partial_control, partial_to) = quad_prefix(from, control, to, t);
                let partial = PathCommand::QuadTo {
                    control: partial_control,
                    to: partial_to,
                };
                if !consume_segment(&mut remaining, len, full, partial, &mut out) {
                    break;
                }
                current = Some(to);
            }
            PathCommand::CubicTo { c1, c2, to } => {
                let Some(from) = current else {
                    continue;
                };
                let len = cubic_length(from, c1, c2, to);
                let full = PathCommand::CubicTo { c1, c2, to };
                let t = cubic_t_at_length(from, c1, c2, to, remaining);
                let (partial_c1, partial_c2, partial_to) = cubic_prefix(from, c1, c2, to, t);
                let partial = PathCommand::CubicTo {
                    c1: partial_c1,
                    c2: partial_c2,
                    to: partial_to,
                };
                if !consume_segment(&mut remaining, len, full, partial, &mut out) {
                    break;
                }
                current = Some(to);
            }
            PathCommand::Close => {
                let (Some(from), Some(to)) = (current, start) else {
                    continue;
                };
                let len = distance(from, to);
                let t = segment_t(remaining, len);
                if !consume_segment(
                    &mut remaining,
                    len,
                    PathCommand::Close,
                    PathCommand::LineTo(lerp(from, to, t)),
                    &mut out,
                ) {
                    break;
                }
                current = Some(to);
            }
        }
    }

    out
}

fn consume_segment(
    remaining: &mut f32,
    len: f32,
    full: PathCommand,
    partial: PathCommand,
    out: &mut Vec<PathCommand>,
) -> bool {
    if len <= 0.0 {
        out.push(full);
        return true;
    }
    if *remaining >= len {
        out.push(full);
        *remaining -= len;
        return true;
    }
    if *remaining > 0.0 {
        out.push(partial);
    }
    false
}

fn segment_t(partial: f32, total: f32) -> f32 {
    if total <= 0.0 {
        1.0
    } else {
        clamp_unit(partial / total)
    }
}

/// Shared fill walk: `alpha_at` maps a path's completion moment on the write
/// clock to a fill opacity ([`fill_alpha`] for [`Write`],
/// [`timed_fill_alpha`] for [`TimedWrite`]).
struct FillWalk<'a, F: Fn(f64) -> f32> {
    slots: &'a [Slot],
    next: usize,
    alpha_at: F,
}

fn fill_node<F: Fn(f64) -> f32>(node: Node, walk: &mut FillWalk<'_, F>) -> Node {
    match node {
        Node::Group(group) => Node::Group(Group {
            transform: group.transform,
            opacity: group.opacity,
            children: group
                .children
                .into_iter()
                .map(|child| fill_node(child, walk))
                .collect(),
        }),
        Node::SingleGroup(group) => Node::SingleGroup(SingleGroup {
            transform: group.transform,
            opacity: group.opacity,
            child: Box::new(fill_node(*group.child, walk)),
        }),
        Node::ClipGroup(group) => Node::ClipGroup(ClipGroup {
            commands: group.commands,
            transform: group.transform,
            child: Box::new(fill_node(*group.child, walk)),
        }),
        Node::Path(path) => fill_path_node(path, walk),
    }
}

fn fill_path_node<F: Fn(f64) -> f32>(path: Path, walk: &mut FillWalk<'_, F>) -> Node {
    let slot = walk.slots[walk.next];
    walk.next += 1;

    let Some(fill) = path.fill.filter(|fill| fill.is_visible()) else {
        return Node::empty();
    };
    let alpha = (walk.alpha_at)(slot.start + slot.duration);
    if alpha <= 0.0 {
        return Node::empty();
    }

    let filled = Node::Path(Path {
        commands: path.commands,
        fill: Some(fill),
        stroke: None,
        transform: path.transform,
    });
    if alpha >= 1.0 {
        filled
    } else {
        Node::single_group(Transform::IDENTITY, alpha, filled)
    }
}

fn stroke_end_point(stroke_end: f32) -> f32 {
    clamp_unit(stroke_end).clamp(0.000_1, 1.0)
}

fn fill_alpha(progress: f32, completion: f32, lead: f32, delay: f32, duration: f32) -> f32 {
    let lead = lead.max(0.0).min(completion.max(0.0));
    let start = (completion - lead + delay.max(0.0)).clamp(0.0, 1.0);
    let available = (1.0 - start).max(0.000_1);
    let duration = duration.max(0.000_1).min(available);
    smoothstep(clamp_unit((progress - start) / duration))
}

fn timed_fill_alpha(elapsed: f64, completion: f64, lead: f64, duration: f64) -> f32 {
    let start = (completion - lead.max(0.0)).max(0.0);
    let duration = duration.max(0.000_1);
    smoothstep(clamp_unit(((elapsed - start) / duration) as f32))
}

fn timed_stroke_speed(stroke_speed: f32) -> f32 {
    stroke_speed.max(0.000_1)
}

/// The moment (in seconds) a timed write is fully settled: the last outline
/// is drawn at `stroke_span` and the last fill has finished fading after the
/// nominal fill schedule ends at `fill_span`.
fn timed_done_at(stroke_span: f64, fill_span: f64, fill_lead: f64, fill_duration: f64) -> f64 {
    let fill_done = (fill_span - fill_lead.max(0.0)).max(0.0) + fill_duration.max(0.000_1);
    stroke_span.max(fill_done)
}

fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

fn distance(a: Vec2, b: Vec2) -> f32 {
    ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt()
}

fn lerp(a: Vec2, b: Vec2, t: f32) -> Vec2 {
    Vec2(a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

fn max_scale(transform: Transform) -> f32 {
    let aa = transform.a * transform.a + transform.b * transform.b;
    let cc = transform.c * transform.c + transform.d * transform.d;
    let ac = transform.a * transform.c + transform.b * transform.d;
    let trace = aa + cc;
    let det = aa * cc - ac * ac;
    let discriminant = (trace * trace - 4.0 * det).max(0.0);
    let lambda = (trace + discriminant.sqrt()) * 0.5;
    if lambda.is_finite() {
        lambda.sqrt()
    } else {
        1.0
    }
}

fn quad_point(p0: Vec2, c: Vec2, p1: Vec2, t: f32) -> Vec2 {
    let p01 = lerp(p0, c, t);
    let p12 = lerp(c, p1, t);
    lerp(p01, p12, t)
}

fn cubic_point(p0: Vec2, c1: Vec2, c2: Vec2, p1: Vec2, t: f32) -> Vec2 {
    let p01 = lerp(p0, c1, t);
    let p12 = lerp(c1, c2, t);
    let p23 = lerp(c2, p1, t);
    let p012 = lerp(p01, p12, t);
    let p123 = lerp(p12, p23, t);
    lerp(p012, p123, t)
}

fn quad_prefix(p0: Vec2, c: Vec2, p1: Vec2, t: f32) -> (Vec2, Vec2) {
    let p01 = lerp(p0, c, t);
    let p12 = lerp(c, p1, t);
    let p012 = lerp(p01, p12, t);
    (p01, p012)
}

fn cubic_prefix(p0: Vec2, c1: Vec2, c2: Vec2, p1: Vec2, t: f32) -> (Vec2, Vec2, Vec2) {
    let p01 = lerp(p0, c1, t);
    let p12 = lerp(c1, c2, t);
    let p23 = lerp(c2, p1, t);
    let p012 = lerp(p01, p12, t);
    let p123 = lerp(p12, p23, t);
    let p0123 = lerp(p012, p123, t);
    (p01, p012, p0123)
}

fn quad_length(p0: Vec2, c: Vec2, p1: Vec2) -> f32 {
    sample_length(QUAD_STEPS, |t| quad_point(p0, c, p1, t))
}

fn cubic_length(p0: Vec2, c1: Vec2, c2: Vec2, p1: Vec2) -> f32 {
    sample_length(CUBIC_STEPS, |t| cubic_point(p0, c1, c2, p1, t))
}

fn sample_length(steps: usize, point_at: impl Fn(f32) -> Vec2) -> f32 {
    let mut prev = point_at(0.0);
    let mut total = 0.0;
    for i in 1..=steps {
        let p = point_at(i as f32 / steps as f32);
        total += distance(prev, p);
        prev = p;
    }
    total
}

fn quad_t_at_length(p0: Vec2, c: Vec2, p1: Vec2, target: f32) -> f32 {
    t_at_sampled_length(QUAD_STEPS, target, |t| quad_point(p0, c, p1, t))
}

fn cubic_t_at_length(p0: Vec2, c1: Vec2, c2: Vec2, p1: Vec2, target: f32) -> f32 {
    t_at_sampled_length(CUBIC_STEPS, target, |t| cubic_point(p0, c1, c2, p1, t))
}

fn t_at_sampled_length(steps: usize, target: f32, point_at: impl Fn(f32) -> Vec2) -> f32 {
    let mut prev_t = 0.0;
    let mut prev = point_at(prev_t);
    let mut walked = 0.0;

    for i in 1..=steps {
        let t = i as f32 / steps as f32;
        let p = point_at(t);
        let len = distance(prev, p);
        if walked + len >= target {
            let local = segment_t(target - walked, len);
            return prev_t + (t - prev_t) * local;
        }
        walked += len;
        prev = p;
        prev_t = t;
    }

    1.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Color;
    use crate::geometry::Transform;
    use crate::shapes::Rectangle;
    use crate::time::{LocalTime, TimelineTime};
    use crate::timeline_component::{Clock, Event, TriggerTable};
    use crate::vector::{Fill, Paint, VectorTransform};

    fn paint() -> Paint {
        Paint::Solid(Color::rgb_u8(20, 30, 40))
    }

    fn rect() -> Rectangle {
        Rectangle {
            size: Vec2(10.0, 10.0),
            fill: Some(Fill { paint: paint() }),
            stroke: None,
        }
    }

    fn rect_path(x: f32, width: f32) -> Node {
        Node::Path(Path {
            commands: vec![
                PathCommand::MoveTo(Vec2(x, 0.0)),
                PathCommand::LineTo(Vec2(x + width, 0.0)),
                PathCommand::LineTo(Vec2(x + width, 10.0)),
                PathCommand::LineTo(Vec2(x, 10.0)),
                PathCommand::Close,
            ],
            fill: Some(Fill { paint: paint() }),
            stroke: None,
            transform: Transform::IDENTITY,
        })
    }

    fn rects_graphic(size: Vec2, rects: Vec<Node>) -> VectorGraphic {
        VectorGraphic {
            view_box: Rect {
                origin: Vec2::ZERO,
                size,
            },
            root: Node::Group(Group {
                transform: Transform::IDENTITY,
                opacity: 1.0,
                children: rects,
            }),
        }
    }

    #[derive(Clone, PartialEq, Hash)]
    struct TwoRects;

    impl VectorComponent for TwoRects {
        fn layout(&self, constraints: Constraints) -> Vec2 {
            constraints.constrain(Vec2(24.0, 10.0))
        }

        fn render(&self, size: Vec2) -> VectorGraphic {
            rects_graphic(size, vec![rect_path(0.0, 10.0), rect_path(14.0, 10.0)])
        }
    }

    /// Two filled rects with very different perimeters: 40 and 80 units.
    #[derive(Clone, PartialEq, Hash)]
    struct UnevenRects;

    impl VectorComponent for UnevenRects {
        fn layout(&self, constraints: Constraints) -> Vec2 {
            constraints.constrain(Vec2(44.0, 10.0))
        }

        fn render(&self, size: Vec2) -> VectorGraphic {
            rects_graphic(size, vec![rect_path(0.0, 10.0), rect_path(14.0, 30.0)])
        }
    }

    #[test]
    fn layout_delegates_to_child() {
        let write = rect().write_on(Phase::HALF);
        assert_eq!(write.layout(Constraints::UNBOUNDED), Vec2(10.0, 10.0));
    }

    #[test]
    fn progress_zero_renders_no_ink() {
        let write = rect().write_on(Phase::ZERO);
        let graphic = write.render(Vec2(10.0, 10.0));
        assert_eq!(graphic.root, Node::empty());
    }

    #[test]
    fn progress_one_returns_original_child() {
        let write = rect().write_on(Phase::ONE);
        let original = rect().render(Vec2(10.0, 10.0));
        assert_eq!(write.render(Vec2(10.0, 10.0)), original);
    }

    #[test]
    fn half_progress_reveals_half_of_rectangle_outline() {
        let write = rect().write_on(Phase::HALF).fill_start(Phase::ONE);
        let graphic = write.render(Vec2(10.0, 10.0));
        let Node::Group(root) = graphic.root else {
            panic!("write reveal should render a group");
        };
        assert_eq!(root.children.len(), 1);
        let Node::ClipGroup(clip) = &root.children[0] else {
            panic!("fill paths should clip the temporary stroke inward");
        };
        let Node::Path(path) = clip.child.as_ref() else {
            panic!("clipped reveal should contain a stroke path");
        };

        assert!(path.fill.is_none());
        assert_eq!(
            path.stroke,
            Some(Stroke {
                paint: paint(),
                width: DEFAULT_STROKE_WIDTH,
                dash: None,
            })
        );
        assert_eq!(path.transform, Transform::IDENTITY);
        assert_eq!(
            path.commands,
            vec![
                PathCommand::MoveTo(Vec2(0.0, 0.0)),
                PathCommand::LineTo(Vec2(10.0, 0.0)),
                PathCommand::LineTo(Vec2(10.0, 10.0)),
            ]
        );
    }

    #[test]
    fn inset_write_does_not_expand_filled_child_bounds() {
        let write = rect()
            .transform(Transform::scale(Vec2(3.0, 2.0)))
            .write_on(Phase::HALF)
            .fill_start(Phase::ONE);
        assert_eq!(
            write.paint_bounds(Vec2(10.0, 10.0)),
            Rect {
                origin: Vec2::ZERO,
                size: Vec2(30.0, 20.0),
            }
        );
    }

    #[test]
    fn fill_waits_then_fades_in_after_write_completes() {
        let closing = rect()
            .write_on(Phase::HALF)
            .stroke_end(Phase::HALF)
            .fill_delay(Phase::saturating(0.1))
            .fill_lead(Phase::ZERO)
            .fill_duration(Phase::saturating(0.2));
        let graphic = closing.render(Vec2(10.0, 10.0));
        let Node::Group(root) = graphic.root else {
            panic!("write reveal should render a group");
        };
        assert_eq!(root.children.len(), 1);
        let Node::SingleGroup(stroke) = &root.children[0] else {
            panic!("the closed path should show a dimmed guide stroke");
        };
        assert!((stroke.opacity - DEFAULT_COMPLETED_STROKE_OPACITY).abs() < 0.000_01);
        assert!(matches!(stroke.child.as_ref(), Node::ClipGroup(_)));

        let fading = rect()
            .write_on(Phase::saturating(0.7))
            .stroke_end(Phase::HALF)
            .fill_delay(Phase::saturating(0.1))
            .fill_lead(Phase::ZERO)
            .fill_duration(Phase::saturating(0.2));
        let graphic = fading.render(Vec2(10.0, 10.0));
        let Node::Group(root) = graphic.root else {
            panic!("write reveal should render a group");
        };

        assert_eq!(root.children.len(), 2);
        let Node::SingleGroup(fill) = &root.children[0] else {
            panic!("first child should be the fading original fill");
        };
        assert!((fill.opacity - 0.5).abs() < 0.000_01);
        let Node::SingleGroup(stroke) = &root.children[1] else {
            panic!("second child should keep the completed temporary stroke");
        };
        assert!((stroke.opacity - DEFAULT_COMPLETED_STROKE_OPACITY).abs() < 0.000_01);
        assert!(matches!(stroke.child.as_ref(), Node::ClipGroup(_)));
    }

    #[test]
    fn completed_paths_fill_before_later_paths_finish_writing() {
        let write = TwoRects
            .write_on(Phase::saturating(0.5))
            .by_length()
            .stroke_end(Phase::saturating(0.8));
        let graphic = write.render(Vec2(24.0, 10.0));
        let Node::Group(root) = graphic.root else {
            panic!("write reveal should render a group");
        };

        assert_eq!(root.children.len(), 2);
        let Node::Group(fill) = &root.children[0] else {
            panic!("first child should be the per-path fill layer");
        };
        assert!(matches!(fill.children[0], Node::SingleGroup(_)));
        assert!(fill.children[1].is_empty());

        let Node::Group(stroke) = &root.children[1] else {
            panic!("second child should be the stroke reveal layer");
        };
        assert!(matches!(stroke.children[0], Node::SingleGroup(_)));
        assert!(matches!(stroke.children[1], Node::ClipGroup(_)));
    }

    #[test]
    fn write_on_defaults_to_staggered_per_path_slots() {
        let write = TwoRects
            .write_on(Phase::saturating(0.25))
            .stroke_end(Phase::saturating(0.75));
        let graphic = write.render(Vec2(24.0, 10.0));
        let Node::Group(root) = graphic.root else {
            panic!("write reveal should render a group");
        };

        // No fill has started yet, so only the stroke layer is present.
        assert_eq!(root.children.len(), 1);
        let Node::Group(stroke) = &root.children[0] else {
            panic!("the remaining child should be the stroke reveal layer");
        };
        let Node::ClipGroup(first) = &stroke.children[0] else {
            panic!("first path should still be writing");
        };
        let Node::Path(first) = first.child.as_ref() else {
            panic!("clipped reveal should contain a stroke path");
        };
        // Slots are normalized onto [0, stroke_end]: the first path writes
        // over [0, 0.5], so progress 0.25 draws half its 40-unit outline.
        assert_eq!(
            first.commands,
            vec![
                PathCommand::MoveTo(Vec2(0.0, 0.0)),
                PathCommand::LineTo(Vec2(10.0, 0.0)),
                PathCommand::LineTo(Vec2(10.0, 10.0)),
            ]
        );
        // The second slot starts lag_ratio (0.5) into the first one, exactly
        // at the current progress, so it has not drawn anything yet.
        assert!(stroke.children[1].is_empty());
    }

    #[test]
    fn write_on_per_path_outlines_complete_by_stroke_end() {
        let write = TwoRects
            .write_on(Phase::saturating(0.75))
            .stroke_end(Phase::saturating(0.75));
        let graphic = write.render(Vec2(24.0, 10.0));
        let Node::Group(root) = graphic.root else {
            panic!("write reveal should render a group");
        };

        assert_eq!(root.children.len(), 2);
        let Node::Group(stroke) = &root.children[1] else {
            panic!("second child should be the stroke reveal layer");
        };
        // Both slots end no later than stroke_end, so at that progress every
        // outline is complete and dimmed while the fills keep fading in.
        for child in &stroke.children {
            let Node::SingleGroup(done) = child else {
                panic!("every path should be complete and dimmed");
            };
            assert!((done.opacity - DEFAULT_COMPLETED_STROKE_OPACITY).abs() < 0.000_01);
        }
    }

    #[test]
    fn fill_can_lead_the_path_closure() {
        let write = rect()
            .write_on(Phase::saturating(0.45))
            .stroke_end(Phase::HALF)
            .fill_delay(Phase::ZERO)
            .fill_lead(Phase::saturating(0.1))
            .fill_duration(Phase::saturating(0.2));
        let graphic = write.render(Vec2(10.0, 10.0));
        let Node::Group(root) = graphic.root else {
            panic!("write reveal should render a group");
        };

        assert_eq!(root.children.len(), 2);
        let Node::SingleGroup(fill) = &root.children[0] else {
            panic!("first child should be the leading original fill");
        };
        assert!(fill.opacity > 0.0);
        assert!(fill.opacity < 1.0);
        let Node::ClipGroup(stroke) = &root.children[1] else {
            panic!("second child should still be the unfinished temporary stroke");
        };
        assert!(matches!(stroke.child.as_ref(), Node::Path(_)));
    }

    #[test]
    fn timed_write_draws_at_constant_path_speed() {
        let write = rect().write_from_with_speed(TimelineTime::new(0.5), 0.0, 40.0);
        let graphic = write.render(Vec2(10.0, 10.0));
        let Node::Group(root) = graphic.root else {
            panic!("timed write reveal should render a group");
        };
        assert_eq!(root.children.len(), 1);
        let Node::ClipGroup(clip) = &root.children[0] else {
            panic!("half-second at 40 units/s should draw half the 40-unit rectangle");
        };
        let Node::Path(path) = clip.child.as_ref() else {
            panic!("clipped reveal should contain a stroke path");
        };

        assert_eq!(
            path.commands,
            vec![
                PathCommand::MoveTo(Vec2(0.0, 0.0)),
                PathCommand::LineTo(Vec2(10.0, 0.0)),
                PathCommand::LineTo(Vec2(10.0, 10.0)),
            ]
        );
    }

    #[test]
    fn per_path_pacing_staggers_equal_time_slots() {
        let write = TwoRects
            .write_elapsed(0.75)
            .per_path_secs(1.0)
            .lag_ratio(0.5);
        let graphic = write.render(Vec2(24.0, 10.0));
        let Node::Group(root) = graphic.root else {
            panic!("per-path write reveal should render a group");
        };

        // No fill has started yet, so only the stroke layer is present.
        assert_eq!(root.children.len(), 1);
        let Node::Group(stroke) = &root.children[0] else {
            panic!("the remaining child should be the stroke reveal layer");
        };
        let Node::ClipGroup(first) = &stroke.children[0] else {
            panic!("first path should still be writing");
        };
        let Node::Path(first) = first.child.as_ref() else {
            panic!("clipped reveal should contain a stroke path");
        };
        // 0.75s into its one-second slot: 30 of 40 units drawn.
        assert_eq!(
            first.commands,
            vec![
                PathCommand::MoveTo(Vec2(0.0, 0.0)),
                PathCommand::LineTo(Vec2(10.0, 0.0)),
                PathCommand::LineTo(Vec2(10.0, 10.0)),
                PathCommand::LineTo(Vec2(0.0, 10.0)),
            ]
        );
        let Node::ClipGroup(second) = &stroke.children[1] else {
            panic!("second path should have started halfway through the first slot");
        };
        let Node::Path(second) = second.child.as_ref() else {
            panic!("clipped reveal should contain a stroke path");
        };
        // 0.25s into its [0.5, 1.5] slot: 10 of 40 units drawn.
        assert_eq!(
            second.commands,
            vec![
                PathCommand::MoveTo(Vec2(14.0, 0.0)),
                PathCommand::LineTo(Vec2(24.0, 0.0)),
            ]
        );
    }

    #[test]
    fn per_path_pacing_gives_long_and_short_paths_the_same_slot() {
        let write = UnevenRects
            .write_elapsed(1.0)
            .per_path_secs(1.0)
            .lag_ratio(1.0);
        let graphic = write.render(Vec2(44.0, 10.0));
        let Node::Group(root) = graphic.root else {
            panic!("per-path write reveal should render a group");
        };

        assert_eq!(root.children.len(), 2);
        let Node::Group(fill) = &root.children[0] else {
            panic!("first child should be the per-path fill layer");
        };
        // The short path just completed, so its fill has started fading in.
        assert!(matches!(fill.children[0], Node::SingleGroup(_)));
        assert!(fill.children[1].is_empty());

        let Node::Group(stroke) = &root.children[1] else {
            panic!("second child should be the stroke reveal layer");
        };
        // The 40-unit path finished exactly at the end of its one-second
        // slot; the 80-unit path has an equal slot and has not started yet.
        let Node::SingleGroup(done) = &stroke.children[0] else {
            panic!("short path should be complete and dimmed");
        };
        assert!((done.opacity - DEFAULT_COMPLETED_STROKE_OPACITY).abs() < 0.000_01);
        assert!(stroke.children[1].is_empty());
    }

    #[test]
    fn timed_fill_can_lead_the_path_closure() {
        let write = rect()
            .write_from_with_speed(TimelineTime::new(0.95), 0.0, 40.0)
            .fill_lead_secs(0.1)
            .fill_duration_secs(0.2);
        let graphic = write.render(Vec2(10.0, 10.0));
        let Node::Group(root) = graphic.root else {
            panic!("timed write reveal should render a group");
        };

        assert_eq!(root.children.len(), 2);
        let Node::SingleGroup(fill) = &root.children[0] else {
            panic!("first child should be the leading original fill");
        };
        assert!(fill.opacity > 0.0);
        assert!(fill.opacity < 1.0);
        let Node::ClipGroup(stroke) = &root.children[1] else {
            panic!("second child should still be the unfinished temporary stroke");
        };
        assert!(matches!(stroke.child.as_ref(), Node::Path(_)));
    }

    #[test]
    fn max_stroke_speed_slows_long_paths() {
        // Short path: 40 units, long path: 80 units. With per_path_secs=1 and
        // lag_ratio=1 the nominal slots are [0,1] and [1,2]. Cap at 40 u/s so
        // the long path needs 2s of stroke time and is only halfway done at t=2.
        let write = UnevenRects
            .write_elapsed(2.0)
            .per_path_secs(1.0)
            .lag_ratio(1.0)
            .max_stroke_speed(40.0)
            .fill_lead_secs(0.0);
        let graphic = write.render(Vec2(44.0, 10.0));
        let Node::Group(root) = graphic.root else {
            panic!("capped write reveal should render a group");
        };

        assert_eq!(root.children.len(), 2);
        let Node::Group(fill) = &root.children[0] else {
            panic!("first child should be the per-path fill layer");
        };
        // Short path finished on schedule (fill fully opaque). Long path's
        // fill has not started yet at the exact slot-end instant with lead=0.
        assert!(matches!(fill.children[0], Node::Path(_)));
        assert!(fill.children[1].is_empty());

        let Node::Group(stroke) = &root.children[1] else {
            panic!("second child should be the stroke reveal layer");
        };
        let Node::SingleGroup(done) = &stroke.children[0] else {
            panic!("short path should be complete and dimmed");
        };
        assert!((done.opacity - DEFAULT_COMPLETED_STROKE_OPACITY).abs() < 0.000_01);

        let Node::ClipGroup(long) = &stroke.children[1] else {
            panic!("long path should still be writing under the speed cap");
        };
        let Node::Path(long) = long.child.as_ref() else {
            panic!("clipped reveal should contain a stroke path");
        };
        // 1s into a 2s stroke at 40 u/s: 40 of 80 units drawn.
        assert_eq!(
            long.commands,
            vec![
                PathCommand::MoveTo(Vec2(14.0, 0.0)),
                PathCommand::LineTo(Vec2(44.0, 0.0)),
                PathCommand::LineTo(Vec2(44.0, 10.0)),
            ]
        );
    }

    #[test]
    fn fill_starts_on_schedule_even_when_stroke_is_capped() {
        // At t=2.1 the long path's fill has started (slot ends at 2.0, lead=0)
        // while its stroke is still incomplete (needs until t=3.0 at 40 u/s).
        let write = UnevenRects
            .write_elapsed(2.1)
            .per_path_secs(1.0)
            .lag_ratio(1.0)
            .max_stroke_speed(40.0)
            .fill_lead_secs(0.0)
            .fill_duration_secs(0.2);
        let graphic = write.render(Vec2(44.0, 10.0));
        let Node::Group(root) = graphic.root else {
            panic!("capped write reveal should render a group");
        };

        assert_eq!(root.children.len(), 2);
        let Node::Group(fill) = &root.children[0] else {
            panic!("first child should be the per-path fill layer");
        };
        let Node::SingleGroup(long_fill) = &fill.children[1] else {
            panic!("long path fill should have started on the nominal schedule");
        };
        assert!(long_fill.opacity > 0.0);
        assert!(long_fill.opacity < 1.0);

        let Node::Group(stroke) = &root.children[1] else {
            panic!("second child should be the stroke reveal layer");
        };
        assert!(
            matches!(stroke.children[1], Node::ClipGroup(_)),
            "long path stroke should still be unfinished under the speed cap"
        );
    }

    #[test]
    fn capped_write_is_not_done_until_stroke_finishes() {
        // Long path stroke ends at t=3.0; fill finishes earlier (2.0 + 0.18).
        // Just before stroke completion the reveal must still be active.
        let almost = UnevenRects
            .write_elapsed(2.99)
            .per_path_secs(1.0)
            .lag_ratio(1.0)
            .max_stroke_speed(40.0);
        let graphic = almost.render(Vec2(44.0, 10.0));
        let Node::Group(root) = graphic.root else {
            panic!("capped write should still be revealing just before stroke end");
        };
        assert!(
            !root.children.is_empty(),
            "should still be drawing the capped stroke"
        );

        let done = UnevenRects
            .write_elapsed(3.0)
            .per_path_secs(1.0)
            .lag_ratio(1.0)
            .max_stroke_speed(40.0);
        let original = UnevenRects.render(Vec2(44.0, 10.0));
        assert_eq!(done.render(Vec2(44.0, 10.0)), original);
    }

    #[test]
    fn timed_write_can_start_from_an_event() {
        let event = Event::new();
        let mut table = TriggerTable::new();
        table.record(event, 1.0);
        let clock = Clock::new(TimelineTime::new(1.5), LocalTime::new(0.0), &table);

        let write = rect().write_since_with_speed(event, &clock, 40.0);
        let graphic = write.render(Vec2(10.0, 10.0));
        let Node::Group(root) = graphic.root else {
            panic!("timed write reveal should render a group");
        };
        let Node::ClipGroup(clip) = &root.children[0] else {
            panic!("half-second after the event should draw half the rectangle");
        };
        let Node::Path(path) = clip.child.as_ref() else {
            panic!("clipped reveal should contain a stroke path");
        };

        assert_eq!(
            path.commands,
            vec![
                PathCommand::MoveTo(Vec2(0.0, 0.0)),
                PathCommand::LineTo(Vec2(10.0, 0.0)),
                PathCommand::LineTo(Vec2(10.0, 10.0)),
            ]
        );
    }

    #[test]
    fn completed_fill_paths_keep_the_temporary_stroke_while_fill_fades() {
        let write = rect()
            .write_on(Phase::saturating(0.7))
            .stroke_end(Phase::HALF)
            .fill_delay(Phase::saturating(0.1))
            .fill_lead(Phase::ZERO)
            .fill_duration(Phase::saturating(0.2));
        let graphic = write.render(Vec2(10.0, 10.0));
        let Node::Group(root) = graphic.root else {
            panic!("write reveal should render a group");
        };
        let Node::SingleGroup(fill) = &root.children[0] else {
            panic!("first child should be the fading original fill");
        };
        let Node::SingleGroup(stroke) = &root.children[1] else {
            panic!("second child should keep the completed temporary stroke");
        };

        assert!((fill.opacity - 0.5).abs() < 0.000_01);
        assert!((stroke.opacity - DEFAULT_COMPLETED_STROKE_OPACITY).abs() < 0.000_01);
    }

    #[test]
    fn partial_line_is_cut_to_remaining_length() {
        let commands = vec![
            PathCommand::MoveTo(Vec2(0.0, 0.0)),
            PathCommand::LineTo(Vec2(10.0, 0.0)),
        ];
        assert_eq!(
            trim_path(&commands, 4.0),
            vec![
                PathCommand::MoveTo(Vec2(0.0, 0.0)),
                PathCommand::LineTo(Vec2(4.0, 0.0)),
            ]
        );
    }
}
