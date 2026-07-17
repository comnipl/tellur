# tellur Time System Tutorial

A step-by-step guide to tellur's time system, starting from its design philosophy. The goal is that whenever you turn time into a value, you can pick the right type without hesitation.

Covered modules: `tellur-core/src/time.rs`, `phase.rs`, `window.rs`, `easing.rs`, `interpolate.rs`, `timeline_component/`, `timeline_container/`

Companion tutorial: [Layout System Tutorial](./layout-tutorial.md)

日本語版: [time-tutorial.ja.md](./time-tutorial.ja.md)

## 0. The big picture — two time worlds

Just as layout has its "canvas world and flow world", time also has **two worlds**.

| | Absolute-time world | Placement-clock world |
|---|---|---|
| Where time comes from | the `TimelineTime` passed by `Timeline::build` | the `Clock` injected via `#[clock]` |
| How moments are decided | the author specifies absolute seconds on the timeline | the container assigns them from `.at(..)` placements |
| Main cast | `Time` / `Phase` / `Window` | `Clock`, `Timeline`, `Sequence`, `Placed`, `Event` |
| Best for | single-piece motion-graphics direction | compositional videos whose clips get reordered and swapped |

The correspondence with the spatial side lines up cleanly.

| Space (layout) | Time (this document) |
|---|---|
| `Layer` (stack) | `Timeline` (stack on time) |
| `Flex` (line up with a cursor) | `Sequence` (line up front to back) |
| `Positioned` (place one) | `Placed` (the result of `.at(..)`) |
| `SizedBox` (fixed-size blank) | `TimeBox` (fixed-length beat, §7) |

In both worlds, the vocabulary for turning time into values is shared: **`Time` → `Phase` / `Window` → eased values**.

## 1. The basic pipeline — `Time` → `Phase` → value

The workhorse of time-driven animation is this one line:

```rust
// Progress through [0.55s, 1.2s], eased, mapped into [0.0, 1.0].
let hero_in = t.phase(0.55, 1.2).ease_out_cubic(0.0, 1.0);
```

Broken into its three stages:

1. **`Time`** — a type that knows only "how many seconds". There are two — `TimelineTime` (absolute timeline time) and `LocalTime` (rebased local time) — and all combinators are shared between them
2. **`Phase`** — a progress scalar validated into `[0.0, 1.0]`. `t.phase(start, end)` maps the interval linearly onto the unit interval and **saturates** outside it (pins to 0 or 1)
3. **`PhaseEasing`** — `phase.ease_*(from, to)` applies a curve and carries you all the way to the target quantity (alpha, radius, position…). `linear` / `ease_smoothstep` / `ease_out_cubic` / `ease_out_quint` / `ease_in_out_quint` / `ease_in_out_expo` stay within `[from, to]`; `ease_in_back` / `ease_out_elastic` intentionally overshoot (the overshoot *is* the effect). When the named curves aren't enough, `ease_bezier(x1, y1, x2, y2, from, to)` lets you build your own curve from the same four control points as CSS `cubic-bezier` (put `y1`/`y2` outside the unit interval to overshoot deliberately)

Timeline seconds are `f64` throughout the component, renderer, and live-preview boundaries. `Phase` and visual value-space quantities remain `f32`: the timeline keeps sample/frame positions precise, then narrows only when it has become a bounded progress or drawing value.

A reverse animation just swaps `(from, to)`:

```rust
let fade_out = t.phase(1.7, 2.15).ease_in_back(1.0, 0.0);  // 1 → 0
```

> **footgun ①**: `phase` / `window` require a finite interval with `end > start` and panic otherwise. No meaningful progress can be defined over a zero-width interval.

## 2. `Phase` — a pure progress scalar

`Phase` knows nothing beyond "an f32 validated into the unit interval". Seconds, intervals, cursors — none of that; those are `Window`'s job (next section).

```rust
Phase::new(0.5)        // Some(Phase) — validating
Phase::saturating(2.0) // Phase(1.0) — clamping
phase.get()            // the inner f32, guaranteed in [0, 1]
phase.map(|x| 4.0 * x * (1.0 - x))  // custom value-space remap (hat curve)
```

Its smallness has a reason. `Phase` is `Eq`/`Hash`ed by the bit pattern of its value, so **it can be used directly as a component field (= a render-cache key)**. Combined with its saturating nature, "once the animation finishes, keys match across frames and the cache keeps hitting" falls out naturally (§4).

## 3. `Window` — a viewpoint that remembers the interval

`Phase` is handy, but it forgets the interval the moment it maps. To answer "how many seconds until the window closes?" or "how long since it opened, cumulatively?" you need a viewpoint that keeps holding the interval and the cursor. That is `Window`.

```rust
let radar = time.window(3.95, 5.4);

radar.phase()      // the saturating Phase view (0 → 1 inside the window)
radar.elapsed()    // seconds since start — keeps counting PAST the end
radar.remaining()  // countdown to the end, clamped at 0
radar.before()     // seconds until the window opens
radar.after()      // seconds past the close
radar.is_inside()  // the same gate as t.during(a, b)
```

The crux is that `phase()` and `elapsed()` are bound to a single declared interval. For example, "a radar that fades in while rotating continuously from its start moment":

```rust
let radar = time.window(3.95, 5.4);
let opacity = radar.phase().ease_out_cubic(0.0, 1.0);
let angle = radar.elapsed() * 2.4;   // keeps accruing while visible
```

> **footgun ②**: `elapsed()` / `after()` do not stop when the window closes. That is their reason to exist ("5 seconds after the intro ended" cannot be expressed with a saturating `Phase`). When you want them to stop, use `remaining()` or `clamped()` (§4).

### `sub_secs` — carving sub-events in window-local seconds

When you want to stagger several sub-events inside one window, `sub_secs(range)` carves out "a range of seconds counted from the window's start" as a new `Window`. It carries the cursor with it, so it is a **total function** — it cannot fail.

```rust
let reveal = time.window(0.05, 1.332);

// The i-th horizon line slides in over [i*8ms, 0.4s + i*8ms] of the reveal.
let line_in = reveal
    .sub_secs((i as f64 * 0.008)..(0.4 + i as f64 * 0.008))
    .ease_in_out_expo(0.0, 1.0);
```

`PhaseEasing` is implemented on `Window` too, so you can go straight to `.ease_*(from, to)` without a `.phase()` in between.

### `envelope` — entrance and exit in one word

```rust
// Rise over the first 0.3s, hold, fall over the last 0.5s.
let alpha = time.window(2.0, 6.0).envelope(0.3, 0.5).get();
```

It captures "appear, hold, disappear" — subtitles, lower thirds — in a single expression. More complex shapes (different curves for rise and fall, etc.) are composed as a **product of factors**, per the §1 idiom: `rise * fall` are both `[0, 1]` f32s, so multiplication means "visible only while both conditions hold".

## 4. Crossing component boundaries — `clamped()` snapshots

When a time-driven value is passed as a component **field**, it becomes part of the cache key. The choice of type here decides your cache efficiency.

- **`Phase`** — constant once saturated. After the animation completes, keys match between frames and the cache hits
- **A raw `Window`** — its cursor moves every frame, so **the key changes every frame and the cache is annihilated**
- **`Window::clamped()`** — a snapshot whose cursor is clamped into `[start, end]`. Before the window it is `(start, end, start)`; once saturated it is constant at `(start, end, end)` — recovering the same cache stability as `Phase`

```rust
// The component receives a saturating snapshot, not the live cursor.
Backdrop::builder()
    .reveal(t.window(REVEAL_START, REVEAL_END).clamped())
    .build()

#[component(vector)]
pub fn Backdrop(reveal: Window, palette: Palette) -> impl VectorComponent {
    // sub_secs staggering still works — and freezes once the reveal saturates.
    let ring_in = reveal.sub_secs(0.55..1.05).ease_in_out_expo(0.0, 1.0);
    // ...
}
```

Rule of thumb: **collapsing to a value on the spot → `phase`; needing sub-events or elapsed seconds → `window`; passing through a field → `Phase`, or a `clamped()` `Window`**.

> **footgun ③**: putting a raw `Window` in a field still compiles. The only symptom is "the cache hit rate drops to zero", which is easy to miss — memorize it as "a `Window` crossing a boundary gets `clamped()`".

## 5. Periodic animation — `cycle` / `bounce` / `wave`

Movement that repeats on a fixed period comes as three siblings. All return `Phase`, so they flow straight into easing.

```rust
t.cycle(2.0)    // sawtooth: 0 → 1 linearly, then snaps back   /| /| /|
t.bounce(2.0)   // triangle: 0 → 1 → 0, linear both ways       /\ /\ /\
t.wave(2.0)     // sine:     0 → 1 → 0, zero slope at the turnarounds
```

- Back-and-forth shuttling is `bounce` (the `timeline_to_mp4` dot)
- Smooth oscillation — sway, breathing, drift — is `wave`. An amplitude of `±amp` is `t.wave(period).linear(-amp, amp)`
- If you need the lap count, `(t.seconds() / period).floor()` gives it

## 6. Typed interpolation — `Easing` / `eased` / `Interpolate`

To interpolate quantities other than `f32` (`Vec2`, `Anchor`, …), apply the curve **while still inside `Phase`**, then hand it to `Interpolate`.

```rust
use tellur_core::easing::Easing;
use tellur_core::interpolate::Interpolate;

// Ease the progress, then drive a typed lerp with it.
let p = t.phase(1.0, 2.0).eased(Easing::OutCubic);
let pos = start_pos.interpolate(end_pos, p);
let anchor = Anchor::CENTER_LEFT.interpolate(Anchor::CENTER, p);
let tint = INK.interpolate(MUTED, p); // Color: straight per-channel lerp in sRGB
```

`Easing` is an enum that holds curves as values (`Linear` / `Smoothstep` / `OutCubic` / `OutQuint` / `InOutQuint` / `InOutExpo` / `InBack` / `OutElastic`, plus `CubicBezier { x1, y1, x2, y2 }` for custom curves), sharing its implementations with the §1 `ease_*` method family.

`Interpolate` is implemented for `f32` / `Vec2` / `Anchor` / `Color`. `Color` is a naive blend that lerps each sRGB channel linearly — not the "physically correct" convert-to-linear-light-then-mix blend — numerically identical to a hand-written `r + (other.r - r) * t`.

> **footgun ④**: `eased` stays within `Phase`, so **overshoot curves (`InBack` / `OutElastic`, and `CubicBezier` whose y leaves the unit interval) are clamped to the unit interval**. To keep the overshoot, ease directly into the value range with the `(from, to)` methods (`p.ease_out_elastic(from, to)` / `p.ease_bezier(x1, y1, x2, y2, from, to)`).

## 7. The placement-clock world — the two axes of `Clock`

A component placed on a `Timeline` / `Sequence` with `.at(..)` receives a `Clock` via `#[clock]`. A `Clock` carries **two time axes**.

```rust
#[component(timeline)]
fn Spinner(#[clock] clock: Clock) -> impl TimelineComponent {
    let local = clock.local();    // 0 at THIS clip's resolved start
    let global = clock.global();  // absolute timeline time — the Event axis
    // ...
}
```

- **`local()`** — zero at this clip's own resolved start. It follows `Sequence` reordering, so self-animation is written against it: `clock.local().phase(0.0, 0.4)`
- **`global()`** — absolute timeline time; the same axis that `Event` triggers live on
- **`window()`** — returns this slot's length as an `Option<Window>` (`[0, length)` on the local axis). Open placements (`.fill()`, bare timeless placements, the root) return `None`. From here the §3 vocabulary applies verbatim:

```rust
// Slide in over 0.32s, out over the last 0.24s of this clip's slot.
let alpha = match clock.window() {
    Some(w) => w.envelope(0.32, 0.24).get(),
    None => clock.local().phase(0.0, 0.32).get(),  // open-ended: fade in only
};
```

The placement vocabulary is only three forms; `trim` is a separate temporal wrapper.

```rust
clip.at(2.0)        // place at 2.0s, play at native length
clip.at(0.0..3.0)   // an explicit window — for a timed clip this is a STRETCH
clip.fill()         // stretch to the container's resolved length (Timeline only)
media.trim(1.0..4.0)  // keep child-local [1s, 4s), rebased to local 0
media.trim(-3.0..-0.5) // endpoints < 0 count backwards from the child end
media.trim(1.0..)       // an open end means the exact child end
```

`trim` is a generic component wrapper, not media-leaf metadata. It affects video, audio, cues, triggers, and arrangement together. The standard range forms are half-open; inclusive ranges are intentionally unsupported.

### Ordered audio effects

Audio gain automation uses the same wrapper model. A numeric negative envelope point is relative to the immediate child's end; use `EnvelopePoint::End` for the exact end.

```rust
use tellur::core::timeline_container::AudioFile;
use tellur::prelude::*;

let voice = AudioFile::builder()
    .path("voice.wav")
    .gain_envelope((0.0, 0.0), (0.35, 1.0))
    .fade_out(0.25)
    .at(2.0);

let tail = AudioFile::builder()
    .path("tail.wav")
    .gain_envelope((-0.5, 1.0), (EnvelopePoint::End, 0.0));
```

Builder calls wrap immediately, so the last call is outermost and order is semantic:

```rust
source.fade_in(1.0).trim(0.5..)
// Trim<GainEnvelope<Source>>: output starts at the existing fade's 0.5s point.

source.trim(0.5..).fade_in(1.0)
// GainEnvelope<Trim<Source>>: a new fade starts at trimmed-local 0s.
```

The canonical order is source settings, then temporal/audio effects, then
placement. Wrapping an `.at(..)` result is allowed when intentional, but its
leading placement interval becomes part of the outer effect's clock. `.fill()`
is the structural marker a `Timeline` uses to exclude a child from its own
length calculation, so it must remain the final, outermost verb:
`source.fade_out(0.25).fill()`, not `source.fill().fade_out(0.25)`.

Where you want "just a length" — a beat inside a `Sequence`, a platform to hang triggers on, pinning down a `Timeline`'s duration — place a `TimeBox`. It draws nothing and sounds nothing; it is a leaf that merely has the `duration` you give it (the temporal counterpart of `SizedBox`).

```rust
// A 1.5s beat between two clips; nothing is drawn or heard.
Sequence::builder()
    .child(intro)
    .child(TimeBox::builder().duration(1.5).build())
    .child(outro)
    .build()
```

### `Event` — sharing resolved moments across the whole tree

"The moment this clip starts, fire that other overlay" is written with `Event`.

```rust
let cue = Event::named("chorus");

Timeline::builder()
    .child(Sequence::builder()
        .child(intro)
        .child(chorus.trigger_at_start(cue))  // fires when ITS resolved start arrives
        .build())
    .child(Reveal::builder().event(cue).build().fill())
    .build()

// Inside Reveal:
let burst = cue.phase(&clock, 0.0, 0.5).ease_out_cubic(0.0, 1.0);
```

The resolve pass bakes trigger times into a table, readable from every component via the `Clock`. An unfired event counts as `+∞` (its `phase` stays at 0).

### `Event::window` — holding the interval since firing as a window

`event.phase(&clock, a, b)`, like §1's `phase`, forgets the interval the moment it maps. When you want to stagger sub-events from the firing moment, or need seconds elapsed since it, build a `Window` based at the trigger time.

```rust
let reveal = cue.window(&clock, 0.0..8.0);  // the Window [fire+0s, fire+8s)
let ring_in = reveal.sub_secs(0.55..1.05).ease_in_out_expo(0.0, 1.0);
let ink = reveal.elapsed();                 // seconds since firing (freezes at close)
```

What you get back is a **clamped snapshot** (§4): before the window opens — and while unfired — it is `(start, end, start)`; once saturated it is constant at `(start, end, end)`. It can go straight into a component field, and once the animation finishes the cache key freezes too. An unfired event (`+∞` trigger time) does not panic; it simply behaves as "before start". The one caveat: unlike a raw `Window`, its `elapsed()` stops at the window's end — if you need endless accrual, make the interval generously long.

> **footgun ⑤**: `.trigger_*` attaches on the **outside**. Writing `x.at(5.0).trigger_at_start(e)` records the trigger at the start time handed to the trigger itself, ignoring the inner `.at(5.0)`. Correct is `x.trigger_at_start(e).at(5.0)`, or attaching it directly to a child whose position the container decides.
>
> **footgun ⑥**: the window of `.at(a..b)` is the half-open interval `[a, b)`. Adjacent clips never double-draw on the boundary frame.

## 8. Decision table

| Goal | Reach for |
|---|---|
| Drive a value by progress through an interval | `t.phase(a, b).ease_*(from, to)` |
| Carve sub-events in seconds inside a window | `t.window(a, b).sub_secs(r).ease_*(..)` |
| Motion that continues after the window closes (spin, drift) | `t.window(a, b).elapsed()` |
| Countdown to the close | `window.remaining()` |
| Entrance / exit fades | `window.envelope(fade_in, fade_out)` |
| Interval gate (draw nothing outside a range) | `t.during(a, b)` / `window.is_inside()` |
| Shuttle / repeat / sway | `t.cycle(p)` / `t.bounce(p)` / `t.wave(p)` |
| Frame-rate quantization (choppy look) | `t.fps(n)` |
| Custom curve (CSS cubic-bezier) | `Easing::CubicBezier` / `p.ease_bezier(..)` |
| Eased interpolation of `Vec2` / `Anchor` / `Color` … | `a.interpolate(b, p.eased(Easing::X))` |
| Passing into a component field | `Phase`, or `Window::clamped()` |
| Self-animation that follows the clip order | `clock.local()` / `clock.window()` |
| Reacting to another clip's resolved start | `Event` + `.trigger_*` + `event.phase(&clock, a, b)` |
| Firing-based window for stagger / elapsed seconds | `event.window(&clock, a..b)` |
| Fixed-length beat / trigger platform / reserving duration | `TimeBox` |
| Crop any component, including from its end | `.trim(a..b)` / `.trim(-a..-b)` |
| Fade or automate an audio contribution | `.fade_in(d)` / `.fade_out(d)` / `.gain_envelope(..)` |

## 9. Worked example — where the two worlds meet

The dot in `tellur-renderer/examples/timeline_to_mp4.rs` is the minimal form of the absolute-time world.

```rust
#[component(raster)]
fn BouncingDot(#[builder(into)] t: LocalTime) -> impl RasterComponent {
    let rx = t.bounce(2.5).linear(0.0, 1.0);   // periodic Phase → anchor ratio
    Frame::builder()
        .width(SizeMode::Fill)
        .height(SizeMode::Fixed(60.0))
        .child(
            circle
                .anchored(Anchor::CENTER)
                .snap_to(Anchor::new(rx, 0.5)),
        )
        .build()
}
```

Meanwhile `tellur-live`'s `timeline_showcase` lives in the placement-clock world, letting `clock.local()`-driven self-animation and `clock.global()`-driven whole-piece progression coexist on one screen. `demo_scene` is the absolute-time world, where every one of the `phase` / `window` / `clamped()` patterns appears. **Direct the choreography in absolute time; compose the structure with placement clocks.** The same split as layout's "direction in coordinates, structure in flow".
