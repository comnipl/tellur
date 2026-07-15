//! The media / subtitle leaves: [`VideoFile`], [`AudioFile`], [`Subtitle`].

use crate::audio;
use crate::geometry::Vec2;
use crate::raster::{RasterImage, RasterResidency, Resolution};
use crate::render_context::RenderContext;
use crate::time::Time;
use crate::timeline_component::{
    Arrangement, AudioBlockMut, AudioRenderContext, Clock, Cue, NodeKind, TimelineComponent, Trim,
    TrimBounds,
};

// ── Leaves ───────────────────────────────────────────────────────────────────

/// A last-resort media duration for the [`probe`](VideoFile::probe) seam when no
/// real length is available (`.sketch/02 §12`): a missing/un-probable file, or a
/// placeholder test path. Real decode reads the header — video via
/// `ffprobe`/`ffmpeg` ([`video_decode`](crate::video_decode)), audio via
/// `symphonia`. An injected `duration` wins over this.
pub(super) const STUB_PROBE_SECONDS: f64 = 1.0;

/// Decoded video — the visual channel. Built with
/// `VideoFile::builder().path("x.mp4")`.
///
/// Its intrinsic length is the file's, read once by the resolve pass via an
/// `ffprobe` header read (`probe`); an injected `duration` (or, when the
/// file is un-probable, `STUB_PROBE_SECONDS`) is the fallback so a
/// test that names a non-existent path still resolves. A generic [`Trim`]
/// wrapper selects a source interval.
///
/// DECODE (step 9): a per-source `ffmpeg` CHILD process emitting raw `rgba`
/// scaled to the target (`.sketch/01` ZONE C). The mutable decoder state (the
/// child + frame cache + decode position) lives OUTSIDE this struct in a
/// process-global pool ([`video_decode`](crate::video_decode)), so `VideoFile`
/// stays `Clone + Keyable` pure data (`path` + injected `duration`) and
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
    /// the real `ffprobe` length (with the stub as fallback).
    #[builder(into)]
    pub duration: Option<f64>,
}

impl VideoFile {
    /// Wraps this source in the generic ordered trim component.
    pub fn trim<R: TrimBounds>(self, bounds: R) -> Trim<Self> {
        Trim::new(self, bounds)
    }

    /// Duration probe (`.sketch/02 §12`). An injected `duration` wins; otherwise
    /// the SOURCE length read by `ffprobe`. If the file is un-probable (e.g. a
    /// placeholder test path), falls back to [`STUB_PROBE_SECONDS`].
    fn probe(&self) -> f64 {
        if let Some(d) = self.duration {
            return d;
        }
        crate::video_decode::probe_duration(&self.path).unwrap_or(STUB_PROBE_SECONDS)
    }
}

impl TimelineComponent for VideoFile {
    fn duration(&self) -> Option<f64> {
        Some(self.probe())
    }

    fn frame(
        &self,
        clock: Clock<'_>,
        canvas: Vec2,
        target: Resolution,
        residency: RasterResidency,
        ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage> {
        // Temporal wrappers have already rebased + speed-scaled `local` to this
        // source's clock, then the leaf decodes scaled to `target` via the
        // per-source ffmpeg child (`video_decode`). `canvas` is ignored: a
        // video decodes scaled to the pixel `target` directly, independent of
        // the logical layout space. The context then materializes the
        // consumer-requested representation.
        let _ = canvas;
        let image =
            crate::video_decode::decode_frame(&self.path, clock.local().seconds(), None, target)?;
        Some(ctx.ensure_residency(image, residency))
    }

    fn arrangement(&self, offset: f64) -> Arrangement {
        Arrangement {
            kind: NodeKind::Video,
            label: self.path.clone(),
            name: None,
            source: None,
            start: offset,
            end: offset + self.probe(),
            trim: None,
            triggers: Vec::new(),
            children: Vec::new(),
        }
    }
}

/// Decoded audio — the audio channel. Built with
/// `AudioFile::builder().path("v.wav").gain(0.25).fade_out(0.4)`.
///
/// Its intrinsic length is the file's, read once by the resolve pass via a
/// `symphonia` decode (`probe`). An injected `duration` (or, if decode
/// fails, `STUB_PROBE_SECONDS`) is the fallback so tests that name a
/// non-existent path still resolve. Trim and gain envelopes are ordered
/// component wrappers rather than leaf configuration.
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
    pub duration: Option<f64>,
}

impl AudioFile {
    /// Wraps this source in the generic ordered trim component.
    pub fn trim<R: TrimBounds>(self, bounds: R) -> Trim<Self> {
        Trim::new(self, bounds)
    }

    /// Duration probe (`.sketch/02 §12`). An injected `duration` wins; otherwise
    /// the SOURCE length read by decoding the file via `symphonia`. If decode
    /// fails (e.g. a placeholder test path), falls back to
    /// [`STUB_PROBE_SECONDS`].
    fn probe(&self) -> f64 {
        if let Some(d) = self.duration {
            return d;
        }
        match audio::decoded_duration(&self.path, None) {
            Ok(duration) => duration,
            Err(_) => STUB_PROBE_SECONDS,
        }
    }
}

impl TimelineComponent for AudioFile {
    fn duration(&self) -> Option<f64> {
        Some(self.probe())
    }

    fn render_audio_block(&self, mut block: AudioBlockMut<'_>, ctx: &mut AudioRenderContext) {
        let request = block.request();
        block.clear();
        let duration = self
            .duration
            .or_else(|| ctx.source_duration(&self.path))
            .unwrap_or(STUB_PROBE_SECONDS);
        if !request.may_overlap_local(0.0, duration) {
            return;
        }
        let Some(buf) =
            ctx.conformed_source(&self.path, request.rate(), request.channels(), self.gain)
        else {
            return;
        };

        let channels = request.channels() as usize;
        let source_frames = buf.samples.len() / channels;
        if source_frames == 0 {
            return;
        }
        for output_frame in 0..request.frame_count() {
            let local_time = request.time_at(output_frame);
            if local_time < 0.0 || local_time >= duration {
                continue;
            }
            let position = local_time * request.rate() as f64;
            let source_frame = position.floor() as usize;
            if source_frame >= source_frames {
                continue;
            }
            let next_frame = (source_frame + 1).min(source_frames - 1);
            let fraction = (position - source_frame as f64) as f32;
            let output_base = output_frame * channels;
            let source_base = source_frame * channels;
            let next_base = next_frame * channels;
            for channel in 0..channels {
                let first = buf.samples[source_base + channel];
                let second = buf.samples[next_base + channel];
                block.samples_mut()[output_base + channel] = first + (second - first) * fraction;
            }
        }
    }

    fn arrangement(&self, offset: f64) -> Arrangement {
        Arrangement {
            kind: NodeKind::Audio,
            label: self.path.clone(),
            name: None,
            source: None,
            start: offset,
            end: offset + self.probe(),
            trim: None,
            triggers: Vec::new(),
            children: Vec::new(),
        }
    }
}

/// An invisible, silent placeholder with an explicit `duration` and no
/// visual, audio, or subtitle output of its own — the timeline twin of the
/// layout side's [`SizedBox`](crate::layout::SizedBox). Built with
/// `TimeBox::builder().duration(1.5)`.
///
/// Useful anywhere a `Timeline`/`Sequence` needs an explicit length to hang
/// triggers on or to reserve a beat, without authoring a stub media file
/// just to give a clip a duration (the role
/// `DialogueDuration` played ad hoc before this leaf existed).
#[crate::component(timeline)]
#[derive(Clone, Copy, crate::Keyable)]
pub struct TimeBox {
    pub duration: f64,
}

impl TimelineComponent for TimeBox {
    fn duration(&self) -> Option<f64> {
        Some(self.duration)
    }

    fn arrangement(&self, offset: f64) -> Arrangement {
        Arrangement {
            kind: NodeKind::Timeline,
            label: "time box".to_owned(),
            name: None,
            source: None,
            start: offset,
            end: offset + self.duration,
            trim: None,
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
/// resolved_len, text }`. `frame` is `None` and audio blocks are silent.
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

    fn cues(&self, offset: f64) -> Vec<Cue> {
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

    fn arrangement(&self, offset: f64) -> Arrangement {
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
