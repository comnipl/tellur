//! The timeline containers and leaves — STEP 4.
//!
//! This module lands the authoring surface sketched in `.sketch/01-timeline-api.rs`
//! (A.2 leaves, A.4 containers) on top of the
//! [`TimelineComponent`](crate::timeline_component::TimelineComponent)
//! contract and
//! the [`resolve`](crate::timeline_component::resolve) pass committed in steps
//! 1–3. It mirrors the SPATIAL side of the library on purpose:
//!
//! | space (`layout/` / `layer.rs`)          | time (this module)                |
//! |-----------------------------------------|-----------------------------------|
//! | `Layer` (overlay children)              | [`Timeline`] (overlay in time)    |
//! | `Flex` (lay along an axis, cursor)      | [`Sequence`] (lay one-after-another) |
//!
//! Both containers are struct-form `#[component(timeline)]` (builder + glue
//! only, NO trait impl from the macro) plus a hand-written
//! `impl TimelineComponent`, exactly as raster `Flex` is a
//! `#[component(raster)] struct` + hand-written `impl RasterComponent`.
//!
//! The leaves ([`VideoFile`], [`AudioFile`], [`Subtitle`]) are buildless
//! builders. Media DECODE is steps 8/9; here their length comes from a stubbed
//! `VideoFile::probe` seam (a caller-injectable `duration`), and
//! `frame` stays `None` and audio block rendering stays silent.

mod containers;
mod leaves;

pub use containers::*;
pub use leaves::*;

#[cfg(test)]
mod tests;
