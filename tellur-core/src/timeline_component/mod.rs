//! The timeline subsystem's public surface.
//!
//! This module defines the time-varying analogue of the spatial component
//! system: the [`TimelineComponent`] trait and its builders/placement verbs,
//! the [`Placed`] / [`Triggered`] wrappers, the resolve pass, and the per-frame
//! [`Clock`]. The timeline macro arm and the container leaves
//! (`timeline_container.rs`) build on these. Every method below is implemented
//! and exercised by focused tests; the media-decode / ffmpeg integration paths
//! sit behind `#[ignore]`.
//!
//! The shape mirrors the spatial side of the library on purpose:
//!
//! | space (`raster.rs` / `builder.rs`)      | time (this module)                |
//! |-----------------------------------------|-----------------------------------|
//! | [`RasterComponent`](crate::raster::RasterComponent) | [`TimelineComponent`] |
//! | [`RasterBuilder`](crate::builder)       | [`TimelineBuilder`]               |
//! | `RasterBuilderPlacement` (`.place_at`)  | [`Timed`] / [`TimedBuilder`]      |
//! | `Positioned`                            | [`Placed`]                        |
//!
//! See `.sketch/01-timeline-api.rs` (ZONE A) for the target authoring API and
//! `.sketch/02-resolve-pass.md` for the resolve-pass architecture every method
//! here leans on (ownership model §3/§7, `Clock` §8, trigger table §11).

mod audio_effect;
mod audio_render;
mod clock;
mod component;
mod output;
mod placed;
mod resolve;
mod trigger;
mod trim;

pub use audio_effect::{AudioEffects, AudioEffectsBuilder, EnvelopePoint, GainEnvelope};
pub use audio_render::{AudioBlockMut, AudioRenderContext, AudioRenderRequest};
pub use clock::Clock;
pub use component::{TimelineBuilder, TimelineComponent, TimelineComponentClone};
pub use output::{Arrangement, AudioBuffer, Cue, NodeKind, SourceLoc, TriggerMark};
pub use placed::{Placed, Placement, Timed, TimedBuilder};
pub use resolve::{
    resolve, resolve_with_canvas, ResolveCtx, ResolveError, ResolvedTimeline, TriggerTable,
    DEFAULT_CANVAS,
};
pub use trigger::{peel_source, Event, Sourced, Triggered, Triggers, TriggersBuilder};
pub use trim::{Trim, TrimBounds};

#[cfg(test)]
mod event_path_tests;
#[cfg(test)]
mod tests;
