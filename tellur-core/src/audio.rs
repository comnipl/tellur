//! Audio decode + eager mix-down for the timeline AUDIO channel — STEP 8.
//!
//! Two pieces live here:
//!
//! 1. [`decode_file`] — pure-Rust decode of an audio file (WAV/mp3/flac/...) to
//!    interleaved f32 PCM at the source's native rate / channel count, via
//!    `symphonia`. It honours a SOURCE-clock trim (`.trim(a..b)`) by decoding
//!    only that span of seconds.
//!
//! 2. [`AudioMix`] — the eager mix-down accumulator (B4 v1, `.sketch/01` ZONE C
//!    / `.sketch/02 §15`). One fixed output rate + channel layout is chosen at
//!    the encoder boundary; every leaf resamples / re-channels / gain-scales its
//!    decoded buffer into that layout and SUMS into the mix at its resolved
//!    sample offset. This is simpler than a per-window `samples(Clock, window)`
//!    pull and is what the audit endorses for v1 (the encoder only ever needs
//!    the whole mixed buffer).
//!
//! Resampling is naive linear interpolation; a placement-window speed change
//! therefore PITCH-SHIFTS the source, which is acceptable for v1.

use std::io;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::timeline_component::AudioBuffer;

/// Maps a symphonia error into an [`io::Error`] so the decode path stays in the
/// crate's existing `io::Result` vocabulary (the encoder/probe seams use it).
fn map_err(e: SymphoniaError) -> io::Error {
    match e {
        SymphoniaError::IoError(io) => io,
        other => io::Error::new(io::ErrorKind::InvalidData, other),
    }
}

/// Decodes `path` to interleaved f32 PCM at the file's NATIVE rate / channel
/// count, optionally cropping to the SOURCE seconds `trim = (start, end)`.
///
/// The returned [`AudioBuffer`] carries the native rate and channel count; the
/// mix-down resamples / re-channels it into the encoder's fixed layout. A `None`
/// trim decodes the whole file.
pub fn decode_file(path: &str, trim: Option<(f32, f32)>) -> io::Result<AudioBuffer> {
    let file = std::fs::File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    // Hint the demuxer with the file extension; symphonia still falls back to
    // content sniffing if the extension is absent or wrong.
    let mut hint = Hint::new();
    if let Some(ext) = std::path::Path::new(path).extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(map_err)?;
    let mut format = probed.format;

    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no decodable audio track"))?;
    let track_id = track.id;

    let rate = track.codec_params.sample_rate.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "audio track has no sample rate")
    })?;

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(map_err)?;

    // The trim is in SOURCE seconds; convert to a per-channel frame window once
    // we know the rate. Channels are read from the first decoded buffer's spec
    // (the codec-params channel map can be absent for some containers).
    let mut channels: u16 = 0;
    let mut samples: Vec<f32> = Vec::new();
    let mut sample_buf: Option<SampleBuffer<f32>> = None;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            // Clean EOF: symphonia signals end-of-stream as an IoError with the
            // UnexpectedEof kind on `next_packet`.
            Err(SymphoniaError::IoError(io)) if io.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(SymphoniaError::ResetRequired) => break,
            Err(e) => return Err(map_err(e)),
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => {
                let spec = *decoded.spec();
                if channels == 0 {
                    channels = spec.channels.count() as u16;
                }
                // (Re)allocate the interleaving scratch buffer to fit this frame.
                let buf = sample_buf.get_or_insert_with(|| {
                    SampleBuffer::<f32>::new(decoded.capacity() as u64, spec)
                });
                buf.copy_interleaved_ref(decoded);
                samples.extend_from_slice(buf.samples());
            }
            // A decode error on a single packet is recoverable — skip it.
            Err(SymphoniaError::DecodeError(_)) | Err(SymphoniaError::IoError(_)) => continue,
            Err(e) => return Err(map_err(e)),
        }
    }

    if channels == 0 {
        // No audio was produced (e.g. an empty file); return silence at the
        // native rate with a single channel so callers have a valid layout.
        channels = 1;
    }

    if let Some((start, end)) = trim {
        crop_interleaved(&mut samples, rate, channels, start, end);
    }

    Ok(AudioBuffer {
        samples,
        rate,
        channels,
    })
}

/// Crops `samples` in place to the SOURCE seconds `[start, end)` at `rate` /
/// `channels`. Out-of-range bounds clamp to the buffer.
fn crop_interleaved(samples: &mut Vec<f32>, rate: u32, channels: u16, start: f32, end: f32) {
    let ch = channels.max(1) as usize;
    let total_frames = samples.len() / ch;
    let start_frame = ((start.max(0.0) * rate as f32).round() as usize).min(total_frames);
    let end_frame = ((end.max(0.0) * rate as f32).round() as usize).min(total_frames);
    if end_frame <= start_frame {
        samples.clear();
        return;
    }
    let lo = start_frame * ch;
    let hi = end_frame * ch;
    samples.copy_within(lo..hi, 0);
    samples.truncate(hi - lo);
}

// ── Buffer transforms (rate / channel / gain / speed) ────────────────────────

/// Naive linear resample of an interleaved buffer from `from_rate` to
/// `to_rate`, keeping the channel count. A no-op when the rates already match.
fn resample(samples: &[f32], channels: u16, from_rate: u32, to_rate: u32) -> Vec<f32> {
    if from_rate == to_rate || samples.is_empty() {
        return samples.to_vec();
    }
    let ch = channels.max(1) as usize;
    let in_frames = samples.len() / ch;
    if in_frames == 0 {
        return Vec::new();
    }
    let ratio = to_rate as f64 / from_rate as f64;
    let out_frames = ((in_frames as f64) * ratio).round().max(0.0) as usize;
    let mut out = vec![0.0f32; out_frames * ch];
    for of in 0..out_frames {
        // Source frame position for this output frame.
        let src_pos = of as f64 / ratio;
        let i0 = src_pos.floor() as usize;
        let frac = (src_pos - i0 as f64) as f32;
        let i1 = (i0 + 1).min(in_frames - 1);
        for c in 0..ch {
            let a = samples[i0 * ch + c];
            let b = samples[i1 * ch + c];
            out[of * ch + c] = a + (b - a) * frac;
        }
    }
    out
}

/// Re-channels an interleaved buffer from `from` channels to `to` channels:
/// mono→N duplicates the single channel; N→mono averages; otherwise channels
/// are truncated / zero-padded. A no-op when the counts match.
fn rechannel(samples: &[f32], from: u16, to: u16) -> Vec<f32> {
    let from = from.max(1) as usize;
    let to_ch = to.max(1) as usize;
    if from == to_ch {
        return samples.to_vec();
    }
    let frames = samples.len() / from;
    let mut out = vec![0.0f32; frames * to_ch];
    for f in 0..frames {
        if from == 1 {
            // Mono → N: duplicate into every output channel.
            let v = samples[f];
            for c in 0..to_ch {
                out[f * to_ch + c] = v;
            }
        } else if to_ch == 1 {
            // N → mono: average the input channels.
            let mut acc = 0.0f32;
            for c in 0..from {
                acc += samples[f * from + c];
            }
            out[f] = acc / from as f32;
        } else {
            // General case: copy the overlapping channels, leave the rest at 0.
            for c in 0..to_ch.min(from) {
                out[f * to_ch + c] = samples[f * from + c];
            }
        }
    }
    out
}

/// Time-scales an interleaved buffer by `speed` (a placement-window stretch):
/// `speed > 1` plays faster (fewer output frames, higher pitch). Implemented as
/// a resample by `1 / speed`; a no-op at unity speed.
fn time_scale(buf: AudioBuffer, speed: f32) -> AudioBuffer {
    if (speed - 1.0).abs() < f32::EPSILON || speed <= 0.0 || buf.samples.is_empty() {
        return buf;
    }
    // Playing at `speed` over the same source means the output covers
    // `len / speed` seconds, i.e. resample to `rate / speed` then relabel back
    // to `rate` so the timeline reads it at the target rate.
    let scaled_rate = ((buf.rate as f32) / speed).round().max(1.0) as u32;
    let samples = resample(&buf.samples, buf.channels, buf.rate, scaled_rate);
    AudioBuffer {
        samples,
        rate: buf.rate,
        channels: buf.channels,
    }
}

/// Conforms `buf` to the target `rate` / `channels` and applies `gain` and the
/// placement `speed`, returning an interleaved buffer ready to sum into a mix.
pub fn conform(buf: AudioBuffer, rate: u32, channels: u16, gain: f32, speed: f32) -> Vec<f32> {
    // 1. Speed (pitch-shifting time scale) on the native buffer.
    let buf = time_scale(buf, speed);
    // 2. Resample to the target rate, then re-channel to the target layout.
    let resampled = resample(&buf.samples, buf.channels, buf.rate, rate);
    let mut out = rechannel(&resampled, buf.channels, channels);
    // 3. Gain.
    if (gain - 1.0).abs() > f32::EPSILON {
        for s in &mut out {
            *s *= gain;
        }
    }
    out
}

// ── The mix-down accumulator ─────────────────────────────────────────────────

/// The eager mix-down target: one interleaved f32 buffer at a fixed `rate` /
/// `channels`, sized to the resolved timeline length. Leaves [`add`](Self::add)
/// their conformed buffers at a resolved sample offset; sums clamp on overflow.
#[derive(Debug, Clone)]
pub struct AudioMix {
    samples: Vec<f32>,
    rate: u32,
    channels: u16,
}

impl AudioMix {
    /// A silent mix of `duration` seconds at `rate` / `channels`.
    pub fn new(duration: f32, rate: u32, channels: u16) -> Self {
        let frames = (duration.max(0.0) * rate as f32).ceil() as usize;
        Self {
            samples: vec![0.0f32; frames * channels.max(1) as usize],
            rate,
            channels,
        }
    }

    /// The mix's fixed output rate.
    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// The mix's fixed channel layout.
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Sums an already-conformed (rate/channels/gain/speed-matched) interleaved
    /// buffer into the mix starting at `start_secs`, clamping each summed sample
    /// to `[-1, 1]` so overlapping tracks never wrap. Frames past the mix end
    /// are dropped (the resolved length is authoritative).
    pub fn add(&mut self, conformed: &[f32], start_secs: f32) {
        let ch = self.channels.max(1) as usize;
        let start_frame = (start_secs.max(0.0) * self.rate as f32).round() as usize;
        let base = start_frame * ch;
        for (i, &s) in conformed.iter().enumerate() {
            let idx = base + i;
            if idx >= self.samples.len() {
                break;
            }
            self.samples[idx] = (self.samples[idx] + s).clamp(-1.0, 1.0);
        }
    }

    /// Consumes the mix into an [`AudioBuffer`].
    pub fn into_buffer(self) -> AudioBuffer {
        AudioBuffer {
            samples: self.samples,
            rate: self.rate,
            channels: self.channels,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_doubles_rate_length() {
        // Mono ramp at 4 Hz upsampled to 8 Hz roughly doubles the frame count.
        let src = vec![0.0, 1.0, 2.0, 3.0];
        let out = resample(&src, 1, 4, 8);
        assert_eq!(out.len(), 8);
        // Linear interpolation keeps the endpoints near the source values.
        assert!((out[0] - 0.0).abs() < 1e-3);
    }

    #[test]
    fn rechannel_mono_to_stereo_duplicates() {
        let mono = vec![0.5, -0.5];
        let stereo = rechannel(&mono, 1, 2);
        assert_eq!(stereo, vec![0.5, 0.5, -0.5, -0.5]);
    }

    #[test]
    fn rechannel_stereo_to_mono_averages() {
        let stereo = vec![1.0, 0.0, 0.0, 1.0];
        let mono = rechannel(&stereo, 2, 1);
        assert_eq!(mono, vec![0.5, 0.5]);
    }

    #[test]
    fn mix_add_sums_and_clamps() {
        let mut mix = AudioMix::new(1.0, 4, 1);
        // Two unit tracks at frame 0 sum to 2.0 then clamp to 1.0.
        mix.add(&[1.0, 1.0], 0.0);
        mix.add(&[1.0, 1.0], 0.0);
        let buf = mix.into_buffer();
        assert_eq!(buf.samples[0], 1.0);
        assert_eq!(buf.samples[1], 1.0);
    }

    #[test]
    fn conform_applies_gain() {
        let buf = AudioBuffer {
            samples: vec![0.5, 0.5],
            rate: 48_000,
            channels: 1,
        };
        // Same rate, mono→mono, gain 0.5 halves the samples.
        let out = conform(buf, 48_000, 1, 0.5, 1.0);
        assert_eq!(out, vec![0.25, 0.25]);
    }
}
