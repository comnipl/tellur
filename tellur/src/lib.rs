//! `tellur` — the single dependency for authoring tellur timelines.
//!
//! Re-exports the engine crates under stable module paths so a project depends
//! on `tellur` alone:
//! - the component model and the `#[component]` / `Keyable` macros via [`core`],
//! - the live/host plugin contract (`TimelineCollection`, `export_timeline!`, …)
//!   at the crate root, and
//! - under the `renderer` feature (on by default) the GPU / encode backend at
//!   [`renderer`].
//! - optional authoring features such as `latex`, forwarded to the engine crates.
//!
//! The `#[component]` and `export_timeline!` macros emit their paths through this
//! facade (resolved at expansion time), so an authored project never needs to
//! depend on `tellur-core` / `tellur-plugin` / `tellur-renderer` directly.

pub use tellur_core as core;

#[cfg(feature = "renderer")]
pub use tellur_renderer as renderer;

// The plugin authoring contract: the collection trait and the `export_*!` macros
// a `cdylib` project uses to expose its timeline(s) to the host.
pub use tellur_plugin::{
    export_legacy_timeline, export_timeline, export_timeline_collection, single_timeline,
    single_timeline_with_canvas, EntryFn, LegacyTimeline, SingleTimeline, TimelineCollection,
    TimelineInfo, ENTRY_SYMBOL,
};

/// Common authoring imports: the component macros plus the most-used value types.
///
/// `use tellur::prelude::*;` brings the `#[component]` / `#[derive(Keyable)]`
/// macros and the everyday geometry/color types into scope.
pub mod prelude {
    pub use tellur_core::color::Color;
    pub use tellur_core::geometry::{Anchor, Vec2};
    pub use tellur_core::{component, raster_component, vector_component, Keyable};

    pub use crate::{export_legacy_timeline, export_timeline, export_timeline_collection};
}
