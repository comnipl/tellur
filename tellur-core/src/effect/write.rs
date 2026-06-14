//! Manim-style "write-on" reveal for vector components.
//!
//! [`Write`] wraps any [`VectorComponent`](crate::vector::VectorComponent) and
//! turns its visible paths into partial strokes driven by a [`Phase`]. It is
//! especially useful with [`Text`](crate::text::Text) and, with the `latex`
//! feature enabled, `MathSpan`: both ultimately render as vector paths, so they
//! can be revealed with the same component.

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
const DEFAULT_TIMED_FILL_LEAD: f32 = 0.08;
const DEFAULT_TIMED_FILL_DURATION: f32 = 0.18;
const DEFAULT_COMPLETED_STROKE_OPACITY: f32 = 0.35;
const QUAD_STEPS: usize = 16;
const CUBIC_STEPS: usize = 24;

/// Reveals a vector component by drawing its paths over time.
///
/// The first part of `progress` draws an inset stroke along each path. As each
/// filled path finishes being written, that path's fill fades in behind the
/// completed temporary stroke while later paths keep writing. At `progress == 1`,
/// the original child graphic is returned, so filled text and math settle into
/// their normal final rendering.
#[crate::component(vector)]
#[derive(Keyable)]
pub struct Write {
    pub progress: Phase,
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
        Self {
            progress,
            stroke_width: DEFAULT_STROKE_WIDTH,
            stroke_end: DEFAULT_STROKE_END,
            fill_delay: DEFAULT_FILL_DELAY,
            fill_lead: DEFAULT_FILL_LEAD,
            fill_duration: DEFAULT_FILL_DURATION,
            completed_stroke_opacity: DEFAULT_COMPLETED_STROKE_OPACITY,
            child: Box::new(child),
        }
    }

    pub fn from_box(progress: Phase, child: Box<dyn VectorComponent>) -> Self {
        Self {
            progress,
            stroke_width: DEFAULT_STROKE_WIDTH,
            stroke_end: DEFAULT_STROKE_END,
            fill_delay: DEFAULT_FILL_DELAY,
            fill_lead: DEFAULT_FILL_LEAD,
            fill_duration: DEFAULT_FILL_DURATION,
            completed_stroke_opacity: DEFAULT_COMPLETED_STROKE_OPACITY,
            child,
        }
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

        let total = node_write_length(&inner.root, self.stroke_width);
        if total <= 0.0 {
            return VectorGraphic {
                view_box,
                root: Node::empty(),
            };
        }

        let stroke_end = stroke_end_point(self.stroke_end);
        let stroke_progress = stroke_progress(self.progress, stroke_end);

        let mut children = Vec::new();
        let mut fill_state = FillState {
            walked: 0.0,
            total,
            progress,
            stroke_end,
            fill_delay: self.fill_delay,
            fill_lead: self.fill_lead,
            fill_duration: self.fill_duration,
            fallback_stroke_width: self.stroke_width,
        };
        let fill = fill_node(inner.root.clone(), &mut fill_state);
        if !node_is_empty(&fill) {
            children.push(fill);
        }

        if stroke_progress > 0.0 {
            let mut stroke_state = StrokeState {
                walked: 0.0,
                drawn: total * stroke_progress,
                completed_stroke_opacity: self.completed_stroke_opacity,
                fallback_stroke_width: self.stroke_width,
            };
            let stroke = stroke_node(inner.root, &mut stroke_state);
            if !node_is_empty(&stroke) {
                children.push(stroke);
            }
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

/// Reveals a vector component from a start time at a constant path speed.
///
/// Unlike [`Write`], which consumes an already-normalized [`Phase`],
/// `TimedWrite` measures the child's path length after layout and converts
/// elapsed seconds into drawn distance. Longer text therefore takes longer
/// instead of making the pen move faster.
#[crate::component(vector)]
#[derive(Keyable)]
pub struct TimedWrite {
    pub time: f32,
    pub start: f32,
    #[builder(default = DEFAULT_STROKE_SPEED)]
    pub stroke_speed: f32,
    #[builder(default = DEFAULT_STROKE_WIDTH)]
    pub stroke_width: f32,
    #[builder(default = DEFAULT_TIMED_FILL_LEAD)]
    pub fill_lead: f32,
    #[builder(default = DEFAULT_TIMED_FILL_DURATION)]
    pub fill_duration: f32,
    #[builder(default = DEFAULT_COMPLETED_STROKE_OPACITY)]
    pub completed_stroke_opacity: f32,
    #[builder(into)]
    pub child: Box<dyn VectorComponent>,
}

impl TimedWrite {
    pub fn new<T: Time, C: VectorComponent + 'static>(time: T, start: f32, child: C) -> Self {
        Self::from_box(time, start, Box::new(child))
    }

    pub fn from_box<T: Time>(time: T, start: f32, child: Box<dyn VectorComponent>) -> Self {
        Self {
            time: time.seconds(),
            start,
            stroke_speed: DEFAULT_STROKE_SPEED,
            stroke_width: DEFAULT_STROKE_WIDTH,
            fill_lead: DEFAULT_TIMED_FILL_LEAD,
            fill_duration: DEFAULT_TIMED_FILL_DURATION,
            completed_stroke_opacity: DEFAULT_COMPLETED_STROKE_OPACITY,
            child,
        }
    }

    pub fn from_elapsed<C: VectorComponent + 'static>(elapsed: f32, child: C) -> Self {
        Self::from_elapsed_box(elapsed, Box::new(child))
    }

    pub fn from_elapsed_box(elapsed: f32, child: Box<dyn VectorComponent>) -> Self {
        Self {
            time: elapsed,
            start: 0.0,
            stroke_speed: DEFAULT_STROKE_SPEED,
            stroke_width: DEFAULT_STROKE_WIDTH,
            fill_lead: DEFAULT_TIMED_FILL_LEAD,
            fill_duration: DEFAULT_TIMED_FILL_DURATION,
            completed_stroke_opacity: DEFAULT_COMPLETED_STROKE_OPACITY,
            child,
        }
    }

    pub fn stroke_speed(mut self, stroke_speed: f32) -> Self {
        self.stroke_speed = stroke_speed;
        self
    }

    pub fn stroke_width(mut self, stroke_width: f32) -> Self {
        self.stroke_width = stroke_width;
        self
    }

    pub fn fill_lead_secs(mut self, fill_lead: f32) -> Self {
        self.fill_lead = fill_lead;
        self
    }

    pub fn fill_duration_secs(mut self, fill_duration: f32) -> Self {
        self.fill_duration = fill_duration;
        self
    }

    pub fn completed_stroke_opacity(mut self, opacity: f32) -> Self {
        self.completed_stroke_opacity = opacity;
        self
    }

    fn elapsed(&self) -> f32 {
        self.time - self.start
    }
}

impl VectorComponent for TimedWrite {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        self.child.layout(constraints)
    }

    fn paint_bounds(&self, size: Vec2) -> Rect {
        let inner = self.child.render(size);
        let total = node_write_length(&inner.root, self.stroke_width);
        let speed = timed_stroke_speed(self.stroke_speed);
        let elapsed = self.elapsed();
        if elapsed <= 0.0 || total <= 0.0 || elapsed >= timed_done_at(total, speed, self) {
            return self.child.paint_bounds(size);
        }

        write_bounds_for_node(inner.view_box, &inner.root, Phase::HALF, self.stroke_width)
    }

    fn render(&self, size: Vec2) -> VectorGraphic {
        let inner = self.child.render(size);
        let total = node_write_length(&inner.root, self.stroke_width);
        let speed = timed_stroke_speed(self.stroke_speed);
        let elapsed = self.elapsed();

        if elapsed >= timed_done_at(total, speed, self) && total > 0.0 {
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
        let mut fill_state = TimedFillState {
            walked: 0.0,
            elapsed,
            stroke_speed: speed,
            fill_lead: self.fill_lead,
            fill_duration: self.fill_duration,
            fallback_stroke_width: self.stroke_width,
        };
        let fill = timed_fill_node(inner.root.clone(), &mut fill_state);
        if !node_is_empty(&fill) {
            children.push(fill);
        }

        let mut stroke_state = StrokeState {
            walked: 0.0,
            drawn: elapsed * speed,
            completed_stroke_opacity: self.completed_stroke_opacity,
            fallback_stroke_width: self.stroke_width,
        };
        let stroke = stroke_node(inner.root, &mut stroke_state);
        if !node_is_empty(&stroke) {
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

    fn write_from<T: Time>(self, time: T, start: f32) -> TimedWrite {
        TimedWrite::new(time, start, self)
    }

    fn write_from_with_speed<T: Time>(self, time: T, start: f32, stroke_speed: f32) -> TimedWrite {
        TimedWrite::new(time, start, self).stroke_speed(stroke_speed)
    }

    fn write_elapsed(self, elapsed: f32) -> TimedWrite {
        TimedWrite::from_elapsed(elapsed, self)
    }

    fn write_elapsed_with_speed(self, elapsed: f32, stroke_speed: f32) -> TimedWrite {
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

    fn write_from<T: Time>(self, time: T, start: f32) -> TimedWrite {
        TimedWrite::new(time, start, self.build_component())
    }

    fn write_from_with_speed<T: Time>(self, time: T, start: f32, stroke_speed: f32) -> TimedWrite {
        TimedWrite::new(time, start, self.build_component()).stroke_speed(stroke_speed)
    }

    fn write_elapsed(self, elapsed: f32) -> TimedWrite {
        TimedWrite::from_elapsed(elapsed, self.build_component())
    }

    fn write_elapsed_with_speed(self, elapsed: f32, stroke_speed: f32) -> TimedWrite {
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

fn node_write_length(node: &Node, fallback_stroke_width: f32) -> f32 {
    match node {
        Node::Group(group) => group
            .children
            .iter()
            .map(|child| node_write_length(child, fallback_stroke_width))
            .sum(),
        Node::SingleGroup(group) => node_write_length(&group.child, fallback_stroke_width),
        Node::ClipGroup(group) => node_write_length(&group.child, fallback_stroke_width),
        Node::Path(path) => {
            if write_stroke(path, fallback_stroke_width).is_some() {
                path_length(&path.commands)
            } else {
                0.0
            }
        }
    }
}

struct StrokeState {
    walked: f32,
    drawn: f32,
    completed_stroke_opacity: f32,
    fallback_stroke_width: f32,
}

fn stroke_node(node: Node, state: &mut StrokeState) -> Node {
    match node {
        Node::Group(group) => Node::Group(Group {
            transform: group.transform,
            opacity: group.opacity,
            children: group
                .children
                .into_iter()
                .map(|child| stroke_node(child, state))
                .collect(),
        }),
        Node::SingleGroup(group) => Node::SingleGroup(SingleGroup {
            transform: group.transform,
            opacity: group.opacity,
            child: Box::new(stroke_node(*group.child, state)),
        }),
        Node::ClipGroup(group) => Node::ClipGroup(ClipGroup {
            commands: group.commands,
            transform: group.transform,
            child: Box::new(stroke_node(*group.child, state)),
        }),
        Node::Path(path) => stroke_path_node(path, state),
    }
}

fn stroke_path_node(path: Path, state: &mut StrokeState) -> Node {
    let Some(stroke) = write_stroke(&path, state.fallback_stroke_width) else {
        return Node::empty();
    };
    let total = path_length(&path.commands);
    if total <= 0.0 {
        return Node::empty();
    }

    let start = state.walked;
    let end = state.walked + total;
    state.walked = end;

    if state.drawn <= start {
        return Node::empty();
    }

    let take = (state.drawn - start).min(total);
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

    let opacity = if temporary_stroke_from_fill(&path) && take >= total {
        clamp_unit(state.completed_stroke_opacity)
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

struct FillState {
    walked: f32,
    total: f32,
    progress: f32,
    stroke_end: f32,
    fill_delay: f32,
    fill_lead: f32,
    fill_duration: f32,
    fallback_stroke_width: f32,
}

fn fill_node(node: Node, state: &mut FillState) -> Node {
    match node {
        Node::Group(group) => Node::Group(Group {
            transform: group.transform,
            opacity: group.opacity,
            children: group
                .children
                .into_iter()
                .map(|child| fill_node(child, state))
                .collect(),
        }),
        Node::SingleGroup(group) => Node::SingleGroup(SingleGroup {
            transform: group.transform,
            opacity: group.opacity,
            child: Box::new(fill_node(*group.child, state)),
        }),
        Node::ClipGroup(group) => Node::ClipGroup(ClipGroup {
            commands: group.commands,
            transform: group.transform,
            child: Box::new(fill_node(*group.child, state)),
        }),
        Node::Path(path) => fill_path_node(path, state),
    }
}

fn fill_path_node(path: Path, state: &mut FillState) -> Node {
    let path_len = if write_stroke(&path, state.fallback_stroke_width).is_some() {
        path_length(&path.commands)
    } else {
        0.0
    };
    let completion = if state.total > 0.0 {
        ((state.walked + path_len) / state.total) * state.stroke_end
    } else {
        state.stroke_end
    };
    state.walked += path_len;

    let Some(fill) = path.fill.filter(|fill| fill.is_visible()) else {
        return Node::empty();
    };
    let alpha = fill_alpha(
        state.progress,
        completion,
        state.fill_lead,
        state.fill_delay,
        state.fill_duration,
    );
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

fn stroke_progress(progress: Phase, stroke_end: f32) -> f32 {
    if stroke_end >= 1.0 {
        progress.get()
    } else {
        clamp_unit(progress.get() / stroke_end)
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

struct TimedFillState {
    walked: f32,
    elapsed: f32,
    stroke_speed: f32,
    fill_lead: f32,
    fill_duration: f32,
    fallback_stroke_width: f32,
}

fn timed_fill_node(node: Node, state: &mut TimedFillState) -> Node {
    match node {
        Node::Group(group) => Node::Group(Group {
            transform: group.transform,
            opacity: group.opacity,
            children: group
                .children
                .into_iter()
                .map(|child| timed_fill_node(child, state))
                .collect(),
        }),
        Node::SingleGroup(group) => Node::SingleGroup(SingleGroup {
            transform: group.transform,
            opacity: group.opacity,
            child: Box::new(timed_fill_node(*group.child, state)),
        }),
        Node::ClipGroup(group) => Node::ClipGroup(ClipGroup {
            commands: group.commands,
            transform: group.transform,
            child: Box::new(timed_fill_node(*group.child, state)),
        }),
        Node::Path(path) => timed_fill_path_node(path, state),
    }
}

fn timed_fill_path_node(path: Path, state: &mut TimedFillState) -> Node {
    let path_len = if write_stroke(&path, state.fallback_stroke_width).is_some() {
        path_length(&path.commands)
    } else {
        0.0
    };
    let completion = (state.walked + path_len) / state.stroke_speed;
    state.walked += path_len;

    let Some(fill) = path.fill.filter(|fill| fill.is_visible()) else {
        return Node::empty();
    };
    let alpha = timed_fill_alpha(
        state.elapsed,
        completion,
        state.fill_lead,
        state.fill_duration,
    );
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

fn timed_fill_alpha(elapsed: f32, completion: f32, lead: f32, duration: f32) -> f32 {
    let start = (completion - lead.max(0.0)).max(0.0);
    let duration = duration.max(0.000_1);
    smoothstep(clamp_unit((elapsed - start) / duration))
}

fn timed_stroke_speed(stroke_speed: f32) -> f32 {
    stroke_speed.max(0.000_1)
}

fn timed_done_at(total: f32, stroke_speed: f32, write: &TimedWrite) -> f32 {
    let stroke_done = total / stroke_speed;
    let fill_done =
        (stroke_done - write.fill_lead.max(0.0)).max(0.0) + write.fill_duration.max(0.000_1);
    stroke_done.max(fill_done)
}

fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

fn node_is_empty(node: &Node) -> bool {
    match node {
        Node::Group(group) => group.opacity <= 0.0 || group.children.iter().all(node_is_empty),
        Node::SingleGroup(group) => group.opacity <= 0.0 || node_is_empty(&group.child),
        Node::ClipGroup(group) => node_is_empty(&group.child),
        Node::Path(path) => {
            !path.fill.as_ref().is_some_and(|fill| fill.is_visible())
                && !path
                    .stroke
                    .as_ref()
                    .is_some_and(|stroke| stroke.is_visible())
        }
    }
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

    #[derive(PartialEq, Hash)]
    struct TwoRects;

    impl VectorComponent for TwoRects {
        fn layout(&self, constraints: Constraints) -> Vec2 {
            constraints.constrain(Vec2(24.0, 10.0))
        }

        fn render(&self, size: Vec2) -> VectorGraphic {
            let path = |x: f32| {
                Node::Path(Path {
                    commands: vec![
                        PathCommand::MoveTo(Vec2(x, 0.0)),
                        PathCommand::LineTo(Vec2(x + 10.0, 0.0)),
                        PathCommand::LineTo(Vec2(x + 10.0, 10.0)),
                        PathCommand::LineTo(Vec2(x, 10.0)),
                        PathCommand::Close,
                    ],
                    fill: Some(Fill { paint: paint() }),
                    stroke: None,
                    transform: Transform::IDENTITY,
                })
            };
            VectorGraphic {
                view_box: Rect {
                    origin: Vec2::ZERO,
                    size,
                },
                root: Node::Group(Group {
                    transform: Transform::IDENTITY,
                    opacity: 1.0,
                    children: vec![path(0.0), path(14.0)],
                }),
            }
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
        assert!(node_is_empty(&fill.children[1]));

        let Node::Group(stroke) = &root.children[1] else {
            panic!("second child should be the stroke reveal layer");
        };
        assert!(matches!(stroke.children[0], Node::SingleGroup(_)));
        assert!(matches!(stroke.children[1], Node::ClipGroup(_)));
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
