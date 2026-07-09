---
default: major
---

# The write-on effect now draws at a constant speed per path by default

Previously, the timing was controlled to write the stroke at a constant overall rate, which sometimes caused the animation to appear stuck on characters with longer circumferences.  
To resolve this, we have introduced .per_path() to draw at a constant speed for each path and .by_length() for the conventional drawing method. The former is now the default setting.

The default per-path slot is now 0.17s (was 0.25s), and TimedWrite caps pen speed via `.max_stroke_speed()` (default 2400 units/sec) so long glyphs do not race ahead. Fill timing stays on the nominal slot even when a stroke is slowed by the cap.
