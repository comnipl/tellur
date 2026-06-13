//! The overlay [`Timeline`] and the one-after-another [`Sequence`].

use crate::audio::AudioMix;
use crate::composite::{composite_frame_over, composite_frames_over};
use crate::geometry::Vec2;
use crate::raster::{RasterImage, Resolution};
use crate::render_context::RenderContext;
use crate::time::{LocalTime, Time};
use crate::timeline_component::{
    peel_source, Arrangement, AudioBuffer, Clock, Cue, NodeKind, Placed, ResolveCtx, SourceLoc,
    TimelineComponent,
};

// ── Containers ───────────────────────────────────────────────────────────────

/// Overlay container — the temporal twin of [`Layer`](crate::layer::Layer).
///
/// Children combine (visuals composite source-over, audio mixes, cues merge),
/// each placed by an absolute `.at(..)` (default `0.0`) or `.fill()`. There is
/// NO cursor: every child shares the same base start. The resolved length is
/// the latest NON-fill child end; `.fill()` children then take that length in a
/// second sub-pass (`.sketch/02 §5/§6`).
///
/// Struct-form `#[component(timeline)]`: the macro emits the buildless `bon`
/// builder, the `From<..> for Box<dyn TimelineComponent + Send>` glue, and the
/// `TimelineBuilder` marker — but NO trait impl. The behaviour is the
/// hand-written `impl TimelineComponent` below.
#[crate::component(timeline)]
#[derive(crate::Keyable)]
pub struct Timeline {
    // `#[builder(field)]` members (the streamed children) must precede the
    // setter members, per bon's member-ordering rule (mirrors raster `Flex`).
    #[children(each = child)]
    pub children: Vec<Box<dyn TimelineComponent + Send>>,
}

impl Timeline {
    /// The non-fill children, as `Placed` views when the child is a placement.
    /// A child that is not a `Placed` (e.g. a bare visual / a fn-form timeline
    /// component) is treated as a non-fill, start-0.0 child.
    fn classify(&self) -> impl Iterator<Item = ChildView<'_>> {
        self.children.iter().map(|child| {
            // Peel any wrapping `Sourced` (the call-site decorator the generated
            // `.child(...)` setter adds) so we see the real `Placed`/leaf, AND
            // capture its source so `arrangement` can re-stamp it — this path
            // builds the child node from the peeled component and would otherwise
            // bypass the wrapper's own stamping.
            let obj: &(dyn TimelineComponent + Send) = child.as_ref();
            let (source, inner) = peel_source(obj);
            match inner.as_any().downcast_ref::<Placed>() {
                Some(placed) if placed.is_fill() => ChildView::Fill(placed, source),
                Some(placed) => ChildView::PlacedAt(placed, source),
                None => ChildView::Bare(obj),
            }
        })
    }
}

/// How a [`Timeline`] sees one child during measure / place. The placement
/// variants carry the peeled call-site [`SourceLoc`] (if any) so `arrangement`,
/// which builds the node from the inner `Placed`, can re-stamp it. The `Bare`
/// variant keeps the ORIGINAL child (wrapper included) so its own `arrangement`
/// (through any `Sourced`) stamps the source.
enum ChildView<'a> {
    /// A `.fill()` placement — excluded from the length measure; resolved to the
    /// container length in the second sub-pass.
    Fill(&'a Placed, Option<SourceLoc>),
    /// A `.at(..)` placement — measured / placed at its relative start.
    PlacedAt(&'a Placed, Option<SourceLoc>),
    /// A non-placement child (a bare visual etc.) — start 0.0, its own measure.
    Bare(&'a (dyn TimelineComponent + Send)),
}

impl TimelineComponent for Timeline {
    fn measure(&self) -> Option<f32> {
        // max over the NON-fill children of each child's measure footprint
        // (relative start folded in by `Placed::measure`). Fill children are
        // EXCLUDED (the load-bearing acyclicity invariant, `.sketch/02 §5`). If
        // no non-fill child has a length, the container is timeless (`None`).
        let mut acc: Option<f32> = None;
        for view in self.classify() {
            let m = match view {
                ChildView::Fill(..) => continue,
                ChildView::PlacedAt(placed, _) => placed.measure(),
                ChildView::Bare(child) => child.measure(),
            };
            if let Some(end) = m {
                acc = Some(acc.map_or(end, |cur: f32| cur.max(end)));
            }
        }
        acc
    }

    fn resolve(&self, abs_start: f32, out: &mut ResolveCtx) -> f32 {
        // Sub-pass 1 — non-fill children: place each at its relative start and
        // fold the max end into the container length.
        let mut length = 0.0_f32;
        let mut saw_non_fill = false;
        let mut saw_fill = false;
        for view in self.classify() {
            match view {
                ChildView::Fill(..) => {
                    saw_fill = true;
                }
                ChildView::PlacedAt(placed, _) => {
                    saw_non_fill = true;
                    // `Placed::resolve` recurses at the relative start and
                    // returns the placed length; its footprint end is
                    // `relative_start + placed_len`.
                    let len = placed.resolve(abs_start, out);
                    length = length.max(placed.placement().start() + len);
                }
                ChildView::Bare(child) => {
                    saw_non_fill = true;
                    let len = child.resolve(abs_start, out);
                    length = length.max(len);
                }
            }
        }

        // An all-fill / empty interior collapses to a determinate `0.0` — almost
        // always an authoring mistake, so warn (`.sketch/02 §9`).
        if saw_fill && !saw_non_fill {
            out.warn(
                "Timeline has no non-fill child to set its length; \
                 it collapses to 0.0 (place media or an explicit window)",
            );
        }

        // Sub-pass 2 — fill children take the resolved container length. Recurse
        // into the INNER component (the fill `Placed` is a start-0.0 wrapper) so
        // its triggers / cues compose at the container base.
        for view in self.classify() {
            if let ChildView::Fill(placed, _) = view {
                placed.child().resolve(abs_start, out);
            }
        }

        length
    }

    fn cues(&self, offset: f32) -> Vec<Cue> {
        // Concat children cues at `offset + child relative start`. `Placed::cues`
        // already adds its own relative start and stamps a windowed timeless
        // child's interval. A FILL child has no window, so its interval is the
        // container's resolved length — stamp that here (`.sketch/02 §10`).
        let fill_len = self.measure().unwrap_or(0.0);
        let mut cues = Vec::new();
        for view in self.classify() {
            match view {
                ChildView::Fill(placed, _) => {
                    let mut child_cues = placed.child().cues(offset);
                    if placed.child().duration().is_none() {
                        let abs_end = offset + fill_len;
                        for cue in &mut child_cues {
                            cue.end = abs_end;
                        }
                    }
                    cues.extend(child_cues);
                }
                ChildView::PlacedAt(placed, _) => cues.extend(placed.cues(offset)),
                ChildView::Bare(child) => cues.extend(child.cues(offset)),
            }
        }
        cues
    }

    fn frame(
        &self,
        clock: Clock<'_>,
        canvas: Vec2,
        target: Resolution,
        ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage> {
        // Overlay: every child shares the container's base, so each sees the
        // SAME clock (relative offset 0). A `.at(start..)` child is a `Placed`
        // whose own `frame` rebases by its relative start; a `.fill()` child
        // spans the whole container (relative start 0, local unchanged); a bare
        // child is at 0 too. Recurse each child and source-over composite the
        // resulting frames at the IMAGE layer (`.sketch/02 §8`), in child order
        // so later children land on top. `None` frames are dropped.
        //
        // Hard-cut the overlay at its resolved length: nothing contributes
        // outside the container interval `[0, length)` (half-open, matching the
        // `Placed` / `Sequence` gate). This caps a `.fill()` or bare child — and
        // any nested timeline — at the container length recursively, with no
        // per-leaf gating: a clip whose own placement does not gate (a fill has
        // no window of its own) still cannot render past the container that sets
        // its span. An all-fill / timeless overlay (`measure()` is `None`) has no
        // determinate length to gate against and is left open (already a
        // resolve-time warning).
        let local_t = clock.local().seconds();
        if let Some(length) = self.measure() {
            if local_t < 0.0 || local_t >= length {
                return None;
            }
        }
        let mut frames = Vec::new();
        for child in &self.children {
            if let Some(img) = child.frame(clock, canvas, target, ctx) {
                frames.push(img);
            }
        }
        composite_frames_over(frames, target, ctx)
    }

    fn samples(&self, clock: Clock<'_>, window: f32) -> Option<AudioBuffer> {
        // The eager mix-down uses `mix_into`; this per-window seam is unused.
        let _ = (clock, window);
        None
    }

    fn mix_into(&self, mix: &mut AudioMix, start_secs: f32, speed: f32) {
        // Overlay: every child shares the container's base start, so each is
        // mixed at the SAME `start_secs` (a `.at(start..)` child is a `Placed`
        // that adds its own relative start; a `.fill()` child spans from 0).
        // Summing all children into one mix IS the audio overlay.
        for child in &self.children {
            child.mix_into(mix, start_secs, speed);
        }
    }

    fn arrangement(&self, offset: f32) -> Arrangement {
        // Overlay: every child shares the container base `offset`. The resolved
        // length is the measured length (fill children excluded); a `.fill()`
        // child spans `[offset, offset + length]`. Mirror `cues`'s classify walk
        // so each child's node carries its resolved absolute interval.
        let length = self.measure().unwrap_or(0.0);
        let mut children = Vec::with_capacity(self.children.len());
        for view in self.classify() {
            match view {
                ChildView::Fill(placed, source) => {
                    // A fill child takes the container length; build the INNER
                    // component at the base and stamp the span end onto a
                    // timeless inner node (same rule `cues` applies to fill cues).
                    let mut node = placed.child().arrangement(offset);
                    if placed.child().duration().is_none() {
                        node.end = offset + length;
                    }
                    // Re-stamp the call site the peeled-away `Sourced` carried.
                    if node.source.is_none() {
                        node.source = source;
                    }
                    children.push(node);
                }
                ChildView::PlacedAt(placed, source) => {
                    let mut node = placed.arrangement(offset);
                    if node.source.is_none() {
                        node.source = source;
                    }
                    children.push(node);
                }
                ChildView::Bare(child) => children.push(child.arrangement(offset)),
            }
        }
        Arrangement {
            kind: NodeKind::Timeline,
            label: String::new(),
            name: None,
            source: None,
            start: offset,
            end: offset + length,
            trim: None,
            triggers: Vec::new(),
            children,
        }
    }
}

/// In-a-row container — the temporal twin of [`Flex`](crate::layout::raster::Flex).
///
/// Lays children one after another: child N starts at child N-1's resolved end.
/// RE-FLOW falls out for free — the cursor is recomputed from the children's
/// current lengths every resolve, so a length change shifts everything after it
/// (`.sketch/02 §6`). Mirrors `compute_flex_pass`'s `Start` branch.
///
/// A `.fill()` child here is a RESOLVE error (ZONE C #1): a `Sequence` imposes
/// no container length for the fill to take, the same reason the spatial `Flex`
/// has no main-axis `Fill`. Span the row from the parent overlay `Timeline`
/// instead. `.fill()` is a runtime `Placed` value, so this is caught at resolve
/// time (via [`ResolveCtx::error`]), not by the type system.
#[crate::component(timeline)]
#[derive(crate::Keyable)]
pub struct Sequence {
    #[children(each = child)]
    pub children: Vec<Box<dyn TimelineComponent + Send>>,
    /// Gap inserted between consecutive children (seconds). `0.0` by default.
    #[builder(default)]
    pub spacing: f32,
}

impl Sequence {
    /// Whether `child` is a `.fill()` placement (rejected inside a `Sequence`).
    /// Peels any wrapping `Sourced` (the call-site decorator) before downcasting
    /// so the structural check sees the real `Placed`.
    fn is_fill(child: &(dyn TimelineComponent + Send)) -> bool {
        child
            .structural_any()
            .downcast_ref::<Placed>()
            .is_some_and(Placed::is_fill)
    }
}

impl TimelineComponent for Sequence {
    fn measure(&self) -> Option<f32> {
        // Σ over the NON-fill children of each child's length, plus spacing
        // between them. A fill child contributes nothing (it is an error at
        // place time). All-`None` children ⇒ the sequence is timeless.
        let mut total = 0.0_f32;
        let mut any = false;
        let mut count = 0usize;
        for child in &self.children {
            if Self::is_fill(child.as_ref()) {
                continue;
            }
            count += 1;
            if let Some(len) = child.measure() {
                total += len;
                any = true;
            }
        }
        if !any {
            return None;
        }
        let gaps = count.saturating_sub(1) as f32;
        Some(total + self.spacing * gaps)
    }

    fn resolve(&self, abs_start: f32, out: &mut ResolveCtx) -> f32 {
        // Cursor from 0: child N starts at `abs_start + Σ prior lengths (+gaps)`,
        // the time version of `compute_stack_pass`'s Start branch. A `.fill()`
        // child is a fatal error — record it and skip its length contribution.
        let mut cursor = 0.0_f32;
        let mut placed_any = false;
        for child in &self.children {
            if Self::is_fill(child.as_ref()) {
                out.error(
                    ".fill() is not allowed inside a Sequence (it has no container \
                     length to take); use .at(..) here, or .fill() in a parent Timeline",
                );
                continue;
            }
            if placed_any {
                cursor += self.spacing;
            }
            // The cursor is in THIS sequence's local seconds; scale it to absolute
            // by the enclosing window stretch (1.0 unless the whole Sequence sits
            // inside a stretched `.at(a..b)`), mirroring `Placed::resolve`.
            let len = child.resolve(abs_start + cursor * out.local_scale(), out);
            cursor += len;
            placed_any = true;
        }
        cursor
    }

    fn cues(&self, offset: f32) -> Vec<Cue> {
        // Re-flow the cursor the same way `resolve` does so cues land at the
        // child's absolute start. Fill children are skipped (an error).
        let mut cues = Vec::new();
        let mut cursor = 0.0_f32;
        let mut placed_any = false;
        for child in &self.children {
            if Self::is_fill(child.as_ref()) {
                continue;
            }
            if placed_any {
                cursor += self.spacing;
            }
            cues.extend(child.cues(offset + cursor));
            cursor += child.measure().unwrap_or(0.0);
            placed_any = true;
        }
        cues
    }

    fn frame(
        &self,
        clock: Clock<'_>,
        canvas: Vec2,
        target: Resolution,
        ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage> {
        // Re-flow the cursor exactly as `resolve` / `cues` do, then hand the
        // ACTIVE child (whose slot `[cursor, cursor + slot)` contains the current
        // local time) a clock rebased to its slot — `local = clock.local() -
        // cursorN` — carrying the slot length as its window. Half-open slots mean
        // exactly one child draws at a seam (no double-draw); a child outside its
        // slot contributes nothing (`.sketch/02 §8`).
        let mut cursor = 0.0_f32;
        let mut placed_any = false;
        let mut acc: Option<RasterImage> = None;
        let local_t = clock.local().seconds();
        for child in &self.children {
            if Self::is_fill(child.as_ref()) {
                // `.fill()` inside a Sequence is a resolve error; a valid sampled
                // tree never reaches here, but skip it defensively so it neither
                // shifts the cursor nor draws.
                continue;
            }
            if placed_any {
                cursor += self.spacing;
            }
            let slot = child.measure();
            let active = match slot {
                Some(len) => local_t >= cursor && local_t < cursor + len,
                None => true,
            };
            if active {
                let child_clock = clock.with_local_window(LocalTime::new(local_t - cursor), slot);
                if let Some(img) = child.frame(child_clock, canvas, target, ctx) {
                    acc = Some(composite_frame_over(acc, img, target, ctx));
                }
            }
            cursor += slot.unwrap_or(0.0);
            placed_any = true;
        }
        acc
    }

    fn samples(&self, clock: Clock<'_>, window: f32) -> Option<AudioBuffer> {
        // The eager mix-down uses `mix_into`; this per-window seam is unused.
        let _ = (clock, window);
        None
    }

    fn mix_into(&self, mix: &mut AudioMix, start_secs: f32, speed: f32) {
        // Re-flow the cursor exactly as `resolve` / `frame` / `cues` do: child N
        // is mixed at `start_secs + Σ prior lengths (+ gaps)`. A `.fill()` child
        // is a resolve error (skipped here defensively, matching `frame`).
        let mut cursor = 0.0_f32;
        let mut placed_any = false;
        for child in &self.children {
            if Self::is_fill(child.as_ref()) {
                continue;
            }
            if placed_any {
                cursor += self.spacing;
            }
            child.mix_into(mix, start_secs + cursor, speed);
            cursor += child.measure().unwrap_or(0.0);
            placed_any = true;
        }
    }

    fn arrangement(&self, offset: f32) -> Arrangement {
        // Re-flow the cursor exactly as `resolve` / `cues` do so child N's node
        // lands at its absolute start `offset + Σ prior lengths (+ gaps)`. Fill
        // children are skipped (an error inside a `Sequence`).
        let mut children = Vec::with_capacity(self.children.len());
        let mut cursor = 0.0_f32;
        let mut placed_any = false;
        for child in &self.children {
            if Self::is_fill(child.as_ref()) {
                continue;
            }
            if placed_any {
                cursor += self.spacing;
            }
            children.push(child.arrangement(offset + cursor));
            cursor += child.measure().unwrap_or(0.0);
            placed_any = true;
        }
        Arrangement {
            kind: NodeKind::Sequence,
            label: String::new(),
            name: None,
            source: None,
            start: offset,
            end: offset + cursor,
            trim: None,
            triggers: Vec::new(),
            children,
        }
    }
}
