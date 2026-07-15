---
default: major
---

# Increased timeline time precision

Timeline and local times now use `f64` seconds, preserving distinct frame and audio-sample positions in long projects. `Phase`, drawing values, gains, and PCM samples remain `f32`.

Unsuffixed numeric literals need no changes, but explicit second values and custom `Time` or `TimelineComponent` implementations must migrate their timeline-facing types from `f32` to `f64`.

Live-preview plugins now use the v6 entry symbol and must be rebuilt.
