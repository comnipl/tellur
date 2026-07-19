---
default: major
---

# Made component trait objects cloneable

`Box<dyn RasterComponent>`, `Box<dyn VectorComponent>`, and `Box<dyn TimelineComponent>` now implement `Clone`, enabling function-form `#[component]` definitions to accept boxed effects and children and allowing custom wrappers to clone boxed descendants. The canonical `Box<dyn TimelineComponent + Send>` form is cloneable too, so composable timeline effects can be passed through erased fields:

```rust
#[component(timeline)]
fn ProcessedAudio(
    source: Box<dyn TimelineComponent + Send>,
) -> impl TimelineComponent {
    Timeline::builder().child(source).build()
}

let source: Box<dyn TimelineComponent + Send> = AudioFile::builder()
    .path("voice.wav")
    .fade_in(0.2)
    .fade_out(0.2)
    .into();

let track = ProcessedAudio::builder().source(source).build();
```

This changes the component implementation contract: custom raster, vector, and timeline component types must support cloning, normally with `#[derive(Clone)]`. Generic wrappers may also need an explicit `T: Clone` bound.

Live-preview plugins now use the v9 entry symbol and must be rebuilt.
