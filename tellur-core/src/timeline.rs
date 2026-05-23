//! Time-driven scene description.
//!
//! A [`Timeline`] produces a [`RasterImage`] for any given [`TimelineTime`] within
//! its `duration`. Renderers walk the timeline frame by frame to produce a
//! video. The shape of `build` mirrors [`crate::raster::RasterComponent`] so
//! the same `target: Resolution` flow works.

use crate::raster::{RasterImage, Resolution};
use crate::time::TimelineTime;

/// A scene defined over a finite time interval.
pub trait Timeline {
    /// Total length of the timeline, in seconds.
    fn duration(&self) -> f32;

    /// Produces the frame for `t` at the requested `target` resolution.
    fn build(&self, t: TimelineTime, target: Resolution) -> RasterImage;
}

/// Builds a [`Timeline`] from a closure. The closure receives the current
/// time and the target resolution, and returns the rasterized frame.
pub fn timeline<F>(duration: f32, build: F) -> impl Timeline
where
    F: Fn(TimelineTime, Resolution) -> RasterImage,
{
    FnTimeline { duration, build }
}

struct FnTimeline<F> {
    duration: f32,
    build: F,
}

impl<F> Timeline for FnTimeline<F>
where
    F: Fn(TimelineTime, Resolution) -> RasterImage,
{
    fn duration(&self) -> f32 {
        self.duration
    }

    fn build(&self, t: TimelineTime, target: Resolution) -> RasterImage {
        (self.build)(t, target)
    }
}

// Compile-time guarantee that `Timeline` is dyn-safe.
const _: Option<&dyn Timeline> = None;
