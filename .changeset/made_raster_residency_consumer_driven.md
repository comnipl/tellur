---
default: major
---

# Moved CPU/GPU raster residency control to render callers

Raster consumers now request CPU- or GPU-resident component outputs from the render context. Uploads and readbacks are therefore render-context responsibilities, and uploaded GPU representations of raster components participate in the main component cache's frequency-aware admission and eviction instead of living in an independent upload cache.

Timeline components propagate the residency received by `frame` into the main component cache, so custom components must forward that request when rendering descendants.

For example, a CPU consumer now requests its output representation directly:

```diff
- let frame = timeline.frame(t, target, &mut ctx);
+ let frame = timeline.frame(t, target, RasterResidency::Cpu, &mut ctx);
```

Component implementations receive the same request and forward it when rendering descendants:

```diff
 fn render(
     &self,
     size: Vec2,
     target: Resolution,
+    residency: RasterResidency,
     ctx: &mut dyn RenderContext,
 ) -> RasterImage {
     // ...
-    ctx.render(self.child.as_ref(), size, target)
+    ctx.render(self.child.as_ref(), size, target, residency)
 }
```

The `RasterComponent`, `RenderContext`, `GpuRasterBackend`, `TimelineComponent`, `Timeline`, and `TimelineCollection` interfaces changed, so live-preview plugins now use the v5 entry symbol and must be rebuilt.

The independent upload-cache counters and live-preview diagnostic headers were removed; GPU representation usage is now reported through the main component-cache and VRAM metrics.
