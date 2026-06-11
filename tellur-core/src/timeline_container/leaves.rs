//! The media / subtitle leaves: [`VideoFile`], [`AudioFile`], [`Subtitle`].

use crate::audio::{self, AudioMix};
use crate::geometry::Vec2;
use crate::raster::{RasterImage, Resolution};
use crate::render_context::RenderContext;
use crate::time::Time;
use crate::timeline_component::{
    Arrangement, AudioBuffer, Clock, Cue, NodeKind, TimelineComponent,
};

// ── Leaves ───────────────────────────────────────────────────────────────────

/// A last-resort media duration for the [`probe`](VideoFile::probe) seam when no
/// real length is available (`.sketch/02 §12`): a missing/un-probable file, or a
/// placeholder test path. Real decode reads the header — video via
/// `ffprobe`/`ffmpeg` ([`video_decode`](crate::video_decode)), audio via
/// `symphonia`. A `.trim` or an injected `duration` wins over this.
pub(super) const STUB_PROBE_SECONDS: f32 = 1.0;

/// Decoded video — the visual channel. Built with
/// `VideoFile::builder().path("x.mp4")`.
///
/// Its intrinsic length is the file's, read once by the resolve pass via an
/// `ffprobe` header read (`probe`); an injected `duration` (or, when the
/// file is un-probable, `STUB_PROBE_SECONDS`) is the fallback so a
/// test that names a non-existent path still resolves. A `.trim(a..b)` crops the
/// SOURCE seconds, shortening the reported duration to `b - a`.
///
/// DECODE (step 9): a per-source `ffmpeg` CHILD process emitting raw `rgba`
/// scaled to the target (`.sketch/01` ZONE C). The mutable decoder state (the
/// child + frame cache + decode position) lives OUTSIDE this struct in a
/// process-global pool ([`video_decode`](crate::video_decode)), so `VideoFile`
/// stays `Clone + Keyable` pure data (`path` + `trim` + injected `duration`) and
/// the decoder state never enters any cache key.
#[crate::component(timeline)]
// `Clone` so the leaf can be a field of a `#[component(timeline)]` fn (e.g. the
// `.sketch/01` `Dialogue(voice: AudioFile)`): the macro clones `self` to
// destructure the body's fields, so every component field type must be `Clone`.
#[derive(Clone, crate::Keyable)]
pub struct VideoFile {
    #[builder(into)]
    pub path: String,
    /// Optional override for the probed duration. Test-injectable; `None` reads
    /// the real `ffprobe` length (with the trim length / stub as fallbacks).
    #[builder(into)]
    pub duration: Option<f32>,
    /// SOURCE-clock crop `(start, end)` set by [`Self::trim`]; `None` plays the
    /// whole file. The reported [`duration`](TimelineComponent::duration) is
    /// `end - start` when set, and `frame` offsets the source time by `start`.
    #[builder(skip)]
    pub trim: Option<(f32, f32)>,
}

impl VideoFile {
    /// Crops to SOURCE seconds `a..b` (the in/out crop). Shortens
    /// [`duration`](TimelineComponent::duration) to `b - a`. An inherent method
    /// so it shadows the generic [`Timed::trim`](crate::timeline_component::Timed::trim)
    /// no-op for the concrete leaf (`.trim` actually records the crop here).
    pub fn trim(mut self, r: std::ops::Range<f32>) -> Self {
        self.trim = Some((r.start, r.end));
        self
    }

    /// Duration probe (`.sketch/02 §12`). An injected `duration` wins; otherwise
    /// the SOURCE length read by `ffprobe`, cropped by a `.trim`. If the file is
    /// un-probable (e.g. a placeholder test path), falls back to the trim length
    /// or [`STUB_PROBE_SECONDS`] so resolve still has a determinate length.
    fn probe(&self) -> f32 {
        if let Some(d) = self.duration {
            return d;
        }
        if let Some((a, b)) = self.trim {
            // A trim fixes the length regardless of the source (clamped to ≥ 0).
            return (b - a).max(0.0);
        }
        crate::video_decode::probe_duration(&self.path).unwrap_or(STUB_PROBE_SECONDS)
    }
}

impl TimelineComponent for VideoFile {
    fn duration(&self) -> Option<f32> {
        Some(self.probe())
    }

    fn frame(
        &self,
        clock: Clock<'_>,
        canvas: Vec2,
        target: Resolution,
        ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage> {
        // The `Placed` wrapper has already rebased + speed-scaled `local` to this
        // clip's source-local axis; the leaf only adds its `.trim` start to reach
        // the absolute source time, then decodes scaled to `target` via the
        // per-source ffmpeg child (`video_decode`). `ctx` is unused: decode
        // spawns its own child and does not touch the render context. `canvas`
        // is ignored: a video decodes scaled to the pixel `target` directly,
        // independent of the logical layout space.
        let _ = ctx;
        let _ = canvas;
        crate::video_decode::decode_frame(&self.path, clock.local().seconds(), self.trim, target)
    }

    fn arrangement(&self, offset: f32) -> Arrangement {
        Arrangement {
            kind: NodeKind::Video,
            label: self.path.clone(),
            name: None,
            source: None,
            start: offset,
            end: offset + self.probe(),
            trim: self.trim,
            triggers: Vec::new(),
            children: Vec::new(),
        }
    }
}

/// Decoded audio — the audio channel. Built with
/// `AudioFile::builder().path("v.wav").gain(0.25)`.
///
/// Its intrinsic length is the file's, read once by the resolve pass via a
/// `symphonia` decode (`probe`). An injected `duration` (or, if decode
/// fails, `STUB_PROBE_SECONDS`) is the fallback so tests that name a
/// non-existent path still resolve. A `.trim(a..b)` crops the SOURCE seconds,
/// shortening the reported duration to `b - a`.
#[crate::component(timeline)]
// `Clone`: see `VideoFile` — a media leaf may be a `#[component(timeline)]`
// field (the `.sketch/01` `Dialogue(voice: AudioFile)`), which the macro clones.
#[derive(Clone, crate::Keyable)]
pub struct AudioFile {
    #[builder(into)]
    pub path: String,
    /// Linear gain applied to the decoded samples (`1.0` = unity).
    #[builder(default = 1.0)]
    pub gain: f32,
    /// Optional override for the probed duration. Test-injectable; `None` reads
    /// the real decoded length (with the stub as a last-resort fallback).
    #[builder(into)]
    pub duration: Option<f32>,
    /// SOURCE-clock crop `(start, end)` set by [`Self::trim`]; `None` plays the
    /// whole file. The reported [`duration`](TimelineComponent::duration) is
    /// `end - start` when set.
    #[builder(skip)]
    pub trim: Option<(f32, f32)>,
}

impl AudioFile {
    /// Crops to SOURCE seconds `a..b` (the in/out crop). Shortens
    /// [`duration`](TimelineComponent::duration) to `b - a`. An inherent method
    /// so it shadows the generic [`Timed::trim`](crate::timeline_component::Timed::trim)
    /// no-op for the concrete leaf (`.trim` actually records the crop here).
    pub fn trim(mut self, r: std::ops::Range<f32>) -> Self {
        self.trim = Some((r.start, r.end));
        self
    }

    /// Duration probe (`.sketch/02 §12`). An injected `duration` wins; otherwise
    /// the SOURCE length read by decoding the file via `symphonia`, cropped by a
    /// `.trim`. If decode fails (e.g. a placeholder test path), falls back to
    /// the trim length or [`STUB_PROBE_SECONDS`] so resolve still has a
    /// determinate length.
    fn probe(&self) -> f32 {
        if let Some(d) = self.duration {
            return d;
        }
        // A trim fixes the length regardless of the source, as long as the
        // source is at least that long — but we still need the source length to
        // clamp. Decode to read the true length; fall back gracefully.
        match audio::decode_file(&self.path, self.trim) {
            Ok(buf) => {
                let ch = buf.channels.max(1) as usize;
                let frames = buf.samples.len() / ch;
                frames as f32 / buf.rate.max(1) as f32
            }
            Err(_) => self
                .trim
                .map(|(a, b)| (b - a).max(0.0))
                .unwrap_or(STUB_PROBE_SECONDS),
        }
    }
}

impl TimelineComponent for AudioFile {
    fn duration(&self) -> Option<f32> {
        Some(self.probe())
    }

    fn samples(&self, clock: Clock<'_>, window: f32) -> Option<AudioBuffer> {
        // The eager mix-down uses `mix_into`; this per-window seam is unused.
        let _ = (clock, window);
        None
    }

    fn mix_into(&self, mix: &mut AudioMix, start_secs: f32, speed: f32) {
        // Decode the whole (trimmed) source, conform it to the mix's fixed
        // rate / channel layout (applying gain + the placement speed), and sum
        // it in at the resolved start. Decode failure ⇒ contribute silence.
        if let Ok(buf) = audio::decode_file(&self.path, self.trim) {
            let conformed = audio::conform(buf, mix.rate(), mix.channels(), self.gain, speed);
            mix.add(&conformed, start_secs);
        }
    }

    fn arrangement(&self, offset: f32) -> Arrangement {
        Arrangement {
            kind: NodeKind::Audio,
            label: self.path.clone(),
            name: None,
            source: None,
            start: offset,
            end: offset + self.probe(),
            trim: self.trim,
            triggers: Vec::new(),
            children: Vec::new(),
        }
    }
}

/// 字幕 — the subtitle channel only (written to .srt/.vtt, NOT a burned-in
/// telop, which is a visual). Built with `Subtitle::builder().text("…")`.
///
/// TIMELESS (`measure()` = `None`): its interval comes from the placement window
/// (`.at(0.0..dur)`) or a `.fill()` taking the container's resolved length. Its
/// [`cues`](TimelineComponent::cues) emit `Cue { start: offset, end: offset +
/// resolved_len, text }`. `frame` / `samples` are `None`.
#[crate::component(timeline)]
// `Clone`: see `VideoFile` — a leaf may be a `#[component(timeline)]` field that
// the macro clones to build the body.
#[derive(Clone, crate::Keyable)]
pub struct Subtitle {
    #[builder(into)]
    pub text: String,
}

impl TimelineComponent for Subtitle {
    // `duration` defaults to `None` (timeless): the placement window supplies the
    // length, so `measure` (which defaults to `duration`) is `None` too.

    fn cues(&self, offset: f32) -> Vec<Cue> {
        // The resolved length comes from the placement window / `.fill()`, which
        // wraps this leaf in a `Placed`. When called directly (no window) the
        // leaf is timeless, so the cue is a zero-length point at `offset`; the
        // wrapping `Placed` (or container, for `.fill()`) re-stamps the real end.
        let end = offset + self.duration().unwrap_or(0.0);
        vec![Cue {
            start: offset,
            end,
            text: self.text.clone(),
        }]
    }

    fn arrangement(&self, offset: f32) -> Arrangement {
        // Timeless: un-windowed it is a 0-length point at `offset`; the wrapping
        // `Placed` / `.fill()` container stamps the real end (mirrors `cues`).
        Arrangement {
            kind: NodeKind::Subtitle,
            label: self.text.clone(),
            name: None,
            source: None,
            start: offset,
            end: offset + self.duration().unwrap_or(0.0),
            trim: None,
            triggers: Vec::new(),
            children: Vec::new(),
        }
    }
}
