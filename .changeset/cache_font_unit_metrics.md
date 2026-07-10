---
default: patch
---

# Cache font unit metrics at construction

`Font::vertical_metrics` reparsed the entire `rustybuzz::Face` on every call, even on shape-cache hits, making text-heavy renders CPU-bound.  
The metrics are face constants, so they are now read once when the font is constructed and only scaled per call.
