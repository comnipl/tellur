---
default: major
---

# Removed the legacy closure timeline API

The old `tellur_core::timeline::{Timeline, timeline}` closure API, its `LegacyTimeline` adapter, and `export_legacy_timeline!` have been removed. Replace closure scenes with function-form `#[component(timeline)]` components, read time from an injected `#[clock] Clock`, and export the resulting `TimelineComponent` tree with `export_timeline!`:

```rust
#[component(timeline)]
fn Main(#[clock] clock: Clock) -> impl TimelineComponent {
    let scene = Scene::builder().time(clock.global()).build();
    Timeline::builder()
        .child(scene.at(0.0..5.0))
        .build()
}

export_timeline!(
    root = Main::builder().build(),
    title = "Main",
);
```

`export_timeline!` now takes the root component expression directly instead of
a zero-argument factory function. `title` is the human-readable label. A single
exported timeline uses the machine-facing lookup id `"main"` by default; add
`id = "..."` after `title` only when a different stable id is required.

For offline rendering, pass the built tree through `resolve` or
`resolve_with_canvas`, then give the resulting `&ResolvedTimeline` to
`FfmpegEncoder::encode`. The former `encode_timeline` method has been folded
into `encode`, which muxes the resolved audio by default; select
`AudioExport::Omit` when the output should contain video only.
