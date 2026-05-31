//! "Kinetic Motion" — the flagship demo scene, composed declaratively from a
//! handful of self-contained section components.
//!
//! Each section (`backdrop`, `overture`, `field`, `scan`, `resolve`,
//! `overlay`, plus the persistent `Hud`) is a `#[component(vector)]` that
//! computes its animation state and then builds a `VectorLayer` by streaming
//! shapes into the builder. `build_timeline` wires them together by placing
//! each section component full-size at the origin — so the foreground reads as
//! one flat z-ordered stack of leaves even though it is authored as a tree of
//! sub-layers.

mod backdrop;
mod common;
mod field;
mod hud;
mod overlay;
mod overture;
mod resolve;
mod scan;

use tellur_core::builder::{RasterBuilderPlacement, VectorBuilderPlacement};
use tellur_core::color::Color;
use tellur_core::geometry::Vec2;
use tellur_core::layer::{Layer, VectorLayer};
use tellur_core::layout::raster::DecoratedBox;
use tellur_core::placement::RasterPlacement;
use tellur_core::raster::{RasterComponent, Resolution};
use tellur_core::time::Time;
use tellur_core::timeline::{timeline, Timeline};
use tellur_renderer::{DropShadow, Rasterizable, RasterizableBuilder};

use common::{alpha, Palette, DURATION, SCENE_SIZE};
use hud::{Hud, HUD_INTRO_END, HUD_INTRO_START, HUD_OUTRO_END, HUD_OUTRO_START};

use backdrop::Backdrop;
use field::Field;
use overlay::Overlay;
use overture::Overture;
use resolve::Resolve;
use scan::Scan;

pub fn build_timeline() -> impl Timeline + Send {
    timeline(DURATION, move |t, target: Resolution, ctx| {
        // Only the palette is precomputed; the entire frame is then one
        // declarative tree. `DecoratedBox` paints the bg fill and pins
        // paint_bounds to (0,0)..SCENE_SIZE, clipping the shadows' outward
        // spill. Inside it, a raster `Layer` stacks four full-size children
        // placed at the origin: the (cacheable) backdrop, the doubly-shadowed
        // vector foreground, the HUD, and the overlay.
        let palette = Palette {
            bg: Color::rgb_u8(12, 11, 24),
            paper: Color::rgb_u8(247, 240, 224),
            pink: Color::rgb_u8(255, 79, 138),
            cyan: Color::rgb_u8(73, 222, 226),
        };

        DecoratedBox::builder()
            .background(palette.bg)
            .child(
                Layer::builder()
                    .size(SCENE_SIZE)
                    // Backdrop: shadow-free and time-stable after its reveal
                    // saturates (~0.6s), so it caches as its own raster child.
                    .child(
                        Backdrop::builder()
                            .time(t)
                            .palette(palette)
                            .rasterize()
                            .place_at(Vec2::ZERO),
                    )
                    // Foreground: two stacked shadows (a soft paper-tinted halo
                    // with no offset, then a deeper dark drop further back)
                    // behind the vector sections. The sections are each a
                    // full-size child of one VectorLayer; `VectorLayer::render`
                    // wraps every child in an identity-transform, opacity-1.0
                    // group — a transparent passthrough — so the nested grouping
                    // rasterizes identically to one flat layer while the global
                    // leaf z-order (overture before field before …) is kept.
                    .child(
                        DropShadow::builder()
                            .offset(Vec2(0.0, 22.0))
                            .blur(26.0)
                            .color(Color::rgba_u8(0, 0, 0, 170))
                            .child(
                                DropShadow::builder()
                                    // offset omitted → Vec2::ZERO (ambient halo)
                                    .blur(18.0)
                                    .color(alpha(palette.paper, 0.26))
                                    .child(
                                        VectorLayer::builder()
                                            .size(SCENE_SIZE)
                                            .child(
                                                Overture::builder()
                                                    .time(t)
                                                    .palette(palette)
                                                    .place_at(Vec2::ZERO),
                                            )
                                            .child(
                                                Field::builder()
                                                    .time(t)
                                                    .palette(palette)
                                                    .place_at(Vec2::ZERO),
                                            )
                                            .child(
                                                Scan::builder()
                                                    .time(t)
                                                    .palette(palette)
                                                    .place_at(Vec2::ZERO),
                                            )
                                            .child(
                                                Resolve::builder()
                                                    .time(t)
                                                    .palette(palette)
                                                    .place_at(Vec2::ZERO),
                                            )
                                            .rasterize(),
                                    ),
                            )
                            .place_at(Vec2::ZERO),
                    )
                    // HUD: cacheable — its Hash/Eq is driven by a tiny set of
                    // inputs (palette + two phases + section index), so once the
                    // intro saturates and between section-marker switches the
                    // `Rasterize<Hud>` cache lookup reuses the previous raster
                    // instead of re-shaping all the text and brackets.
                    .child(
                        Hud {
                            palette,
                            intro: t.phase(HUD_INTRO_START, HUD_INTRO_END),
                            outro: t.phase(HUD_OUTRO_START, HUD_OUTRO_END),
                            section: hud::section_index_at(t.seconds()),
                        }
                        .rasterize()
                        .place_at(Vec2::ZERO),
                    )
                    .child(
                        Overlay::builder()
                            .time(t)
                            .palette(palette)
                            .rasterize()
                            .place_at(Vec2::ZERO),
                    )
                    .build(),
            )
            .build()
            .render(SCENE_SIZE, target, ctx)
    })
}

pub const TITLE: &str = "Kinetic Motion";

// Consumed by `demo_timeline_mp4` but not by the plugin entry; tell the
// per-binary dead-code lint to allow it.
#[allow(dead_code)]
pub const SCENE_RESOLUTION: (u32, u32) = (1920, 1080);
