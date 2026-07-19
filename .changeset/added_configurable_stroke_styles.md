---
default: major
---

# Added configurable stroke caps and joins

Vector strokes can now select their endpoint caps, segment joins, and miter limit:

```rust
let stroke = Stroke::new(color, 4.0)
    .with_cap(StrokeCap::Butt)
    .with_join(StrokeJoin::Miter)
    .with_miter_limit(4.0);
```

`Stroke::new` defaults to `StrokeCap::Round` and `StrokeJoin::Round`, preserving the GPU renderer's existing appearance. CPU and GPU rendering now use the same explicit style and miter threshold. This intentionally changes CPU output that previously inherited tiny-skia's implicit butt-cap and miter-join defaults. Stroke-based outline extraction also preserves the configured style and dash gaps.

`Stroke` is now non-exhaustive. Replace direct struct construction with `Stroke::new(paint, width)` and the corresponding `with_*` methods, and add `..` to destructuring patterns. The live-preview plugin ABI is now v8, so rebuild plugins before loading them into an updated host.
