//! Compare 60 / 30 / 15 / 10 fps quantization on a back-and-forth dot.
//!
//! Renders a 5-second timeline where four blue dots bounce horizontally.
//! Each dot is a `BouncingDot` whose motion is driven by the time fed to
//! it; the four instances receive the same `t` quantized to different
//! framerates via `Time::fps`. The instances are stacked vertically with
//! `Anchor` so the stutter at lower fps is directly comparable against the
//! smooth top track.

use std::path::Path;

use tellur_core::color::Color;
use tellur_core::geometry::{Anchor, Vec2};
use tellur_core::layer::VectorLayer;
use tellur_core::placement::VectorPlacement;
use tellur_core::raster::{RasterComponent, Resolution};
use tellur_core::shapes::{Circle, Rectangle};
use tellur_core::time::{LocalTime, Time};
use tellur_core::timeline::timeline;
use tellur_core::vector::Paint;
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

    VectorLayer {
        size,
        children: vec![Circle {
            radius: 30.0,
            fill: Paint::Solid(Color::hsl(200.0, 0.7, 0.6)).into(),
            stroke: None,
        }
        .anchored(Anchor::CENTER)
        .snap_to(Anchor::new(phase.interpolate(0.0, 1.0), 0.5).point(size))],
    }
}

fn main() {
    let scene_size = Vec2(1280.0, 720.0);
    let tl = timeline(5.0, move |t, target| {
        let scene = VectorLayer {
            size: scene_size,
            children: vec![
                Rectangle {
                    size: scene_size,
                    fill: Paint::Solid(Color::rgb_u8(20, 20, 30)).into(),
                    stroke: None,
                }
                .at(Vec2::ZERO),
                BouncingDot {
                    t: t.fps(60).into(),
                    scene_width: scene_size.0,
                }
                .anchored(Anchor::CENTER)
                .snap_to(Anchor::new(0.5, 0.125).point(scene_size)),
                BouncingDot {
                    t: t.fps(30).into(),
                    scene_width: scene_size.0,
                }
                .anchored(Anchor::CENTER)
                .snap_to(Anchor::new(0.5, 0.375).point(scene_size)),
                BouncingDot {
                    t: t.fps(24).into(),
                    scene_width: scene_size.0,
                }
                .anchored(Anchor::CENTER)
                .snap_to(Anchor::new(0.5, 0.625).point(scene_size)),
                BouncingDot {
                    t: t.fps(16).into(),
                    scene_width: scene_size.0,
                }
                .anchored(Anchor::CENTER)
                .snap_to(Anchor::new(0.5, 0.875).point(scene_size)),
            ],
        };

        scene.rasterize().render(target)
    });

    let out = Path::new("/tmp/timeline.mp4");
    FfmpegEncoder::new(Resolution::new(1920, 1080), 60)
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "20"])
        .encode(&tl, out)
        .expect("encode mp4");

    println!("Wrote {}", out.display());
}
