//! Audio decode and format conformance for the timeline AUDIO channel.
//!
//! Two pieces live here:
//!
//! 1. [`decode_file`] — pure-Rust decode of an audio file (WAV/mp3/flac/...) to
//!    interleaved f32 PCM at the source's native rate / channel count, via
//!    `symphonia`. It honours a SOURCE-clock trim (`.trim(a..b)`) by decoding
//!    only that span of seconds.
//!
//! 2. Decode/conform helpers and the compatibility [`AudioMix`] accumulator.
//!    Timeline rendering itself uses integer-frame
//!    [`AudioRenderRequest`](crate::timeline_component::AudioRenderRequest)
//!    blocks and direct component-tree recursion; overlay containers sum each
//!    child's block while temporal/effect wrappers remap it before recursion.
//!
//! Resampling is naive linear interpolation; a placement-window speed change
//! therefore PITCH-SHIFTS the source, which is acceptable for v1.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::UNIX_EPOCH;

use lru::LruCache;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time as SymphoniaTime;

use crate::cache_budget::{cache_ram_capacity, try_reserve_cache_ram, BudgetReservation};
use crate::timeline_component::AudioBuffer;

const AUDIO_CACHE_CAPACITY_BYTES: usize = 512 * 1024 * 1024;
const CACHE_FULL_ON_TRIM_SOURCE_BYTES_LIMIT: u64 = 64 * 1024 * 1024;

static DECODE_CACHE: LazyLock<Mutex<AudioDecodeCache>> =
    LazyLock::new(|| Mutex::new(AudioDecodeCache::default()));
static CONFORM_CACHE: LazyLock<Mutex<ConformedAudioCache>> =
    LazyLock::new(|| Mutex::new(ConformedAudioCache::default()));

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AudioCacheKey {
    path: PathBuf,
    len: u64,
    modified_ns: Option<u128>,
}

struct AudioDecodeCache {
    entries: LruCache<AudioCacheKey, CachedAudioBuffer>,
    bytes: usize,
}

struct CachedAudioBuffer {
    buffer: Arc<AudioBuffer>,
    _reservation: BudgetReservation,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ConformedAudioCacheKey {
    source: AudioCacheKey,
    trim: Option<(u64, u64)>,
    rate: u32,
    channels: u16,
    gain_bits: u32,
    speed_bits: u64,
}

struct ConformedAudioCache {
    entries: LruCache<ConformedAudioCacheKey, CachedAudioBuffer>,
    bytes: usize,
}

impl Default for AudioDecodeCache {
    fn default() -> Self {
        Self {
            entries: LruCache::unbounded(),
            bytes: 0,
        }
    }
}

impl AudioDecodeCache {
    fn get(&mut self, key: &AudioCacheKey) -> Option<Arc<AudioBuffer>> {
        self.entries.get(key).map(|entry| Arc::clone(&entry.buffer))
    }

    fn insert(&mut self, key: AudioCacheKey, buffer: Arc<AudioBuffer>) {
        let bytes = audio_buffer_bytes(&buffer);
        let capacity = cache_ram_capacity(AUDIO_CACHE_CAPACITY_BYTES);
        if bytes > capacity {
            return;
        }

        while self.bytes + bytes > capacity {
            let Some((_, old)) = self.entries.pop_lru() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(audio_buffer_bytes(&old.buffer));
        }

        let Some(reservation) = try_reserve_cache_ram(bytes) else {
            return;
        };
        let entry = CachedAudioBuffer {
            buffer,
            _reservation: reservation,
        };
        if let Some(old) = self.entries.put(key, entry) {
            self.bytes = self.bytes.saturating_sub(audio_buffer_bytes(&old.buffer));
        }
        self.bytes += bytes;
    }
}

impl Default for ConformedAudioCache {
    fn default() -> Self {
        Self {
            entries: LruCache::unbounded(),
            bytes: 0,
        }
    }
}

impl ConformedAudioCache {
    fn get(&mut self, key: &ConformedAudioCacheKey) -> Option<Arc<AudioBuffer>> {
        self.entries.get(key).map(|entry| Arc::clone(&entry.buffer))
    }

    fn insert(&mut self, key: ConformedAudioCacheKey, buffer: Arc<AudioBuffer>) {
        let bytes = audio_buffer_bytes(&buffer);
        let capacity = cache_ram_capacity(AUDIO_CACHE_CAPACITY_BYTES);
        if bytes > capacity {
            return;
        }

        while self.bytes + bytes > capacity {
            let Some((_, old)) = self.entries.pop_lru() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(audio_buffer_bytes(&old.buffer));
        }

        let Some(reservation) = try_reserve_cache_ram(bytes) else {
            return;
        };
        let entry = CachedAudioBuffer {
            buffer,
            _reservation: reservation,
        };
        if let Some(old) = self.entries.put(key, entry) {
            self.bytes = self.bytes.saturating_sub(audio_buffer_bytes(&old.buffer));
        }
        self.bytes += bytes;
    }
}

fn audio_buffer_bytes(buffer: &AudioBuffer) -> usize {
    buffer.samples.len() * std::mem::size_of::<f32>()
}

fn audio_cache_key(path: &str) -> io::Result<AudioCacheKey> {
    let raw_path = Path::new(path);
    let metadata = std::fs::metadata(raw_path)?;
    let canonical = std::fs::canonicalize(raw_path).unwrap_or_else(|_| raw_path.to_path_buf());
    let modified_ns = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos());
    Ok(AudioCacheKey {
        path: canonical,
        len: metadata.len(),
        modified_ns,
    })
}

fn cached_full_audio(key: &AudioCacheKey) -> Option<Arc<AudioBuffer>> {
    DECODE_CACHE.lock().ok()?.get(key)
}

fn cache_full_audio(key: AudioCacheKey, buffer: Arc<AudioBuffer>) {
    if let Ok(mut cache) = DECODE_CACHE.lock() {
        cache.insert(key, buffer);
    }
}

fn cached_conformed_audio(key: &ConformedAudioCacheKey) -> Option<Arc<AudioBuffer>> {
    CONFORM_CACHE.lock().ok()?.get(key)
}

fn cache_conformed_audio(key: ConformedAudioCacheKey, buffer: Arc<AudioBuffer>) {
    if let Ok(mut cache) = CONFORM_CACHE.lock() {
        cache.insert(key, buffer);
    }
}

#[cfg(test)]
fn clear_decode_cache_for_tests() {
    if let Ok(mut cache) = DECODE_CACHE.lock() {
        cache.entries.clear();
        cache.bytes = 0;
    }
    if let Ok(mut cache) = CONFORM_CACHE.lock() {
        cache.entries.clear();
        cache.bytes = 0;
    }
}

#[cfg(test)]
fn decode_cache_len_for_tests() -> usize {
    DECODE_CACHE
        .lock()
        .map(|cache| cache.entries.len())
        .unwrap_or(0)
}

#[cfg(test)]
fn conform_cache_len_for_tests() -> usize {
    CONFORM_CACHE
        .lock()
        .map(|cache| cache.entries.len())
        .unwrap_or(0)
}

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
pub fn decode_file(path: &str, trim: Option<(f64, f64)>) -> io::Result<AudioBuffer> {
    let key = audio_cache_key(path)?;

    if let Some(cached) = cached_full_audio(&key) {
        return Ok(slice_audio_buffer(&cached, trim));
    }

    let should_cache_full = trim.is_none() || key.len <= CACHE_FULL_ON_TRIM_SOURCE_BYTES_LIMIT;
    if should_cache_full {
        let decoded = Arc::new(decode_file_uncached(&key.path, None)?);
        let out = slice_audio_buffer(&decoded, trim);
        cache_full_audio(key, decoded);
        return Ok(out);
    }

    decode_file_uncached(&key.path, trim)
}

/// Decoded duration in seconds, using the same cache as [`decode_file`] without
/// cloning a large sample buffer just to count frames.
pub fn decoded_duration(path: &str, trim: Option<(f64, f64)>) -> io::Result<f64> {
    let key = audio_cache_key(path)?;

    if let Some(cached) = cached_full_audio(&key) {
        return Ok(audio_buffer_duration_for_trim(&cached, trim));
    }

    let should_cache_full = trim.is_none() || key.len <= CACHE_FULL_ON_TRIM_SOURCE_BYTES_LIMIT;
    if should_cache_full {
        let decoded = Arc::new(decode_file_uncached(&key.path, None)?);
        let duration = audio_buffer_duration_for_trim(&decoded, trim);
        cache_full_audio(key, Arc::clone(&decoded));
        return Ok(duration);
    }

    let decoded = decode_file_uncached(&key.path, trim)?;
    Ok(audio_buffer_duration(&decoded))
}

/// Decodes and conforms a whole source (or source trim) once for repeated
/// timeline windows. Live preview segments can then slice the conformed PCM
/// instead of re-decoding and re-resampling the same media on every request.
pub(crate) fn conform_file_cached(
    path: &str,
    trim: Option<(f64, f64)>,
    rate: u32,
    channels: u16,
    gain: f32,
    speed: f64,
) -> io::Result<Arc<AudioBuffer>> {
    let source = audio_cache_key(path)?;
    let key = ConformedAudioCacheKey {
        source,
        trim: trim.map(|(start, end)| (start.to_bits(), end.to_bits())),
        rate,
        channels,
        gain_bits: gain.to_bits(),
        speed_bits: speed.to_bits(),
    };

    if let Some(cached) = cached_conformed_audio(&key) {
        return Ok(cached);
    }

    let decoded = decode_file(path, trim)?;
    let samples = conform(decoded, rate, channels, gain, speed);
    let conformed = Arc::new(AudioBuffer {
        samples,
        rate,
        channels,
    });
    cache_conformed_audio(key, Arc::clone(&conformed));
    Ok(conformed)
}

fn decode_file_uncached(path: &Path, trim: Option<(f64, f64)>) -> io::Result<AudioBuffer> {
    let file = std::fs::File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    // Hint the demuxer with the file extension; symphonia still falls back to
    // content sniffing if the extension is absent or wrong.
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
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
    let codec_params = track.codec_params.clone();

    let rate = codec_params.sample_rate.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "audio track has no sample rate")
    })?;
    let channel_hint = codec_params
        .channels
        .map(|channels| channels.count() as u16)
        .unwrap_or(0);

    let mut decoder = symphonia::default::get_codecs()
        .make(&codec_params, &DecoderOptions::default())
        .map_err(map_err)?;

    let (start_secs, end_secs) = trim
        .map(|(start, end)| (start.max(0.0), end.max(0.0)))
        .unwrap_or((0.0, f64::INFINITY));
    if end_secs <= start_secs {
        return Ok(AudioBuffer {
            samples: Vec::new(),
            rate,
            channels: channel_hint.max(1),
        });
    }

    let start_frame = seconds_to_frame(start_secs, rate);
    let end_frame = end_secs
        .is_finite()
        .then(|| seconds_to_frame(end_secs, rate));
    let mut cursor_frame = 0u64;
    if start_secs > 0.0 {
        if let Ok(seeked) = format.seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time: SymphoniaTime::from(start_secs),
                track_id: Some(track_id),
            },
        ) {
            decoder.reset();
            if seeked.track_id == track_id {
                if let Some(time_base) = codec_params.time_base {
                    cursor_frame = time_to_frame(time_base.calc_time(seeked.actual_ts), rate);
                }
            }
        }
    }

    // The trim is in SOURCE seconds; append only the requested source-frame
    // window. A successful seek skips the leading packets for seekable formats;
    // the frame window below keeps the fallback path correct for unseekable
    // formats and trims codec/seek pre-roll precisely.
    let mut channels: u16 = channel_hint;
    let mut samples: Vec<f32> = Vec::new();
    let mut sample_buf: Option<SampleBuffer<f32>> = None;

    loop {
        if end_frame.is_some_and(|end| cursor_frame >= end) {
            break;
        }
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
                // Reallocate the interleaving scratch buffer when a codec emits
                // a larger packet than the previous one.
                let required = decoded.capacity().saturating_mul(spec.channels.count());
                if sample_buf
                    .as_ref()
                    .is_none_or(|buf| buf.capacity() < required)
                {
                    sample_buf = Some(SampleBuffer::<f32>::new(decoded.capacity() as u64, spec));
                }
                let buf = sample_buf.as_mut().expect("sample buffer allocated");
                buf.copy_interleaved_ref(decoded);
                let decoded = buf.samples();
                let ch = channels.max(1) as usize;
                let decoded_frames = (decoded.len() / ch) as u64;
                let chunk_start = cursor_frame;
                let chunk_end = cursor_frame.saturating_add(decoded_frames);
                if chunk_end > start_frame {
                    let lo_frame = start_frame.max(chunk_start);
                    let hi_frame = end_frame.map_or(chunk_end, |end| end.min(chunk_end));
                    if hi_frame > lo_frame {
                        let lo = ((lo_frame - chunk_start) as usize).saturating_mul(ch);
                        let hi = ((hi_frame - chunk_start) as usize).saturating_mul(ch);
                        samples.extend_from_slice(&decoded[lo..hi]);
                    }
                }
                cursor_frame = chunk_end;
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

    Ok(AudioBuffer {
        samples,
        rate,
        channels,
    })
}

pub(crate) fn slice_audio_buffer(buffer: &AudioBuffer, trim: Option<(f64, f64)>) -> AudioBuffer {
    let Some((start, end)) = trim else {
        return buffer.clone();
    };

    let start = start.max(0.0);
    let end = end.max(0.0);
    if end <= start {
        return AudioBuffer::empty(buffer.rate, buffer.channels);
    }

    let channels = buffer.channels.max(1) as usize;
    let total_frames = buffer.samples.len() / channels;
    let start_frame = seconds_to_frame(start, buffer.rate).min(total_frames as u64) as usize;
    let end_frame = seconds_to_frame(end, buffer.rate).min(total_frames as u64) as usize;
    if end_frame <= start_frame {
        return AudioBuffer::empty(buffer.rate, buffer.channels);
    }

    let lo = start_frame * channels;
    let hi = end_frame * channels;
    AudioBuffer {
        samples: buffer.samples[lo..hi].to_vec(),
        rate: buffer.rate,
        channels: buffer.channels,
    }
}

fn audio_buffer_duration(buffer: &AudioBuffer) -> f64 {
    let channels = buffer.channels.max(1) as usize;
    let frames = buffer.samples.len() / channels;
    frames as f64 / buffer.rate.max(1) as f64
}

/// Duration of a source-clock slice without cloning the slice's PCM.
fn audio_buffer_duration_for_trim(buffer: &AudioBuffer, trim: Option<(f64, f64)>) -> f64 {
    let channels = buffer.channels.max(1) as usize;
    let total_frames = buffer.samples.len() / channels;
    let Some((start, end)) = trim else {
        return total_frames as f64 / buffer.rate.max(1) as f64;
    };

    let start = start.max(0.0);
    let end = end.max(0.0);
    if end <= start {
        return 0.0;
    }
    let start_frame = seconds_to_frame(start, buffer.rate).min(total_frames as u64);
    let end_frame = seconds_to_frame(end, buffer.rate).min(total_frames as u64);
    end_frame.saturating_sub(start_frame) as f64 / buffer.rate.max(1) as f64
}

fn seconds_to_frame(seconds: f64, rate: u32) -> u64 {
    (seconds.max(0.0) * rate as f64).round().max(0.0) as u64
}

fn time_to_frame(time: SymphoniaTime, rate: u32) -> u64 {
    let seconds = time.seconds as f64 + time.frac;
    (seconds.max(0.0) * rate as f64).round() as u64
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
fn time_scale(buf: AudioBuffer, speed: f64) -> AudioBuffer {
    if (speed - 1.0).abs() < f64::EPSILON || speed <= 0.0 || buf.samples.is_empty() {
        return buf;
    }
    // Playing at `speed` over the same source means the output covers
    // `len / speed` seconds, i.e. resample to `rate / speed` then relabel back
    // to `rate` so the timeline reads it at the target rate.
    let scaled_rate = ((buf.rate as f64) / speed).round().max(1.0) as u32;
    let samples = resample(&buf.samples, buf.channels, buf.rate, scaled_rate);
    AudioBuffer {
        samples,
        rate: buf.rate,
        channels: buf.channels,
    }
}

/// Conforms `buf` to the target `rate` / `channels` and applies `gain` and the
/// placement `speed`, returning an interleaved buffer ready to sum into a mix.
pub fn conform(buf: AudioBuffer, rate: u32, channels: u16, gain: f32, speed: f64) -> Vec<f32> {
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

/// A low-level interleaved f32 accumulator retained for callers that already
/// have conformed buffers. Timeline components no longer render through it;
/// [`ResolvedTimeline::render_audio`](crate::timeline_component::ResolvedTimeline::render_audio)
/// recursively evaluates caller-sized blocks instead.
#[derive(Debug, Clone)]
pub struct AudioMix {
    samples: Vec<f32>,
    rate: u32,
    channels: u16,
}

impl AudioMix {
    /// A silent mix of `duration` seconds at `rate` / `channels`.
    pub fn new(duration: f64, rate: u32, channels: u16) -> Self {
        let frames = (duration.max(0.0) * rate as f64).ceil() as usize;
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

    /// The mix duration in seconds.
    pub fn duration(&self) -> f64 {
        let ch = self.channels.max(1) as usize;
        let frames = self.samples.len() / ch;
        frames as f64 / self.rate.max(1) as f64
    }

    /// Sums an already-conformed (rate/channels/gain/speed-matched) interleaved
    /// buffer into the mix starting at `start_secs`. Values may exceed
    /// `[-1, 1]`; limiting/clipping belongs at the ffmpeg output stage. Frames
    /// before the mix start and past the mix end are dropped (the resolved
    /// length is authoritative).
    pub fn add(&mut self, conformed: &[f32], start_secs: f64) {
        let ch = self.channels.max(1) as usize;
        let start_frame = (start_secs * self.rate as f64).round() as isize;
        let (base, source_offset) = if start_frame >= 0 {
            (start_frame as usize * ch, 0)
        } else {
            let skipped_frames = (-start_frame) as usize;
            (0, skipped_frames.saturating_mul(ch))
        };
        for (i, &s) in conformed.iter().skip(source_offset).enumerate() {
            let idx = base + i;
            if idx >= self.samples.len() {
                break;
            }
            self.samples[idx] += s;
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

    static AUDIO_CACHE_TEST_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    fn temp_wav(samples: &[i16], rate: u32, channels: u16) -> std::path::PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after unix epoch")
            .as_nanos();
        let mut path = std::env::temp_dir();
        path.push(format!(
            "tellur-audio-cache-{}-{nanos}.wav",
            std::process::id()
        ));

        let bits = 16u16;
        let data_bytes = (samples.len() * 2) as u32;
        let byte_rate = rate * channels as u32 * (bits as u32 / 8);
        let block_align = channels * (bits / 8);
        let mut bytes = Vec::with_capacity(44 + samples.len() * 2);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_bytes).to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&channels.to_le_bytes());
        bytes.extend_from_slice(&rate.to_le_bytes());
        bytes.extend_from_slice(&byte_rate.to_le_bytes());
        bytes.extend_from_slice(&block_align.to_le_bytes());
        bytes.extend_from_slice(&bits.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_bytes.to_le_bytes());
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        std::fs::write(&path, bytes).expect("write wav fixture");
        path
    }

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
    fn mix_add_preserves_float_headroom() {
        let mut mix = AudioMix::new(1.0, 4, 1);
        // Two unit tracks at frame 0 sum to 2.0; output limiting is a later
        // encoder concern, not a mix-stage concern.
        mix.add(&[1.0, 1.0], 0.0);
        mix.add(&[1.0, 1.0], 0.0);
        let buf = mix.into_buffer();
        assert_eq!(buf.samples[0], 2.0);
        assert_eq!(buf.samples[1], 2.0);
    }

    #[test]
    fn mix_add_clips_negative_start() {
        let mut mix = AudioMix::new(1.0, 4, 1);

        mix.add(&[0.1, 0.2, 0.3, 0.4], -0.5);

        let buf = mix.into_buffer();
        assert_eq!(buf.samples, vec![0.3, 0.4, 0.0, 0.0]);
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

    #[test]
    fn cached_buffer_duration_counts_trimmed_frames_without_slicing() {
        let buffer = AudioBuffer {
            samples: vec![0.0; 8],
            rate: 4,
            channels: 2,
        };

        assert_eq!(audio_buffer_duration_for_trim(&buffer, None), 1.0);
        assert_eq!(
            audio_buffer_duration_for_trim(&buffer, Some((0.25, 0.75))),
            0.5
        );
        assert_eq!(
            audio_buffer_duration_for_trim(&buffer, Some((-1.0, 2.0))),
            1.0
        );
        assert_eq!(
            audio_buffer_duration_for_trim(&buffer, Some((0.75, 0.25))),
            0.0
        );
    }

    #[test]
    fn trimmed_decode_populates_and_reuses_full_cache_for_small_sources() {
        let _guard = AUDIO_CACHE_TEST_LOCK
            .lock()
            .expect("audio cache test lock should not be poisoned");
        clear_decode_cache_for_tests();
        let path = temp_wav(&[0, 1000, 2000, 3000, 4000, 5000], 6, 1);
        let path_str = path.to_string_lossy();

        let window = decode_file(&path_str, Some((0.0, 0.5))).expect("decode trimmed wav");

        assert_eq!(decode_cache_len_for_tests(), 1);
        assert_eq!(window.rate, 6);
        assert_eq!(window.channels, 1);
        assert_eq!(window.samples.len(), 3);

        let duration = decoded_duration(&path_str, None).expect("duration from cached wav");
        assert!((duration - 1.0).abs() < 1e-6);

        let _ = std::fs::remove_file(path);
        clear_decode_cache_for_tests();
    }

    #[test]
    fn conformed_audio_is_cached_for_repeated_windows() {
        let _guard = AUDIO_CACHE_TEST_LOCK
            .lock()
            .expect("audio cache test lock should not be poisoned");
        clear_decode_cache_for_tests();
        let path = temp_wav(&[0, 1000, 2000, 3000, 4000, 5000], 6, 1);
        let path_str = path.to_string_lossy();

        let first =
            conform_file_cached(&path_str, None, 12, 2, 0.5, 1.0).expect("conform first time");
        let second =
            conform_file_cached(&path_str, None, 12, 2, 0.5, 1.0).expect("conform second time");

        assert_eq!(conform_cache_len_for_tests(), 1);
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first.rate, 12);
        assert_eq!(first.channels, 2);

        let _ = std::fs::remove_file(path);
        clear_decode_cache_for_tests();
    }
}
