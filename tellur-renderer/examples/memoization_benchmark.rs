//! Compare `CachingRenderContext` against `PassThrough` on the same
//! timeline, with no ffmpeg subprocess in the loop, so the wall-clock
//! difference reflects the rendering pipeline only.
//!
//! Renders the same `timeline_to_mp4` scene twice — once through the
//! bounded LRU cache, once through `PassThrough` — and prints
//! per-frame and total times for both. Cache hits should make the
//! cached run noticeably faster on the static blur subtree
//! (`DropShadow`), while time-varying nodes (`Stack`, `Padding`,
//! `BouncingDot { t }`) hit the cache rarely or not at all.

use std::time::Instant;

use tellur_core::color::Color;
use tellur_core::geometry::{Anchor, EdgeInsets, Vec2};
use tellur_core::layout::raster::{Frame, RasterLayoutExt, Stack};
use tellur_core::layout::{Axis, CrossAlign, MainAlign, SizeMode};
use tellur_core::raster::{RasterComponent, Resolution};
use tellur_core::raster_component;
use tellur_core::render_context::{PassThrough, RenderContext};
use tellur_core::shapes::Circle;
use tellur_core::time::{LocalTime, Time, TimelineTime};
use tellur_core::timeline::{timeline, Timeline};
use tellur_core::vector::Paint;
use tellur_renderer::{CachingRenderContext, DropShadow, Rasterizable};

#[raster_component]
fn BouncingDot(t: LocalTime) -> impl RasterComponent {
    let (phase, _) = t.bounce(2.5);
    let rx = phase.interpolate(0.0, 1.0);
    let radius = 30.0;
    Frame {
        width: SizeMode::Fill,
        height: SizeMode::Fixed(60.0),
        child_anchor: Anchor::CENTER,
        at: Anchor::new(rx, 0.5),
        child: DropShadow {
            offset: Vec2(0.0, 8.0),
            blur: 4.0,
            color: Color::rgba_u8(255, 255, 255, 100),
            child: Circle {
                radius,
                fill: Paint::Solid(Color::hsl(200.0, 0.7, 0.6)).into(),
                stroke: None,
            }
            .rasterize()
            .boxed(),
        }
        .boxed(),
    }
}

fn bench(
    label: &str,
    tl: &impl Timeline,
    resolution: Resolution,
    fps: u32,
    total_frames: u64,
    ctx: &mut dyn RenderContext,
) {
    let start = Instant::now();
    for frame_idx in 0..total_frames {
        let t = TimelineTime::new(frame_idx as f32 / fps as f32);
        // Pull the image into a local so the optimizer can't elide the
        // render — `bytes::Bytes` clone is cheap so this doesn't skew
        // the comparison.
        let _image = tl.build(t, resolution, ctx);
    }
    let elapsed = start.elapsed();
    let per_frame_ms = elapsed.as_secs_f64() * 1000.0 / total_frames as f64;
    println!(
        "{label:<22}  {:>7.2}s total  ({:>6.2} ms / frame, {:>6.2} fps)",
        elapsed.as_secs_f64(),
        per_frame_ms,
        1000.0 / per_frame_ms,
    );
}

fn main() {
    let scene_size = Vec2(1280.0, 720.0);
    let resolution = Resolution::new(1920, 1080);
    let fps = 60u32;
    let duration = 5.0f32;
    let total_frames = (duration * fps as f32).ceil() as u64;

    let tl = timeline(duration, move |t, target, ctx| {
        Stack {
            axis: Axis::Vertical,
            size: None,
            spacing: 0.0,
            main_align: MainAlign::SpaceEvenly,
            cross_align: CrossAlign::Stretch,
            children: vec![
                BouncingDot {
                    t: t.fps(60).into(),
                }
                .boxed(),
                BouncingDot {
                    t: t.fps(30).into(),
                }
                .boxed(),
                BouncingDot {
                    t: t.fps(24).into(),
                }
                .boxed(),
                BouncingDot {
                    t: t.fps(16).into(),
                }
                .boxed(),
            ],
        }
        .padding(EdgeInsets::all(100.0))
        .background(Color::rgb_u8(20, 20, 30))
        .render(scene_size, target, ctx)
    });

    println!(
        "Rendering {} frames at {}x{} ({} fps, {:.1}s timeline)\n",
        total_frames, resolution.width, resolution.height, fps, duration,
    );

    // PassThrough first: no warmup state to carry between runs, so this
    // gives a clean baseline. CachingRenderContext after, with its
    // cache cold at the start (mirroring how a real export starts).
    let mut pass_ctx = PassThrough;
    bench(
        "PassThrough",
        &tl,
        resolution,
        fps,
        total_frames,
        &mut pass_ctx,
    );

    let mut cache_ctx = CachingRenderContext::new();
    bench(
        "CachingRenderContext",
        &tl,
        resolution,
        fps,
        total_frames,
        &mut cache_ctx,
    );

    println!();
    print!("{}", cache_ctx.metrics());
}
