//! Ordered, component-generic timeline trimming.
//!
//! [`Trim`] is a real wrapper rather than media-leaf metadata: it shortens the
//! immediate child's local interval and rebases every channel so output-local
//! time zero samples the resolved trim start. Consequently wrapper order is
//! observable (`child.effect().trim(..)` and `child.trim(..).effect()` are
//! different trees), while the implementation remains direct component
//! recursion.

use std::hash::{Hash, Hasher};
use std::ops::{Bound, Range, RangeBounds, RangeFrom, RangeFull, RangeTo};

use crate::geometry::Vec2;
use crate::raster::{RasterImage, RasterResidency, Resolution};
use crate::render_context::RenderContext;
use crate::time::{LocalTime, Time};

use super::{
    Arrangement, AudioBlockMut, AudioRenderContext, Clock, Cue, ResolveCtx, TimelineComponent,
};

mod private {
    pub trait Sealed {}
}

/// A supported half-open trim range.
///
/// Only the standard `a..b`, `a..`, `..b`, and `..` forms are accepted. In
/// particular, inclusive ranges are deliberately not supported: every Tellur
/// timeline interval is half-open. An omitted start means `0`; an omitted end
/// means the immediate child's end. A finite negative endpoint is relative to
/// that end (`-1.0` means one second before it).
pub trait TrimBounds: RangeBounds<f64> + private::Sealed {}

impl private::Sealed for Range<f64> {}
impl TrimBounds for Range<f64> {}

impl private::Sealed for RangeFrom<f64> {}
impl TrimBounds for RangeFrom<f64> {}

impl private::Sealed for RangeTo<f64> {}
impl TrimBounds for RangeTo<f64> {}

impl private::Sealed for RangeFull {}
impl TrimBounds for RangeFull {}

/// Authored trim endpoints before end-relative values are resolved.
///
/// `None` is the corresponding open bound. Floating-point identity follows
/// the rest of Tellur's key types and is bit-exact, including signed zero.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TrimSpec {
    start: Option<f64>,
    end: Option<f64>,
}

impl TrimSpec {
    fn from_bounds<R: TrimBounds>(bounds: &R) -> Self {
        let start = match bounds.start_bound() {
            Bound::Unbounded => None,
            Bound::Included(value) => Some(*value),
            Bound::Excluded(_) => {
                unreachable!("sealed TrimBounds never has an excluded start")
            }
        };
        let end = match bounds.end_bound() {
            Bound::Unbounded => None,
            Bound::Excluded(value) => Some(*value),
            Bound::Included(_) => {
                unreachable!("sealed TrimBounds never has an included end")
            }
        };
        Self { start, end }
    }

    fn resolve(self, child_end: f64) -> Result<Range<f64>, String> {
        if !child_end.is_finite() || child_end < 0.0 {
            return Err(format!(
                "the immediate child must have a finite non-negative duration, got {child_end}"
            ));
        }

        fn endpoint(value: Option<f64>, open: f64, child_end: f64) -> Result<f64, String> {
            let Some(value) = value else {
                return Ok(open);
            };
            if !value.is_finite() {
                return Err(format!("trim endpoints must be finite, got {value}"));
            }
            Ok(if value < 0.0 {
                child_end + value
            } else {
                value
            })
        }

        let start = endpoint(self.start, 0.0, child_end)?;
        let end = endpoint(self.end, child_end, child_end)?;
        if start < 0.0 || start > child_end {
            return Err(format!(
                "trim start resolves outside the child: {start} not in 0..={child_end}"
            ));
        }
        if end < 0.0 || end > child_end {
            return Err(format!(
                "trim end resolves outside the child: {end} not in 0..={child_end}"
            ));
        }
        if start > end {
            return Err(format!(
                "trim range is reversed after resolution: {start}..{end}"
            ));
        }
        Ok(start..end)
    }
}

impl PartialEq for TrimSpec {
    fn eq(&self, other: &Self) -> bool {
        fn bits(value: Option<f64>) -> Option<u64> {
            value.map(f64::to_bits)
        }
        bits(self.start) == bits(other.start) && bits(self.end) == bits(other.end)
    }
}

impl Eq for TrimSpec {}

impl Hash for TrimSpec {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.start.map(f64::to_bits).hash(state);
        self.end.map(f64::to_bits).hash(state);
    }
}

/// A component whose output is the selected interval of its immediate child.
///
/// The wrapper's local interval always starts at zero. For example, trimming a
/// ten-second child with `2.0..5.0` gives a three-second component, and its
/// local `0.0` samples the child at local `2.0`.
#[derive(Debug, Clone)]
pub struct Trim<C> {
    child: C,
    spec: TrimSpec,
}

impl<C: TimelineComponent> Trim<C> {
    /// Wraps `child` with a validated half-open trim range.
    ///
    /// Invalid, non-finite, reversed, or out-of-child ranges are authoring
    /// errors and panic immediately, matching the validation style of the
    /// existing time/window combinators.
    pub fn new<R: TrimBounds>(child: C, bounds: R) -> Self {
        let trimmed = Self {
            child,
            spec: TrimSpec::from_bounds(&bounds),
        };
        // Validate at the authoring boundary so a bad end-relative range cannot
        // later degrade into the resolve pass's generic "timeless root" error.
        let _ = trimmed.resolved_range();
        trimmed
    }

    /// The immediate wrapped child.
    pub fn child(&self) -> &C {
        &self.child
    }

    /// Resolves open and negative endpoints against the immediate child.
    pub(crate) fn resolved_range(&self) -> Range<f64> {
        let child_end = self.child.measure().unwrap_or_else(|| {
            panic!("invalid trim range: the immediate child has no resolved duration")
        });
        self.spec
            .resolve(child_end)
            .unwrap_or_else(|message| panic!("invalid trim range: {message}"))
    }
}

impl<C: PartialEq> PartialEq for Trim<C> {
    fn eq(&self, other: &Self) -> bool {
        self.child == other.child && self.spec == other.spec
    }
}

impl<C: Eq> Eq for Trim<C> {}

impl<C: Hash> Hash for Trim<C> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.child.hash(state);
        self.spec.hash(state);
    }
}

impl<C> TimelineComponent for Trim<C>
where
    C: TimelineComponent + Clone + PartialEq + Hash + 'static,
{
    fn duration(&self) -> Option<f64> {
        let range = self.resolved_range();
        Some(range.end - range.start)
    }

    fn measure(&self) -> Option<f64> {
        self.duration()
    }

    fn resolve(&self, abs_start: f64, out: &mut ResolveCtx) -> f64 {
        let range = self.resolved_range();
        let duration = range.end - range.start;
        let scale = out.local_scale();
        let child_abs_start = abs_start - range.start * scale;
        let output_abs_range = abs_start..abs_start + duration * scale;
        out.resolve_trimmed(&self.child, child_abs_start, output_abs_range);
        duration
    }

    fn frame(
        &self,
        clock: Clock<'_>,
        canvas: Vec2,
        target: Resolution,
        residency: RasterResidency,
        ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage> {
        let range = self.resolved_range();
        let duration = range.end - range.start;
        let local = clock.local().seconds();
        if local < 0.0 || local >= duration {
            return None;
        }

        let child_clock =
            clock.with_local_window(LocalTime::new(range.start + local), self.child.measure());
        self.child
            .frame(child_clock, canvas, target, residency, ctx)
    }

    fn render_audio_block(&self, mut block: AudioBlockMut<'_>, ctx: &mut AudioRenderContext) {
        let range = self.resolved_range();
        let duration = range.end - range.start;
        let request = block.request();
        if !request.may_overlap_local(0.0, duration) {
            block.clear();
            return;
        }
        let child_request = request.shift_local(range.start);
        self.child
            .render_audio_block(AudioBlockMut::new(child_request, block.samples_mut()), ctx);

        let channels = request.channels() as usize;
        for frame in 0..request.frame_count() {
            let local = request.time_at(frame);
            if local < 0.0 || local >= duration {
                block.samples_mut()[frame * channels..(frame + 1) * channels].fill(0.0);
            }
        }
    }

    fn cues(&self, offset: f64) -> Vec<Cue> {
        let range = self.resolved_range();
        let output = offset..offset + (range.end - range.start);
        if output.start == output.end {
            return Vec::new();
        }

        self.child
            .cues(offset - range.start)
            .into_iter()
            .filter_map(|mut cue| {
                if cue.end <= output.start || cue.start >= output.end {
                    return None;
                }
                cue.start = cue.start.max(output.start);
                cue.end = cue.end.min(output.end);
                Some(cue)
            })
            .collect()
    }

    fn arrangement(&self, offset: f64) -> Arrangement {
        let range = self.resolved_range();
        let output = offset..offset + (range.end - range.start);
        let mut node = self.child.arrangement(offset - range.start);
        clip_arrangement(&mut node, &output);
        // The returned node represents the wrapper itself even when the child
        // had an unusual or empty structural interval.
        node.start = output.start;
        node.end = output.end;
        node.trim = Some((range.start, range.end));
        node
    }
}

impl<C> From<Trim<C>> for Box<dyn TimelineComponent + Send>
where
    C: TimelineComponent + Clone + PartialEq + Hash + Send + 'static,
{
    fn from(trimmed: Trim<C>) -> Self {
        Box::new(trimmed)
    }
}

fn clip_arrangement(node: &mut Arrangement, output: &Range<f64>) {
    node.start = node.start.max(output.start).min(output.end);
    node.end = node.end.max(output.start).min(output.end);
    if node.end < node.start {
        node.end = node.start;
    }
    node.triggers
        .retain(|trigger| trigger.time >= output.start && trigger.time <= output.end);
    node.children.retain_mut(|child| {
        let overlaps = child.end > output.start && child.start < output.end;
        if overlaps {
            clip_arrangement(child, output);
        }
        overlaps
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline_component::{resolve, Event, NodeKind, TriggerMark, Triggers};

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    struct TenSeconds;

    impl TimelineComponent for TenSeconds {
        fn duration(&self) -> Option<f64> {
            Some(10.0)
        }

        fn cues(&self, offset: f64) -> Vec<Cue> {
            vec![
                Cue {
                    start: offset + 1.0,
                    end: offset + 4.0,
                    text: "left".into(),
                },
                Cue {
                    start: offset + 8.0,
                    end: offset + 10.0,
                    text: "right".into(),
                },
            ]
        }

        fn arrangement(&self, offset: f64) -> Arrangement {
            Arrangement {
                kind: NodeKind::Audio,
                label: "ten seconds".into(),
                name: None,
                source: None,
                start: offset,
                end: offset + 10.0,
                trim: None,
                triggers: vec![TriggerMark {
                    time: offset + 1.0,
                    name: Some("trimmed away".into()),
                }],
                children: vec![Arrangement {
                    kind: NodeKind::Subtitle,
                    label: "tail".into(),
                    name: None,
                    source: None,
                    start: offset + 8.0,
                    end: offset + 10.0,
                    trim: None,
                    triggers: Vec::new(),
                    children: Vec::new(),
                }],
            }
        }
    }

    #[test]
    fn resolves_open_and_end_relative_bounds() {
        assert_eq!(Trim::new(TenSeconds, ..).resolved_range(), 0.0..10.0);
        assert_eq!(Trim::new(TenSeconds, 2.0..).resolved_range(), 2.0..10.0);
        assert_eq!(Trim::new(TenSeconds, ..-1.0).resolved_range(), 0.0..9.0);
        assert_eq!(Trim::new(TenSeconds, -3.0..-0.5).resolved_range(), 7.0..9.5);
    }

    #[test]
    fn empty_half_open_range_is_valid() {
        let trimmed = Trim::new(TenSeconds, 4.0..4.0);
        assert_eq!(trimmed.duration(), Some(0.0));
    }

    #[test]
    #[should_panic(expected = "trim endpoints must be finite")]
    fn rejects_non_finite_endpoint() {
        let _ = Trim::new(TenSeconds, f64::NAN..);
    }

    #[test]
    #[should_panic(expected = "resolves outside the child")]
    fn rejects_out_of_bounds_endpoint() {
        let _ = Trim::new(TenSeconds, ..11.0);
    }

    #[test]
    #[should_panic(expected = "trim range is reversed")]
    fn rejects_reversed_range() {
        let _ = Trim::new(TenSeconds, 8.0..2.0);
    }

    #[test]
    fn cues_are_rebased_and_clipped() {
        let cues = Trim::new(TenSeconds, 2.0..9.0).cues(100.0);
        assert_eq!(cues.len(), 2);
        assert_eq!((cues[0].start, cues[0].end), (100.0, 102.0));
        assert_eq!((cues[1].start, cues[1].end), (106.0, 107.0));
    }

    #[test]
    fn arrangement_is_rebased_clipped_and_marked() {
        let node = Trim::new(TenSeconds, 2.0..9.0).arrangement(100.0);
        assert_eq!((node.start, node.end), (100.0, 107.0));
        assert_eq!(node.trim, Some((2.0, 9.0)));
        assert!(node.triggers.is_empty());
        assert_eq!(node.children.len(), 1);
        assert_eq!(
            (node.children[0].start, node.children[0].end),
            (106.0, 107.0)
        );
    }

    #[test]
    fn resolve_rebases_surviving_triggers_and_discards_trimmed_ones() {
        let before = Event::new();
        let inside = Event::new();
        let at_boundary = Event::new();
        let child = TenSeconds
            .trigger_at(1.0, before)
            .trigger_at(3.0, inside)
            .trigger_at(5.0, at_boundary);
        let resolved = resolve(Trim::new(child, 2.0..5.0)).expect("finite trim");

        assert!(!resolved.triggers().contains(before.id()));
        assert_eq!(resolved.triggers().get(inside.id()).seconds(), 1.0);
        assert_eq!(resolved.triggers().get(at_boundary.id()).seconds(), 3.0);
    }

    #[test]
    fn no_op_trim_preserves_the_exact_end_trigger() {
        let end = Event::new();
        let resolved =
            resolve(Trim::new(TenSeconds.trigger_at_end(end), ..)).expect("finite no-op trim");

        assert_eq!(resolved.triggers().get(end.id()).seconds(), 10.0);
    }
}
