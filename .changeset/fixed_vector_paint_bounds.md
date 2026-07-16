---
default: major
---

# Fixed clipped and distorted vector decorations

Vector fragments and stroked paths now preserve their full painted extent during rasterization, preventing overflowing shadows, rounded borders, and path strokes from being stretched or clipped at component edges. The correction is applied consistently by both CPU and GPU renderers.

Existing projects that contain affected decorations may produce different video pixels after upgrading, so regenerate and review reference renders or snapshots.
