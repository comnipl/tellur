# Changelog

All notable changes to tellur are documented in this file.
Versions are lockstep across the workspace.

## 0.3.0 (2026-07-19)

### Breaking Changes

- **Added configurable stroke caps and joins**

  Vector strokes can now select their endpoint caps, segment joins, and miter limit:

  ```rust
  let stroke = Stroke::new(color, 4.0)
      .with_cap(StrokeCap::Butt)
      .with_join(StrokeJoin::Miter)
      .with_miter_limit(4.0);
  ```

  `Stroke::new` defaults to `StrokeCap::Round` and `StrokeJoin::Round`, preserving the GPU renderer's existing appearance. CPU and GPU rendering now use the same explicit style and miter threshold. This intentionally changes CPU output that previously inherited tiny-skia's implicit butt-cap and miter-join defaults. Stroke-based outline extraction also preserves the configured style and dash gaps.

  `Stroke` is now non-exhaustive. Replace direct struct construction with `Stroke::new(paint, width)` and the corresponding `with_*` methods, and add `..` to destructuring patterns. The live-preview plugin ABI is now v8, so rebuild plugins before loading them into an updated host.

- **Added composable timeline trimming and audio envelopes**

  Timeline components now support `trim`, while audio components support `fade_in`, `fade_out`, and `gain_envelope` directly in builder chains. Trim ranges use the immediate child's local clock, with negative endpoints counted from its end and open endpoints resolved to the exact child end.

  Temporal effects wrap in call order, so changing their order changes the result:

  ```rust
  source.fade_in(1.0).trim(0.5..); // starts halfway through the existing fade
  source.trim(0.5..).fade_in(1.0); // starts a new fade from silence
  ```

  `AudioFile` effects now finish the source builder immediately: place `path`, `gain`, and `duration` before the first effect, omit a trailing `.build()`, and keep `.fill()` outermost. Custom audio components must implement `render_audio_block` instead of `samples` or `mix_into`.

- **Fixed clipped and distorted vector decorations**

  Vector fragments and stroked paths now preserve their full painted extent during rasterization, preventing overflowing shadows, rounded borders, and path strokes from being stretched or clipped at component edges. The correction is applied consistently by both CPU and GPU renderers.

  Existing projects that contain affected decorations may produce different video pixels after upgrading, so regenerate and review reference renders or snapshots.

- **Increased timeline time precision**

  Timeline and local times now use `f64` seconds, preserving distinct frame and audio-sample positions in long projects. `Phase`, drawing values, gains, and PCM samples remain `f32`.

  Unsuffixed numeric literals need no changes, but explicit second values and custom `Time` or `TimelineComponent` implementations must migrate their timeline-facing types from `f32` to `f64`.

  Live-preview plugins now use the v6 entry symbol and must be rebuilt.

- **Made component trait objects cloneable**

  `Box<dyn RasterComponent>`, `Box<dyn VectorComponent>`, and `Box<dyn TimelineComponent>` now implement `Clone`, enabling function-form `#[component]` definitions to accept boxed effects and children and allowing custom wrappers to clone boxed descendants. The canonical `Box<dyn TimelineComponent + Send>` form is cloneable too, so composable timeline effects can be passed through erased fields:

  ```rust
  #[component(timeline)]
  fn ProcessedAudio(
      source: Box<dyn TimelineComponent + Send>,
  ) -> impl TimelineComponent {
      Timeline::builder().child(source).build()
  }

  let source: Box<dyn TimelineComponent + Send> = AudioFile::builder()
      .path("voice.wav")
      .fade_in(0.2)
      .fade_out(0.2)
      .into();

  let track = ProcessedAudio::builder().source(source).build();
  ```

  This changes the component implementation contract: custom raster, vector, and timeline component types must support cloning, normally with `#[derive(Clone)]`. Generic wrappers may also need an explicit `T: Clone` bound.

  Live-preview plugins now use the v9 entry symbol and must be rebuilt.

- **Moved CPU/GPU raster residency control to render callers**

  Raster consumers now request CPU- or GPU-resident component outputs from the render context. Uploads and readbacks are therefore render-context responsibilities, and uploaded GPU representations of raster components participate in the main component cache's frequency-aware admission and eviction instead of living in an independent upload cache.

  Timeline components propagate the residency received by `frame` into the main component cache, so custom components must forward that request when rendering descendants.

  For example, a CPU consumer now requests its output representation directly:

  ```diff
  - let frame = timeline.frame(t, target, &mut ctx);
  + let frame = timeline.frame(t, target, RasterResidency::Cpu, &mut ctx);
  ```

  Component implementations receive the same request and forward it when rendering descendants:

  ```diff
   fn render(
       &self,
       size: Vec2,
       target: Resolution,
  +    residency: RasterResidency,
       ctx: &mut dyn RenderContext,
   ) -> RasterImage {
       // ...
  -    ctx.render(self.child.as_ref(), size, target)
  +    ctx.render(self.child.as_ref(), size, target, residency)
   }
  ```

  The `RasterComponent`, `RenderContext`, `GpuRasterBackend`, `TimelineComponent`, `Timeline`, and `TimelineCollection` interfaces changed, so live-preview plugins now use the v5 entry symbol and must be rebuilt.

  The independent upload-cache counters and live-preview diagnostic headers were removed; GPU representation usage is now reported through the main component-cache and VRAM metrics.

- **Removed the legacy closure timeline API**

  The old `tellur_core::timeline::{Timeline, timeline}` closure API, its `LegacyTimeline` adapter, and `export_legacy_timeline!` have been removed. Replace closure scenes with function-form `#[component(timeline)]` components, read time from an injected `#[clock] Clock`, and export the resulting `TimelineComponent` tree with `export_timeline!`:

  ```rust
  #[component(timeline)]
  fn Main(#[clock] clock: Clock) -> impl TimelineComponent {
      let scene = Scene::builder().time(clock.global()).build();
      Timeline::builder()
          .child(scene.at(0.0..5.0))
          .build()
  }

  export_timeline!(
      root = Main::builder().build(),
      title = "Main",
  );
  ```

  `export_timeline!` now takes the root component expression directly instead of
  a zero-argument factory function. `title` is the human-readable label. A single
  exported timeline uses the machine-facing lookup id `"main"` by default; add
  `id = "..."` after `title` only when a different stable id is required.

  For offline rendering, pass the built tree through `resolve` or
  `resolve_with_canvas`, then give the resulting `&ResolvedTimeline` to
  `FfmpegEncoder::encode`. The former `encode_timeline` method has been folded
  into `encode`, which muxes the resolved audio by default; select
  `AudioExport::Omit` when the output should contain video only.

- **Unified placement vocabulary and added Stack containers**

  `Positioned` now snaps a child anchor to either an absolute `Vec2` point or an `Anchor` on its resolved parent box, with a shared `.offset(Vec2)` adjustment. `Alignment`, `Anchor::to`, and `Frame.align` have been removed; wrap a child with `.anchored(child_anchor).snap_to(parent_anchor)` for box-relative placement. Direct `Positioned` construction must use its new `child`, `anchor`, `target`, and `offset` representation.

  Vector and raster `Stack` containers now size themselves from one required `base` child, lay out `under` and `over` children tightly at that resolved size, preserve overlay paint-bounds overflow, and paint in `under → base → over` order.

### Fixes

- **Fixed GPU artifacts on explicitly closed strokes**

  Closed vector paths whose final segment already returns to the subpath start no longer produce detached bands during GPU rasterization. Affected strokes preserve their closed joins while remaining within the same painted footprint as CPU rendering.

- **Made component caching reuse-aware**

  Component outputs are now admitted after their second observation instead of immediately. Cached CPU and GPU rasters are retained according to decayed reuse frequency, observed rerendering cost, and byte size, reducing memory spent on one-off results while preserving entries that avoid the most rendering work.

- **Removed the redundant child-composition cache**

  The cache only skipped the final blend after independently cached child images had already been rendered, while retaining both its inputs and output outside the main component cache. That limited reuse did not justify its memory and lifecycle cost, so child batches are now recomposited when needed.

- **Removed the timeline composite cache**

  Real-project testing showed no meaningful rendering speedup from reusing completed timeline composites, while the global cache retained its input and output rasters outside the render context's lifecycle and memory accounting. Timeline frame batches are now recomposited when needed.


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
