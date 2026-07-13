---
default: patch
---

# Removed the timeline composite cache

Real-project testing showed no meaningful rendering speedup from reusing completed timeline composites, while the global cache retained its input and output rasters outside the render context's lifecycle and memory accounting. Timeline frame batches are now recomposited when needed.
