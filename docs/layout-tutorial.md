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
| Main cast | `Layer`, `Positioned`, `Fragment` | `Frame`, `Flex`, `Padding`, `DecoratedBox`, `SizedBox` |
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

> **footgun ①**: `SizeMode::Fill` takes the parent's max constraint, but when the max is infinite (`UNBOUNDED`) it **collapses to 0**. If something "Filled and disappeared", suspect a parent passing an infinite constraint (inside a `Fragment`, or a child placed via `place_at` / `anchored()` — the canvas world measures children under unbounded constraints).

## 2. The canvas world — placing on a `Layer`

Make a fixed-size canvas and place things at absolute coordinates. The basic form of motion graphics.

```rust
Layer::builder()
    .size(Vec2(1920.0, 1080.0))
    .child(background.place_at(Vec2::ZERO))
    .child(title.anchored(Anchor::CENTER).snap_to(Vec2(960.0, 400.0)))
    .build()
```

There are two fluent APIs for placement.

- `.place_at(pos)` — put the component's top-left corner at `pos`
- `.anchored(anchor).snap_to(point)` — snap an anchor point on the component to a point on the canvas

`anchored().snap_to()` lifts the geometric vocabulary (`Vec2::anchored` → `AnchoredSize::snap_to`) straight onto components; its charm is that "**which point of the child goes where**" reads left to right. Both simply return a `Positioned` — an ordinary component — so there is no special "placed world".

A placed child renders **at its intrinsic size (its measurement under unbounded constraints)**. The canvas never forces a size onto its children, so a circle larger than the canvas is not squashed — overflow is the clipper's job.

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

With `Positioned` (offset one) and `Fragment` (group many / none), all of "none, one, many, positioned" are expressible as components — this is tellur's React-like core.

## 4. Flow world ① — `Frame` decides size and alignment

`Frame` is the flow-world container that single-handedly covers "size declaration + anchor alignment". Per axis, `SizeMode` decides the outer size; inside it, the child is placed with an `Alignment`.

```rust
pub enum SizeMode {
    Fill,        // take the parent's max (CSS width: 100%)
    Hug,         // shrink-wrap the child (default)
    Fixed(f32),  // exactly this many logical units
}
```

The default is "both axes `Hug`, top-left alignment" — a transparent box that does nothing — so you write only what you want to change.

```rust
// Width fills the parent, height is fixed; child centered.
Frame::builder()
    .width(SizeMode::Fill)
    .height(SizeMode::Fixed(60.0))
    .align(Anchor::CENTER)
    .child(circle)
    .build()
```

`.align()` comes in two forms.

```rust
// Symmetric: the same anchor on both boxes (the common case).
.align(Anchor::CENTER)             // centering
.align(Anchor::BOTTOM_RIGHT)       // pin to the bottom-right corner

// Asymmetric: snap the child's anchor onto a different box anchor.
.align(Anchor::CENTER.to(Anchor::new(rx, 0.5)))
```

Read `Anchor::CENTER.to(...)` as "the child's center goes to the box's `(rx, 0.5)` point". Drive `rx` with time and the child glides across the `Frame` (the `BouncingDot` in `timeline_to_mp4` is this pattern).

> Where the canvas world's `anchored().snap_to(point)` snaps to a **point**, `Frame.align` snaps to a **relative position**. The former is absolute coordinates; the latter tracks the box's size.

## 5. Flow world ② — lining up with `Flex`

`Flex` is a single-line arrangement container modeled on CSS flexbox. Stacking is the job of `Layer`/`Fragment`; `Flex` is strictly for lining things up.

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

> **footgun ②**: `Flexible` must be a **direct child** of `Flex`. Put a `Padding` or similar in between and grow is ignored (the wrapper just behaves transparently).
>
> **footgun ③**: once any child grows, the leftover space is consumed by grow first, so `MainAlign`'s `Center`/`End`/`Space*` effectively degenerate to `Start`. Same as the CSS relationship between `flex-grow` and `justify-content`.
>
> **footgun ④**: grow is inert when the main axis is under an infinite constraint (there is no defined "leftover" to share). Also the same behavior as CSS.

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

## 7. Worked example — where the two worlds meet

One dot track from `tellur-renderer/examples/timeline_to_mp4.rs`.

```rust
#[component(raster)]
fn BouncingDot(#[builder(into)] t: LocalTime) -> impl RasterComponent {
    let rx = t.bounce(2.5).linear(0.0, 1.0);
    Frame::builder()
        .width(SizeMode::Fill)            // the track spans the parent width
        .height(SizeMode::Fixed(60.0))    // fixed track height
        .align(Anchor::CENTER.to(Anchor::new(rx, 0.5)))  // time-driven sweep
        .child(circle)
        .build()
}
```

`Flex` stacks these vertically at even spacing, and `Padding` + `DecoratedBox` wrap the scene. The `tellur-live` demo scenes, by contrast, are built almost entirely in the canvas world (`Layer` + `place_at`). **Direction in coordinates, structure in flow.** Comparing these two examples is the fastest way to absorb the split.

## 8. Decision table

| Goal | Reach for |
|---|---|
| Place at coordinates on a fixed canvas | `Layer` + `.place_at()` / `.anchored().snap_to()` |
| Group siblings / conditionally render nothing | `Fragment` / `Fragment::empty()` |
| Offset just one thing | `.place_at()` (= `Positioned`) |
| Declare a size / align inside a box | `Frame` (`SizeMode` × `Alignment`) |
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
use tellur_core::layout::{Frame, Flex, Flexible};          // vector
use tellur_core::layout::raster::{Frame, Flex, Flexible};  // raster
```

In the source, both variants live in one file per container (`layout/frame.rs` etc.). The raster side's only extra responsibilities are `paint_bounds` (the drawn extent including overflow such as drop shadows) and its relationship with the cache (`Flexible` and `Positioned` use `CachePolicy::Transparent` to hand their cache slot to the child).

Two exceptions: `Clip` is vector-only (for raster, cut first, then `.rasterize()`), and conversely `.opacity()` also exists on the raster side under the same name (= `Opacity`; alpha multiplication only — `.transform()` / `.transform_around()` remain vector-only).
