//! Time-driven scene description.
//!
//! A [`Timeline`] produces a [`RasterImage`] for any given [`TimelineTime`] within
//! its `duration`. Renderers walk the timeline frame by frame to produce a
//! video. The shape of `build` mirrors [`crate::raster::RasterComponent`] so
//! the same `target: Resolution` flow works, and a [`RenderContext`] is
//! threaded through so memoization can survive across frames.

use crate::raster::{RasterImage, RasterResidency, Resolution};
use crate::render_context::RenderContext;
use crate::time::TimelineTime;

/// A scene defined over a finite time interval.
pub trait Timeline {
    /// Total length of the timeline, in seconds.
    fn duration(&self) -> f64;

    /// Produces the frame for `t` at the requested `target` resolution and
    /// consumer-requested representation. `ctx` is supplied by the renderer
    /// driver so any caching layer can persist across frames.
    fn build(
        &self,
        t: TimelineTime,
        target: Resolution,
        residency: RasterResidency,
        ctx: &mut dyn RenderContext,
    ) -> RasterImage;
}

/// Builds a [`Timeline`] from a closure. The closure receives the current time,
/// target resolution, consumer-requested residency, and render context, and
/// returns the rasterized frame.
pub fn timeline<F>(duration: f64, build: F) -> impl Timeline
where
    F: Fn(TimelineTime, Resolution, RasterResidency, &mut dyn RenderContext) -> RasterImage,
{
    FnTimeline { duration, build }
}

struct FnTimeline<F> {
    duration: f64,
    build: F,
}

impl<F> Timeline for FnTimeline<F>
where
    F: Fn(TimelineTime, Resolution, RasterResidency, &mut dyn RenderContext) -> RasterImage,
{
    fn duration(&self) -> f64 {
        self.duration
    }

    fn build(
        &self,
        t: TimelineTime,
        target: Resolution,
        residency: RasterResidency,
        ctx: &mut dyn RenderContext,
    ) -> RasterImage {
        (self.build)(t, target, residency, ctx)
    }
}

// Compile-time guarantee that `Timeline` is dyn-safe.
const _: Option<&dyn Timeline> = None;

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::sync::Arc;

    use super::*;
    use crate::raster::{GpuSurface, PixelFormat};
    use crate::render_context::PassThrough;

    fn cpu_frame(target: Resolution) -> RasterImage {
        RasterImage::cpu(
            target.width,
            target.height,
            PixelFormat::Rgba8,
            vec![0; (target.width * target.height * 4) as usize],
        )
    }

    #[test]
    fn closure_receives_consumer_request() {
        let seen = Cell::new(RasterResidency::Cpu);
        let timeline = timeline(1.0, |_t, target, residency, _ctx| {
            seen.set(residency);
            match residency {
                RasterResidency::Cpu => cpu_frame(target),
                RasterResidency::Gpu => RasterImage::Gpu(GpuSurface::new(
                    target.width,
                    target.height,
                    PixelFormat::Rgba8,
                    "test-gpu",
                    Arc::new(()),
                )),
            }
        });
        let mut ctx = PassThrough;

        let frame = timeline.build(
            TimelineTime::new(0.0),
            Resolution::new(1, 1),
            RasterResidency::Gpu,
            &mut ctx,
        );
        assert_eq!(seen.get(), RasterResidency::Gpu);
        assert!(matches!(frame, RasterImage::Gpu(_)));

        let frame = timeline.build(
            TimelineTime::new(0.0),
            Resolution::new(1, 1),
            RasterResidency::Cpu,
            &mut ctx,
        );
        assert_eq!(seen.get(), RasterResidency::Cpu);
        assert!(matches!(frame, RasterImage::Cpu(_)));
    }
}
