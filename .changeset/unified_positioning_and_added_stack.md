---
default: major
---

# Unified placement vocabulary and added Stack containers

`Positioned` now snaps a child anchor to either an absolute `Vec2` point or an `Anchor` on its resolved parent box, with a shared `.offset(Vec2)` adjustment. `Alignment`, `Anchor::to`, and `Frame.align` have been removed; wrap a child with `.anchored(child_anchor).snap_to(parent_anchor)` for box-relative placement. Direct `Positioned` construction must use its new `child`, `anchor`, `target`, and `offset` representation.

Vector and raster `Stack` containers now size themselves from one required `base` child, lay out `under` and `over` children tightly at that resolved size, preserve overlay paint-bounds overflow, and paint in `under → base → over` order.
