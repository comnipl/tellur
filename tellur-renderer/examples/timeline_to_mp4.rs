//! Compare 60 / 30 / 24 / 16 fps quantization on a back-and-forth dot.
//!
//! Renders a 5-second timeline where four blue dots bounce horizontally.
//! Each dot is a `BouncingDot` whose motion is driven by the time fed
//! to it; the four instances receive the same `t` quantized to
//! different framerates via `Time::fps`. A vertical `Stack`
//! distributes the four tracks evenly inside a padded scene with
//! `CrossAlign::Stretch`. The dot itself is purely a tree of layout
//! containers — `Frame` declares its outer shape and anchors the
//! shadowed circle inside it; `.padding(...)` keeps the dot off the
//! track edges. The `DropShadow` is wrapped directly around the
//! circle, where the shadow conceptually belongs.

use std::path::Path;

use tellur_core::color::Color;
use tellur_core::geometry::{Anchor, EdgeInsets, Vec2};
use tellur_core::layout::raster::{Frame, RasterLayoutExt, Stack};
use tellur_core::layout::{Axis, CrossAlign, MainAlign, SizeMode};
use tellur_core::raster::{RasterComponent, Resolution};
use tellur_core::raster_component;
use tellur_core::shapes::Circle;
use tellur_core::time::{LocalTime, Time};
use tellur_core::timeline::timeline;
use tellur_core::vector::Paint;
use tellur_renderer::{DropShadow, FfmpegEncoder, Rasterizable};

/// A circle that triangle-wave scrubs left-to-right-to-left across the
/// track's width. `Frame` declares the track's outer shape (fill the
/// parent width, fix the height at 60) and anchors the circle so it
/// stays fully inside: both `child_anchor` and `at` use the same
/// bounce-driven ratio, so the dot's left edge touches the frame's
/// left at `rx = 0` and its right edge touches at `rx = 1`. The whole
/// track is wrapped in a `DropShadow`.
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

fn main() {
    let scene_size = Vec2(1280.0, 720.0);
    let tl = timeline(5.0, move |t, target| {
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
        .render(scene_size, target)
    });

    let out = Path::new("/tmp/timeline.mp4");
    FfmpegEncoder::new(Resolution::new(1920, 1080), 60)
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "18"])
        .encode(&tl, out)
        .expect("encode mp4");

    println!("Wrote {}", out.display());
}
