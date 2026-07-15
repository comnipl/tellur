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

/// The largest forward gap (in FRAMES) that is served by ADVANCING a running
/// child rather than a cold `-ss` SEEK. Within this window the next decoded
/// frames are cheap to walk to; beyond it a seek + GOP realign wins.
const MAX_ADVANCE_FRAMES: u64 = 16;

/// How many independent decode CURSORS (running `ffmpeg` children, each at its
/// own position) a single [`VideoDecoder`] keeps. The decoder is shared per
/// `(path, target)`, but concurrent consumers play DIFFERENT positions (the live
/// preview's two video slots + a preload stream), so a single cursor would be
/// dragged back and forth — every request landing as a cold `-ss` seek (measured
/// ~5–20× slowdown). One cursor per concurrent consumer keeps each one on the
/// cheap forward-advance path; an LRU evicts the stalest when the cap is hit (an
/// evicted consumer just re-seeks once on its next request).
const MAX_CURSORS: usize = 4;

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
        RasterImage::cpu(
            self.width,
            self.height,
            PixelFormat::Rgba8,
            self.pixels.clone(),
        )
    }
}

// ── duration probe ────────────────────────────────────────────────────────────

/// `path → probed duration (seconds)`, filled once per path by [`probe_duration`].
/// A shared map so the resolve pass never re-probes the same file (`.sketch/02
/// §12`); the duration is pure data, so unlike the decoder state it COULD live on
/// the leaf, but keeping it here keeps [`VideoFile`] trivially `Keyable`.
fn duration_cache() -> &'static Mutex<HashMap<String, f64>> {
    static CACHE: OnceLock<Mutex<HashMap<String, f64>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Reads the duration of `path` (seconds) via `ffprobe`, caching the result.
/// Returns `None` if `ffprobe` is unavailable, the file is missing, or the
/// duration cannot be parsed — the caller falls back to a stub so resolve still
/// has a determinate length (mirrors `AudioFile`'s graceful decode fallback).
pub fn probe_duration(path: &str) -> Option<f64> {
    if let Some(d) = duration_cache().lock().unwrap().get(path).copied() {
        return Some(d);
    }
    let d = run_ffprobe_duration(path)?;
    duration_cache().lock().unwrap().insert(path.to_string(), d);
    Some(d)
}

/// Runs `ffprobe -show_entries format=duration` and parses the seconds value.
fn run_ffprobe_duration(path: &str) -> Option<f64> {
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
    let secs: f64 = text.trim().parse().ok()?;
    if secs.is_finite() && secs >= 0.0 {
        Some(secs)
    } else {
        None
    }
}

// ── the source-time remap (pure, unit-tested without ffmpeg) ───────────────────

/// Maps the leaf's `local` clock seconds to a SOURCE time (seconds into the
/// file). Temporal wrappers normally shift, stretch, and trim the local clock
/// before `VideoFile` calls this helper with `None`. The optional range remains
/// a low-level compatibility seam: when supplied, its start is added as
/// `source = trim_start + local`, clamped at 0.
pub fn source_time(local_secs: f64, trim: Option<(f64, f64)>) -> f64 {
    let trim_start = trim.map(|(a, _)| a).unwrap_or(0.0);
    (trim_start + local_secs).max(0.0)
}

/// Converts a source time (seconds) to a frame index on the fixed decode grid.
fn frame_index_for(source_secs: f64) -> u64 {
    (source_secs * DECODE_FPS).round().max(0.0) as u64
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

/// A set of running `ffmpeg` children decoding `path` to raw `rgba` frames
/// scaled to `target`, plus a small LRU frame cache shared across them.
///
/// Several CURSORS may run at once (see [`MAX_CURSORS`]): one per concurrent
/// consumer playing a different position, so each stays on the cheap
/// forward-advance path instead of fighting over a single child's position.
///
/// `Send` (an [`std::process::Child`] + plain owned buffers are `Send`); held in
/// the process-global pool behind a `Mutex`.
struct VideoDecoder {
    path: String,
    target: Resolution,
    /// Independent decode positions, each a running child. Capped at
    /// [`MAX_CURSORS`]; the stalest is evicted (LRU by `last_used`) when a new
    /// seek needs a slot.
    cursors: Vec<Cursor>,
    /// LRU of recently decoded frames, keyed by absolute frame index — shared by
    /// all cursors (one cursor's decoded frames serve another's cache hits).
    cache: LruCache<u64, Frame>,
    /// Monotonic counter stamped onto a cursor's `last_used` on every access, so
    /// the LRU eviction picks the cursor untouched longest. Avoids wall-clock.
    tick: u64,
}

/// One decode position: a running child plus the frame index it will emit NEXT.
struct Cursor {
    running: Running,
    /// Frame index the child will emit on the next read (the read head on the
    /// fixed decode grid).
    next_index: u64,
    /// `VideoDecoder::tick` at this cursor's last use, for LRU eviction.
    last_used: u64,
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
            cursors: Vec::new(),
            cache: LruCache::new(
                NonZeroUsize::new(FRAME_CACHE_CAPACITY).expect("cache capacity is non-zero"),
            ),
            tick: 0,
        }
    }

    /// Returns the frame at `source_secs`, decoding as needed. `None` if no child
    /// can be spawned or none yields a frame (e.g. a bad path / past EOF).
    ///
    /// Serve order: a cache hit; else the cursor that can reach `target_index` by
    /// the SMALLEST forward advance (so the request rides an existing child); else
    /// a cold seek on a fresh cursor. Keying the decision per cursor — not against
    /// one shared position — is what stops concurrent consumers from thrashing.
    fn frame_at(&mut self, source_secs: f64) -> Option<RasterImage> {
        let target_index = frame_index_for(source_secs);
        self.tick = self.tick.wrapping_add(1);

        if let Some(frame) = self.cache.get(&target_index) {
            return Some(frame.to_image());
        }

        // Pick the cursor needing the smallest advance to reach the target; a
        // cursor that is ahead or too far behind is not a candidate.
        let best = self
            .cursors
            .iter()
            .enumerate()
            .filter_map(
                |(i, c)| match decide(target_index, Some(c.next_index), false) {
                    Action::Advance(n) => Some((i, n)),
                    _ => None,
                },
            )
            .min_by_key(|&(_, n)| n);

        match best {
            Some((i, n)) => self.advance(i, target_index, n),
            None => self.seek(target_index, source_secs),
        }
    }

    /// Advances cursor `i` forward `n` frames, returning the frame at
    /// `target_index`. Each decoded frame is cached so a later request for an
    /// intermediate frame is a cache hit.
    fn advance(&mut self, i: usize, target_index: u64, n: u64) -> Option<RasterImage> {
        // Walk `n + 1` frames: the `n` to skip plus the one we want. The cursor's
        // `next_index` is `target_index - n`.
        let frame_bytes = self.frame_bytes();
        let target = self.target;
        let mut last: Option<Frame> = None;
        for _ in 0..=n {
            let idx = self.cursors[i].next_index;
            let frame = match read_frame(&mut self.cursors[i].running.stdout, frame_bytes, target) {
                Some(f) => f,
                None => {
                    // EOF / read error mid-advance: this child is spent — drop the
                    // cursor and serve the closest frame we have for this target.
                    self.cursors.remove(i);
                    return self.cache.get(&target_index).map(Frame::to_image);
                }
            };
            self.cursors[i].next_index = idx + 1;
            self.cache.put(idx, frame.clone());
            if idx == target_index {
                last = Some(frame);
            }
        }
        self.cursors[i].last_used = self.tick;
        last.or_else(|| self.cache.get(&target_index).cloned())
            .map(|f| f.to_image())
    }

    /// Cold `-ss` seek: spawn a fresh cursor positioned at `source_secs` and read
    /// its first emitted frame as `target_index`. Evicts the stalest cursor first
    /// when at [`MAX_CURSORS`]. The frame cache survives (it is keyed by absolute
    /// frame index, not child position).
    fn seek(&mut self, target_index: u64, source_secs: f64) -> Option<RasterImage> {
        if self.cursors.len() >= MAX_CURSORS {
            if let Some((stalest, _)) = self
                .cursors
                .iter()
                .enumerate()
                .min_by_key(|(_, c)| c.last_used)
            {
                // Drop kills + reaps the evicted child.
                self.cursors.remove(stalest);
            }
        }

        let mut running = spawn_decoder(&self.path, self.target, source_secs)?;
        let frame_bytes = self.frame_bytes();
        let frame = read_frame(&mut running.stdout, frame_bytes, self.target)?;
        self.cache.put(target_index, frame.clone());
        self.cursors.push(Cursor {
            running,
            next_index: target_index + 1,
            last_used: self.tick,
        });
        Some(frame.to_image())
    }

    /// Bytes in one decoded frame at the target resolution (RGBA, 4 bytes/px).
    fn frame_bytes(&self) -> usize {
        (self.target.width as usize) * (self.target.height as usize) * 4
    }
}

/// Reads exactly one frame's worth of bytes off a cursor's stdout. `None` on EOF
/// (clean end of stream) or read error.
fn read_frame(stdout: &mut ChildStdout, frame_bytes: usize, target: Resolution) -> Option<Frame> {
    let mut buf = vec![0u8; frame_bytes];
    match read_exact_or_eof(stdout, &mut buf) {
        Ok(true) => Some(Frame {
            width: target.width,
            height: target.height,
            pixels: buf,
        }),
        // Clean EOF or a short final read: out of frames.
        Ok(false) | Err(_) => None,
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
fn spawn_decoder(path: &str, target: Resolution, start_secs: f64) -> Option<Running> {
    let ss = start_secs.max(0.0).to_string();
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

/// Decodes one frame of `path` at the leaf's already-remapped local clock time,
/// scaled to `target`. An optional low-level source offset is still accepted for
/// compatibility and tests. This is the entry the
/// [`VideoFile`](crate::timeline_container::VideoFile) leaf calls from its
/// `frame`. Resolves (or lazily spawns) the process-global decoder for
/// `(path, target)` and pulls the frame at the remapped source time.
///
/// `None` ⇒ no frame (bad path, ffmpeg missing, or past the source end); the
/// leaf then contributes nothing visually for this frame.
pub fn decode_frame(
    path: &str,
    local_secs: f64,
    trim: Option<(f64, f64)>,
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

    // ── access-order independence (the "render can't break" guarantee) ───────
    //
    // Behind `#[ignore]` (needs ffmpeg): the leaf must return the SAME frame for
    // a given source time no matter HOW the decoder reached it (cold `-ss` seek
    // vs. forward advance). If this holds, decode speed only affects latency, not
    // which frame a time maps to — so a slow/thrashing decoder degrades to "late"
    // but never "wrong/desynced". This test reaches each time two ways and asserts
    // byte-identical frames.

    /// Writes a `testsrc` mp4 (moving pattern + frame counter, distinct per frame)
    /// to `path`. `secs` long at 30 fps, `w`x`h`.
    #[cfg(test)]
    fn write_testsrc_mp4(path: &std::path::Path, secs: u32, w: u32, h: u32) {
        let lavfi = format!("testsrc=size={w}x{h}:rate=30:duration={secs}");
        let status = Command::new("ffmpeg")
            .args(["-y", "-v", "error"])
            .args(["-f", "lavfi", "-i", &lavfi])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(path)
            .status()
            .expect("spawn ffmpeg testsrc fixture");
        assert!(status.success(), "ffmpeg testsrc fixture write failed");
    }

    fn frame_bytes_at(path: &str, t: f64, target: Resolution) -> Vec<u8> {
        let img = decode_frame(path, t, None, target)
            .unwrap_or_else(|| panic!("decoded a frame at t={t}"));
        img.as_cpu().expect("cpu frame").pixels.to_vec()
    }

    #[test]
    #[ignore = "requires ffmpeg on PATH"]
    fn frame_for_time_is_independent_of_access_order() {
        // Two byte-identical copies so each gets its OWN pooled decoder (the pool
        // is keyed by path): copy A is walked monotonically (all `advance`), copy
        // B in a zig-zag that forces a cold `-ss` seek on nearly every request.
        let dir = std::env::temp_dir();
        let pid = std::process::id();
        let a = dir.join(format!("tellur_order_a_{pid}.mp4"));
        let b = dir.join(format!("tellur_order_b_{pid}.mp4"));
        write_testsrc_mp4(&a, 2, 160, 120);
        std::fs::copy(&a, &b).expect("copy fixture");
        let (a_str, b_str) = (a.to_str().unwrap(), b.to_str().unwrap());
        let target = Resolution::new(64, 48);

        // One source second on the 30 fps decode grid.
        let times: Vec<f64> = (0..30).map(|i| i as f64 / DECODE_FPS).collect();

        // Monotonic baseline on copy A: every request is a forward advance.
        let monotonic: Vec<Vec<u8>> = times
            .iter()
            .map(|&t| frame_bytes_at(a_str, t, target))
            .collect();

        // Thrash order on copy B: 0, 29, 1, 28, 2, 27, … — alternating far-forward
        // and backward jumps, each beyond MAX_ADVANCE_FRAMES, so `decide` picks
        // `Seek` almost every time (the worst case the live preview hits).
        let mut order: Vec<usize> = Vec::with_capacity(times.len());
        let (mut lo, mut hi) = (0usize, times.len() - 1);
        while lo <= hi {
            order.push(lo);
            if hi != lo {
                order.push(hi);
            }
            lo += 1;
            if hi == 0 {
                break;
            }
            hi -= 1;
        }
        let mut thrash = vec![Vec::new(); times.len()];
        for &i in &order {
            thrash[i] = frame_bytes_at(b_str, times[i], target);
        }

        for (i, &t) in times.iter().enumerate() {
            assert_eq!(
                monotonic[i], thrash[i],
                "frame at t={t} differs between forward-advance and seek access — \
                 the time→frame mapping is NOT order-independent"
            );
        }

        let _ = std::fs::remove_file(&a);
        let _ = std::fs::remove_file(&b);
    }

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
        assert_eq!(
            decide(5 + MAX_ADVANCE_FRAMES + 1, Some(5), false),
            Action::Seek
        );
    }
}
