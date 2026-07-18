# tellur Layout System Tutorial

A step-by-step guide to tellur's layout system, starting from its design philosophy. The goal is that you can pick the right container for any job without hesitation.

Covered modules: `tellur-core/src/layout/`, `layer.rs`, `fragment.rs`, `placement.rs`, `clip.rs`, `geometry.rs`

Companion tutorial: [Time System Tutorial](./time-tutorial.md)

日本語版: [layout-tutorial.ja.md](./layout-tutorial.ja.md)

## 0. The big picture — two worlds

tellur's layout consists of **two worlds** riding on the same component protocol.

| | Canvas world | Flow world |
|---|---|---|
| Metaphor | After Effects compositions | CSS flexbox / Figma Auto Layout |
| How positions are decided | The author specifies absolute coordinates | The parent hands down constraints; the child answers with its size |
| Main cast | `Layer`, `Fragment`, `Positioned` with a point target | `Frame`, `Flex`, `Stack`, `Padding`, `DecoratedBox`, `SizedBox`, `Positioned` with an anchor target |
| Best for | staging motion-graphics direction | "UI-like" structure: subtitle bars, HUDs, ranking tables |

Residents of both worlds are `VectorComponent`s (or `RasterComponent`s), so **they nest freely**. "A box placed on a canvas whose inside is laid out with flow" and "a small canvas inside a flow cell" are both natural to write.

## 1. The foundation — the layout protocol

Every component follows a two-pass protocol.

```rust
// Pass 1: the parent hands down constraints; the child answers its size.
fn layout(&self, constraints: Constraints) -> Vec2;

// Pass 2: the parent fixes the size; the child renders at exactly that size.
fn render(&self, size: Vec2) -> VectorGraphic;
```

`Constraints` is an envelope from the parent saying "answer within this size range."

```rust
Constraints::tight(size)   // exactly this size
Constraints::loose(max)    // anywhere from zero up to max
Constraints::UNBOUNDED     // no upper bound — answer your intrinsic size
```

The principle: **constraints flow down, sizes flow up, the parent decides positions**. A child never knows its own position; the parent always decides it.

One more principle: **what is invisible is not drawn.** Shapes draw emptiness themselves when invisible (fill/stroke alpha 0, zero size), `Text` when the string is empty or the fill invisible, `Transformed` at opacity 0. Callers never need visibility guards like "skip it when alpha is 0."

> **footgun ①**: `SizeMode::Fill` takes the parent's max constraint, but when the max is infinite (`UNBOUNDED`) it **collapses to 0**. If something "Filled and disappeared", suspect a parent passing an infinite constraint.

## 2. The canvas world — placing on a `Layer`

Make a fixed-size canvas and place things at absolute coordinates. The basic form of motion graphics.

```rust
Layer::builder()
    .size(Vec2(1920.0, 1080.0))
    .child(background.place_at(Vec2::ZERO))
    .child(title.anchored(Anchor::CENTER).snap_to(Vec2(960.0, 400.0)))
    .child(badge.anchored(Anchor::TOP_RIGHT).snap_to(Anchor::TOP_RIGHT))
    .build()
```

`Positioned` uses one sentence for both absolute and box-relative placement:
"snap this anchor on the child to that target, then offset it".

- `.place_at(pos)` — put the component's top-left corner at `pos`
- `.anchored(child_anchor).snap_to(point)` — snap to an absolute `Vec2`
- `.anchored(child_anchor).snap_to(parent_anchor)` — snap to a proportional point on the parent box
- `.offset(Vec2(dx, dy))` — add a constant pixel translation after either kind of snap

The target is represented by `SnapTarget`: `Vec2` converts to
`SnapTarget::Point`, while `Anchor` converts to `SnapTarget::Anchor`.
`anchored().snap_to()` therefore reads left to right as "**which point of the
child goes where**" without changing grammar between the two layout worlds.
Every form returns an ordinary `Positioned`; there is no separate "placed"
world.

A point target is **out of flow**: it measures the child under unbounded
constraints and reports that intrinsic size. The canvas therefore never
squashes an oversized circle. An anchor target instead reports the finite
maximum offered by its parent, measures the actual child loosely inside that
box, and resolves the target from the chosen box size during painting. This is
the same fill rule used by `SizeMode::Fill`.

```rust
chip
    .anchored(Anchor::CENTER_LEFT)       // point on the child
    .snap_to(Anchor::TOP_LEFT)           // point on the parent box
    .offset(Vec2(28.0, 0.0))             // final pixel nudge
```

> **footgun ②**: `SnapTarget::Anchor` collapses an axis to `0` when the
> parent's maximum on that axis is `UNBOUNDED`, just like `SizeMode::Fill`.
> Use it under a finite box such as `Layer`, `Frame`, or `Stack`; use a `Vec2`
> point target for out-of-flow placement in an auto-fitting `Fragment`.

A `Layer`'s size is **required**. When you want a "group that shrinks to its children", use `Fragment` below.

## 3. `Fragment` — the transparent group

`Fragment` is the equivalent of React's `<>...</>`: a group that adds **nothing**.

```rust
// Group several siblings into one component.
Fragment::builder()
    .child(glow.place_at(Vec2(-20.0, -20.0)))
    .child(core_shape.place_at(Vec2::ZERO))
    .build()

// The "render nothing" form (React's `null`).
Fragment::empty()
```

- Never offsets its children (identity transform)
- Its own size auto-fits the bounding box of its children's paint bounds
- An empty `Fragment` expresses "draw nothing"

With `Positioned` (place one) and `Fragment` (group many / none), all of "none, one, many, positioned" are expressible as components — this is tellur's React-like core.

## 4. Flow world ① — `Frame` decides size, `Positioned` decides placement

`Frame` is a deliberately small sizing container. Per axis, `SizeMode` decides
the outer size; the child starts at the top-left. When the child should be
aligned inside that box, wrap the child in the same `Positioned` vocabulary
used on a canvas.

```rust
pub enum SizeMode {
    Fill,        // take the parent's max (CSS width: 100%)
    Hug,         // shrink-wrap the child (default)
    Fixed(f32),  // exactly this many logical units
}
```

The default is `Hug` on both axes, so a plain `Frame` is a transparent sizing
wrapper and you write only what you want to change.

```rust
// Width fills the parent, height is fixed; the child is centered in that box.
Frame::builder()
    .width(SizeMode::Fill)
    .height(SizeMode::Fixed(60.0))
    .child(
        circle
            .anchored(Anchor::CENTER)
            .snap_to(Anchor::CENTER),
    )
    .build()
```

Symmetric and asymmetric placement now use the same expression:

```rust
// The same anchor on both boxes: centering or corner-pinning.
child.anchored(Anchor::CENTER).snap_to(Anchor::CENTER)
child.anchored(Anchor::BOTTOM_RIGHT).snap_to(Anchor::BOTTOM_RIGHT)

// Different anchors: the child's center follows a proportional target.
child.anchored(Anchor::CENTER).snap_to(Anchor::new(rx, 0.5))
```

Drive `rx` with time and the child glides across the box; this is the
`BouncingDot` pattern in `timeline_to_mp4`. Because an anchor-targeted
`Positioned` fills both offered axes, put it under a finite reference box. In
particular, wrapping it in a `Frame` with a `Hug` axis makes that axis fill
rather than hug; use the unwrapped child when an axis should derive its size
from the child's intrinsic size.

## 5. Flow world ② — lining up with `Flex`

`Flex` is a single-line arrangement container modeled on CSS flexbox. Use
`Layer` / `Stack` for overlays and `Flex` for lining things up.

```rust
Flex::builder()
    .axis(Axis::Vertical)
    .spacing(12.0)
    .main_align(MainAlign::Start)      // justify-content
    .cross_align(CrossAlign::Stretch)  // align-items
    .child(row1)
    .child(row2)
    .build()
```

- The **main axis** is the direction given by `axis`. `MainAlign` is `Start` / `Center` / `End` / `SpaceBetween` / `SpaceAround` / `SpaceEvenly`
- The **cross axis** is perpendicular to it. `CrossAlign` is `Start` / `Center` / `End` / `Stretch` (`Stretch` passes the child a tight cross constraint, stretching it)
- `Flex` itself sizes its main axis as "expand to the parent's max (if finite) / shrink to the children's total if infinite". To be explicit, wrap it in an outer `Frame`

### grow — sharing leftover space by weight

The flexbox `flex-grow` equivalent. A child with `.grow(weight)` receives the **leftover main-axis space** after fixed-size siblings have taken theirs, in proportion to its weight.

```rust
use tellur_core::layout::VectorFlex; // for .grow() on components

Flex::builder()
    .axis(Axis::Horizontal)
    .child(label)                        // intrinsic size
    .child(Flexible::spacer(1.0))        // empty space that absorbs leftovers
    .child(value.grow(2.0))              // takes 2 shares of the leftover
    .build()
```

- `.grow(w)` exists on both components and builders (`VectorFlex` / `VectorBuilderFlex` and their raster counterparts)
- `Flexible::spacer(1.0)` is a "growing blank". It builds "right-aligned group" without `MainAlign::End`:

```rust
// left ........ right1 right2
Flex::builder()
    .axis(Axis::Horizontal)
    .child(left)
    .child(Flexible::spacer(1.0))
    .child(right1)
    .child(right2)
    .build()
```

> **footgun ③**: `Flexible` must be a **direct child** of `Flex`. Put a `Padding` or similar in between and grow is ignored (the wrapper just behaves transparently).
>
> **footgun ④**: once any child grows, the leftover space is consumed by grow first, so `MainAlign`'s `Center`/`End`/`Space*` effectively degenerate to `Start`. Same as the CSS relationship between `flex-grow` and `justify-content`.
>
> **footgun ⑤**: grow is inert when the main axis is under an infinite constraint (there is no defined "leftover" to share). Also the same behavior as CSS.

## 6. Flow world ③ — spacing and decoration

```rust
// CSS-style "padded box with a background".
DecoratedBox::builder()
    .background(Color::rgb_u8(20, 20, 30))
    .child(
        Padding::builder()
            .insets(EdgeInsets::all(16.0))
            .child(content),
    )
    .build()
```

- `Padding` — adds space around its child. `EdgeInsets::all / symmetric / only`
- `DecoratedBox` — paints a background (and, in the vector variant, a border) across the child's full layout size. Does not affect layout. The raster variant pins its paint bounds to its own box, so it doubles as a **clip rectangle for overflow** such as drop shadows
- `SizedBox` — a fixed-size empty box, for fixed spacers and reserving area (the growing spacer is `Flexible::spacer`)
- `Clip` — a vector container that **geometrically** cuts its child by a rectangle (or any path): `Clip::builder().region(ClipRegion::rect(rect)).child(x)`. Layout passes through to the child untouched; only the drawing is cut. Distinct from `DecoratedBox`'s "pin the paint bounds to the box" — `Clip` truly cuts at the given region, whether mid-box or an arbitrary path

### `Stack` — overlays sized by one base child

`Stack` fills the missing "decorate or overlay whatever size this content
chooses" role. It has three slots and deliberately contains no placement
grammar of its own:

- exactly one `.base(child)` decides the Stack's layout size
- zero or more `.under(child)` paint behind the base
- zero or more `.over(child)` paint in front of the base
- `.maybe_under(option)` / `.maybe_over(option)` add optional layers

`base` is a required builder field, so `.build()` is unavailable until it has
been supplied.

```rust
Stack::builder()
    .under(decorations) // receives the base size as tight constraints
    .base(
        Padding::builder()
            .insets(EdgeInsets::all(16.0))
            .child(content),
    )
    .over(
        chip
            .anchored(Anchor::CENTER_LEFT)
            .snap_to(Anchor::TOP_LEFT)
            .offset(Vec2(28.0, 0.0)),
    )
    .build()
```

The Stack passes its incoming constraints unchanged to `base` and reports the
base's answer. Every under/over child is then laid out with tight constraints
at that resolved size, so `SizeMode::Fill`, `#[available]`, and
`SnapTarget::Anchor` all see the same box. Paint order is unders in declaration
order, then base, then overs in declaration order.

Under/over children never change layout size, but their full paint bounds are
unioned into the Stack's paint bounds. An offset shadow, overhanging chip, or
outset stroke therefore survives rasterization instead of being clipped. Use
`Positioned` inside a slot for all edge-relative or offset placement.

## 7. Worked example — where the two worlds meet

One dot track from `tellur-renderer/examples/timeline_to_mp4.rs`.

```rust
#[component(raster)]
fn BouncingDot(#[builder(into)] t: LocalTime) -> impl RasterComponent {
    let rx = t.bounce(2.5).linear(0.0, 1.0);
    Frame::builder()
        .width(SizeMode::Fill)            // the track spans the parent width
        .height(SizeMode::Fixed(60.0))    // fixed track height
        .child(
            circle
                .anchored(Anchor::CENTER)
                .snap_to(Anchor::new(rx, 0.5)), // time-driven sweep
        )
        .build()
}
```

`Flex` stacks these vertically at even spacing, and `Padding` + `DecoratedBox` wrap the scene. The `tellur-live` demo scenes, by contrast, are built almost entirely in the canvas world (`Layer` + `place_at`). **Direction in coordinates, structure in flow.** Comparing these two examples is the fastest way to absorb the split.

## 8. Decision table

| Goal | Reach for |
|---|---|
| Place at coordinates on a fixed canvas | `Layer` + `.place_at()` / `.snap_to(Vec2)` |
| Place relative to a resolved box | `.anchored(child_anchor).snap_to(parent_anchor)` |
| Group siblings / conditionally render nothing | `Fragment` / `Fragment::empty()` |
| Nudge a snapped child by pixels | `.offset(Vec2)` on `Positioned` |
| Declare a size | `Frame` + `SizeMode` |
| Paint behind/over content at the content's size | `Stack` (`under` / `base` / `over`) |
| Line up vertically / horizontally | `Flex` |
| Share leftover space by ratio / growing blank | `.grow(w)` / `Flexible::spacer(w)` |
| Spacing | `Padding` |
| Background / border / overflow clipping | `DecoratedBox` |
| Fixed-size blank | `SizedBox` |
| Cut by rectangle or arbitrary path | `Clip` |
| Rotate / scale / opacity (layout-invariant) | `.transform()` / `.opacity()` (= `Transformed`) |
| Rotate / scale around an anchor (center spin etc.) | `.transform_around(Anchor::CENTER, t)` |

## 9. vector / raster correspondence

Every container comes in vector and raster variants with the **same name and the same semantics**.

```rust
use tellur_core::layout::{Frame, Flex, Flexible, Stack};          // vector
use tellur_core::layout::raster::{Frame, Flex, Flexible, Stack};  // raster
```

In the source, both variants live in one file per container (`layout/frame.rs` etc.). The raster side's only extra responsibilities are `paint_bounds` (the drawn extent including overflow such as drop shadows) and its relationship with the cache (`Flexible` and `Positioned` use `CachePolicy::Transparent` to hand their cache slot to the child).

Two exceptions: `Clip` is vector-only (for raster, cut first, then `.rasterize()`), and conversely `.opacity()` also exists on the raster side under the same name (= `Opacity`; alpha multiplication only — `.transform()` / `.transform_around()` remain vector-only).
