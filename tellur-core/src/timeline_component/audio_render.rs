//! Integer-frame requests and caller-owned output blocks for recursive audio
//! rendering.
//!
//! A request keeps its root-timeline span as integer sample frames while an
//! affine `f64` mapping describes the local time seen by the current component.
//! Wrappers transform that mapping directly, so evaluating a sample never
//! depends on where a caller happened to split the surrounding render blocks.

use std::collections::HashMap;
use std::sync::Arc;

use super::AudioBuffer;

/// One half-open block request for recursive audio rendering.
///
/// `start_frame..start_frame + frame_count` is always expressed on the root
/// output sample grid. `local_start` is the component-local time, in seconds,
/// at `start_frame`; `local_step` is the number of local seconds advanced by
/// one root output frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioRenderRequest {
    start_frame: i64,
    frame_count: usize,
    rate: u32,
    channels: u16,
    local_start: f64,
    local_step: f64,
}

impl AudioRenderRequest {
    /// Creates a root request whose local clock is the root output clock.
    pub fn new(start_frame: i64, frame_count: usize, rate: u32, channels: u16) -> Self {
        assert!(rate > 0, "audio sample rate must be non-zero");
        assert!(channels > 0, "audio channel count must be non-zero");

        let local_step = 1.0 / f64::from(rate);
        let local_start = start_frame as f64 * local_step;
        let request = Self {
            start_frame,
            frame_count,
            rate,
            channels,
            local_start,
            local_step,
        };
        // Fail at construction rather than much later when a buffer is sized.
        let _ = request.sample_len();
        request
    }

    /// The first requested frame on the root output sample grid.
    pub fn start_frame(self) -> i64 {
        self.start_frame
    }

    /// The number of requested root output frames.
    pub fn frame_count(self) -> usize {
        self.frame_count
    }

    /// The root output sample rate, in Hz.
    pub fn rate(self) -> u32 {
        self.rate
    }

    /// The number of interleaved output channels.
    pub fn channels(self) -> u16 {
        self.channels
    }

    /// Component-local time, in seconds, at the request's first frame.
    pub fn local_start(self) -> f64 {
        self.local_start
    }

    /// Component-local seconds advanced by one root output frame.
    pub fn local_step(self) -> f64 {
        self.local_step
    }

    /// The exact number of interleaved `f32` samples required by this request.
    pub fn sample_len(self) -> usize {
        self.frame_count
            .checked_mul(usize::from(self.channels))
            .expect("audio block sample count overflow")
    }

    /// Returns the component-local time at `frame_offset` within this request.
    ///
    /// The value is derived directly from the affine mapping on every call; no
    /// incrementally accumulated clock is involved. `frame_offset ==
    /// frame_count` is allowed so callers can evaluate the half-open end.
    pub fn time_at(self, frame_offset: usize) -> f64 {
        assert!(
            frame_offset <= self.frame_count,
            "audio frame offset {frame_offset} exceeds request length {}",
            self.frame_count
        );
        (frame_offset as f64).mul_add(self.local_step, self.local_start)
    }

    /// Whether at least one requested sample may lie in `[start, end)` on the
    /// current component's local clock.
    ///
    /// This conservative endpoint test lets containers skip whole inactive
    /// children before they decode or allocate effect state. It never skips a
    /// block that could contribute, including for a reversed affine clock.
    pub fn may_overlap_local(self, start: f64, end: f64) -> bool {
        if self.frame_count == 0 || !start.is_finite() || !end.is_finite() || end <= start {
            return false;
        }
        let first = self.time_at(0);
        let last = self.time_at(self.frame_count - 1);
        let (earliest, latest) = if first <= last {
            (first, last)
        } else {
            (last, first)
        };
        earliest < end && latest >= start
    }

    /// Replaces the affine local-time mapping without changing the root span or
    /// output format.
    pub fn with_local_timing(mut self, local_start: f64, local_step: f64) -> Self {
        assert!(local_start.is_finite(), "audio local start must be finite");
        assert!(local_step.is_finite(), "audio local step must be finite");
        self.local_start = local_start;
        self.local_step = local_step;
        self
    }

    /// Translates the component-local clock by `delta` seconds.
    pub fn shift_local(mut self, delta: f64) -> Self {
        assert!(delta.is_finite(), "audio local translation must be finite");
        self.local_start += delta;
        assert!(
            self.local_start.is_finite(),
            "translated audio local start must be finite"
        );
        self
    }

    /// Alias for [`shift_local`](Self::shift_local), phrased as a time transform.
    pub fn translated(self, delta: f64) -> Self {
        self.shift_local(delta)
    }

    /// Alias for [`with_local_timing`](Self::with_local_timing), phrased as an
    /// affine remap.
    pub fn remapped(self, local_start: f64, local_step: f64) -> Self {
        self.with_local_timing(local_start, local_step)
    }

    /// Selects a frame subrange and advances both the root and local starts.
    pub fn subrange(self, frame_offset: usize, frame_count: usize) -> Self {
        let end = frame_offset
            .checked_add(frame_count)
            .expect("audio subrange overflow");
        assert!(
            end <= self.frame_count,
            "audio subrange {frame_offset}..{end} exceeds request length {}",
            self.frame_count
        );
        let start_delta = i64::try_from(frame_offset).expect("audio frame offset exceeds i64");
        Self {
            start_frame: self
                .start_frame
                .checked_add(start_delta)
                .expect("audio root frame overflow"),
            frame_count,
            local_start: self.time_at(frame_offset),
            ..self
        }
    }

    /// Expands the request by root output frames on both sides.
    ///
    /// The local affine mapping is extended backwards and forwards unchanged,
    /// which makes this suitable for finite filter halos and resampler kernels.
    pub fn expanded(self, left_frames: usize, right_frames: usize) -> Self {
        let left = i64::try_from(left_frames).expect("left audio halo exceeds i64");
        let frame_count = self
            .frame_count
            .checked_add(left_frames)
            .and_then(|count| count.checked_add(right_frames))
            .expect("expanded audio request length overflow");
        let local_start = (-(left_frames as f64)).mul_add(self.local_step, self.local_start);
        let expanded = Self {
            start_frame: self
                .start_frame
                .checked_sub(left)
                .expect("expanded audio root frame underflow"),
            frame_count,
            local_start,
            ..self
        };
        let _ = expanded.sample_len();
        expanded
    }
}

/// A caller-owned interleaved PCM block paired with its render request.
///
/// A callee must overwrite the entire block, including writing zeroes where it
/// contributes silence. The constructor enforces the request/buffer shape.
pub struct AudioBlockMut<'a> {
    request: AudioRenderRequest,
    samples: &'a mut [f32],
}

impl<'a> AudioBlockMut<'a> {
    /// Pairs `samples` with `request`, panicking when their shapes differ.
    pub fn new(request: AudioRenderRequest, samples: &'a mut [f32]) -> Self {
        assert_eq!(
            samples.len(),
            request.sample_len(),
            "audio block must contain frame_count * channels interleaved samples"
        );
        Self { request, samples }
    }

    /// The request represented by this block.
    pub fn request(&self) -> AudioRenderRequest {
        self.request
    }

    /// The interleaved PCM samples.
    pub fn samples(&self) -> &[f32] {
        self.samples
    }

    /// Mutable access to the interleaved PCM samples.
    pub fn samples_mut(&mut self) -> &mut [f32] {
        self.samples
    }

    /// Fills the entire block with silence.
    pub fn clear(&mut self) {
        self.samples.fill(0.0);
    }

    /// The number of interleaved samples in the block.
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// Whether the block contains no interleaved samples.
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }
}

/// Per-render scratch storage passed through recursive audio evaluation.
///
/// Scratch vectors are returned by value so a component may keep one while it
/// recursively re-borrows the context for its child.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ConformedSourceMemoKey {
    path: String,
    rate: u32,
    channels: u16,
    gain_bits: u32,
}

#[derive(Debug, Default)]
pub struct AudioRenderContext {
    scratch: Vec<Vec<f32>>,
    source_durations: HashMap<String, Option<f64>>,
    conformed_sources: HashMap<ConformedSourceMemoKey, Option<Arc<AudioBuffer>>>,
}

impl AudioRenderContext {
    /// Creates an empty render context.
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes a zero-filled scratch vector of exactly `len` samples.
    pub fn take_scratch(&mut self, len: usize) -> Vec<f32> {
        let mut scratch = self.scratch.pop().unwrap_or_default();
        scratch.resize(len, 0.0);
        scratch.fill(0.0);
        scratch
    }

    /// Returns a scratch vector for reuse by a later recursive call.
    pub fn recycle_scratch(&mut self, mut scratch: Vec<f32>) {
        scratch.clear();
        self.scratch.push(scratch);
    }

    /// Returns a source's native duration, memoizing both decode success and
    /// failure for this render traversal.
    pub(crate) fn source_duration(&mut self, path: &str) -> Option<f64> {
        self.source_duration_with(path, || crate::audio::decoded_duration(path, None).ok())
    }

    fn source_duration_with<F>(&mut self, path: &str, load: F) -> Option<f64>
    where
        F: FnOnce() -> Option<f64>,
    {
        if let Some(duration) = self.source_durations.get(path) {
            return *duration;
        }

        let duration = load();
        self.source_durations.insert(path.to_owned(), duration);
        duration
    }

    /// Returns source PCM conformed to the root output format, retaining the
    /// resulting buffer even when the process-wide best-effort cache declines
    /// it. Decode failures are retained too, so later blocks stay silent
    /// without retrying the same source.
    pub(crate) fn conformed_source(
        &mut self,
        path: &str,
        rate: u32,
        channels: u16,
        gain: f32,
    ) -> Option<Arc<AudioBuffer>> {
        let key = ConformedSourceMemoKey {
            path: path.to_owned(),
            rate,
            channels,
            gain_bits: gain.to_bits(),
        };
        self.conformed_source_with(key, || {
            crate::audio::conform_file_cached(path, None, rate, channels, gain, 1.0).ok()
        })
    }

    fn conformed_source_with<F>(
        &mut self,
        key: ConformedSourceMemoKey,
        load: F,
    ) -> Option<Arc<AudioBuffer>>
    where
        F: FnOnce() -> Option<Arc<AudioBuffer>>,
    {
        if let Some(source) = self.conformed_sources.get(&key) {
            return source.clone();
        }

        let source = load();
        self.conformed_sources.insert(key, source.clone());
        source
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_offset_keeps_adjacent_48khz_samples_distinct() {
        // Ten hours is far beyond the point where f32 can represent adjacent
        // absolute 48 kHz sample indices, while f64 retains ample precision.
        let start_frame = 48_000_i64 * 60 * 60 * 10;
        let request = AudioRenderRequest::new(start_frame, 2, 48_000, 2);

        let first = request.time_at(0);
        let second = request.time_at(1);
        let sample_period = 1.0 / 48_000.0;

        assert_ne!(first, second);
        assert!(((second - first) - sample_period).abs() < 1.0e-11);
        assert_eq!(first, start_frame as f64 / 48_000.0);
    }

    #[test]
    fn subrange_and_expansion_preserve_the_affine_mapping() {
        let request = AudioRenderRequest::new(100, 20, 1_000, 2).with_local_timing(5.0, 0.002);
        let subrange = request.subrange(4, 6);

        assert_eq!(subrange.start_frame(), 104);
        assert_eq!(subrange.frame_count(), 6);
        assert_eq!(subrange.local_start(), request.time_at(4));
        assert_eq!(subrange.time_at(6), request.time_at(10));

        let expanded = subrange.expanded(2, 3);
        assert_eq!(expanded.start_frame(), 102);
        assert_eq!(expanded.frame_count(), 11);
        assert!((expanded.local_start() - request.time_at(2)).abs() < 1.0e-14);
        assert!((expanded.time_at(11) - request.time_at(13)).abs() < 1.0e-14);

        let shifted = expanded.shift_local(1.25);
        assert_eq!(shifted.start_frame(), expanded.start_frame());
        assert_eq!(shifted.local_step(), expanded.local_step());
        assert_eq!(shifted.time_at(5), expanded.time_at(5) + 1.25);

        let remapped = shifted.remapped(-3.0, 0.5);
        assert_eq!(remapped.time_at(0), -3.0);
        assert_eq!(remapped.time_at(2), -2.0);
    }

    #[test]
    fn overlap_test_respects_half_open_local_intervals() {
        let request = AudioRenderRequest::new(10, 4, 10, 1);
        assert!(request.may_overlap_local(1.0, 1.1));
        assert!(request.may_overlap_local(1.2, 2.0));
        assert!(!request.may_overlap_local(0.0, 1.0));
        assert!(!request.may_overlap_local(1.4, 2.0));
        assert!(!request.subrange(0, 0).may_overlap_local(0.0, 2.0));
    }

    #[test]
    fn audio_block_enforces_shape_and_exposes_samples() {
        let request = AudioRenderRequest::new(0, 3, 48_000, 2);
        assert_eq!(request.sample_len(), 6);

        let mut samples = vec![1.0_f32; request.sample_len()];
        let mut block = AudioBlockMut::new(request, &mut samples);
        assert_eq!(block.request(), request);
        assert_eq!(block.len(), 6);
        assert!(!block.is_empty());
        block.samples_mut()[1] = 0.5;
        assert_eq!(block.samples()[1], 0.5);
        block.clear();
        assert!(block.samples().iter().all(|sample| *sample == 0.0));
    }

    #[test]
    #[should_panic(expected = "audio block must contain frame_count * channels")]
    fn audio_block_rejects_wrong_interleaved_length() {
        let request = AudioRenderRequest::new(0, 3, 48_000, 2);
        let mut samples = vec![0.0_f32; 5];
        let _ = AudioBlockMut::new(request, &mut samples);
    }

    #[test]
    fn render_context_recycles_zeroed_scratch() {
        let mut context = AudioRenderContext::new();
        let mut scratch = context.take_scratch(4);
        scratch.fill(1.0);
        let capacity = scratch.capacity();
        context.recycle_scratch(scratch);

        let reused = context.take_scratch(3);
        assert!(reused.capacity() >= capacity);
        assert_eq!(reused, vec![0.0; 3]);
    }

    #[test]
    fn render_context_memoizes_source_duration_success_and_failure() {
        let mut context = AudioRenderContext::new();

        assert_eq!(
            context.source_duration_with("voice.wav", || Some(2.5)),
            Some(2.5)
        );
        assert_eq!(
            context.source_duration_with("voice.wav", || panic!("duration reloaded")),
            Some(2.5)
        );

        assert_eq!(context.source_duration_with("missing.wav", || None), None);
        assert_eq!(
            context.source_duration_with("missing.wav", || panic!("failure retried")),
            None
        );
    }

    #[test]
    fn render_context_memoizes_conformed_source_success_and_failure() {
        let mut context = AudioRenderContext::new();
        let key = ConformedSourceMemoKey {
            path: "voice.wav".to_owned(),
            rate: 48_000,
            channels: 2,
            gain_bits: 0.5_f32.to_bits(),
        };
        let source = Arc::new(AudioBuffer {
            samples: vec![0.25, 0.25],
            rate: 48_000,
            channels: 2,
        });

        let first = context
            .conformed_source_with(key.clone(), || Some(Arc::clone(&source)))
            .expect("first load succeeds");
        let second = context
            .conformed_source_with(key, || panic!("source reloaded"))
            .expect("memoized load succeeds");
        assert!(Arc::ptr_eq(&first, &second));

        let missing_key = ConformedSourceMemoKey {
            path: "missing.wav".to_owned(),
            rate: 48_000,
            channels: 2,
            gain_bits: 1.0_f32.to_bits(),
        };
        assert!(context
            .conformed_source_with(missing_key.clone(), || None)
            .is_none());
        assert!(context
            .conformed_source_with(missing_key, || panic!("failure retried"))
            .is_none());
    }
}
