//! A minimal authored timeline that depends ONLY on the `tellur` facade.
//!
//! This is the shape `tellur create` will scaffold: a `cdylib` exporting one
//! timeline via [`tellur::export_timeline!`]. It exercises both macro families —
//! the `#[component]` proc-macro and the `export_timeline!` macro_rules — to
//! confirm their generated paths resolve through `tellur::core` without any
//! direct dependency on `tellur-core` / `tellur-plugin`.

use tellur::core::geometry::{Constraints, Vec2};
use tellur::core::raster::{PixelFormat, RasterComponent, RasterImage, Resolution};
use tellur::core::render_context::RenderContext;
use tellur::core::timeline_component::Timed;
use tellur::core::timeline_container::Timeline;
use tellur::prelude::*;

// A trivial CPU raster leaf. `#[component(raster)]` attaches the builder + glue;
// `#[derive(Keyable)]` gives the float-aware `PartialEq`/`Eq`/`Hash` the cache
// key needs. Every path these macros emit must resolve via `tellur::core`.
#[component(raster)]
#[derive(Clone, Keyable)]
pub struct Dot {
    pub radius: f32,
}

impl RasterComponent for Dot {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        constraints.constrain(Vec2(self.radius * 2.0, self.radius * 2.0))
    }

    fn render(&self, size: Vec2, _target: Resolution, _ctx: &mut dyn RenderContext) -> RasterImage {
        let w = (size.0 as u32).max(1);
        let h = (size.1 as u32).max(1);
        RasterImage::cpu(
            w,
            h,
            PixelFormat::Rgba8,
            vec![0u8; w as usize * h as usize * 4],
        )
    }
}

fn build() -> Timeline {
    // A raster component reaches the timeline through the blanket impl, so the
    // built `Dot` drops straight into a windowed timeline child.
    Timeline::builder()
        .child(Dot::builder().radius(8.0).build().at(0.0..2.0))
        .build()
}

tellur::export_timeline!("main", "Quickstart", build, canvas = (1920.0, 1080.0));
