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
use tellur_core::raster::{RasterComponent, Resolution};
use tellur_core::shapes::{Circle, Rectangle};
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
    const PERIOD: f32 = 2.5;
    const RADIUS: f32 = 30.0;
    const SIDE_PADDING: f32 = 40.0;

    let (phase, _) = t.bounce(PERIOD);
    let view = Vec2(scene_width, RADIUS * 2.0);
    let center_y = view.1 * 0.5;
    let target = phase.interpolate(
        Vec2(SIDE_PADDING + RADIUS, center_y),
        Vec2(scene_width - SIDE_PADDING - RADIUS, center_y),
    );

    let circle = Circle {
        radius: RADIUS,
        fill: Paint::Solid(Color::hsl(200.0, 0.7, 0.6)).into(),
        stroke: None,
    };

    VectorLayer {
        size: view,
        children: vec![(
            circle.view_box().anchor(Anchor::CENTER).snap_to(target),
            circle.boxed(),
        )],
    }
}

fn main() {
    let scene_size = Vec2(1280.0, 720.0);
    let tl = timeline(5.0, move |t, target| {
        let background = Rectangle {
            size: scene_size,
            fill: Paint::Solid(Color::rgb_u8(20, 20, 30)).into(),
            stroke: None,
        };

        // Distribute four dots evenly along the Y axis by snapping each one's
        // CENTER_LEFT onto a fractional anchor at (0, (i + 0.5) / N) of the scene.
        let dots = [60u32, 30, 24, 16].iter().enumerate().map(|(i, &fps)| {
            let dot = BouncingDot {
                t: t.fps(fps).into(),
                scene_width: scene_size.0,
            };
            let stripe_anchor = Anchor::new(0.0, (i as f32 + 0.5) / 4f32);
            let position = dot
                .view_box()
                .anchor(Anchor::CENTER_LEFT)
                .snap_to_anchor(scene_size, stripe_anchor);
            (position, dot.boxed())
        });

        let scene = VectorLayer {
            size: scene_size,
            children: std::iter::once((Vec2::ZERO, background.boxed()))
                .chain(dots)
                .collect(),
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
