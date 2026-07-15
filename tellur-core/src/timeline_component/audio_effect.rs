//! Ordered, component-native audio gain effects.
//!
//! A [`GainEnvelope`] is an ordinary timeline wrapper: its position in the
//! builder tree is its execution order.  It keeps the visual, subtitle, and
//! arrangement channels transparent and only transforms recursively rendered
//! audio blocks.

use std::hash::{Hash, Hasher};

use crate::geometry::Vec2;
use crate::raster::{RasterImage, RasterResidency, Resolution};
use crate::render_context::RenderContext;

use super::{
    Arrangement, AudioBlockMut, AudioRenderContext, Clock, Cue, ResolveCtx, TimelineBuilder,
    TimelineComponent,
};

/// A point on the immediate child's local timeline.
///
/// Numeric conversion is deliberately asymmetric: non-negative values count
/// from the child's start, while negative values count backwards from its end.
/// Use [`End`](Self::End) when the exact end is intended; it avoids using a
/// floating-point sentinel for that structurally meaningful position.
#[derive(Debug, Clone, Copy)]
pub enum EnvelopePoint {
    /// `seconds` after the immediate child's start.
    FromStart(f64),
    /// `seconds` before the immediate child's end.  The stored value is a
    /// non-negative magnitude.
    BeforeEnd(f64),
    /// The exact end of the immediate child.
    End,
}

impl From<f64> for EnvelopePoint {
    fn from(seconds: f64) -> Self {
        if seconds < 0.0 {
            Self::BeforeEnd(-seconds)
        } else {
            Self::FromStart(seconds)
        }
    }
}

// Float-bearing component keys use bit identity.  This makes equality total
// (including NaNs) and keeps `Eq`/`Hash` coherent for the dynamic component
// identity machinery.
impl PartialEq for EnvelopePoint {
    fn eq(&self, other: &Self) -> bool {
        match (*self, *other) {
            (Self::FromStart(a), Self::FromStart(b)) | (Self::BeforeEnd(a), Self::BeforeEnd(b)) => {
                a.to_bits() == b.to_bits()
            }
            (Self::End, Self::End) => true,
            _ => false,
        }
    }
}

impl Eq for EnvelopePoint {}

impl Hash for EnvelopePoint {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match *self {
            Self::FromStart(seconds) => {
                0_u8.hash(state);
                seconds.to_bits().hash(state);
            }
            Self::BeforeEnd(seconds) => {
                1_u8.hash(state);
                seconds.to_bits().hash(state);
            }
            Self::End => 2_u8.hash(state),
        }
    }
}

/// A two-point, piecewise-linear gain envelope around `child`.
///
/// Before the first point the first gain is held; after the second point the
/// second gain is held.  Both points are resolved against the *immediate*
/// child's duration, so wrapping order remains observable.  Invalid envelopes
/// are reported during timeline resolution and render silence rather than
/// allowing a NaN or an ambiguous end-relative value into the mix.
#[derive(Debug, Clone)]
pub struct GainEnvelope<C> {
    child: C,
    from: EnvelopePoint,
    from_gain: f32,
    to: EnvelopePoint,
    to_gain: f32,
    // Invalid fade durations are specified to be a transparent identity while
    // preserving the wrapper's concrete return type.  Keep that intent
    // explicit rather than encoding it as an otherwise-invalid pair of points.
    identity: bool,
}

impl<C> GainEnvelope<C> {
    /// Wraps `child` in a two-point gain envelope.
    pub fn new<F, T>(child: C, from: (F, f32), to: (T, f32)) -> Self
    where
        F: Into<EnvelopePoint>,
        T: Into<EnvelopePoint>,
    {
        Self {
            child,
            from: from.0.into(),
            from_gain: from.1,
            to: to.0.into(),
            to_gain: to.1,
            identity: false,
        }
    }

    fn identity(child: C) -> Self {
        Self {
            child,
            from: EnvelopePoint::FromStart(0.0),
            from_gain: 1.0,
            to: EnvelopePoint::End,
            to_gain: 1.0,
            identity: true,
        }
    }

    /// The wrapped child.
    pub fn child(&self) -> &C {
        &self.child
    }

    /// The first `(point, gain)` pair.
    pub fn from(&self) -> (EnvelopePoint, f32) {
        (self.from, self.from_gain)
    }

    /// The second `(point, gain)` pair.
    pub fn to(&self) -> (EnvelopePoint, f32) {
        (self.to, self.to_gain)
    }

    fn resolved(&self, duration: Option<f64>) -> Result<ResolvedEnvelope, EnvelopeError> {
        if self.identity {
            return Ok(ResolvedEnvelope {
                from: 0.0,
                from_gain: 1.0,
                to: 1.0,
                to_gain: 1.0,
            });
        }
        if !self.from_gain.is_finite() || !self.to_gain.is_finite() {
            return Err(EnvelopeError::NonFiniteGain);
        }
        let duration = duration.ok_or(EnvelopeError::MissingDuration)?;
        if !duration.is_finite() || duration < 0.0 {
            return Err(EnvelopeError::InvalidDuration);
        }
        let from = resolve_point(self.from, duration)?;
        let to = resolve_point(self.to, duration)?;
        let span = to - from;
        if !(span.is_finite() && span > 0.0) {
            return Err(EnvelopeError::ReversedOrEmpty);
        }
        Ok(ResolvedEnvelope {
            from,
            from_gain: self.from_gain,
            to,
            to_gain: self.to_gain,
        })
    }
}

impl<C: PartialEq> PartialEq for GainEnvelope<C> {
    fn eq(&self, other: &Self) -> bool {
        self.child == other.child
            && self.from == other.from
            && self.from_gain.to_bits() == other.from_gain.to_bits()
            && self.to == other.to
            && self.to_gain.to_bits() == other.to_gain.to_bits()
            && self.identity == other.identity
    }
}

impl<C: Eq> Eq for GainEnvelope<C> {}

impl<C: Hash> Hash for GainEnvelope<C> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.child.hash(state);
        self.from.hash(state);
        self.from_gain.to_bits().hash(state);
        self.to.hash(state);
        self.to_gain.to_bits().hash(state);
        self.identity.hash(state);
    }
}

impl<C> TimelineComponent for GainEnvelope<C>
where
    C: TimelineComponent + PartialEq + Hash + 'static,
{
    fn duration(&self) -> Option<f64> {
        self.child.duration()
    }

    fn measure(&self) -> Option<f64> {
        self.child.measure()
    }

    fn resolve(&self, abs_start: f64, out: &mut ResolveCtx) -> f64 {
        if let Err(error) = self.resolved(self.child.measure()) {
            out.error(format!("invalid gain envelope: {error}"));
        }
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
        self.child.frame(clock, canvas, target, residency, ctx)
    }

    fn render_audio_block(&self, mut block: AudioBlockMut<'_>, ctx: &mut AudioRenderContext) {
        if self.identity {
            self.child.render_audio_block(block, ctx);
            return;
        }

        let Ok(envelope) = self.resolved(self.child.measure()) else {
            block.clear();
            return;
        };

        let request = block.request();
        let mut scratch = ctx.take_scratch(request.sample_len());
        self.child
            .render_audio_block(AudioBlockMut::new(request, &mut scratch), ctx);

        let channels = usize::from(request.channels());
        for frame in 0..request.frame_count() {
            let gain = envelope.gain_at(request.time_at(frame));
            let start = frame * channels;
            for (output, input) in block.samples_mut()[start..start + channels]
                .iter_mut()
                .zip(&scratch[start..start + channels])
            {
                *output = *input * gain;
            }
        }
        ctx.recycle_scratch(scratch);
    }

    fn cues(&self, offset: f64) -> Vec<Cue> {
        self.child.cues(offset)
    }

    fn arrangement(&self, offset: f64) -> Arrangement {
        self.child.arrangement(offset)
    }
}

/// Lets an effect-wrapped component drop directly into timeline containers.
impl<C> From<GainEnvelope<C>> for Box<dyn TimelineComponent + Send>
where
    C: TimelineComponent + PartialEq + Hash + Send + 'static,
{
    fn from(envelope: GainEnvelope<C>) -> Self {
        Box::new(envelope)
    }
}

/// Gain-envelope verbs for already-built timeline components.
///
/// Each call immediately wraps `self`, making the last call the outermost
/// effect and therefore preserving source-order semantics.
pub trait AudioEffects: TimelineComponent + PartialEq + Hash + Sized + 'static {
    /// Applies a two-point linear envelope.  Each argument is `(time, gain)`;
    /// negative numeric times are relative to the immediate child's end.
    fn gain_envelope<F, T>(self, from: (F, f32), to: (T, f32)) -> GainEnvelope<Self>
    where
        F: Into<EnvelopePoint>,
        T: Into<EnvelopePoint>,
    {
        GainEnvelope::new(self, from, to)
    }

    /// Fades from silence to unity over the first `duration` seconds.
    fn fade_in(self, duration: f64) -> GainEnvelope<Self> {
        if duration.is_finite() && duration > 0.0 {
            GainEnvelope::new(self, (0.0, 0.0), (duration, 1.0))
        } else {
            GainEnvelope::identity(self)
        }
    }

    /// Fades from unity to silence over the final `duration` seconds.
    fn fade_out(self, duration: f64) -> GainEnvelope<Self> {
        if duration.is_finite() && duration > 0.0 {
            GainEnvelope::new(
                self,
                (EnvelopePoint::BeforeEnd(duration), 1.0),
                (EnvelopePoint::End, 0.0),
            )
        } else {
            GainEnvelope::identity(self)
        }
    }
}

impl<C> AudioEffects for C where C: TimelineComponent + PartialEq + Hash + Sized + 'static {}

/// Builder-side gain-envelope verbs, so complete builders never need an
/// explicit `.build()` before applying an audio effect.
pub trait AudioEffectsBuilder: TimelineBuilder {
    /// Builds immediately, then applies [`AudioEffects::gain_envelope`].
    fn gain_envelope<F, T>(self, from: (F, f32), to: (T, f32)) -> GainEnvelope<Self::Output>
    where
        F: Into<EnvelopePoint>,
        T: Into<EnvelopePoint>,
    {
        GainEnvelope::new(self.build_component(), from, to)
    }

    /// Builds immediately, then fades in over `duration` seconds.
    fn fade_in(self, duration: f64) -> GainEnvelope<Self::Output> {
        let child = self.build_component();
        if duration.is_finite() && duration > 0.0 {
            GainEnvelope::new(child, (0.0, 0.0), (duration, 1.0))
        } else {
            GainEnvelope::identity(child)
        }
    }

    /// Builds immediately, then fades out over the child's final `duration`
    /// seconds.
    fn fade_out(self, duration: f64) -> GainEnvelope<Self::Output> {
        let child = self.build_component();
        if duration.is_finite() && duration > 0.0 {
            GainEnvelope::new(
                child,
                (EnvelopePoint::BeforeEnd(duration), 1.0),
                (EnvelopePoint::End, 0.0),
            )
        } else {
            GainEnvelope::identity(child)
        }
    }
}

impl<B: TimelineBuilder> AudioEffectsBuilder for B {}

#[derive(Debug, Clone, Copy)]
struct ResolvedEnvelope {
    from: f64,
    from_gain: f32,
    to: f64,
    to_gain: f32,
}

impl ResolvedEnvelope {
    fn gain_at(self, time: f64) -> f32 {
        if time <= self.from {
            return self.from_gain;
        }
        if time >= self.to {
            return self.to_gain;
        }
        let phase = (time - self.from) / (self.to - self.from);
        ((f64::from(self.to_gain) - f64::from(self.from_gain))
            .mul_add(phase, f64::from(self.from_gain))) as f32
    }
}

#[derive(Debug, Clone, Copy)]
enum EnvelopeError {
    MissingDuration,
    InvalidDuration,
    InvalidPoint,
    NonFiniteGain,
    ReversedOrEmpty,
}

impl std::fmt::Display for EnvelopeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::MissingDuration => "the immediate child has no finite duration",
            Self::InvalidDuration => "the immediate child has an invalid duration",
            Self::InvalidPoint => "an endpoint is non-finite or has a negative magnitude",
            Self::NonFiniteGain => "a gain is non-finite",
            Self::ReversedOrEmpty => "the second endpoint must be after the first",
        };
        f.write_str(message)
    }
}

fn resolve_point(point: EnvelopePoint, duration: f64) -> Result<f64, EnvelopeError> {
    let time = match point {
        EnvelopePoint::FromStart(seconds) => {
            if !seconds.is_finite() || seconds < 0.0 {
                return Err(EnvelopeError::InvalidPoint);
            }
            seconds
        }
        EnvelopePoint::BeforeEnd(seconds) => {
            if !seconds.is_finite() || seconds < 0.0 {
                return Err(EnvelopeError::InvalidPoint);
            }
            duration - seconds
        }
        EnvelopePoint::End => duration,
    };
    // Envelope knots may intentionally sit outside the child's playable
    // interval. This keeps a fade longer than its child meaningful: only the
    // in-range portion of the ramp is observed, matching the former AudioFile
    // fade behaviour. End-relative points still need the finite child duration
    // above, but a magnitude greater than that duration simply resolves before
    // local zero.
    if !time.is_finite() {
        return Err(EnvelopeError::InvalidPoint);
    }
    Ok(time)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline_component::{AudioRenderRequest, NodeKind};

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    struct ConstantAudio {
        duration_millis: u32,
    }

    impl TimelineComponent for ConstantAudio {
        fn duration(&self) -> Option<f64> {
            Some(f64::from(self.duration_millis) / 1_000.0)
        }

        fn render_audio_block(&self, mut block: AudioBlockMut<'_>, _ctx: &mut AudioRenderContext) {
            block.samples_mut().fill(1.0);
        }

        fn arrangement(&self, offset: f64) -> Arrangement {
            Arrangement {
                kind: NodeKind::Audio,
                label: "constant".into(),
                name: None,
                source: None,
                start: offset,
                end: offset + self.duration().unwrap(),
                trim: None,
                triggers: Vec::new(),
                children: Vec::new(),
            }
        }
    }

    fn render<C>(component: &C, frames: usize, rate: u32) -> Vec<f32>
    where
        C: TimelineComponent,
    {
        let request = AudioRenderRequest::new(0, frames, rate, 1);
        let mut samples = vec![0.0; frames];
        component.render_audio_block(
            AudioBlockMut::new(request, &mut samples),
            &mut AudioRenderContext::new(),
        );
        samples
    }

    #[test]
    fn numeric_negative_points_are_end_relative() {
        assert_eq!(EnvelopePoint::from(-1.25), EnvelopePoint::BeforeEnd(1.25));
        assert_eq!(EnvelopePoint::from(1.25), EnvelopePoint::FromStart(1.25));
    }

    #[test]
    fn envelope_interpolates_and_holds_endpoint_gains() {
        let effect = ConstantAudio {
            duration_millis: 2_000,
        }
        .gain_envelope((0.5, 0.0), (1.0, 1.0));

        assert_eq!(
            render(&effect, 8, 4),
            vec![0.0, 0.0, 0.0, 0.5, 1.0, 1.0, 1.0, 1.0]
        );
    }

    #[test]
    fn fade_out_resolves_against_the_immediate_child_end() {
        let effect = ConstantAudio {
            duration_millis: 2_000,
        }
        .fade_out(1.0);

        assert_eq!(render(&effect, 4, 2), vec![1.0, 1.0, 1.0, 0.5]);
    }

    #[test]
    fn fade_longer_than_child_keeps_the_in_range_partial_ramp() {
        let fade_in = ConstantAudio {
            duration_millis: 500,
        }
        .fade_in(1.0);
        let fade_out = ConstantAudio {
            duration_millis: 500,
        }
        .fade_out(1.0);

        let mut resolve = ResolveCtx::new();
        fade_in.resolve(0.0, &mut resolve);
        fade_out.resolve(0.0, &mut resolve);
        assert!(resolve.errors().is_empty());
        assert_eq!(render(&fade_in, 2, 4), vec![0.0, 0.25]);
        assert_eq!(render(&fade_out, 2, 4), vec![0.5, 0.25]);
    }

    #[test]
    fn reversed_envelope_reports_an_error_and_renders_silence() {
        let effect = ConstantAudio {
            duration_millis: 2_000,
        }
        .gain_envelope((1.5, 0.0), (0.5, 1.0));
        let mut resolve = ResolveCtx::new();

        assert_eq!(effect.resolve(0.0, &mut resolve), 2.0);
        assert_eq!(resolve.errors().len(), 1);
        assert_eq!(render(&effect, 4, 2), vec![0.0; 4]);
    }

    #[test]
    fn invalid_fade_duration_is_an_identity_wrapper() {
        let effect = ConstantAudio {
            duration_millis: 2_000,
        }
        .fade_in(f64::NAN);
        let mut resolve = ResolveCtx::new();

        effect.resolve(0.0, &mut resolve);
        assert!(resolve.errors().is_empty());
        assert_eq!(render(&effect, 4, 2), vec![1.0; 4]);
    }
}
