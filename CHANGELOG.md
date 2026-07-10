# Changelog

All notable changes to tellur are documented in this file.
Versions are lockstep across the workspace.

## 0.2.1 (2026-07-10)

### Fixes

- **Cache font unit metrics at construction**

  `Font::vertical_metrics` reparsed the entire `rustybuzz::Face` on every call, even on shape-cache hits, making text-heavy renders CPU-bound.  
  The metrics are face constants, so they are now read once when the font is constructed and only scaled per call.


## 0.2.0 (2026-07-09)

### Breaking Changes

- **The write-on effect now draws at a constant speed per path by default**

  Previously, the timing was controlled to write the stroke at a constant overall rate, which sometimes caused the animation to appear stuck on characters with longer circumferences.  
  To resolve this, we have introduced .per_path() to draw at a constant speed for each path and .by_length() for the conventional drawing method. The former is now the default setting.

  The default per-path slot is now 0.17s (was 0.25s), and TimedWrite caps pen speed via `.max_stroke_speed()` (default 2400 units/sec) so long glyphs do not race ahead. Fill timing stays on the nominal slot even when a stroke is slowed by the cap.

### Features

- Added a SKILL (/tellur-authoring) for AI agents

- **Added a rich startup banner to Tellur Live**

  Tellur Live now prints a Vite-style startup banner after the preview server is ready. It shows the plugin path, watch paths when auto-rebuild is enabled, host binding, CPU and GPU details, memory budgets, and a clickable hint to open the preview URL in your browser.

### Fixes

- **Excluded line_gap from text intrinsic height**

  This prevents excessive bottom margins when a single line of text is enclosed in a box or similar element.  
  Information regarding line_gap is now provided via `Font::vertical_metrics`, which has been made a public API.

- **Fixed a memory error occurring during plugin execution**

  When the versions of crates providing types across the host/plugin dylib boundary did not match, it was not being detected, leading to errors such as `free(): invalid pointer`.  
  Included fingerprints for crates that cross boundaries and ensured that the startup is rejected if a mismatch is detected.

- Fixed an issue in Tellur Live where errors were repeatedly displayed in the console when zooming in on the timeline

- Fixed an issue in Tellur Live where "TypeError: Failed to fetch" continues to be displayed even after reconnection


## 0.1.0 (2026-07-07)

### Features

- Initial public release of the tellur workspace crates.
