---
name: tellur-authoring
description: >
  Write idiomatic "Tellur-style" Rust code for videos and motion graphics built
  on the tellur crate. Use when creating or editing tellur scenes, animations,
  timelines, or video project crates. Covers the declarative component model,
  builder-pattern trees, layout/time vocabulary that replaces magic numbers and
  magic formulas, project structure, and a self-review checklist.
---

# Writing Tellur-style code

tellur is a declarative video and motion-graphics engine written in Rust. This
skill defines the norms for writing not merely "code that works" but
"**Tellur-style code**". tellur code has a distinct feel, and code that misses
it gets rewritten in review.

日本語版: [SKILL.ja.md](./SKILL.ja.md)（人間向けの翻訳。エージェントが読む正本はこの英語版です）

## 0. Required reading

Before writing any code, **read both tutorials** in the tellur repository in
full (this skill layers norms on top of them; it does not replace them):

- `docs/layout-tutorial.md` — the layout system (canvas world and flow world)
- `docs/time-tutorial.md` — the time system (absolute-time world and
  placement-clock world)

If this skill directory lives outside the tellur repository, fetch them from a
tellur checkout (or from docs/ at https://github.com/comnipl/tellur). Japanese
translations exist as `docs/*.ja.md`; the English files are canonical.

Features covered by the tutorials are not "use if you happen to know them" —
they are **assumed to be used**. Before hand-writing any formula or coordinate
arithmetic, check both decision tables (layout §8 / time §8) for the matching
vocabulary.

## 1. Mental model — tellur as React

tellur follows a React-like declarative model. Each frame's picture is a pure
function of time, produced by evaluating a component tree every frame.

- **Never write state mutation or event loops.** Animation is expressed as the
  transformation `Time → Phase/Window → value`
- **Component fields are render-cache keys.** Write code so that "same field
  values ⇒ same picture" holds (§5)
- The correspondences `Fragment` = `<>...</>`, `Fragment::empty()` = `null`,
  `Positioned` = placement mean "none, one, many, positioned" are all
  expressible as components

## 2. Iron rule ① — everything is a component

**Functions without `#[component(...)]` are rare in tellur code.** The Tellur
way is to componentize everything and return a tree built with the builder
pattern:

```rust
#[component(vector)]
fn TitleCard(#[builder(into)] source: String, alpha: f32) -> impl VectorComponent {
    Text::builder()
        .font(SERIF.clone())
        .size(72.0)
        .fill(INK.multiply_alpha(alpha))
        .span(MathSpan::new(source))
        .anchored(Anchor::CENTER)
        .snap_to(CENTER)
}
```

- Function-form components are **PascalCase**. The three kinds are
  `#[component(vector)]` / `#[component(raster)]` / `#[component(timeline)]`
- A complete builder flows into a parent's `.child(...)` without `.build()`
  (a `From<TBuilder>` impl is generated). The convention: **no `.build()` when
  passing as a child**; call `.build()` only when returning from a function
- Use the builder machinery: `#[builder(into)]` (`&str` → `String` etc.),
  `maybe_*` setters (pass an `Option` as-is — prefer this over branching with
  `if let` to reshape the builder), `#[children(each = child)]` (accumulate a
  `Vec` of children through repeated `.child()` calls)
- `#[clock] clock: Clock` is exclusive to timeline components. `#[available]`
  is the layout-side hole that receives the parent-assigned size

**Bare functions are acceptable** almost only for pure data preparation that
does not return a render tree (building a `Vec<PathCommand>`, assembling a
`Keyable` state struct, etc.). Whenever you feel the urge to write a bare
function, first ask: "does tellur's existing vocabulary (shapes, layout, time
combinators) really not cover this?" Historically, most hand-written helpers
(custom easing, dash splitting, stagger math, color lerp) **disappeared once
tellur grew the vocabulary**. A hand-written helper is usually a signal of a
gap in tellur, not something a video crate should keep carrying.

## 3. Iron rule ② — eliminate magic numbers and magic formulas with vocabulary

When a hand-written formula appears, tellur almost certainly has the matching
vocabulary.

### Time conversion table

| Hand-written formula | The Tellur way |
|---|---|
| `((x - a) / (b - a)).clamp(0.0, 1.0)` | `t.phase(a, b)` |
| Custom easing polynomial / cubic-bezier solver | `phase.ease_out_cubic(from, to)` etc. / `ease_bezier(x1, y1, x2, y2, from, to)` / `Easing::CubicBezier` |
| `a + (b - a) * p` | `p.linear(a, b)` / `a.interpolate(b, p)` (`f32`/`Vec2`/`Anchor`/`Color`) |
| Hand-composed fade-in × fade-out expressions | `window.envelope(rise, fall)` |
| `progress - i as f32 * stagger` loops | stagger with `window.sub_secs(range)` |
| `(t % period) / period` / triangle wave / `sin` | `t.cycle(p)` / `t.bounce(p)` / `t.wave(p)` |
| Hand-computed "n seconds after it ended" | `window.elapsed()` / `window.after()` |
| Countdown | `window.remaining()` |
| `if (a..b).contains(&t)` guards | `t.during(a, b)` — though per §6 the visibility guard itself is usually unnecessary |

- For a reverse animation, don't touch the formula — **swap `(from, to)`**:
  `phase.ease_in_back(1.0, 0.0)`
- Bundle compounds like "fade in while rotating" into one `Window`: take
  `w.phase()` and `w.elapsed()` from the same declared interval
- Compose multi-condition visibility as a **product of `[0,1]` factors**:
  `let alpha = after(cue_in) * (1.0 - entering(cue_out, 0.0, 0.45));`

### Space conversion table

| Hand-written computation | The Tellur way |
|---|---|
| Centering via `Vec2(canvas.0 * 0.5 - w * 0.5, ...)` | `.anchored(Anchor::CENTER).snap_to(point)` / `Frame` with `.align(Anchor::CENTER)` |
| Sibling coordinates as an arithmetic progression | `Flex` + `.spacing(n)` |
| Back-computing right-/bottom-aligned coordinates | `Anchor::BOTTOM_RIGHT` family / `Flexible::spacer(1.0)` / `MainAlign::End` |
| Adding margin offsets by hand | `Padding` + `EdgeInsets` |
| Translate-rotate-translate sandwiches for center spin | `.transform_around(Anchor::CENTER, t)` |
| Interpolating coordinates so a child "glides across a box" | drive the anchor itself with time: `.align(Anchor::CENTER.to(Anchor::new(rx, 0.5)))` |
| Clamping coordinates to hide overflow | `Clip` / `DecoratedBox` (raster) |

**Direction in coordinates, structure in flow.** In motion-graphics staging
(canvas world: `Layer` + `place_at` / `anchored().snap_to()`), absolute
coordinate literals *are* the artistic decision and are acceptable. The moment
you start building "UI-like" structure — subtitle bars, HUDs, tables — out of
canvas-world coordinate arithmetic, that is the flow world's job
(`Frame`/`Flex`/`Padding`).

## 4. Iron rule ③ — proliferating `const`s signal a design mistake

If `const` definitions start piling up in the name of avoiding magic numbers,
**the problem is structure, not naming**. The fewer global `const`s the
better. When they multiply, suspect that work belonging to layout or
time-management components is being faked through named numbers and manual
placement.

Checks to run before reaching for a `const`:

1. **Is the value derived from another value?** An arithmetic relationship
   like `const TITLE_X: f32 = CANVAS_W * 0.5 - 120.0;` should be expressed
   structurally with `Anchor`-relative placement or `Frame`/`Flex`/`Padding`.
   Locking a derivation formula inside a const hides it; it doesn't solve it
2. **Is it a timetable?** A cascade of absolute times like
   `const SCENE2_START: f64 = 12.3;` should become `Sequence` ordering,
   `TimeBox` beats, and `Event` triggers, so that times are *derived from
   placement*. The correct structure survives a script reshuffle
3. **Is the number used in two or more places?** A one-off staging literal
   (place at `Vec2(320.0, 750.0)`, fade over 0.45s) is fine as a local
   variable or written inline. Not every number needs a name

**Legitimate `const`s (design tokens)**: palette colors (`INK`, `BLUE`), the
canvas size, a small number of geometry tokens shared across scenes (a
diagram's center and radius), meaningful shared durations (a fade length that
several places must agree on). Keep these few, in `style.rs`. Non-obvious
values (audio gains, windows padded with slack, etc.) must carry a **comment
explaining their origin**:

```rust
// -6.63 dB over the 91.816s preview BGM window: -15.37 LUFS -> -22.0 LUFS.
const BGM_GAIN: f32 = 0.466122427;
```

## 5. Iron rule ④ — design boundaries around cache keys

Component fields are render-cache keys. When passing time-driven values across
a component boundary, the type choice decides cache efficiency.

- Collapsing to a value on the spot → `phase`; needing sub-events or elapsed
  seconds → `window`
- **Fields take `Phase`, or a `clamped()` `Window`.** A raw `Window`'s cursor
  moves every frame and annihilates the cache (it still compiles, so it's easy
  to miss)
- From an `Event`, `event.window(&clock, a..b)` returns an already-clamped
  snapshot that can go straight into a field
- State structs get `#[derive(Clone, Keyable)]` (add `Copy` when possible).
  `Keyable` is tellur's derive that makes f32s `Eq`/`Hash` by bit pattern

## 6. Drawing and animation idioms

- **Never write visibility guards.** Invisible things (alpha 0, empty strings,
  opacity 0) draw emptiness themselves.
  `if alpha > 0.0 { ... } else { Fragment::empty() }` is unnecessary. Use
  `Fragment::empty()` only to express "structurally nothing"
- **Compose alpha as a product of factors**, applied to colors via
  `Color::multiply_alpha(alpha)` / `Color::with_alpha(base * alpha)`
- Write-on animation is `.write_elapsed(secs)` / `.write_on(phase)` (plus
  `.stroke_end(Phase::ONE)`) on `Text` / shapes. Get the elapsed seconds from
  an `Event::window`'s `.elapsed()`. Pacing defaults to an equal, staggered
  time slot per glyph; tune the rhythm with `.per_path_secs(secs)` /
  `.lag_ratio(r)`, or switch to a constant pen speed with `.stroke_speed(u)`
  (`.by_length()` on `Write`) when intricate glyphs should take longer
- Overshoot curves (`ease_in_back` / `ease_out_elastic` / beziers leaving the
  unit interval) must ease directly into the value range with the
  `(from, to)` methods — `eased(Easing::X)` stays inside `Phase` and clamps.
  Where a negative overshoot would hurt, append `.max(0.0)`
- When position and size move together, derive both from one `Phase` via
  `interpolate` (the `MovingTitleMath` pattern:
  `start_*.interpolate(end_*, progress)`)

## 7. Standard structure of a video project

The proven layout (the shorts_sqrt2_plus_sqrt3 shape):

```
src/
  lib.rs        -- build() -> Timeline; export_timeline!; top-level composition only
  script.rs     -- the script (a Sequence of audio/subtitle clips) + a Cues struct (bundle of Events)
  <canvas>/
    mod.rs      -- the #[component(timeline)] root: derives every scene's state
                   from cues + #[clock], stacks scenes on a Layer
    style.rs    -- a small set of design tokens (palette, shared geometry)
    widgets.rs  -- small reusable components (math text, shape wrappers)
    scenes/     -- one scene per file: a SceneState struct + #[component(vector)]
```

What makes this structure work:

1. **Cue-driven**: the script (a `Sequence` of audio clips etc.) fires a
   bundle of `Event::named`s (the `Cues` struct, `Copy + Keyable`) via
   `.trigger_at_start(cue)` / `.trigger_at(offset, cue)`, and every visual
   reacts to Events. The correct state of the world: the script's timing can
   change without touching a single line of the visuals
2. **Time is concentrated at the root**: the root component holding `#[clock]`
   derives every time-driven scalar (alphas, progress values, `Window`s) from
   the cues **in one place**, bundles them into `Keyable` state structs, and
   hands them to the scenes. Everything below the root is a pure function of
   state that knows nothing about time — cacheable and testable
3. **Scene overlap is a product of factors**: express cross-fades between
   scenes as `after(prev_cue) * (1.0 - entering(next_cue, ..))` and keep all
   scenes permanently placed on the `Layer`
4. Bundle repeated helpers at the root as closures:
   `let after = |cue: Event| cue.phase(&clock, 0.0, 0.45).ease_out_cubic(0.0, 1.0);`

The temporal placement vocabulary is just four forms: `.at(secs)` (absolute),
`.at(a..b)` (a window — a stretch for timed clips), `.fill()` (match the
container's resolved length), `.trim(a..b)` (wrap and rebase any component).
Negative trim endpoints count backwards from the immediate child end; open end
means the exact end. "Beats" and "reserved duration" are `TimeBox`.

Audio gain effects are ordered wrappers too: `.gain_envelope((time, gain),
(time, gain))`, `.fade_in(seconds)`, and `.fade_out(seconds)`. Builder calls
wrap immediately, so `x.fade_in(1.0).trim(0.5..)` begins at the inner fade's
0.5-second point, while `x.trim(0.5..).fade_in(1.0)` starts a new fade at local
zero. Prefer source settings → effects → placement. `.fill()` is a structural
marker and must stay the final, outermost verb: write
`source.fade_out(0.25).fill()`, never `source.fill().fade_out(0.25)`.

## 8. Footgun list (from the tutorials — memorize)

1. `SizeMode::Fill` collapses to 0 under an `UNBOUNDED` parent (the canvas
   world measures children under infinite constraints)
2. `Flexible` / `.grow()` only work as **direct children** of `Flex`
3. Once any child grows, `MainAlign`'s `Center`/`End`/`Space*` degenerate to
   `Start`
4. grow is inert under an infinite main-axis constraint
5. `phase` / `window` require `end > start` and panic otherwise
6. `Window::elapsed()` / `after()` don't stop when the window closes (that's
   their purpose). To stop, use `remaining()` or `clamped()`. Exception: an
   `elapsed()` from `Event::window` freezes at the window's end
7. A raw `Window` in a field annihilates the cache (§5)
8. `eased(Easing::X)` clamps overshoot curves (§6)
9. `.trigger_*` attaches on the **outside** (before) of `.at(..)`:
   `x.trigger_at_start(e).at(5.0)`
10. `.at(a..b)` is the half-open interval `[a, b)`; boundary frames never
    double-draw
11. `Clip` is vector-only (for raster: cut first, then `.rasterize()`).
    `.transform()` / `.transform_around()` are also vector-only; `.opacity()`
    exists on both
12. When using `#[component]` through a re-export (`some_crate::tellur`), the
    video crate must also declare a direct `tellur` dependency of the same
    version, for the macro's path resolution
13. Temporal builder calls are ordered wrappers; swapping `.trim()` and an
    audio effect intentionally changes which local clock the effect sees
14. `.fill()` must remain the outermost temporal verb so its parent `Timeline`
    can recognize and exclude it while resolving the container length

## 9. Reference implementations

When unsure how to write something, consult real examples in this order:

- `tellur-renderer/examples/timeline_to_mp4.rs` — the minimal form of the
  absolute-time world (`BouncingDot`: the textbook pattern of time-driving a
  `Frame.align` anchor)
- the `tellur-live` demo scenes — canvas-world staging with every
  `phase`/`window`/`clamped()` pattern, plus the placement-clock world
  (`timeline_showcase`)
- `movies/202606/shorts_sqrt2_plus_sqrt3` in the youtube repository — a
  complete video with the cue-driven architecture (§7). Caveat: it predates
  the §4 `const` norm, so do not imitate its abundance of named consts

## 10. Self-review checklist

Verify before submitting code:

- [ ] Are functions without `#[component]` limited to pure data preparation?
      No render trees or progress math in bare functions?
- [ ] Any hand-written clamp / lerp / easing / stagger formulas left?
      (Replaced via the §3 conversion tables?)
- [ ] Any relative positioning expressed through coordinate arithmetic
      (`* 0.5`, `- width / 2.0`, arithmetic progressions)? Could `Anchor` /
      `Frame` / `Flex` / `Padding` express it?
- [ ] Are global `const`s genuine design tokens only? No derived values,
      timetables, or single-use values promoted to consts (§4)?
- [ ] Does every non-obvious numeric literal carry a comment explaining its
      origin?
- [ ] Is every `Window` crossing a component boundary `clamped()` (or from
      `event.window`)?
- [ ] Do state structs derive `Keyable`?
- [ ] Any visibility guards (`if alpha > 0.0`) written?
- [ ] Is alpha composed as a product of factors? Are reverse animations
      written by swapping `(from, to)`?
- [ ] Does the structure survive script timing/order changes without touching
      the visuals (Event-driven)?
