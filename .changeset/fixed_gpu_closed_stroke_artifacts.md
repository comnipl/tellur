---
default: patch
---

# Fixed GPU artifacts on explicitly closed strokes

Closed vector paths whose final segment already returns to the subpath start no longer produce detached bands during GPU rasterization. Affected strokes preserve their closed joins while remaining within the same painted footprint as CPU rendering.
