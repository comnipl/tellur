//! Temporal motion blur for timeline components.
//!
//! [`MotionBlur`] wraps a [`TimelineComponent`] and, each frame, evaluates it
//! at `samples` sub-clocks spread across a trailing shutter window
//! `[t - shutter, t]`, then averages the resulting frames. A frame with
//! motion blur is a pure function of the scene over the shutter interval —
//! not of the instant `t` — and this component encodes exactly that: the
//! per-sample state is baked into the child's raster tree by the shifted
//! [`Clock`], so every sub-frame memoizes through `ctx.render` under the
//! existing content-hash cache key with NO cache changes:
//!
//! - While the child animates, each sample bakes a different state, so each
//!   sub-frame has independent reuse history (and overlapping shutters can
//!   admit and share entries on their second observation).
//! - Once the child's animation saturates (every Phase clamped), all samples
//!   bake the SAME state. After the second-use warm-up, every `ctx.render`
//!   returns one shared cache entry and the average short-circuits to that
//!   entry — a static stretch costs the same as having no blur at all.
//!
//! The averaging itself happens at the IMAGE layer (like a container's
//! per-frame composite), so a blurred result can never alias an unblurred
//! cache entry — which is what makes the preview's motion-blur toggle
//! ([`RenderContext::motion_blur_enabled`]) safe to flip at any time.
//!
//! Apply the wrapper BEFORE the placement verb (`MotionBlur` around the
//! component, then `.at(..)` / `.fill()`): containers detect `.fill()` by
//! downcasting their direct child to `Placed`, and an opaque wrapper around
//! the `Placed` would hide it.

use tellur_core::geometry::Vec2;
use tellur_core::raster::{CpuRasterImage, PixelFormat, RasterImage, RasterResidency, Resolution};
use tellur_core::render_context::RenderContext;
use tellur_core::timeline_component::{
    Arrangement, AudioBlockMut, AudioRenderContext, Clock, Cue, ResolveCtx, TimelineComponent,
};
use tellur_core::Keyable;

/// Hard cap on the per-frame sample count. Keeps the u32 premultiplied
/// accumulators (CPU and GPU alike) far from overflow: 64 × 255 × 255 ≈
/// 4.2M ≪ u32::MAX.
const MAX_SAMPLES: u32 = 64;

#[tellur_core::component(timeline)]
#[derive(Keyable)]
pub struct MotionBlur {
    /// How long the virtual shutter stays open, in the child's local
    /// seconds. Samples cover the trailing window `[t - shutter, t]`.
    pub shutter: f64,
    /// How many sub-clocks to sample across the shutter (clamped to
    /// `1..=64`). The current frame is always one of them.
    #[builder(default = 8)]
    pub samples: u32,
    #[builder(into)]
    pub child: Box<dyn TimelineComponent + Send>,
}

impl TimelineComponent for MotionBlur {
    fn duration(&self) -> Option<f64> {
        self.child.duration()
    }

    fn measure(&self) -> Option<f64> {
        self.child.measure()
    }

    fn resolve(&self, abs_start: f64, out: &mut ResolveCtx) -> f64 {
        self.child.resolve(abs_start, out)
    }

    fn frame(
        &self,
        clock: Clock<'_>,
        canvas: Vec2,
        target: Resolution,
        residency: RasterResidency,
        ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage> {
        let total = self.samples.clamp(1, MAX_SAMPLES);
        if total == 1 || self.shutter <= 0.0 || !ctx.motion_blur_enabled() {
            return self.child.frame(clock, canvas, target, residency, ctx);
        }

        let gpu_available = ctx.prefers_gpu() && ctx.gpu_backend().is_some();
        let execution_residency = if gpu_available {
            RasterResidency::Gpu
        } else {
            RasterResidency::Cpu
        };

        // Trailing shutter: sample 0 is the current frame, sample `total-1`
        // reaches back the full shutter. Offsets are derived from the two
        // baked params only, so sampling is deterministic.
        let frames: Vec<Option<RasterImage>> = (0..total)
            .map(|i| {
                let dt = -self.shutter * (i as f64 / (total - 1) as f64);
                self.child
                    .frame(clock.shifted(dt), canvas, target, execution_residency, ctx)
            })
            .collect();

        let present: Vec<&RasterImage> = frames.iter().flatten().collect();
        // The child contributed nothing anywhere in the shutter window.
        if present.is_empty() {
            return None;
        }

        // Static short-circuit: when every sample resolved to one shared
        // cache entry, the average IS that entry — skip the accumulate so a
        // saturated stretch costs the same as no blur. (Shared storage is a
        // sufficient identity check; samples rendered into distinct buffers
        // just take the full path.)
        if present.len() == frames.len()
            && present.iter().all(|frame| frame.shares_storage(present[0]))
        {
            let image = frames.into_iter().next().unwrap()?;
            return Some(ctx.ensure_residency(image, residency));
        }

        // Every present frame must be a target-sized RGBA8 image (the frame
        // contract). If a child violates that, degrade to the unblurred
        // current sample instead of mis-compositing.
        if present.iter().any(|frame| {
            frame.width() != target.width
                || frame.height() != target.height
                || frame.format() != PixelFormat::Rgba8
        }) {
            let image = frames.into_iter().next().unwrap()?;
            return Some(ctx.ensure_residency(image, residency));
        }

        if gpu_available
            && present
                .iter()
                .all(|frame| frame.residency() == RasterResidency::Gpu)
        {
            if let Some(gpu) = ctx.gpu_backend() {
                if let Some(image) = gpu.temporal_average(target, &present, total) {
                    return Some(ctx.ensure_residency(image, residency));
                }
            }
        }

        drop(present);
        let cpu_frames: Vec<CpuRasterImage> = frames
            .into_iter()
            .flatten()
            .map(|frame| ctx.readback(frame))
            .collect();
        let image = average_frames_cpu(&cpu_frames, total, target);
        Some(ctx.ensure_residency(image, residency))
    }

    fn render_audio_block(&self, block: AudioBlockMut<'_>, ctx: &mut AudioRenderContext) {
        // Audio is never temporally averaged by a video motion-blur wrapper.
        self.child.render_audio_block(block, ctx);
    }

    fn cues(&self, offset: f64) -> Vec<Cue> {
        self.child.cues(offset)
    }

    fn arrangement(&self, offset: f64) -> Arrangement {
        // Transparent in the live panel: the wrapper adds no tree level.
        self.child.arrangement(offset)
    }
}

/// CPU twin of the GPU accumulate/resolve pair in `gpu.rs` — premultiplied
/// u32 channel sums, then color = the alpha-weighted average of the straight
/// source colors and alpha = the mean over ALL `total` shutter samples
/// (missing samples counted as transparent). Integer math throughout so the
/// two paths stay byte-identical; change them in lockstep.
pub(crate) fn average_frames_cpu(
    frames: &[CpuRasterImage],
    total: u32,
    target: Resolution,
) -> RasterImage {
    let px = (target.width as usize) * (target.height as usize);
    let mut acc = vec![0u32; px * 4];
    for frame in frames {
        debug_assert_eq!(frame.format, PixelFormat::Rgba8);
        for (i, p) in frame.pixels.chunks_exact(4).enumerate().take(px) {
            let a = p[3] as u32;
            let base = i * 4;
            acc[base] += p[0] as u32 * a;
            acc[base + 1] += p[1] as u32 * a;
            acc[base + 2] += p[2] as u32 * a;
            acc[base + 3] += a;
        }
    }
    let mut out = vec![0u8; px * 4];
    for i in 0..px {
        let base = i * 4;
        let sum_a = acc[base + 3];
        if sum_a == 0 {
            continue;
        }
        let half = sum_a / 2;
        out[base] = ((acc[base] + half) / sum_a).min(255) as u8;
        out[base + 1] = ((acc[base + 1] + half) / sum_a).min(255) as u8;
        out[base + 2] = ((acc[base + 2] + half) / sum_a).min(255) as u8;
        out[base + 3] = ((sum_a + total / 2) / total).min(255) as u8;
    }
    RasterImage::cpu(target.width, target.height, PixelFormat::Rgba8, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tellur_core::geometry::Constraints;
    use tellur_core::raster::{RasterComponent, RasterResidency};
    use tellur_core::render_context::{GpuPreference, PassThrough};
    use tellur_core::time::{LocalTime, Time, TimelineTime};
    use tellur_core::timeline_component::{NodeKind, TriggerTable};

    use crate::render_context::CachingRenderContext;

    /// A timed leaf that paints one opaque white pixel at
    /// `x = round(local seconds)` of a `target`-wide strip, and nothing at
    /// all before `appear_at`. Fresh buffer every call (a timeline leaf is
    /// never memoized), so it exercises the full averaging path.
    #[derive(PartialEq, Hash)]
    struct MovingDot {
        appear_at_ms: i64,
    }

    impl TimelineComponent for MovingDot {
        fn duration(&self) -> Option<f64> {
            Some(10.0)
        }

        fn frame(
            &self,
            clock: Clock<'_>,
            _canvas: Vec2,
            target: Resolution,
            _residency: RasterResidency,
            _ctx: &mut dyn RenderContext,
        ) -> Option<RasterImage> {
            let t = clock.local().seconds();
            if t < self.appear_at_ms as f64 / 1000.0 {
                return None;
            }
            let mut pixels = vec![0u8; (target.width * target.height * 4) as usize];
            let x = t.round() as i64;
            if (0..target.width as i64).contains(&x) {
                let base = (x as usize) * 4;
                pixels[base..base + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
            Some(RasterImage::cpu(
                target.width,
                target.height,
                PixelFormat::Rgba8,
                pixels,
            ))
        }

        fn arrangement(&self, offset: f64) -> Arrangement {
            Arrangement {
                kind: NodeKind::Video,
                label: String::new(),
                name: None,
                source: None,
                start: offset,
                end: offset + 10.0,
                trim: None,
                triggers: Vec::new(),
                children: Vec::new(),
            }
        }
    }

    fn clock_at(table: &TriggerTable, t: f64) -> Clock<'_> {
        Clock::new(TimelineTime::new(t), LocalTime::new(t), table)
    }

    fn blur(shutter: f64, samples: u32, appear_at_ms: i64) -> MotionBlur {
        MotionBlur {
            shutter,
            samples,
            child: Box::new(MovingDot { appear_at_ms }),
        }
    }

    #[test]
    fn averages_across_the_shutter() {
        let table = TriggerTable::new();
        let mut ctx = PassThrough;
        // Two samples one second apart land the dot on x=1 (current) and
        // x=0 (one shutter back): each position is lit by 1 of 2 samples,
        // so both come out white at alpha (255 + 1) / 2 = 128.
        let image = blur(1.0, 2, 0)
            .frame(
                clock_at(&table, 1.0),
                Vec2(3.0, 1.0),
                Resolution::new(3, 1),
                RasterResidency::Cpu,
                &mut ctx,
            )
            .expect("dot is visible");
        let cpu = ctx.readback(image);
        assert_eq!(
            cpu.pixels.as_ref(),
            &[255, 255, 255, 128, 255, 255, 255, 128, 0, 0, 0, 0]
        );
    }

    #[test]
    fn missing_samples_fade_instead_of_brightening() {
        let table = TriggerTable::new();
        let mut ctx = PassThrough;
        // The dot appears at t=0.5, so the t=0.0 sample contributes nothing:
        // the lone present sample still divides by total=2.
        let image = blur(1.0, 2, 500)
            .frame(
                clock_at(&table, 1.0),
                Vec2(3.0, 1.0),
                Resolution::new(3, 1),
                RasterResidency::Cpu,
                &mut ctx,
            )
            .expect("dot is visible at the current sample");
        let cpu = ctx.readback(image);
        assert_eq!(
            cpu.pixels.as_ref(),
            &[0, 0, 0, 0, 255, 255, 255, 128, 0, 0, 0, 0]
        );
    }

    #[test]
    fn fully_absent_shutter_returns_none() {
        let table = TriggerTable::new();
        let mut ctx = PassThrough;
        assert!(blur(1.0, 2, 5_000)
            .frame(
                clock_at(&table, 1.0),
                Vec2(3.0, 1.0),
                Resolution::new(3, 1),
                RasterResidency::Cpu,
                &mut ctx
            )
            .is_none());
    }

    #[test]
    fn disabled_context_renders_the_unblurred_frame() {
        let table = TriggerTable::new();
        let mut ctx = CachingRenderContext::new().with_gpu_preference(GpuPreference::Disabled);
        ctx.set_motion_blur_enabled(false);
        let image = blur(1.0, 2, 0)
            .frame(
                clock_at(&table, 1.0),
                Vec2(3.0, 1.0),
                Resolution::new(3, 1),
                RasterResidency::Cpu,
                &mut ctx,
            )
            .expect("dot is visible");
        let cpu = ctx.readback(image);
        // Only the current sample, at full alpha — no shutter trail.
        assert_eq!(
            cpu.pixels.as_ref(),
            &[0, 0, 0, 0, 255, 255, 255, 255, 0, 0, 0, 0]
        );
    }

    /// A timeless raster leaf: routed through `ctx.render` by the blanket
    /// `RasterComponent → TimelineComponent` impl, so identical samples
    /// resolve to one shared cache entry after second-use warm-up.
    #[derive(PartialEq, Hash)]
    struct StaticSquare;

    impl RasterComponent for StaticSquare {
        fn layout(&self, _constraints: Constraints) -> Vec2 {
            Vec2(2.0, 2.0)
        }

        fn render(
            &self,
            _size: Vec2,
            target: Resolution,
            _residency: RasterResidency,
            _ctx: &mut dyn RenderContext,
        ) -> RasterImage {
            let pixels = vec![200u8; (target.width * target.height * 4) as usize];
            RasterImage::cpu(target.width, target.height, PixelFormat::Rgba8, pixels)
        }
    }

    #[test]
    fn static_subtree_short_circuits_to_the_cache_entry() {
        let table = TriggerTable::new();
        let mut ctx = CachingRenderContext::new().with_gpu_preference(GpuPreference::Disabled);
        let blurred = MotionBlur {
            shutter: 1.0,
            samples: 4,
            child: Box::new(StaticSquare),
        };
        let size = Vec2(2.0, 2.0);
        let target = Resolution::new(2, 2);
        let first = blurred
            .frame(
                clock_at(&table, 1.0),
                size,
                target,
                RasterResidency::Cpu,
                &mut ctx,
            )
            .expect("visible");
        let second = blurred
            .frame(
                clock_at(&table, 5.0),
                size,
                target,
                RasterResidency::Cpu,
                &mut ctx,
            )
            .expect("visible");
        let third = blurred
            .frame(
                clock_at(&table, 9.0),
                size,
                target,
                RasterResidency::Cpu,
                &mut ctx,
            )
            .expect("visible");
        // The first frame warms the second-use admission history. Once warm,
        // both frames are the SAME cache entry: the average short-circuited
        // instead of re-accumulating (an averaged copy would live in a fresh
        // buffer and alpha-divide cleanly, hiding the regression).
        assert!(second.shares_storage(&third));
        let cpu = ctx.readback(first);
        assert_eq!(cpu.pixels.as_ref(), &[200u8; 2 * 2 * 4]);
    }
}
