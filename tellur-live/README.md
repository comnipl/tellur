# tellur-live

`tellur-live` is a local preview host for editing tellur timelines. It loads a
timeline plugin from a Rust `cdylib`, keeps the render process alive across
frame requests, and reuses one `CachingRenderContext` for the session.

The dynamic-library boundary is a Rust-internal ABI. Build the host and plugin
from the same workspace/toolchain; this is not intended as a stable C ABI.

## Build a Plugin

```rust
use tellur_core::timeline::{timeline, Timeline};

fn build_timeline() -> impl Timeline {
    timeline(5.0, move |t, target, ctx| {
        // build and render a RasterComponent here
        todo!()
    })
}

tellur_live::export_timeline!("main", "Main", build_timeline);
```

The bundled demo plugin can be built with:

```sh
cargo build --release -p tellur-live --example demo_timeline_plugin
```

Cargo writes it to:

```text
target/release/examples/libdemo_timeline_plugin.so
```

## Run the Preview Host

```sh
cargo run -p tellur-live -- serve \
  -p tellur-live \
  --example demo_timeline_plugin \
  --host 127.0.0.1 \
  --port 4317 \
  --fps 30
```

Open `http://127.0.0.1:4317/` for the minimal browser client.
Use `--host 0.0.0.0` when the preview server should be reachable from other
devices on the network.
Pass `--verbose` to print per-frame timing and cache statistics to stdout.

Passing `-p <package> --example <example>` makes `tellur-live` infer the release
cdylib path (`target/release/examples/lib<example>.so`) and run
`cargo build --release -p <package> --example <example>` when watched source
files change. `--examples` is accepted as an alias for `--example`.

```sh
cargo run -p tellur-live -- serve \
  -p tellur-live \
  --examples demo_timeline_plugin
```

By default, watch paths are inferred from the package: its `Cargo.toml`, `src`,
the selected example file, the workspace lockfile/manifest, and local `path`
dependencies. Use `--plugin <path>` or repeated `--watch-path <path>` arguments
to override those inferred values.

When a release build succeeds and the cdylib contents change, `tellur-live`
reloads the plugin, clears the server render cache, and publishes a new
`cacheKey` to the browser. The browser uses that key in image/video URLs,
stores media responses as blobs in IndexedDB, and records the green cache
ranges separately. Old IndexedDB media entries and green ranges are revoked
only after a successful cdylib update. Failed builds leave the previous plugin
and cache key in place. Video cache entries are variable-length ranges. Starting
playback inside a cached range seeks within that blob instead of creating a
duplicate cache entry. Missing video ranges fall back to direct streaming
immediately; playback does not wait for IndexedDB cache fill. During playback
the client scans the continuous cached range from the current position and
starts one background stream from the next cache gap. When that stream finishes,
its full range is saved to IndexedDB. When stopped, it fills only the next
three seconds from the current position.

The browser UI is intentionally a thin validation client. It requests
coalesced PNG frames for still previews and seeking, and fragmented MP4/H.264
for playback. The Size and FPS controls lower the request resolution and frame
rate when full-resolution playback is too expensive. The Size control sends an
explicit `width` and `height` selected from browser presets, including low,
HD, 4K, and vertical variants. While idle, the client
preloads the beginning of the MP4 stream for the current position so the play
button can reuse already-buffered video data.

## HTTP Endpoints

- `GET /api/info` returns resolution, fps, the current media `cacheKey`,
  compile status (`compiled`, `compiling`, or `failed`), hot-reload errors, and
  timeline metadata.
- `GET /api/events` streams the same info payload as Server-Sent Events. The
  browser client uses this instead of polling `/api/info`.
- `GET /api/frame?time=1.25&timeline=main` returns one PNG frame.
- `GET /api/frame?frame=42&timeline=main` returns one PNG frame by frame index.
- `GET /api/frame?time=1.25&timeline=main&format=rgba` returns raw RGBA8 bytes
  with `X-Tellur-Width` / `X-Tellur-Height` headers.
- `GET /api/video.mp4?time=1.25&timeline=main&fps=60&gop=12&crf=23`
  streams fragmented MP4/H.264 through `ffmpeg`. The browser client uses this
  path for playback so `<video>` handles decode and presentation timing.
  `duration=<seconds>` limits the generated stream length and is used for
  IndexedDB video cache segments.
  Frame and stream endpoints also accept `width=<pixels>&height=<pixels>` or
  `scale=<ratio>` to override the default preview resolution.
- `GET /api/stream?time=0&timeline=main&fps=30` returns a simple multipart PNG
  stream. This endpoint is useful for experiments.
