//! Compare 60 / 30 / 24 / 16 fps quantization on a back-and-forth dot.
//!
//! Renders a 5-second timeline where four blue dots bounce horizontally.
//! Each dot is a `BouncingDot` whose motion is driven by the time fed
//! to it; the four instances receive the same `t` quantized to
//! different framerates via `Time::fps`. A vertical `Flex`
//! distributes the four tracks evenly inside a padded scene with
//! `CrossAlign::Stretch`. The dot itself is purely a tree of layout
//! containers — `Frame` declares its outer shape and anchors the
//! decorated circle inside it. The circle is decorated via
//! `.rasterize().effect(Outline).effect(DropShadow)` — the first
//! `.effect()` is innermost, so the white stroke applies first and the
//! `DropShadow` falls behind the combined stroked shape.

use std::path::Path;

use tellur_core::builder::RasterEffect;
use tellur_core::color::Color;
use tellur_core::component;
use tellur_core::easing::PhaseEasing;
use tellur_core::geometry::{Anchor, EdgeInsets, Vec2};
use tellur_core::layout::raster::{DecoratedBox, Flex, Frame, Padding};
use tellur_core::layout::{Axis, CrossAlign, MainAlign, SizeMode};
use tellur_core::raster::{RasterComponent, Resolution};
use tellur_core::shapes::Circle;
use tellur_core::time::{LocalTime, Time};
use tellur_core::timeline::timeline;
use tellur_core::vector::Paint;
use tellur_renderer::{DropShadow, FfmpegEncoder, Outline, RasterizableBuilder};

/// A circle that triangle-wave scrubs left-to-right-to-left across the
/// track's width. `Frame` declares the track's outer shape (fill the
/// parent width, fix the height at 60) and anchors the circle so it
/// stays fully inside: both sides of the alignment use the same
/// bounce-driven ratio. The circle itself is decorated via
/// `.rasterize().effect(Outline).effect(DropShadow)` — the first
/// `.effect()` is innermost, so the white `Outline` runs first and the
/// `DropShadow` falls behind the stroked shape.
///
/// `#[builder(into)] t: LocalTime` lets the call site pass a `TimelineTime`
/// straight in (`From<TimelineTime> for LocalTime`). The single `.build()`
/// is at this component's own tree root; every child below it is buildless.
#[component(raster)]
fn BouncingDot(#[builder(into)] t: LocalTime) -> impl RasterComponent {
    let rx = t.bounce(2.5).linear(0.0, 1.0);
    Frame::builder()
        .width(SizeMode::Fill)
        .height(SizeMode::Fixed(60.0))
        .align(Anchor::CENTER.to(Anchor::new(rx, 0.5)))
        .child(
            Circle::builder()
                .radius(30.0)
                .fill(Paint::Solid(Color::hsl(200.0, 0.7, 0.6)))
                .rasterize()
                .effect(
                    Outline::builder()
                        .width(4.0)
                        .color(Color::rgb_u8(255, 255, 255)),
                )
                .effect(
                    DropShadow::builder()
                        .offset(Vec2(0.0, 8.0))
                        .blur(10.0)
                        .color(Color::rgba_u8(0, 0, 0, 200)),
                ),
        )
        .build()
}

fn main() {
    let scene_size = Vec2(1280.0, 720.0);
    let tl = timeline(5.0, move |t, target, residency, ctx| {
        DecoratedBox::builder()
            .background(Color::rgb_u8(20, 20, 30))
            .child(
                Padding::builder().insets(EdgeInsets::all(100.0)).child(
                    Flex::builder()
                        .axis(Axis::Vertical)
                        .main_align(MainAlign::SpaceEvenly)
                        .cross_align(CrossAlign::Stretch)
                        .child(BouncingDot::builder().t(t))
                        .child(BouncingDot::builder().t(t.fps(60)))
                        .child(BouncingDot::builder().t(t.fps(30)))
                        .child(BouncingDot::builder().t(t.fps(24)))
                        .child(BouncingDot::builder().t(t.fps(16))),
                ),
            )
            .build()
            .render(scene_size, target, residency, ctx)
    });

    let out = Path::new("/tmp/timeline.mp4");
    FfmpegEncoder::new(Resolution::new(1920, 1080), 60)
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "18"])
        .encode(&tl, out)
        .expect("encode mp4");

    println!("Wrote {}", out.display());
}
