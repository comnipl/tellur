---
default: patch
---

# Excluded line_gap from text intrinsic height

This prevents excessive bottom margins when a single line of text is enclosed in a box or similar element.  
Information regarding line_gap is now provided via `Font::vertical_metrics`, which has been made a public API.
