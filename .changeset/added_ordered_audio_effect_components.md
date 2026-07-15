---
default: major
---

# Added composable timeline trimming and audio envelopes

Timeline components now support `trim`, while audio components support `fade_in`, `fade_out`, and `gain_envelope` directly in builder chains. Trim ranges use the immediate child's local clock, with negative endpoints counted from its end and open endpoints resolved to the exact child end.

Temporal effects wrap in call order, so changing their order changes the result:

```rust
source.fade_in(1.0).trim(0.5..); // starts halfway through the existing fade
source.trim(0.5..).fade_in(1.0); // starts a new fade from silence
```

`AudioFile` effects now finish the source builder immediately: place `path`, `gain`, and `duration` before the first effect, omit a trailing `.build()`, and keep `.fill()` outermost. Custom audio components must implement `render_audio_block` instead of `samples` or `mix_into`.
