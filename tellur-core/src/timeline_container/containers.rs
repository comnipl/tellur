//! The overlay [`Timeline`] and the one-after-another [`Sequence`].

use crate::composite::composite_frames_over;
use crate::geometry::Vec2;
use crate::raster::{RasterImage, RasterResidency, Resolution};
use crate::render_context::RenderContext;
use crate::time::{LocalTime, Time};
use crate::timeline_component::{
    peel_source, Arrangement, AudioBlockMut, AudioRenderContext, Clock, Cue, NodeKind, Placed,
    ResolveCtx, SourceLoc, TimelineComponent,
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
#[derive(Clone, crate::Keyable)]
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

impl ChildView<'_> {
    /// Whether this child can contribute at `t` according to the placement
    /// gate. This is intentionally conservative for fill, bare, and custom
    /// components: only a placement window can prove that a child is inactive
    /// without rendering it.
    fn may_contribute_at(&self, t: f64) -> bool {
        match self {
            Self::Fill(..) | Self::Bare(..) => true,
            Self::PlacedAt(placed, _) => {
                let placement = placed.placement();
                match placement.end() {
                    Some(end) => t >= placement.start() && t < end,
                    None => match placed.child().measure() {
                        Some(duration) => {
                            t >= placement.start() && t < placement.start() + duration
                        }
                        None => true,
                    },
                }
            }
        }
    }
}

impl TimelineComponent for Timeline {
    fn measure(&self) -> Option<f64> {
        // max over the NON-fill children of each child's measure footprint
        // (relative start folded in by `Placed::measure`). Fill children are
        // EXCLUDED (the load-bearing acyclicity invariant, `.sketch/02 §5`). If
        // no non-fill child has a length, the container is timeless (`None`).
        let mut acc: Option<f64> = None;
        for view in self.classify() {
            let m = match view {
                ChildView::Fill(..) => continue,
                ChildView::PlacedAt(placed, _) => placed.measure(),
                ChildView::Bare(child) => child.measure(),
            };
            if let Some(end) = m {
                acc = Some(acc.map_or(end, |cur: f64| cur.max(end)));
            }
        }
        acc
    }

    fn resolve(&self, abs_start: f64, out: &mut ResolveCtx) -> f64 {
        // Sub-pass 1 — non-fill children: place each at its relative start and
        // fold the max end into the container length.
        let mut length = 0.0_f64;
        let mut saw_non_fill = false;
        let mut saw_fill = false;
        for view in self.classify() {
            match view {
                ChildView::Fill(..) => {
                    saw_fill = true;
                }
                ChildView::PlacedAt(placed, _) => {
                    saw_non_fill = true;
                    let resolved = placed.resolve(abs_start, out);
                    // `Placed::resolve` deliberately returns its playable
                    // interval (the contract used by an outer Triggered), while
                    // `measure()` carries the leading-offset footprint a
                    // container must fold into its extent.
                    length = length.max(placed.measure().unwrap_or(resolved));
                }
                ChildView::Bare(child) => {
                    saw_non_fill = true;
                    let resolved = child.resolve(abs_start, out);
                    // An outer transparent/effect wrapper may delegate a Placed
                    // resolve return that excludes its leading offset. Its
                    // delegated measure remains the authoritative footprint.
                    length = length.max(child.measure().unwrap_or(resolved));
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

        // Sub-pass 2 — fill children take the resolved container length. Keep
        // the Placed wrapper in the recursive path: for a timed child it folds
        // `content_duration / container_length` into the resolve clock so
        // interior/end triggers land on their stretched global times; for a
        // timeless child it remains the identity used before this stretch fix.
        for view in self.classify() {
            if let ChildView::Fill(placed, _) = view {
                placed.resolve_fill(abs_start, length, out);
            }
        }

        length
    }

    fn cues(&self, offset: f64) -> Vec<Cue> {
        // Concat children cues at `offset + child relative start`. `Placed::cues`
        // already adds its own relative start and stamps a windowed timeless
        // child's interval. A FILL child has no window, so its interval is the
        // container's resolved length — stamp that here (`.sketch/02 §10`).
        let fill_len = self.measure().unwrap_or(0.0);
        let mut cues = Vec::new();
        for view in self.classify() {
            match view {
                ChildView::Fill(placed, _) => {
                    cues.extend(placed.cues_fill(offset, fill_len));
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
        residency: RasterResidency,
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
        let length = clock
            .window()
            .map(|window| window.width())
            .or_else(|| self.measure());
        if let Some(length) = length {
            if local_t < 0.0 || local_t >= length {
                return None;
            }
        }
        // An overlay with only one child that can contribute at this instant
        // needs no composite operation, so preserve the consumer's requested
        // representation and avoid a pointless upload/readback round trip.
        // Multiple possible contributors use GPU-resident inputs when the GPU
        // composite path is available, independently of final residency.
        let possible_frames = self
            .classify()
            .filter(|view| view.may_contribute_at(local_t))
            .take(2)
            .count();
        let gpu_path = possible_frames > 1 && ctx.prefers_gpu() && ctx.gpu_backend().is_some();
        let child_residency = if gpu_path {
            RasterResidency::Gpu
        } else {
            residency
        };
        let mut frames = Vec::new();
        for view in self.classify() {
            let image = match view {
                ChildView::Fill(placed, _) => match length {
                    Some(fill_length) => {
                        placed.frame_fill(clock, fill_length, canvas, target, child_residency, ctx)
                    }
                    None => placed.frame(clock, canvas, target, child_residency, ctx),
                },
                ChildView::PlacedAt(placed, _) => {
                    placed.frame(clock, canvas, target, child_residency, ctx)
                }
                ChildView::Bare(child) => child.frame(clock, canvas, target, child_residency, ctx),
            };
            if let Some(img) = image {
                frames.push(img);
            }
        }
        composite_frames_over(frames, target, residency, ctx)
    }

    fn render_audio_block(&self, mut block: AudioBlockMut<'_>, ctx: &mut AudioRenderContext) {
        let request = block.request();
        let length = self.measure();
        if length.is_some_and(|length| !request.may_overlap_local(0.0, length)) {
            block.clear();
            return;
        }
        block.clear();
        let mut scratch = ctx.take_scratch(request.sample_len());
        for view in self.classify() {
            match view {
                ChildView::Fill(placed, _) => match length {
                    Some(fill_length) => placed.render_audio_fill(
                        AudioBlockMut::new(request, &mut scratch),
                        fill_length,
                        ctx,
                    ),
                    None => {
                        placed.render_audio_block(AudioBlockMut::new(request, &mut scratch), ctx)
                    }
                },
                ChildView::PlacedAt(placed, _) => {
                    placed.render_audio_block(AudioBlockMut::new(request, &mut scratch), ctx)
                }
                ChildView::Bare(child) => {
                    child.render_audio_block(AudioBlockMut::new(request, &mut scratch), ctx)
                }
            }
            for (output, contribution) in block.samples_mut().iter_mut().zip(&scratch) {
                *output += contribution;
            }
        }

        // A fill or otherwise timeless child is capped by the overlay's own
        // resolved interval, just like the video path.
        if let Some(length) = length {
            let channels = request.channels() as usize;
            for frame in 0..request.frame_count() {
                let t = request.time_at(frame);
                if t < 0.0 || t >= length {
                    block.samples_mut()[frame * channels..(frame + 1) * channels].fill(0.0);
                }
            }
        }
        ctx.recycle_scratch(scratch);
    }

    fn arrangement(&self, offset: f64) -> Arrangement {
        // Overlay: every child shares the container base `offset`. The resolved
        // length is the measured length (fill children excluded); a `.fill()`
        // child spans `[offset, offset + length]`. Mirror `cues`'s classify walk
        // so each child's node carries its resolved absolute interval.
        let length = self.measure().unwrap_or(0.0);
        let mut children = Vec::with_capacity(self.children.len());
        for view in self.classify() {
            match view {
                ChildView::Fill(placed, source) => {
                    // Keep the Placed remap so a timed node, all descendants,
                    // and its trigger markers stretch to the container length.
                    // Timeless nodes retain the old end-stamping behaviour.
                    let mut node = placed.arrangement_fill(offset, length);
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
#[derive(Clone, crate::Keyable)]
pub struct Sequence {
    #[children(each = child)]
    pub children: Vec<Box<dyn TimelineComponent + Send>>,
    /// Gap inserted between consecutive children (seconds). `0.0` by default.
    #[builder(default)]
    pub spacing: f64,
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

    /// Counts up to two children whose sequence slots can contribute at
    /// `local_t`, mirroring the cursor walk in [`TimelineComponent::frame`].
    /// Timeless children have no finite slot and remain possible contributors.
    fn possible_contributors_at(&self, local_t: f64) -> usize {
        let mut cursor = 0.0_f64;
        let mut placed_any = false;
        let mut count = 0usize;
        for child in &self.children {
            if Self::is_fill(child.as_ref()) {
                continue;
            }
            if placed_any {
                cursor += self.spacing;
            }
            let slot = child.measure();
            if slot.is_some() && local_t < cursor {
                break;
            }
            let active = match slot {
                Some(len) => local_t >= cursor && local_t < cursor + len,
                None => true,
            };
            if active {
                count += 1;
                if count == 2 {
                    return count;
                }
            }
            cursor += slot.unwrap_or(0.0);
            placed_any = true;
        }
        count
    }
}

impl TimelineComponent for Sequence {
    fn measure(&self) -> Option<f64> {
        // Σ over the NON-fill children of each child's length, plus spacing
        // between them. A fill child contributes nothing (it is an error at
        // place time). All-`None` children ⇒ the sequence is timeless.
        let mut total = 0.0_f64;
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
        let gaps = count.saturating_sub(1) as f64;
        Some(total + self.spacing * gaps)
    }

    fn resolve(&self, abs_start: f64, out: &mut ResolveCtx) -> f64 {
        // Cursor from 0: child N starts at `abs_start + Σ prior lengths (+gaps)`,
        // the time version of `compute_stack_pass`'s Start branch. A `.fill()`
        // child is a fatal error — record it and skip its length contribution.
        let mut cursor = 0.0_f64;
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
            let resolved = child.resolve(abs_start + cursor * out.local_scale(), out);
            // Advance by the measured component footprint when available. This
            // retains a Placed child's leading offset even through an outer
            // effect wrapper, while its resolve return keeps the established
            // trigger-interval semantics.
            cursor += child.measure().unwrap_or(resolved);
            placed_any = true;
        }
        cursor
    }

    fn cues(&self, offset: f64) -> Vec<Cue> {
        // Re-flow the cursor the same way `resolve` does so cues land at the
        // child's absolute start. Fill children are skipped (an error).
        let mut cues = Vec::new();
        let mut cursor = 0.0_f64;
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
        residency: RasterResidency,
        ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage> {
        // Re-flow the cursor exactly as `resolve` / `cues` do, then hand the
        // ACTIVE child (whose slot `[cursor, cursor + slot)` contains the current
        // local time) a clock rebased to its slot — `local = clock.local() -
        // cursorN` — carrying the slot length as its window. Half-open slots mean
        // exactly one child draws at a seam (no double-draw); a child outside its
        // slot contributes nothing (`.sketch/02 §8`).
        let mut cursor = 0.0_f64;
        let mut placed_any = false;
        let mut frames = Vec::new();
        let local_t = clock.local().seconds();
        let gpu_path = self.possible_contributors_at(local_t) > 1
            && ctx.prefers_gpu()
            && ctx.gpu_backend().is_some();
        let child_residency = if gpu_path {
            RasterResidency::Gpu
        } else {
            residency
        };
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
            // Children are ordered; once a finite future slot starts, no later
            // finite child can be active at this local time.
            if slot.is_some() && local_t < cursor {
                break;
            }
            let active = match slot {
                Some(len) => local_t >= cursor && local_t < cursor + len,
                None => true,
            };
            if active {
                let child_clock = clock.with_local_window(LocalTime::new(local_t - cursor), slot);
                if let Some(img) = child.frame(child_clock, canvas, target, child_residency, ctx) {
                    frames.push(img);
                }
            }
            cursor += slot.unwrap_or(0.0);
            placed_any = true;
        }
        composite_frames_over(frames, target, residency, ctx)
    }

    fn render_audio_block(&self, mut block: AudioBlockMut<'_>, ctx: &mut AudioRenderContext) {
        let request = block.request();
        block.clear();
        let mut scratch = ctx.take_scratch(request.sample_len());
        let request_latest = (request.frame_count() > 0).then(|| {
            request
                .time_at(0)
                .max(request.time_at(request.frame_count() - 1))
        });
        let mut cursor = 0.0_f64;
        let mut placed_any = false;
        for child in &self.children {
            if Self::is_fill(child.as_ref()) {
                continue;
            }
            if placed_any {
                cursor += self.spacing;
            }
            let slot = child.measure();
            // Children are ordered. Once a finite future slot begins strictly
            // after the request's latest local sample, this and every later
            // finite slot are unreachable; stop instead of walking the tail.
            // A timeless slot remains observable at any time, matching frame().
            if slot.is_some()
                && request_latest.is_some_and(|request_latest| request_latest < cursor)
            {
                break;
            }
            if slot.is_some_and(|len| !request.may_overlap_local(cursor, cursor + len)) {
                cursor += slot.unwrap_or(0.0);
                placed_any = true;
                continue;
            }
            let child_request = request.shift_local(-cursor);
            child.render_audio_block(AudioBlockMut::new(child_request, &mut scratch), ctx);

            let channels = request.channels() as usize;
            for frame in 0..request.frame_count() {
                let parent_t = request.time_at(frame);
                let active = match slot {
                    Some(len) => parent_t >= cursor && parent_t < cursor + len,
                    None => true,
                };
                if active {
                    let base = frame * channels;
                    for channel in 0..channels {
                        block.samples_mut()[base + channel] += scratch[base + channel];
                    }
                }
            }
            cursor += slot.unwrap_or(0.0);
            placed_any = true;
        }
        ctx.recycle_scratch(scratch);
    }

    fn arrangement(&self, offset: f64) -> Arrangement {
        // Re-flow the cursor exactly as `resolve` / `cues` do so child N's node
        // lands at its absolute start `offset + Σ prior lengths (+ gaps)`. Fill
        // children are skipped (an error inside a `Sequence`).
        let mut children = Vec::with_capacity(self.children.len());
        let mut cursor = 0.0_f64;
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
