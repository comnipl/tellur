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
cargo build -p tellur-live --example demo_timeline_plugin
```

Cargo writes it to:

```text
target/debug/examples/libdemo_timeline_plugin.so
```

## Run the Preview Host

```sh
cargo run -p tellur-live -- serve \
  --plugin target/debug/examples/libdemo_timeline_plugin.so \
  --host 127.0.0.1 \
  --port 4317 \
  --size 1280x720 \
  --fps 30
```

Open `http://127.0.0.1:4317/` for the minimal browser client.
Use `--host 0.0.0.0` when the preview server should be reachable from other
devices on the network.
Pass `--verbose` to print per-frame timing and cache statistics to stdout.

The browser UI is intentionally a thin validation client. It requests
coalesced PNG frames for still previews and seeking, and fragmented MP4/H.264
for playback. The Size and FPS controls lower the request resolution and frame
rate when full-resolution playback is too expensive. While idle, the client
preloads the beginning of the MP4 stream for the current position so the play
button can reuse already-buffered video data.

## HTTP Endpoints

- `GET /api/info` returns resolution, fps, hot-reload errors, and timeline
  metadata.
- `GET /api/frame?time=1.25&timeline=main` returns one PNG frame.
- `GET /api/frame?frame=42&timeline=main` returns one PNG frame by frame index.
- `GET /api/frame?time=1.25&timeline=main&format=rgba` returns raw RGBA8 bytes
  with `X-Tellur-Width` / `X-Tellur-Height` headers.
- `GET /api/video.mp4?time=1.25&timeline=main&fps=60&gop=12&crf=23`
  streams fragmented MP4/H.264 through `ffmpeg`. The browser client uses this
  path for playback so `<video>` handles decode and presentation timing.
  Frame and stream endpoints also accept `width=<pixels>&height=<pixels>` or
  `scale=<ratio>` to override the default preview resolution.
- `GET /api/stream?time=0&timeline=main&fps=30` returns a simple multipart PNG
  stream. This endpoint is useful for experiments.
