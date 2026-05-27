use tellur_core::color::Color;
use tellur_core::geometry::{Anchor, EdgeInsets, Vec2};
use tellur_core::layout::raster::{Frame, RasterLayoutExt, Stack};
use tellur_core::layout::{Axis, CrossAlign, MainAlign, SizeMode};
use tellur_core::raster::{RasterComponent, Resolution};
use tellur_core::raster_component;
use tellur_core::shapes::Circle;
use tellur_core::time::{LocalTime, Time};
use tellur_core::timeline::{timeline, Timeline};
use tellur_core::vector::Paint;
use tellur_renderer::{DropShadow, Outline, Rasterizable};

#[raster_component]
fn bouncing_dot(t: LocalTime, hue: f32) -> impl RasterComponent {
    let (phase, _) = t.bounce(2.5);
    let rx = phase.interpolate(0.0, 1.0);
    Frame {
        width: SizeMode::Fill,
        height: SizeMode::Fixed(80.0),
        child_anchor: Anchor::CENTER,
        at: Anchor::new(rx, 0.5),
        child: DropShadow {
            offset: Vec2(0.0, 8.0),
            blur: 12.0,
            color: Color::rgba_u8(0, 0, 0, 160),
            child: Outline {
                width: 5.0,
                color: Color::rgb_u8(255, 255, 255),
                child: Circle {
                    radius: 34.0,
                    fill: Paint::Solid(Color::hsl(hue, 0.7, 0.58)).into(),
                    stroke: None,
                }
                .rasterize()
                .boxed(),
            }
            .boxed(),
        }
        .boxed(),
    }
}

fn build_timeline() -> impl Timeline + Send {
    let scene_size = Vec2(1280.0, 720.0);
    timeline(6.0, move |t, target: Resolution, ctx| {
        Stack {
            axis: Axis::Vertical,
            size: None,
            spacing: 18.0,
            main_align: MainAlign::SpaceEvenly,
            cross_align: CrossAlign::Stretch,
            children: vec![
                BouncingDot {
                    t: t.into(),
                    hue: 190.0,
                }
                .boxed(),
                BouncingDot {
                    t: t.fps(24).into(),
                    hue: 30.0,
                }
                .boxed(),
                BouncingDot {
                    t: t.fps(12).into(),
                    hue: 280.0,
                }
                .boxed(),
            ],
        }
        .padding(EdgeInsets::all(96.0))
        .background(Color::rgb_u8(18, 21, 28))
        .render(scene_size, target, ctx)
    })
}

tellur_live::export_timeline!("main", "Demo Timeline", build_timeline);
