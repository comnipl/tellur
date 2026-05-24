//! Compare 60 / 30 / 24 / 16 fps quantization on a back-and-forth dot.
//!
//! Renders a 5-second timeline where four blue dots bounce horizontally.
//! Each dot is a `BouncingDot` whose motion is driven by the time fed to
//! it; the four instances receive the same `t` quantized to different
//! framerates via `Time::fps`. A vertical `Stack` distributes the four
//! tracks evenly inside the scene, with a `DecoratedBox` painting the
//! dark background.

use std::path::Path;

use tellur_core::color::Color;
use tellur_core::geometry::{EdgeInsets, Vec2};
use tellur_core::layer::VectorLayer;
use tellur_core::layout::{Axis, CrossAlign, MainAlign, Stack, VectorLayoutExt};
use tellur_core::placement::VectorPlacement;
use tellur_core::raster::{RasterComponent, Resolution};
use tellur_core::shapes::Circle;
use tellur_core::time::{LocalTime, Time};
use tellur_core::timeline::timeline;
use tellur_core::vector::{Paint, VectorComponent};
use tellur_core::vector_component;
use tellur_renderer::{FfmpegEncoder, Rasterizable};

/// A circle that triangle-wave scrubs left-to-right-to-left across a track
/// of `scene_width`, with one full round trip per `PERIOD` seconds. The
/// motion is driven entirely by `t` — a `LocalTime` clock that the
/// component reads independently of the global timeline. Callers can pass
/// `TimelineTime` directly via `.into()` since it converts to `LocalTime`.
#[vector_component]
fn BouncingDot(t: LocalTime, scene_width: f32) -> impl VectorComponent {
    let (phase, _) = t.bounce(2.5);
    let size = Vec2(scene_width - 200.0, 60.0);
    let x = phase.interpolate(0.0, 1.0) * size.0;

    VectorLayer {
        size,
        children: vec![Circle {
            radius: 30.0,
            fill: Paint::Solid(Color::hsl(200.0, 0.7, 0.6)).into(),
            stroke: None,
        }
        .anchored(tellur_core::geometry::Anchor::CENTER)
        .snap_to(Vec2(x, size.1 * 0.5))],
    }
}

fn main() {
    let scene_size = Vec2(1280.0, 720.0);
    let tl = timeline(5.0, move |t, target| {
        let track = |fps: u32| {
            BouncingDot {
                t: t.fps(fps).into(),
                scene_width: scene_size.0,
            }
            .boxed()
        };

        let scene = Stack {
            axis: Axis::Vertical,
            size: Some(scene_size),
            spacing: 0.0,
            main_align: MainAlign::SpaceEvenly,
            cross_align: CrossAlign::Center,
            children: vec![track(60), track(30), track(24), track(16)],
        }
        .padding(EdgeInsets::all(100.0))
        .background(Paint::Solid(Color::rgb_u8(20, 20, 30)));

        scene.rasterize().render(target)
    });

    let out = Path::new("/tmp/timeline.mp4");
    FfmpegEncoder::new(Resolution::new(1920, 1080), 60)
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "20"])
        .encode(&tl, out)
        .expect("encode mp4");

    println!("Wrote {}", out.display());
}
