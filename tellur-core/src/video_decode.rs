//! Video decode for the timeline VIDEO channel — STEP 9.
//!
//! The visual twin of [`audio`](crate::audio): decode lives in `tellur-core`,
//! self-contained behind the [`VideoFile`](crate::timeline_container::VideoFile)
//! leaf, and spawns its own `ffmpeg` child (it needs nothing from a
//! `RenderContext`). Decode is decided as a per-source `ffmpeg` CHILD process
//! (`.sketch/01` ZONE C): codec coverage, consistent with the existing encoder.
//!
//! Three pieces live here:
//!
//! 1. [`probe_duration`] — a `ffprobe` header read of a file's duration, cached
//!    process-globally so the resolve pass probes each path ONCE (`.sketch/02
//!    §12`, the OnceCell-cache role, here a shared map keyed by path).
//!
//! 2. [`VideoDecoder`] — a per-`(path, target)` decoder: a running `ffmpeg`
//!    child emitting raw `rgba` frames SCALED to the target resolution, a small
//!    LRU frame cache, and a current decode position. It serves two access
//!    modes behind ONE [`frame_at`](VideoDecoder::frame_at) rule:
//!      - EXPORT (monotonic forward): advance the running child frame-by-frame
//!        to the requested frame (decode-ahead-ish; near-non-blocking for the
//!        common "next frame" request).
//!      - LIVE scrub (random / far jump): `-ss <t>` seek by restarting the child
//!        at the requested time (cold seek + GOP realign is acceptable).
//!
//!    The unified rule: serve from cache, else ADVANCE if the request is just
//!    ahead, else SEEK.
//!
//! 3. [`decode_frame`] — the entry the leaf calls: resolves the process-global
//!    decoder for `(path, target)` and pulls one frame at a source time.
//!
//! OWNERSHIP: the `ffmpeg` child + frame cache + decode position are mutable,
//! non-`Clone`, non-`Hash` state, so they CANNOT be fields of the `Clone +
//! Keyable` [`VideoFile`]. They live OUTSIDE the struct in a process-global pool
//! ([`DECODERS`]) keyed by `(path, target)`; the leaf stays pure data and the
//! decoder state never enters any cache key. Everything here is `Send`.

use std::collections::HashMap;
use std::io::{self, Read};
use std::num::NonZeroUsize;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::{Mutex, OnceLock};

use lru::LruCache;

use crate::raster::{PixelFormat, RasterImage, Resolution};

/// How many decoded frames a [`VideoDecoder`] keeps in its LRU frame cache. A
/// small window is enough: forward export reuses the most-recent frames and a
/// scrub re-seeks anyway (`.sketch/01` A.2 "a small frame cache").
const FRAME_CACHE_CAPACITY: usize = 8;

/// How many running decoders the process-global pool keeps. One per
/// `(path, target)`; an LRU so a timeline with many sources does not hold an
/// unbounded number of `ffmpeg` children open.
const DECODER_POOL_CAPACITY: usize = 8;

/// The largest forward gap (in FRAMES) that is served by ADVANCING the running
/// child rather than a cold `-ss` SEEK. Within this window the next decoded
/// frames are cheap to walk to; beyond it a seek + GOP realign wins.
const MAX_ADVANCE_FRAMES: u64 = 16;

/// The frame rate the decoder samples the source at. The source is decoded to a
/// fixed grid so a request time maps deterministically to a frame index; this
/// matches the export/live frame cadence closely enough for v1 (the encoder and
/// the live server both sample at their own fps, and the nearest decoded frame
/// is returned).
const DECODE_FPS: f64 = 30.0;

/// A decoded RGBA frame at a known resolution. Interleaved 8-bit RGBA, row-major,
/// no padding (ffmpeg `rawvideo` `rgba`).
#[derive(Clone)]
struct Frame {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Frame {
    /// Wraps the decoded bytes as a CPU [`RasterImage`] for the leaf to return.
    fn to_image(&self) -> RasterImage {
        RasterImage::cpu(self.width, self.height, PixelFormat::Rgba8, self.pixels.clone())
    }
}

// ── duration probe ────────────────────────────────────────────────────────────

/// `path → probed duration (seconds)`, filled once per path by [`probe_duration`].
/// A shared map so the resolve pass never re-probes the same file (`.sketch/02
/// §12`); the duration is pure data, so unlike the decoder state it COULD live on
/// the leaf, but keeping it here keeps [`VideoFile`] trivially `Keyable`.
fn duration_cache() -> &'static Mutex<HashMap<String, f32>> {
    static CACHE: OnceLock<Mutex<HashMap<String, f32>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Reads the duration of `path` (seconds) via `ffprobe`, caching the result.
/// Returns `None` if `ffprobe` is unavailable, the file is missing, or the
/// duration cannot be parsed — the caller falls back to a stub so resolve still
/// has a determinate length (mirrors `AudioFile`'s graceful decode fallback).
pub fn probe_duration(path: &str) -> Option<f32> {
    if let Some(d) = duration_cache().lock().unwrap().get(path).copied() {
        return Some(d);
    }
    let d = run_ffprobe_duration(path)?;
    duration_cache()
        .lock()
        .unwrap()
        .insert(path.to_string(), d);
    Some(d)
}

/// Runs `ffprobe -show_entries format=duration` and parses the seconds value.
fn run_ffprobe_duration(path: &str) -> Option<f32> {
    let output = Command::new("ffprobe")
        .args(["-v", "error"])
        .args(["-show_entries", "format=duration"])
        .args(["-of", "default=noprint_wrappers=1:nokey=1"])
        .arg(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let secs: f32 = text.trim().parse().ok()?;
    if secs.is_finite() && secs >= 0.0 {
        Some(secs)
    } else {
        None
    }
}

// ── the source-time remap (pure, unit-tested without ffmpeg) ───────────────────

/// Maps the leaf's `local` clock seconds to a SOURCE time (seconds into the
/// file). The `Placed` wrapper has already shifted local to the clip start and
/// folded the placement `speed` into it before the leaf sees it (`Placed::frame`
/// in `timeline_component.rs`), so the leaf only adds its own `.trim` start:
/// `source = trim_start + local`, clamped at 0. With no trim, `trim_start = 0`.
pub fn source_time(local_secs: f32, trim: Option<(f32, f32)>) -> f32 {
    let trim_start = trim.map(|(a, _)| a).unwrap_or(0.0);
    (trim_start + local_secs).max(0.0)
}

/// Converts a source time (seconds) to a frame index on the fixed decode grid.
fn frame_index_for(source_secs: f32) -> u64 {
    (source_secs as f64 * DECODE_FPS).round().max(0.0) as u64
}

// ── the decode-position rule (pure, unit-tested with a mock seam) ──────────────

/// What a [`VideoDecoder`] should do to serve a request for `target` frame given
/// where it currently is. The decision is isolated from the `ffmpeg` plumbing so
/// it can be unit-tested with a fake decoder (no subprocess).
#[derive(Debug, PartialEq, Eq)]
enum Action {
    /// The frame is already cached — serve it directly.
    Cache,
    /// The request is just ahead of the current position — advance the running
    /// child forward `n` frames (monotonic export path).
    Advance(u64),
    /// A non-forward or far jump — cold `-ss` seek to the target (live scrub).
    Seek,
}

/// Decides how to reach `target` from `current` (the next frame index the
/// running child would emit; `None` if no child is running yet) given whether
/// the frame is already cached.
///
/// Rule (`.sketch/01` A.2, the "serve from cache / advance / seek" unified rule):
/// - cached ⇒ [`Action::Cache`];
/// - no child yet, or `target < current` (a backward jump), or
///   `target - current > MAX_ADVANCE_FRAMES` (a far forward jump) ⇒
///   [`Action::Seek`];
/// - otherwise a small forward gap ⇒ [`Action::Advance`].
fn decide(target: u64, current: Option<u64>, cached: bool) -> Action {
    if cached {
        return Action::Cache;
    }
    match current {
        Some(cur) if target >= cur && target - cur <= MAX_ADVANCE_FRAMES => {
            Action::Advance(target - cur)
        }
        _ => Action::Seek,
    }
}

// ── the per-source decoder ─────────────────────────────────────────────────────

/// A running `ffmpeg` child decoding `path` to raw `rgba` frames scaled to
/// `target`, plus a small LRU frame cache and the current decode position.
///
/// `Send` (an [`std::process::Child`] + plain owned buffers are `Send`); held in
/// the process-global pool behind a `Mutex`.
struct VideoDecoder {
    path: String,
    target: Resolution,
    /// The running child + its stdout reader, if a decode is in flight. `None`
    /// before the first request and after EOF.
    running: Option<Running>,
    /// Frame index the running child will emit NEXT (the read cursor on the
    /// fixed decode grid). `None` when no child is running.
    next_index: Option<u64>,
    /// LRU of recently decoded frames, keyed by frame index.
    cache: LruCache<u64, Frame>,
}

/// The live child handle: the process plus its piped stdout. Split out so it can
/// be dropped (killing the child) on a seek/restart without disturbing the cache.
struct Running {
    child: Child,
    stdout: ChildStdout,
}

impl Drop for Running {
    fn drop(&mut self) {
        // Best-effort: stop the child and reap it so a restart/seek does not
        // leak an ffmpeg process. Errors are ignored (the child may have exited).
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl VideoDecoder {
    fn new(path: String, target: Resolution) -> Self {
        Self {
            path,
            target,
            running: None,
            next_index: None,
            cache: LruCache::new(
                NonZeroUsize::new(FRAME_CACHE_CAPACITY).expect("cache capacity is non-zero"),
            ),
        }
    }

    /// Returns the frame at `source_secs`, decoding as needed. `None` if the
    /// child cannot be spawned or yields no frame (e.g. a bad path / past EOF).
    fn frame_at(&mut self, source_secs: f32) -> Option<RasterImage> {
        let target_index = frame_index_for(source_secs);
        let cached = self.cache.contains(&target_index);
        match decide(target_index, self.next_index, cached) {
            Action::Cache => self.cache.get(&target_index).map(Frame::to_image),
            Action::Advance(n) => self.advance(target_index, n),
            Action::Seek => self.seek(target_index, source_secs),
        }
    }

    /// Advances the running child forward `n` frames, returning the frame at
    /// `target_index`. Each decoded frame is cached so a later request for an
    /// intermediate frame is a cache hit.
    fn advance(&mut self, target_index: u64, n: u64) -> Option<RasterImage> {
        // Walk `n + 1` frames: the `n` to skip plus the one we want. The running
        // child's `next_index` is `target_index - n`.
        let frame_bytes = self.frame_bytes();
        let mut last: Option<Frame> = None;
        for _ in 0..=n {
            let idx = self.next_index?;
            let frame = match self.read_one(frame_bytes) {
                Some(f) => f,
                None => {
                    // EOF / read error mid-advance: serve the closest frame we
                    // have for this target, else give up (the child is spent).
                    self.running = None;
                    self.next_index = None;
                    return self.cache.get(&target_index).map(Frame::to_image);
                }
            };
            self.next_index = Some(idx + 1);
            self.cache.put(idx, frame.clone());
            if idx == target_index {
                last = Some(frame);
            }
        }
        last.or_else(|| self.cache.get(&target_index).cloned())
            .map(|f| f.to_image())
    }

    /// Cold `-ss` seek: (re)spawn the child positioned at `source_secs`, then
    /// read the first emitted frame as `target_index`. The frame cache survives
    /// the restart (it is keyed by absolute frame index, not child position).
    fn seek(&mut self, target_index: u64, source_secs: f32) -> Option<RasterImage> {
        // Drop any running child first (its Drop kills + reaps it).
        self.running = None;
        self.next_index = None;

        let running = spawn_decoder(&self.path, self.target, source_secs)?;
        self.running = Some(running);
        self.next_index = Some(target_index);

        let frame_bytes = self.frame_bytes();
        let frame = self.read_one(frame_bytes)?;
        self.next_index = Some(target_index + 1);
        self.cache.put(target_index, frame.clone());
        Some(frame.to_image())
    }

    /// Reads exactly one frame's worth of bytes off the running child's stdout.
    /// `None` on EOF (clean end of stream) or read error.
    fn read_one(&mut self, frame_bytes: usize) -> Option<Frame> {
        let running = self.running.as_mut()?;
        let mut buf = vec![0u8; frame_bytes];
        match read_exact_or_eof(&mut running.stdout, &mut buf) {
            Ok(true) => Some(Frame {
                width: self.target.width,
                height: self.target.height,
                pixels: buf,
            }),
            // Clean EOF or a short final read: out of frames.
            Ok(false) | Err(_) => None,
        }
    }

    /// Bytes in one decoded frame at the target resolution (RGBA, 4 bytes/px).
    fn frame_bytes(&self) -> usize {
        (self.target.width as usize) * (self.target.height as usize) * 4
    }
}

/// Reads exactly `buf.len()` bytes, returning `Ok(true)` on a full read,
/// `Ok(false)` on a clean EOF before any byte (out of frames), and an error on a
/// partial/torn read. A torn frame at EOF is treated as `false` by the caller.
fn read_exact_or_eof(reader: &mut impl Read, buf: &mut [u8]) -> io::Result<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => {
                // EOF: a full frame only if we happened to land exactly on the
                // boundary (filled == 0 here means a clean end of stream).
                return Ok(filled == buf.len());
            }
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

/// Spawns an `ffmpeg` child that decodes `path` to raw `rgba` frames scaled to
/// `target` on a fixed [`DECODE_FPS`] grid, seeked to `start_secs`. Frames stream
/// on the child's stdout as contiguous RGBA byte runs.
///
/// `-ss` BEFORE `-i` is the fast (keyframe) input seek; ffmpeg then realigns to
/// the requested time. The `fps` + `scale` filters fix the output cadence and
/// size so a frame index maps deterministically to bytes.
fn spawn_decoder(path: &str, target: Resolution, start_secs: f32) -> Option<Running> {
    let ss = format!("{:.6}", start_secs.max(0.0));
    let vf = format!(
        "fps={},scale={}:{}:flags=bilinear",
        DECODE_FPS, target.width, target.height
    );
    let mut child = Command::new("ffmpeg")
        .args(["-v", "error"])
        .args(["-ss", &ss])
        .args(["-i", path])
        .args(["-vf", &vf])
        .args(["-pix_fmt", "rgba"])
        .args(["-f", "rawvideo"])
        .arg("-")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    Some(Running { child, stdout })
}

// ── the process-global decoder pool ────────────────────────────────────────────

/// Key into the decoder pool: the source path and the target resolution. A
/// different target spawns a different (separately scaled) decoder. Decoder
/// STATE is never part of this key (the constraint: decoder state must not enter
/// the cache key).
#[derive(Clone, PartialEq, Eq, Hash)]
struct DecoderKey {
    path: String,
    width: u32,
    height: u32,
}

/// Process-global LRU of running decoders keyed by `(path, target)`. Holds the
/// mutable, non-`Clone` decoder state OUTSIDE the pure-data [`VideoFile`] leaf,
/// behind a `Mutex` so it stays `Send` and shareable across frames/threads.
fn decoder_pool() -> &'static Mutex<LruCache<DecoderKey, VideoDecoder>> {
    static POOL: OnceLock<Mutex<LruCache<DecoderKey, VideoDecoder>>> = OnceLock::new();
    POOL.get_or_init(|| {
        Mutex::new(LruCache::new(
            NonZeroUsize::new(DECODER_POOL_CAPACITY).expect("pool capacity is non-zero"),
        ))
    })
}

/// Decodes one frame of `path` at the leaf's `local` clock time, scaled to
/// `target`, honoring the `.trim` start. The entry the
/// [`VideoFile`](crate::timeline_container::VideoFile) leaf calls from its
/// `frame`. Resolves (or lazily spawns) the process-global decoder for
/// `(path, target)` and pulls the frame at the remapped source time.
///
/// `None` ⇒ no frame (bad path, ffmpeg missing, or past the source end); the
/// leaf then contributes nothing visually for this frame.
pub fn decode_frame(
    path: &str,
    local_secs: f32,
    trim: Option<(f32, f32)>,
    target: Resolution,
) -> Option<RasterImage> {
    let source = source_time(local_secs, trim);
    let key = DecoderKey {
        path: path.to_string(),
        width: target.width,
        height: target.height,
    };
    let mut pool = decoder_pool().lock().unwrap();
    let decoder = pool.get_or_insert_mut(key, || VideoDecoder::new(path.to_string(), target));
    decoder.frame_at(source)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── source-time remap (no ffmpeg) ────────────────────────────────────────

    #[test]
    fn source_time_no_trim_is_identity() {
        // No trim: the leaf's local seconds ARE the source seconds.
        assert_eq!(source_time(0.0, None), 0.0);
        assert_eq!(source_time(1.5, None), 1.5);
    }

    #[test]
    fn source_time_adds_trim_start() {
        // `.trim(2.0..5.0)`: source-local 0 maps to source 2.0, etc. (the
        // placement speed is already folded into `local` by `Placed` upstream).
        let trim = Some((2.0, 5.0));
        assert_eq!(source_time(0.0, trim), 2.0);
        assert_eq!(source_time(1.0, trim), 3.0);
    }

    #[test]
    fn source_time_clamps_at_zero() {
        // A negative local (before this clip's start) clamps to the source head.
        assert_eq!(source_time(-1.0, None), 0.0);
        assert_eq!(source_time(-1.0, Some((0.5, 1.0))), 0.0);
    }

    #[test]
    fn frame_index_quantizes_to_grid() {
        // At 30 fps, 0.0s ⇒ frame 0, 1.0s ⇒ frame 30, 0.5s ⇒ frame 15.
        assert_eq!(frame_index_for(0.0), 0);
        assert_eq!(frame_index_for(1.0), 30);
        assert_eq!(frame_index_for(0.5), 15);
    }

    // ── the cache / advance / seek decision (mock seam, no ffmpeg) ────────────

    #[test]
    fn cached_frame_serves_from_cache() {
        // A cached frame is served directly regardless of position.
        assert_eq!(decide(10, Some(0), true), Action::Cache);
        assert_eq!(decide(10, None, true), Action::Cache);
    }

    #[test]
    fn no_child_yet_seeks() {
        // First request (no running child) is always a cold seek.
        assert_eq!(decide(0, None, false), Action::Seek);
        assert_eq!(decide(100, None, false), Action::Seek);
    }

    #[test]
    fn small_forward_gap_advances() {
        // Export's monotonic "next frame" advances the running child.
        assert_eq!(decide(5, Some(5), false), Action::Advance(0));
        assert_eq!(decide(6, Some(5), false), Action::Advance(1));
        assert_eq!(
            decide(5 + MAX_ADVANCE_FRAMES, Some(5), false),
            Action::Advance(MAX_ADVANCE_FRAMES)
        );
    }

    #[test]
    fn backward_jump_seeks() {
        // A scrub backwards cannot advance a forward-only child — it seeks.
        assert_eq!(decide(3, Some(10), false), Action::Seek);
    }

    #[test]
    fn far_forward_jump_seeks() {
        // A jump past the advance window is cheaper to reach by an `-ss` seek.
        assert_eq!(decide(5 + MAX_ADVANCE_FRAMES + 1, Some(5), false), Action::Seek);
    }
}
