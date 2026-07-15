---
default: major
---

# Made raster residency consumer-driven

Raster consumers now request CPU- or GPU-resident component outputs from the render context. Uploads and readbacks are therefore render-context responsibilities, and uploaded GPU representations of raster components participate in the main component cache's frequency-aware admission and eviction instead of living in an independent upload cache.

Timeline components propagate the residency received by `frame` into the main component cache, so custom components must forward that request when rendering descendants.

The `RasterComponent`, `RenderContext`, `GpuRasterBackend`, `TimelineComponent`, `Timeline`, and `TimelineCollection` interfaces changed, so live-preview plugins now use the v5 entry symbol and must be rebuilt.

The independent upload-cache counters and live-preview diagnostic headers were removed; GPU representation usage is now reported through the main component-cache and VRAM metrics.
