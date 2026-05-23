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
use tellur_core::time::{Time, TimeView};
use tellur_core::timeline::timeline;
use tellur_core::vector::{Paint, VectorComponent, VectorGraphic};
use tellur_renderer::{FfmpegEncoder, Rasterizable};

/// A circle that triangle-wave scrubs left-to-right-to-left across a track
/// of `scene_width`, with one full round trip per `Self::PERIOD` seconds.
/// The motion is driven entirely by `t`, so callers can quantize or gate
/// the time independently per instance.
struct BouncingDot {
    t: TimeView,
    scene_width: f32,
}

impl BouncingDot {
    const PERIOD: f32 = 2.5;
    const RADIUS: f32 = 30.0;
    const SIDE_PADDING: f32 = 40.0;
}

impl VectorComponent for BouncingDot {
    fn view_box(&self) -> Vec2 {
        // Track footprint: full track width, just tall enough to bound the dot.
        Vec2(self.scene_width, Self::RADIUS * 2.0)
    }

    fn render(&self) -> VectorGraphic {
        // phase ∈ [0, 1) over one period.
        let phase = self.t.seconds().rem_euclid(Self::PERIOD) / Self::PERIOD;
        // Triangle wave: 0 → 1 → 0 over one period.
        let normalized = 1.0 - (2.0 * phase - 1.0).abs();
        let travel = self.scene_width - 2.0 * (Self::SIDE_PADDING + Self::RADIUS);
        let center_x = Self::SIDE_PADDING + Self::RADIUS + normalized * travel;

        let mut layer = VectorLayer::new(self.view_box());
        layer.add(
            Vec2(center_x - Self::RADIUS, 0.0),
            Circle {
                radius: Self::RADIUS,
                fill: Paint::Solid(Color::hsl(200.0, 0.7, 0.6)).into(),
                stroke: None,
            },
        );
        layer.render()
    }
}

fn main() {
    let scene_size = Vec2(1280.0, 720.0);
    let tl = timeline(5.0, move |t, target| {
        let mut scene = VectorLayer::new(scene_size);

        scene.add(
            Vec2::ZERO,
            Rectangle {
                size: scene_size,
                fill: Paint::Solid(Color::rgb_u8(20, 20, 30)).into(),
                stroke: None,
            },
        );

        // Distribute four dots evenly along the Y axis by snapping each one's
        // CENTER_LEFT onto a fractional anchor at (0, (i + 0.5) / N) of the scene.
        for (i, &fps) in [60u32, 30, 24, 16].iter().enumerate() {
            let dot = BouncingDot {
                t: t.fps(fps),
                scene_width: scene_size.0,
            };
            let stripe_anchor = Anchor::new(0.0, (i as f32 + 0.5) / 4f32);
            let position = dot
                .view_box()
                .anchor(Anchor::CENTER_LEFT)
                .snap_to(scene_size, stripe_anchor);
            scene.add(position, dot);
        }

        scene.rasterize().render(target)
    });

    let out = Path::new("/tmp/timeline.mp4");
    FfmpegEncoder::new(Resolution::new(1280, 720), 60)
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "20"])
        .encode(&tl, out)
        .expect("encode mp4");

    println!("Wrote {}", out.display());
}
