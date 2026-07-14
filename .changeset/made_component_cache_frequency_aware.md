---
default: patch
---

# Made component caching reuse-aware

Component outputs are now admitted after their second observation instead of immediately. Cached CPU and GPU rasters are retained according to decayed reuse frequency, observed rerendering cost, and byte size, reducing memory spent on one-off results while preserving entries that avoid the most rendering work.
