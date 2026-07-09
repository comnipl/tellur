import type { CacheRange } from "../types";
import type { MediaCache } from "../mediaCache";

// All time comparisons are EPSILON-tolerant: with per-segment timestampOffset the
// buffered timeline is exact only to within a frame, so equality/containment must
// never be bit-exact (a sub-EPSILON gap hangs a seek on Arc/older Chromium).
const EPSILON = 0.001;

// Lookahead targets (seconds). Priming keeps ~3s cached ahead of a paused playhead
// (requirement 1); playing keeps a larger window so the stream stays ahead of the
// clock. They are the SAME fill loop with two lookahead constants — there is never a
// second concurrent filler.
const PRIME_AHEAD = 3;
const PLAY_AHEAD = 10;
// Buffered history kept behind the playhead before eviction frees it. The persistent
// green bar comes from IndexedDB, so evicting MSE never shrinks it.
const KEEP_BEHIND = 6;
// Max seconds streamed (and cached) per /api/video.mp4 request. Active playback
// uses a modestly larger cap than priming: long chunks delay durable cache
// commits and make slow first-pass renders expensive to abort/retry, while very
// small chunks pay too much ffmpeg/audio setup overhead.
const PRIME_STREAM_PIECE = 3;
const PLAY_STREAM_PIECE = 5;
// Minimum length for a cached segment row to be worth re-appending from IndexedDB.
// Shorter rows (shards from the old lookahead-clamped frontier) are streamed over in
// full pieces instead — putSegment's subsume then deletes them, so a fragmented store
// heals itself. Re-appending shards one by one is strictly worse: every segmentAt is
// a cursor walk over ALL of the group's rows, and every shard seam is a potential
// sub-frame gap for playback to trip on.
const MIN_SEGMENT_REUSE = 1;
// Paused scrubbing fires many seeks; defer the actual FETCH until the playhead has been
// quiet this long so a drag issues ~1 fetch at its resting point, not 120.
const SEEK_DEBOUNCE_MS = 150;
// `seeked` can never fire when the target sits at the very edge of a freshly-appended
// MSE buffer (observed in Arc); the settle wait resolves on the first of several
// events OR this timeout, and NEVER rejects, so a missing `seeked` degrades to "try
// to play anyway" rather than a dead playback.
const SETTLE_TIMEOUT_MS = 1500;
// End-of-timeline stall: the stream's buffered end routinely lands a few frames short
// of duration (the server clamps the last renderable frame), so `ended` never fires.
// Detect a sustained no-progress stall AT the buffer end, but only when that end is
// within END_STALL_TAIL of duration, so a mid-clip buffering wait is never mistaken
// for the end.
const END_STALL_TICKS = 6;
const END_STALL_TAIL = 0.5;
// Mid-clip stall watchdog: while playing, if currentTime makes no progress for this
// many RAF ticks (~300ms at 60Hz), try to un-stick it. Distinct from END_STALL_*,
// which only handles the timeline tail. Without this, a sub-frame seam gap (coded
// frame removal / float jitter where two segments meet) freezes playback forever
// while the fill loop keeps caching ahead — the green bar grows but the playhead
// never moves.
const STALL_NUDGE_TICKS = 18;
// Max gap (seconds) the watchdog will jump to reach the next buffered range. Seam
// gaps are sub-frame; anything larger is real missing content that the fill loop
// must stream first, so jumping it would skip frames the user should see.
const STALL_GAP_JUMP_MAX = 0.25;
// After a stream fetch fails (server error / unparsable body), hold off re-fetching
// this long. The ticker kicks the fill loop every animation frame, so without a
// backoff a persistently failing server gets hammered ~60 times per second.
const STREAM_ERROR_BACKOFF_MS = 1000;
// A single SourceBuffer append failure can be caused by an aborted stream leaving a
// partial MP4 box in the parser. Reset and retry silently first; surface it only if
// the stream keeps failing after recovery.
const APPEND_FAILURES_BEFORE_ERROR = 3;
// Buffered lead required past the playhead before playback STARTS (the reveal), and
// the larger lead required before it RESUMES after starving mid-clip. Without a lead,
// a stream that encodes at ~1x realtime glues the playhead to the live edge and
// playback inches forward one GOP fragment (fps/4 ≈ 12-15 frames) at a time — a
// rhythmic micro-stutter. With it, playback rides a cushion: encode-at-speed plays
// smoothly, and slower-than-realtime degrades to a few clean rebuffers instead.
// Either lead is satisfied early when the buffer already reaches the timeline tail.
const PLAYBACK_LEAD = 1;
const REBUFFER_LEAD = 2;
// Consecutive no-progress ticks (~100ms) and the look-ahead probe distance that
// classify a stall as "starved at the live edge" (vs a seam gap, which has data
// right behind it and is handled by the nudge watchdog).
const STARVE_TICKS = 6;
const STARVE_PROBE = 0.2;

// A/V-with-FLAC variants FIRST: the preview stream ALWAYS carries a FLAC audio track
// (silent when the timeline has no audio), so the SourceBuffer must declare the audio
// codec or appended segments fail to parse. FLAC (not AAC) because AAC's per-encode
// priming left an audible click at every fresh-encode cache seam; FLAC has zero
// encoder delay so segments concatenate gaplessly. The three avc1 profiles cover
// browsers that support different H.264 levels; the bare MP4 fallback is last for
// browsers that accept container-level probing without explicit codecs. Video-only
// codec strings are intentionally omitted because the server always muxes audio.
const MP4_MIME_TYPES = [
  'video/mp4; codecs="avc1.42E01E, flac"',
  'video/mp4; codecs="avc1.4D401E, flac"',
  'video/mp4; codecs="avc1.64001E, flac"',
  "video/mp4",
];

export type DisplayMode = "video" | "still";

// How the shell should cover the <video> when switching to a still, chosen so the user
// never sees a frame for the WRONG time:
// - "hold":     keep whatever is on screen (the parked video frame or the trailing
//               still) and swap to the still only once the fresh still for stillTime
//               has decoded. Used for pause / end-stop / paused scrubs over cold
//               frames — the current display stays up for continuity, never a flash.
// - "blank":    the <video> shows a DIFFERENT time (a seek while playing into a cold
//               region). Clear to the neutral background immediately — never flash a
//               recognizable stale frame — then show the fresh still when it decodes.
// - "trailing": mount. Show the still layer immediately and refine it once the first
//               frame loads.
// - "hold-video": keep the current surface up without fetching a PNG still. Used when
//                 the next reveal should come from MP4/video decode too, avoiding a
//                 PNG/RGBA -> H.264/YUV color jump on play.
export type StillCover = "hold" | "hold-video" | "blank" | "trailing";

export interface TimelinePlayerEvents {
  onTime?: (seconds: number) => void;
  onRanges?: (ranges: CacheRange[]) => void;
  onPlaying?: (playing: boolean) => void;
  onDisplayMode?: (
    mode: DisplayMode,
    stillTime: number,
    cover: StillCover,
  ) => void;
  onEnded?: () => void;
  onError?: (message: string | null) => void;
}

export interface TimelinePlayerConfig {
  groupKey: string;
  pluginKey: string;
  duration: number;
  fps: number;
  initialPosition: number;
  isMuted: () => boolean;
  // Builds the /api/video.mp4 URL for the half-open segment [start, end). When
  // end >= duration the caller should omit the `duration` param (stream to the tail);
  // this closure handles that.
  videoUrl: (start: number, end: number) => string;
  cache: MediaCache;
}

type PlayMode = "idle" | "priming" | "playing";

interface InFlight {
  start: number;
  end: number;
  controller: AbortController;
  fillGen: number;
  received: ArrayBuffer[];
  liveEnd: number;
}

interface AppendJob {
  // timestampOffset for this segment = its frame-aligned start time.
  offset: number;
  data: ArrayBuffer;
  fillGen: number;
}

interface RemoveJob {
  start: number;
  end: number;
}

type QueuedOp =
  | ({ kind: "append" } & AppendJob & { settle: (ok: boolean) => void })
  | ({ kind: "remove" } & RemoveJob & { settle: () => void })
  | { kind: "reset"; settle: () => void };

// A single self-contained MSE-backed timeline player. Owns ONE MediaSource + ONE
// SourceBuffer attached to the supplied <video>, a single cursor-driven fill loop,
// and the only writer (a serialized op queue) over the SourceBuffer. The hook
// recreates the player when the plugin/resolution/fps group changes — so "epoch" is
// simply this instance's lifetime, guarded by `disposed`. `fillGen` invalidates a
// stale fill cursor / in-flight fetch on seek/play/pause WITHOUT tearing down MSE.
export class TimelinePlayer {
  private readonly video: HTMLVideoElement;
  private readonly config: TimelinePlayerConfig;
  private readonly events: TimelinePlayerEvents;

  private readonly mediaSource: MediaSource;
  private readonly objectUrl: string;
  private sourceBuffer: SourceBuffer | null = null;
  private readonly ready: Promise<void>;

  private disposed = false;
  private fillGen = 0;
  private mode: PlayMode = "priming";

  // Logical playhead in timeline seconds; the source of truth for the cursor and the
  // paused still time. During playback the ticker advances it from video.currentTime.
  private position: number;

  // Committed cache = the cached ranges durably stored in IndexedDB (a mirror of the
  // segment store). This — NOT sourceBuffer.buffered — is the persistent green bar, so
  // UA/eviction never shrinks it.
  private committedRanges: CacheRange[] = [];
  private inflight: InFlight | null = null;

  // Serialized op queue: the ONLY code that mutates the SourceBuffer.
  private readonly opQueue: QueuedOp[] = [];
  private pumping = false;
  // The timestampOffset currently applied to the SourceBuffer, so we only re-set it when
  // a different segment is appended.
  private lastAppliedOffset: number | null = null;
  private parserResetQueued = false;
  private appendFailureCount = 0;

  private fillActiveGen = -1;
  private lastEvictedTo = 0;
  // Earliest time (Date.now ms) the fill loop may issue another stream fetch after a
  // failure; 0 when streaming is healthy.
  private streamBlockedUntil = 0;
  // True while playback is parked (element paused, mode still "playing") waiting for
  // the REBUFFER_LEAD to build after starving at the live edge.
  private rebuffering = false;
  private seekDebounceTimer: ReturnType<typeof setTimeout> | null = null;
  // One-shot "reveal the playhead and (if playing) start" once its chunk is buffered.
  private pendingReveal: { target: number; fillGen: number } | null = null;
  private rafId: number | null = null;

  constructor(
    video: HTMLVideoElement,
    config: TimelinePlayerConfig,
    events: TimelinePlayerEvents,
  ) {
    this.video = video;
    this.config = config;
    this.events = events;
    this.position = clamp(config.initialPosition, 0, config.duration);
    this.mediaSource = new MediaSource();
    this.objectUrl = URL.createObjectURL(this.mediaSource);
    this.ready = this.init();
    const committed = this.loadCommitted();
    // Cover the element with the paused still immediately; prime ahead from the start
    // so play() is instant (requirement 1).
    this.setDisplayMode("still", this.position);
    this.emitTime(this.position);
    // Start filling only once BOTH the SourceBuffer is open AND the committed ranges are
    // loaded from IndexedDB. Otherwise, on a warm-cache reload, runFill would start with an
    // empty committedRanges mirror and re-stream a region that is already cached (req 7).
    void Promise.all([this.ready, committed])
      .then(() => {
        if (!this.disposed) this.kickFill();
      })
      .catch(() => {
        // init() already surfaced the error; do not retry without a SourceBuffer.
      });
  }

  // ---- public API -------------------------------------------------------------

  get currentTime(): number {
    return this.currentPlaybackSeconds();
  }

  seek(seconds: number): void {
    if (this.disposed) return;
    this.clearError();
    const target = clamp(seconds, 0, this.config.duration);
    this.position = target;
    this.fillGen++;
    this.lastEvictedTo = 0;
    this.rebuffering = false;
    this.abortInflight();
    this.clearSeekDebounce();
    this.emitTime(target);

    if (this.mode === "playing") {
      // Seeking to the very end while playing is an end-stop: nothing is renderable at
      // duration, so the fill loop has no work and a reveal would never fire — playback
      // would hang forever on the still. Settle at the end instead.
      if (target >= this.config.duration - EPSILON) {
        this.settleAtEnd();
        return;
      }
      // Keep playing through the seek. If the target is already buffered, reposition
      // instantly; else cover with the still and reveal once its chunk arrives.
      if (this.isTimeBuffered(target) && !this.video.paused) {
        this.pendingReveal = null;
        this.video.currentTime = this.clampToBufferedStart(target);
        this.setDisplayMode("video", target);
        // Re-arm the ticker under the NEW fillGen: the old ticker bails on the gen change
        // and stops rescheduling, but the element keeps playing — without this the
        // playhead readout, eviction, and the end-stall watchdog would all freeze while
        // the video advances.
        this.startTicker(this.fillGen);
        this.kickFill();
      } else {
        // Target not buffered: pause the element so the OLD region's audio/video stops at
        // once (it is hidden behind the cover anyway), and cover to the neutral background
        // rather than flashing a stale frame. revealAt re-seeks and resumes playback once
        // the target chunk has streamed in.
        this.video.pause();
        this.setDisplayMode("still", target, "blank");
        this.pendingReveal = { target, fillGen: this.fillGen };
        this.kickFill();
      }
    } else {
      // Paused scrub / frame step.
      this.mode = "priming";
      if (this.isTimeBuffered(target) || this.cachedRangeContaining(target)) {
        // The frame is locally available (MSE buffer or a committed IndexedDB
        // segment): show it from the <video> element instead of fetching a PNG
        // still — no server round-trip. Keep whatever is on screen until the
        // reveal swaps in the correct frame.
        this.pendingReveal = { target, fillGen: this.fillGen };
        void this.revealFromCache(target, this.fillGen);
      } else {
        // Cold frame: keep the current display up for drag continuity, but do
        // not fetch a PNG still. Instead, prime a short MP4 segment and reveal
        // the parked <video> frame once it lands. That keeps paused and playing
        // frames on the same browser decode path, so there is no PNG/RGBA ->
        // H.264/YUV color jump when playback starts.
        this.pendingReveal = { target, fillGen: this.fillGen };
        this.setDisplayMode("still", target, "hold-video");
      }
      this.scheduleDebouncedFill();
    }
  }

  // Must be called inside the user gesture so unmuted playback satisfies autoplay policy.
  play(): void {
    if (this.disposed) return;
    this.clearError();
    // Apply the user's audio state synchronously within the gesture.
    // React does not re-apply the `muted` attribute after mount, so set the property.
    this.video.muted = this.config.isMuted();
    // Play-from-the-end replays from the start: at duration the cursor loop has no work
    // and playback would silently never begin.
    const replayFromEnd = this.position >= this.config.duration - EPSILON;
    if (replayFromEnd) {
      this.position = 0;
      this.emitTime(0);
    }
    // Only invalidate the fill cursor + drop the in-flight stream when the playhead
    // actually MOVES (replay-from-end). For a plain paused->playing at the same spot,
    // let the in-flight prime keep streaming FORWARD — it is already exactly what
    // playback wants. This avoids re-streaming from the buffer edge AND avoids the green
    // bar flashing to 0 when an unfinished prime would otherwise be dropped (req 7/8).
    if (replayFromEnd) {
      this.fillGen++;
      this.lastEvictedTo = 0;
      this.abortInflight();
    }
    this.clearSeekDebounce();
    this.mode = "playing";
    this.events.onPlaying?.(true);
    this.pendingReveal = { target: this.position, fillGen: this.fillGen };
    // Keep the current surface up without fetching a PNG still; revealAt swaps
    // straight to running video once enough MP4 is buffered.
    this.setDisplayMode("still", this.position, "hold-video");
    this.tryResolveReveal();
    this.kickFill();
  }

  setMuted(muted: boolean): void {
    if (this.disposed) return;
    this.video.muted = muted;
  }

  // Wake / online recovery: drop a hung stream, clear transient errors, and retry fill.
  recoverFromNetwork(): void {
    if (this.disposed) return;
    this.clearError();
    this.streamBlockedUntil = 0;
    if (this.inflight) {
      this.abortInflight();
    }
    this.kickFill();
  }

  pause(): void {
    if (this.disposed) return;
    const hadPendingReveal = this.pendingReveal != null;
    this.position = this.currentPlaybackSeconds();
    this.video.pause();
    this.stopTicker();
    this.mode = "priming";
    this.rebuffering = false;
    this.pendingReveal = null;
    this.events.onPlaying?.(false);
    this.emitTime(this.position);
    // During normal playback the video is already parked on the correct frame.
    // Keep displaying that decoded frame instead of swapping to a PNG still:
    // H.264/YUV and PNG/RGBA are not byte-identical, so switching surfaces on
    // pause creates a subtle color jump. If pause lands while a reveal is still
    // pending, the element may be showing an old frame; use the still fallback.
    const videoFrameStep = 1 / Math.max(this.config.fps, 1);
    const videoOnFrame =
      !hadPendingReveal &&
      this.video.readyState >= HTMLMediaElement.HAVE_CURRENT_DATA &&
      Math.abs(this.video.currentTime - this.position) <= videoFrameStep * 2;
    if (videoOnFrame) {
      this.setDisplayMode("video", this.position);
    } else {
      this.setDisplayMode("still", this.position, "hold");
    }
    // Let short priming pieces finish and persist, but stop a long playback stream
    // when the user pauses. Otherwise pausing near the start of a long timeline would keep
    // the server encoding far past the now-still playhead.
    if (
      this.inflight &&
      this.inflight.end - this.inflight.start > PRIME_STREAM_PIECE + EPSILON
    ) {
      this.abortInflight();
    }
    // The fill loop re-reads `mode` each iteration, so it transparently downshifts to
    // priming look-ahead. If it already idled, kickFill restarts it in priming mode.
    this.kickFill();
  }

  async dispose(): Promise<void> {
    if (this.disposed) return;
    this.disposed = true;
    this.fillGen++;
    this.clearSeekDebounce();
    this.stopTicker();
    this.abortInflight();
    // Drop every queued op so awaiters don't hang and the pump exits.
    for (const op of this.opQueue.splice(0)) {
      if (op.kind === "append") {
        op.settle(false);
      } else {
        op.settle();
      }
    }
    try {
      this.video.pause();
    } catch {
      // ignore
    }
    this.video.onended = null;
    this.video.onerror = null;
    try {
      this.video.removeAttribute("src");
      this.video.load();
    } catch {
      // ignore
    }
    if (this.mediaSource.readyState === "open") {
      try {
        this.mediaSource.endOfStream();
      } catch {
        // ignore
      }
    }
    URL.revokeObjectURL(this.objectUrl);
  }

  // ---- init -------------------------------------------------------------------

  private async init(): Promise<void> {
    try {
      // Mount muted so the element can buffer/seek/decode WITHOUT a user gesture.
      this.video.muted = true;
      this.video.playsInline = true;
      this.video.preload = "auto";
      this.video.src = this.objectUrl;
      try {
        this.video.load();
      } catch {
        // ignore
      }
      await waitForSourceOpen(this.mediaSource);
      if (this.disposed) return;
      const sourceBuffer = this.mediaSource.addSourceBuffer(selectMimeType());
      try {
        // Place appended media by its timestamps + timestampOffset rather than
        // concatenating; some browsers expose `mode` as readonly, hence the try/catch.
        sourceBuffer.mode = "segments";
      } catch {
        // ignore
      }
      this.sourceBuffer = sourceBuffer;
      try {
        // Set up front so native `ended` can fire at the true timeline end.
        this.mediaSource.duration = this.config.duration;
      } catch {
        // ignore
      }
      // Native ended is the happy path; the stall watchdog (ticker) is the backup.
      this.video.onended = () => this.settleAtEnd();
      this.video.onerror = () => {
        if (!this.disposed) this.events.onError?.("video element error");
      };
    } catch (e) {
      if (!this.disposed) this.events.onError?.(String(e));
      throw e;
    }
  }

  private async loadCommitted(): Promise<void> {
    const ranges = await this.config.cache.cachedRanges(this.config.groupKey);
    if (this.disposed) return;
    // MERGE, never overwrite: a streamed piece can finish and recordCommitted() between the
    // cachedRanges() read snapshot and this assignment, and a blind `= ranges` would drop
    // that fresh range (shrinking the green bar and forcing a re-stream).
    this.committedRanges = mergeRanges([...this.committedRanges, ...ranges]);
    this.emitRanges();
  }

  private recordCommitted(start: number, end: number): void {
    if (!(end > start + EPSILON)) return;
    this.committedRanges = mergeRanges([...this.committedRanges, { start, end }]);
  }

  // Subtract [start, end] from the committed mirror — used when a cached segment turns
  // out to be unusable, so the fill loop streams the span fresh instead of retrying the
  // same doomed append forever. IndexedDB is left alone: the replacement putSegment
  // subsumes the stale row.
  private uncommitRange(start: number, end: number): void {
    const next: CacheRange[] = [];
    for (const r of this.committedRanges) {
      if (r.end <= start + EPSILON || r.start >= end - EPSILON) {
        next.push(r);
        continue;
      }
      if (r.start < start - EPSILON) next.push({ start: r.start, end: start });
      if (r.end > end + EPSILON) next.push({ start: end, end: r.end });
    }
    this.committedRanges = next;
  }

  // Quantize a time DOWN to the frame boundary at or before t, so every streamed segment
  // starts exactly on a frame boundary. Two reasons: (1) segments from DIFFERENT seek
  // points stay sample-aligned where they meet (an off-grid start would shift a whole
  // segment by a sub-frame and overlap its neighbour at the seam — a click/stutter);
  // (2) flooring (never rounding up past t) guarantees the segment CONTAINS the playhead,
  // so the reveal's `isTimeBuffered(position)` succeeds (rounding up could leave the
  // playhead in a sub-frame gap before the segment start, stranding playback).
  private frameAlign(t: number): number {
    const fps = this.config.fps;
    if (!(fps > 0)) return t;
    return Math.floor(t * fps + EPSILON) / fps;
  }

  // In-memory committed-cache queries (the green-bar mirror), so the fill loop's
  // gap-finding does NO IndexedDB work on the cold-streaming hot path.
  private cachedRangeContaining(t: number): CacheRange | null {
    for (const r of this.committedRanges) {
      if (r.start <= t + EPSILON && r.end > t + EPSILON) return r;
    }
    return null;
  }

  private nextCachedStart(t: number): number {
    let best = Infinity;
    for (const r of this.committedRanges) {
      if (r.start > t + EPSILON && r.start < best) best = r.start;
    }
    return best;
  }

  // ---- fill loop --------------------------------------------------------------

  private kickFill(): void {
    if (this.disposed) return;
    if (!this.sourceBuffer) {
      void this.ready
        .then(() => {
          if (!this.disposed) this.kickFill();
        })
        .catch(() => {
          // init() already surfaced the error; do not retry without a SourceBuffer.
        });
      return;
    }
    const gen = this.fillGen;
    if (this.fillActiveGen === gen) return; // already filling for this cursor
    this.fillActiveGen = gen;
    void this.runFill(gen).finally(() => {
      if (this.fillActiveGen === gen) this.fillActiveGen = -1;
    });
  }

  private scheduleDebouncedFill(): void {
    this.clearSeekDebounce();
    this.seekDebounceTimer = setTimeout(() => {
      this.seekDebounceTimer = null;
      this.kickFill();
    }, SEEK_DEBOUNCE_MS);
  }

  private clearSeekDebounce(): void {
    if (this.seekDebounceTimer != null) {
      clearTimeout(this.seekDebounceTimer);
      this.seekDebounceTimer = null;
    }
  }

  private async runFill(gen: number): Promise<void> {
    const { duration, groupKey, cache } = this.config;
    let cursor = this.position;
    while (!this.disposed && gen === this.fillGen) {
      const lookahead =
        this.mode === "playing"
          ? PLAY_AHEAD
          : this.mode === "priming"
            ? PRIME_AHEAD
            : 0;
      if (lookahead <= 0) return;

      // Always fill FORWARD from the playhead; if it advanced past the cursor, catch up.
      if (cursor < this.position) cursor = this.position;
      const targetEnd = Math.min(duration, this.position + lookahead);
      if (cursor >= targetEnd - EPSILON || cursor >= duration - EPSILON) {
        // Lookahead window satisfied. If we played to the end, finalize so native
        // `ended` can fire.
        if (this.mode === "playing" && cursor >= duration - EPSILON) {
          this.endOfStreamIfOpen();
        }
        return;
      }

      // Already in the live MSE buffer? Skip past it (don't re-fetch). In-memory.
      if (this.isTimeBuffered(cursor)) {
        cursor = Math.max(cursor + EPSILON, this.bufferedEndContaining(cursor));
        continue;
      }

      // Covered by a committed cache range (in-memory check — NO IndexedDB on the hot /
      // cold-streaming path)? Only then fetch the exact segment blob to re-append it
      // (this only happens for a cached segment the UA evicted from MSE). Shard rows
      // are not reused (see MIN_SEGMENT_REUSE) — fall through and stream over them,
      // except at the timeline tail, where a legitimately short final piece lives.
      if (this.cachedRangeContaining(cursor)) {
        const seg = await cache.segmentAt(groupKey, cursor);
        if (this.disposed || gen !== this.fillGen) return;
        const reusable =
          seg != null &&
          (seg.end - seg.start >= MIN_SEGMENT_REUSE ||
            seg.end >= duration - EPSILON);
        if (seg && reusable && seg.end > cursor + EPSILON) {
          const appended = await this.appendSegment(
            seg.start,
            await blobToArrayBuffer(seg.blob),
            gen,
          );
          if (this.disposed || gen !== this.fillGen) return;
          if (!appended || !this.isTimeBuffered(cursor)) {
            // The append never landed (quota that even eviction couldn't relieve, or
            // an unparsable blob). Drop the range from the in-memory mirror and
            // stream it fresh — putSegment subsumes the stale row, so a bad blob
            // self-heals. Never just stop here: the ticker re-kicks the fill every
            // animation frame, so a permanently failing append would re-read a
            // multi-MB blob from IndexedDB at 60Hz while playback stays frozen.
            this.uncommitRange(seg.start, seg.end);
            continue;
          }
          this.emitRanges();
          this.tryResolveReveal();
          cursor = Math.max(cursor + EPSILON, seg.end);
          continue;
        }
        // Row missing or a shard — fall through and stream over it.
      }

      // Cache miss: stream FORWARD from the cursor (frame-aligned), bounded by the next
      // committed cache range (in-memory). While playing, request a large continuous
      // stream toward that boundary/tail, capped by PLAY_STREAM_PIECE; while priming,
      // keep small bounded pieces so paused scrubs do not waste long encodes.
      // Deliberately NOT bounded by targetEnd: the lookahead only gates WHEN to stream,
      // never the piece size. Clamping to the moving lookahead edge degenerates the
      // frontier into per-tick frame-sized requests once it catches up.
      // While the post-failure backoff is armed, end this pass instead of fetching —
      // the ticker re-kicks the fill every frame, so the retry happens as soon as the
      // backoff expires.
      if (Date.now() < this.streamBlockedUntil) return;
      const start = this.frameAlign(cursor);
      const maxEnd =
        start +
        (this.mode === "playing" ? PLAY_STREAM_PIECE : PRIME_STREAM_PIECE);
      const end = Math.min(
        this.nextCachedStart(cursor),
        maxEnd,
        duration,
      );
      if (end <= start + EPSILON) {
        cursor = Math.max(cursor + EPSILON, end);
        continue;
      }
      const reachedTo = await this.streamSegment(start, end, gen);
      if (this.disposed || gen !== this.fillGen) return;
      cursor = Math.max(cursor + EPSILON, reachedTo);
    }
  }

  // Stream a segment FORWARD from `start` to `end`, appending each slice as it arrives and
  // persisting the WHOLE piece on natural EOF. A partial (aborted) fetch is NOT persisted
  // — a truncated fMP4 isn't a self-contained segment and could overlap a neighbour — so a
  // cached segment is always frame-aligned and gap-free. Paused priming bounds the dropped
  // amount; active playback aborts its long stream on seek/pause. Returns how far it
  // actually reached (frame-aligned buffered end).
  private async streamSegment(start: number, end: number, gen: number): Promise<number> {
    const controller = new AbortController();
    const inflight: InFlight = {
      start,
      end,
      controller,
      fillGen: gen,
      received: [],
      liveEnd: start,
    };
    this.inflight = inflight;
    const url = this.config.videoUrl(start, end);
    let reached = start;
    let reader: ReadableStreamDefaultReader<Uint8Array> | null = null;
    try {
      const response = await fetch(url, { cache: "no-store", signal: controller.signal });
      if (this.disposed || gen !== this.fillGen) return reached;
      if (!response.ok) throw new Error(`${url} failed: ${response.status}`);
      reader = response.body?.getReader() ?? null;
      if (!reader) throw new Error("video stream has no body");
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        if (this.disposed || gen !== this.fillGen) return reached;
        if (!value) continue;
        // Copy exactly the view's region; the reader reuses one backing buffer, so
        // appending value.buffer directly would splice in bytes from other reads.
        const slice = value.buffer.slice(
          value.byteOffset,
          value.byteOffset + value.byteLength,
        ) as ArrayBuffer;
        inflight.received.push(slice);
        const appended = await this.appendSegment(start, slice.slice(0), gen);
        if (this.disposed || gen !== this.fillGen) return reached;
        if (!appended) throw new SourceBufferAppendError();
        this.noteAppendSuccess();
        // The live green edge is the buffered end of THIS segment only — never bytes
        // received (which over-report before parse) and never the global buffered end
        // (contaminated by lookahead / eviction). buffered advances per GOP fragment.
        inflight.liveEnd = this.liveEndOf(start, end);
        reached = inflight.liveEnd;
        this.emitRanges();
        this.tryResolveReveal();
      }
      // Natural EOF: persist [start, actual buffered end]. The recorded end is the last
      // complete fragment (frame-aligned) so the next segment butts up gaplessly.
      const persistedEnd = Math.max(this.liveEndOf(start, end), start);
      if (this.inflight === inflight) this.inflight = null;
      if (!(persistedEnd > start + EPSILON)) {
        // The request completed but yielded nothing appendable (e.g. the sub-frame
        // tail past the last renderable frame, or every append was refused). Report
        // the span as covered so the fill cursor moves past it instead of spinning
        // on the same fetch; a later pass retries it from a fresh cursor anyway.
        this.emitRanges();
        return end;
      }
      reached = persistedEnd;
      this.streamBlockedUntil = 0;
      this.clearError();
      const blob = new Blob(inflight.received, { type: "video/mp4" });
      const ok = await this.config.cache.putSegment(
        this.config.groupKey,
        this.config.pluginKey,
        start,
        persistedEnd,
        blob,
      );
      if (this.disposed) return reached;
      if (ok) this.recordCommitted(start, persistedEnd);
      this.emitRanges();
      return reached;
    } catch (e) {
      if (this.inflight === inflight) this.inflight = null;
      if (isAbortError(e)) {
        // Intentional interruption (seek/pause/dispose): drop the partial and recede the
        // live edge. Stream spans are bounded so the re-streamed amount stays finite.
        this.emitRanges();
        return reached;
      }
      if (this.disposed || gen !== this.fillGen) return reached;
      // Arm the backoff and leave this span uncovered; without the backoff, the
      // per-frame kickFill would turn a failing server or SourceBuffer into a
      // fetch storm.
      this.streamBlockedUntil = Date.now() + STREAM_ERROR_BACKOFF_MS;
      if (inflight.received.length > 0) this.enqueueParserReset();
      try {
        controller.abort();
      } catch {
        // ignore
      }
      try {
        await reader?.cancel();
      } catch {
        // ignore
      }
      if (isSourceBufferAppendError(e)) {
        this.appendFailureCount++;
        if (this.appendFailureCount >= APPEND_FAILURES_BEFORE_ERROR) {
          this.events.onError?.(String(e));
        }
      } else if (isTransientNetworkError(e)) {
        this.events.onError?.(String(e));
      } else {
        this.events.onError?.(String(e));
      }
      return reached;
    } finally {
      try {
        reader?.releaseLock();
      } catch {
        // ignore
      }
    }
  }

  // Enqueue an append for a segment slice and resolve once it has drained (backpressure
  // so the reader can't outrun the SourceBuffer). The op carries fillGen so a stale
  // cursor's slices are dropped by the pump rather than polluting buffered ranges.
  private appendSegment(
    offset: number,
    data: ArrayBuffer,
    fillGen: number,
  ): Promise<boolean> {
    return new Promise<boolean>((resolve) => {
      this.opQueue.push({ kind: "append", offset, data, fillGen, settle: resolve });
      void this.pump();
    });
  }

  // ---- op pump (the only SourceBuffer writer) ---------------------------------

  private async pump(): Promise<void> {
    if (this.pumping) return;
    this.pumping = true;
    try {
      while (this.opQueue.length > 0 && !this.disposed) {
        const sb = this.sourceBuffer;
        // Tolerate readyState "ended": appendBuffer / timestampOffset transition it
        // back to "open" automatically (per the MSE spec), which is exactly how a
        // seek-back or replay-from-end re-fills after the tail endOfStream(). Only
        // "closed" (detached) is unusable.
        if (!sb || this.mediaSource.readyState === "closed") break;
        const op = this.opQueue.shift()!;
        if (op.kind === "append" && op.fillGen !== this.fillGen) {
          op.settle(false); // stale cursor — drop, but settle so the awaiter continues
          continue;
        }
        let appended = false;
        try {
          if (op.kind === "append") {
            appended = await this.appendWithQuota(sb, op.offset, op.data);
          } else if (op.kind === "remove") {
            await this.removeRange(sb, op.start, op.end);
          } else {
            await this.resetSourceBufferParser(sb);
          }
        } catch (e) {
          if (isInvalidState(e)) {
            // MediaSource detached mid-op (teardown) — stop draining.
            if (op.kind === "append") {
              op.settle(false);
            } else {
              if (op.kind === "reset") this.parserResetQueued = false;
              op.settle();
            }
            break;
          }
          if (op.kind === "append" && isSourceBufferAppendError(e)) {
            await this.resetSourceBufferParser(sb).catch(() => {});
          }
          if (
            op.kind === "append" &&
            !this.disposed &&
            op.fillGen === this.fillGen &&
            !isAbortError(e) &&
            !isSourceBufferAppendError(e)
          ) {
            this.events.onError?.(String(e));
          }
        }
        if (op.kind === "append") {
          op.settle(appended);
        } else if (op.kind === "reset") {
          this.parserResetQueued = false;
          op.settle();
        } else {
          op.settle();
        }
      }
    } finally {
      this.pumping = false;
    }
  }

  private async appendWithQuota(
    sb: SourceBuffer,
    offset: number,
    data: ArrayBuffer,
  ): Promise<boolean> {
    for (let attempt = 0; ; attempt++) {
      try {
        return await this.appendOne(sb, offset, data);
      } catch (e) {
        // QuotaExceededError is thrown synchronously when the buffer is full. Evict
        // and retry; this is the normal path during long playback, not an edge case.
        if (isQuotaExceeded(e) && attempt < 4) {
          if (await this.evictForQuota(sb, offset)) continue;
          // Nothing evictable: give up this append; it will be retried as the
          // playhead advances and frees room.
          return false;
        }
        throw e;
      }
    }
  }

  // Free SourceBuffer room for an append at `offset`. Prefer the oldest data well
  // behind the playhead; when nothing is buffered back there, fall back to evicting
  // far AHEAD of both the playhead and the appending segment. The far-ahead data is
  // committed in IndexedDB and re-appends when the playhead approaches, whereas
  // refusing the append at the playhead would deadlock playback outright (the
  // playhead can never advance to free room behind itself).
  private async evictForQuota(sb: SourceBuffer, offset: number): Promise<boolean> {
    const playhead = this.currentPlaybackSeconds();
    const behindEnd = playhead - KEEP_BEHIND;
    if (behindEnd > EPSILON && this.hasBufferedBefore(behindEnd)) {
      try {
        await this.removeRange(sb, 0, behindEnd);
        this.emitRanges();
        return true;
      } catch {
        // fall through to the far-ahead fallback
      }
    }
    // PLAY_AHEAD past both the playhead and the appending segment keeps everything
    // the player is about to need; include the active stream's nominal end as well so
    // quota eviction never splits the row that will be persisted when it reaches EOF.
    const inflightEnd =
      this.inflight && this.inflight.start <= offset + EPSILON
        ? this.inflight.end
        : offset;
    const aheadStart = Math.max(playhead, offset, inflightEnd) + PLAY_AHEAD;
    if (this.bufferedEnd() > aheadStart + EPSILON) {
      try {
        await this.removeRange(sb, aheadStart, Number.POSITIVE_INFINITY);
        this.emitRanges();
        return true;
      } catch {
        // give up
      }
    }
    return false;
  }

  private async appendOne(
    sb: SourceBuffer,
    offset: number,
    data: ArrayBuffer,
  ): Promise<boolean> {
    await waitForSourceBufferIdle(sb);
    // "ended" is fine here — setting timestampOffset / appendBuffer transitions the
    // MediaSource back to "open". Bail only when it's "closed" (detached on teardown).
    if (this.disposed || this.mediaSource.readyState === "closed") return false;
    if (offset !== this.lastAppliedOffset) {
      // Position this segment so buffered-time == timeline-time. Each segment is a
      // self-contained encode with its own IDR at `offset` (a frame-aligned time) and an
      // exact integer number of FLAC packets, so leaving appendWindow at its defaults
      // (open) butts adjacent segments up sample-exact with no overlap and no dropped
      // boundary frame.
      sb.timestampOffset = offset;
      this.lastAppliedOffset = offset;
    }
    return await new Promise<boolean>((resolve, reject) => {
      const onEnd = () => {
        cleanup();
        resolve(true);
      };
      const onErr = () => {
        cleanup();
        reject(new SourceBufferAppendError());
      };
      const cleanup = () => {
        sb.removeEventListener("updateend", onEnd);
        sb.removeEventListener("error", onErr);
        clearTimeout(timer);
      };
      // Teardown guard: if the MediaSource detaches mid-append, neither updateend nor error
      // ever fires; resolve after a bound so the pump and its streamSegment awaiter can't
      // hang forever. Normal slice appends complete in well under this, so it never fires
      // during real playback.
      const timer = setTimeout(() => {
        cleanup();
        resolve(false);
      }, SETTLE_TIMEOUT_MS);
      sb.addEventListener("updateend", onEnd);
      sb.addEventListener("error", onErr);
      try {
        // appendBuffer can throw QuotaExceededError synchronously; clean up the
        // listeners before rejecting so a quota retry doesn't leak them.
        sb.appendBuffer(data);
      } catch (e) {
        cleanup();
        reject(e);
      }
    });
  }

  private enqueueParserReset(): void {
    if (this.disposed || this.parserResetQueued || !this.sourceBuffer) return;
    this.parserResetQueued = true;
    this.opQueue.push({
      kind: "reset",
      settle: () => {
        this.parserResetQueued = false;
      },
    });
    void this.pump();
  }

  private async resetSourceBufferParser(sb: SourceBuffer): Promise<void> {
    await waitForSourceBufferIdle(sb);
    this.lastAppliedOffset = null;
    if (this.disposed || this.mediaSource.readyState !== "open") return;
    try {
      sb.abort();
    } catch (e) {
      if (!isInvalidState(e)) throw e;
    }
  }

  private noteAppendSuccess(): void {
    this.appendFailureCount = 0;
    this.clearError();
  }

  private clearError(): void {
    this.events.onError?.(null);
  }

  private async removeRange(sb: SourceBuffer, start: number, end: number): Promise<void> {
    if (!(end > start + EPSILON)) return;
    await waitForSourceBufferIdle(sb);
    if (this.disposed || this.mediaSource.readyState !== "open") return;
    await new Promise<void>((resolve) => {
      const onEnd = () => {
        sb.removeEventListener("updateend", onEnd);
        clearTimeout(timer);
        resolve();
      };
      // Teardown guard (see appendOne): a detached buffer never fires updateend.
      const timer = setTimeout(onEnd, SETTLE_TIMEOUT_MS);
      sb.addEventListener("updateend", onEnd);
      try {
        sb.remove(start, end);
      } catch {
        onEnd();
      }
    });
  }

  private enqueueEviction(): void {
    // Evict everything older than KEEP_BEHIND seconds behind the playhead. Never touch
    // the playhead's range; clamp the upper bound strictly below currentTime. Throttle
    // to ~2s steps so the per-frame ticker doesn't queue a sliver remove every frame.
    const evictEnd = this.currentPlaybackSeconds() - KEEP_BEHIND;
    if (evictEnd <= EPSILON || evictEnd <= this.lastEvictedTo + 2) return;
    if (!this.hasBufferedBefore(evictEnd)) return;
    this.lastEvictedTo = evictEnd;
    this.opQueue.push({
      kind: "remove",
      start: 0,
      end: evictEnd,
      // The green bar includes live buffered ranges, so refresh it once the
      // eviction lands (committed parts stay green; only uncommitted ones recede).
      settle: () => this.emitRanges(),
    });
    void this.pump();
  }

  // ---- playback ticker --------------------------------------------------------

  private async tryResolveReveal(): Promise<void> {
    const pending = this.pendingReveal;
    if (!pending || this.disposed || pending.fillGen !== this.fillGen) return;
    if (!this.isTimeBuffered(pending.target)) return;
    // Starting playback the instant the first fragment lands would pin the playhead
    // to the live edge (GOP-cadence stutter when encoding runs near realtime); wait
    // for a lead. Paused reveals (frame display) need no lead — show the frame now.
    if (this.mode === "playing" && !this.hasLead(pending.target, PLAYBACK_LEAD)) {
      return;
    }
    this.pendingReveal = null;
    await this.revealAt(pending.target, this.mode === "playing", pending.fillGen);
  }

  // Whether at least `lead` seconds are buffered contiguously past `t`, or the buffer
  // already reaches everything the timeline can produce (the tail lands a few frames
  // short of duration, hence the END_STALL_TAIL allowance).
  private hasLead(t: number, lead: number): boolean {
    const end = this.bufferedEndContaining(t);
    if (end - t >= lead) return true;
    return end >= this.config.duration - END_STALL_TAIL;
  }

  private async revealAt(
    target: number,
    startPlaying: boolean,
    gen: number,
  ): Promise<void> {
    this.video.currentTime = this.clampToBufferedStart(target);
    await this.waitForSettledAfterSeek();
    if (this.revealInvalidated(startPlaying, gen)) return;
    await this.waitForCurrentData();
    if (this.revealInvalidated(startPlaying, gen)) return;
    if (startPlaying) {
      try {
        await this.video.play();
      } catch (e) {
        if (this.disposed || gen !== this.fillGen) return;
        // pause() during the play() settle rejects it with AbortError — the user's
        // pause already owns the state, so stay quiet. Any other rejection
        // (autoplay policy etc.) means we are NOT playing: downshift so the UI
        // doesn't sit in a frozen "playing" state while the fill loop runs ahead.
        if (this.mode === "playing" && !isAbortError(e)) {
          this.mode = "priming";
          this.events.onPlaying?.(false);
          this.events.onError?.(String(e));
        }
        return;
      }
      if (this.revealInvalidated(startPlaying, gen)) {
        // pause() landed while play() was settling; undo the resume it would
        // otherwise override (zombie playback with a paused UI).
        this.video.pause();
        return;
      }
      this.startTicker(gen);
    }
    this.setDisplayMode("video", target);
  }

  // A reveal is stale once the player is disposed, the fill cursor moved on, or the
  // play/pause mode it was started for no longer matches (pause() or play() landed
  // between its awaits — the newer action owns the element and the display now).
  private revealInvalidated(startPlaying: boolean, gen: number): boolean {
    return (
      this.disposed ||
      gen !== this.fillGen ||
      startPlaying !== (this.mode === "playing")
    );
  }

  // Show the frame at `target` straight from the local cache while paused: when MSE
  // doesn't already hold it, append the IndexedDB segment bracketing it, then let the
  // pending reveal swap the <video> in. Falls back to the server-still path when the
  // committed range turns out to be unusable (row evicted / append refused).
  private async revealFromCache(target: number, gen: number): Promise<void> {
    try {
      if (!this.isTimeBuffered(target)) {
        const seg = await this.config.cache.segmentAt(this.config.groupKey, target);
        if (this.disposed || gen !== this.fillGen) return;
        if (seg && seg.end > target + EPSILON) {
          await this.appendSegment(seg.start, await blobToArrayBuffer(seg.blob), gen);
          if (this.disposed || gen !== this.fillGen) return;
          this.emitRanges();
        }
      }
    } catch {
      // fall through to the still fallback below
    }
    if (this.disposed || gen !== this.fillGen) return;
    if (this.isTimeBuffered(target)) {
      void this.tryResolveReveal();
    } else if (this.mode !== "playing") {
      // Mode check: play() pressed mid-load re-targets pendingReveal at the same
      // gen — clearing it here would strand playback waiting for a reveal that
      // never comes. When playing, the fill loop streams the gap and reveals.
      this.pendingReveal = null;
      this.setDisplayMode("still", target, "hold");
    }
  }

  private startTicker(gen: number): void {
    this.stopTicker();
    this.rebuffering = false;
    let lastTime = -1;
    let stalledTicks = 0;
    let settled = false;
    const tick = () => {
      if (this.disposed || gen !== this.fillGen || this.mode !== "playing") return;
      const t = this.video.currentTime;
      // Monotonic forward: near a seam the clock can momentarily read below where we
      // are, which would flash the playhead backward. Skip the backward reading but
      // keep the RAF running so forward progress and the end-clamp still apply.
      if (t >= this.position - EPSILON) {
        this.position = clamp(t, 0, this.config.duration);
        this.emitTime(this.position);
      }
      if (!settled) {
        stalledTicks = Math.abs(t - lastTime) < EPSILON ? stalledTicks + 1 : 0;
        lastTime = t;
        const bufferedEnd = this.bufferedEnd();
        if (
          stalledTicks >= END_STALL_TICKS &&
          t >= bufferedEnd - EPSILON &&
          bufferedEnd >= this.config.duration - END_STALL_TAIL
        ) {
          settled = true;
          this.settleAtEnd();
          return;
        }
        if (this.rebuffering) {
          if (this.hasLead(t, REBUFFER_LEAD)) {
            this.rebuffering = false;
            // Rejection = pause()/seek() landed first and owns the element now.
            void this.video.play().catch(() => {});
          }
        } else if (
          stalledTicks >= STARVE_TICKS &&
          !this.video.paused &&
          !this.isTimeBuffered(t + STARVE_PROBE)
        ) {
          // Starved at the live edge mid-clip: park and rebuild a lead instead of
          // resuming the moment one GOP fragment lands (which inches playback
          // forward 12-15 frames at a time). The element stays on its last frame
          // and mode stays "playing", so the UI keeps its playing state.
          this.rebuffering = true;
          this.video.pause();
        } else if (stalledTicks >= STALL_NUDGE_TICKS) {
          stalledTicks = 0;
          this.nudgeThroughStall(t);
        }
      }
      this.enqueueEviction();
      this.kickFill();
      this.rafId = requestAnimationFrame(tick);
    };
    this.rafId = requestAnimationFrame(tick);
  }

  private stopTicker(): void {
    if (this.rafId != null) {
      cancelAnimationFrame(this.rafId);
      this.rafId = null;
    }
  }

  // Un-stick a stalled playhead. Three cases:
  // - parked at a range end with the next range starting within a sub-frame seam gap
  //   (coded frame removal / float jitter at a segment boundary): jump over the gap —
  //   the element will not cross it on its own and would otherwise hang forever;
  // - parked with ample data ahead in its own range (decoder hiccup, or a seek that
  //   never settled): re-seek in place to re-prime the decoder;
  // - parked at the live edge genuinely waiting for data: leave it alone.
  private nudgeThroughStall(t: number): void {
    const sb = this.sourceBuffer;
    if (!sb || this.video.paused) return;
    // The stall is just buffering: the in-flight stream brackets the playhead, so its
    // data is en route — jumping now would skip real frames that are about to land.
    const inflight = this.inflight;
    if (inflight && inflight.start <= t + EPSILON && inflight.end > t + EPSILON) {
      return;
    }
    let containedEnd = t;
    let nextStart = Infinity;
    try {
      const buffered = sb.buffered;
      for (let i = 0; i < buffered.length; i++) {
        const start = buffered.start(i);
        const end = buffered.end(i);
        if (start <= t + EPSILON && end > t + EPSILON) {
          containedEnd = Math.max(containedEnd, end);
        } else if (start > t + EPSILON) {
          nextStart = Math.min(nextStart, start);
        }
      }
    } catch {
      return;
    }
    if (
      containedEnd - t <= STALL_GAP_JUMP_MAX &&
      nextStart - t <= STALL_GAP_JUMP_MAX
    ) {
      this.video.currentTime = nextStart;
    } else if (containedEnd - t > STALL_GAP_JUMP_MAX) {
      this.video.currentTime = t;
    }
  }

  private settleAtEnd(): void {
    if (this.disposed || this.mode !== "playing") return;
    this.stopTicker();
    this.rebuffering = false;
    try {
      this.video.pause();
    } catch {
      // ignore
    }
    this.mode = "idle";
    this.position = this.config.duration;
    this.emitTime(this.config.duration);
    this.events.onPlaying?.(false);
    this.events.onEnded?.();
    // The timeline is half-open [0,duration); the last representable frame is at
    // duration-EPSILON (exact =duration returns 500). The video is parked on its final
    // frame, so hold it until the fresh end still decodes rather than flashing a stale one.
    this.setDisplayMode(
      "still",
      Math.max(0, this.config.duration - EPSILON),
      "hold",
    );
  }

  private endOfStreamIfOpen(): void {
    if (this.mediaSource.readyState === "open") {
      try {
        this.mediaSource.endOfStream();
      } catch {
        // ignore
      }
    }
  }

  // ---- helpers ----------------------------------------------------------------

  private currentPlaybackSeconds(): number {
    // video.currentTime is authoritative ONLY once the playhead is actually revealed and
    // running at the logical position. While a reveal is pending (e.g. a seek-while-playing
    // into a cold region, where the element is paused/covered and currentTime still sits at
    // the OLD position) the logical `position` is the truth — otherwise pausing or eviction
    // would snap back to the stale pre-seek time.
    if (this.mode === "playing" && this.pendingReveal == null) {
      return clamp(this.video.currentTime, 0, this.config.duration);
    }
    return this.position;
  }

  private emitTime(seconds: number): void {
    this.events.onTime?.(seconds);
  }

  private setDisplayMode(
    mode: DisplayMode,
    stillTime: number,
    cover: StillCover = "trailing",
  ): void {
    this.events.onDisplayMode?.(mode, stillTime, cover);
  }

  private emitRanges(): void {
    if (this.disposed) return;
    // The green bar = everything playable without re-fetching from the server:
    // IndexedDB-committed ranges (never shrink under MSE eviction) and whatever
    // currently sits in the MSE buffer. The live buffer matters twice: an aborted
    // partial stream stays playable even though it was never committed, and when
    // IndexedDB writes fail the buffer is the only cache there is.
    const ranges = [...this.committedRanges, ...this.bufferedRanges()];
    this.events.onRanges?.(mergeRanges(ranges));
  }

  private bufferedRanges(): CacheRange[] {
    const sb = this.sourceBuffer;
    if (!sb) return [];
    try {
      const buffered = sb.buffered;
      const ranges: CacheRange[] = [];
      for (let i = 0; i < buffered.length; i++) {
        ranges.push({ start: buffered.start(i), end: buffered.end(i) });
      }
      return ranges;
    } catch {
      // buffered throws once the SourceBuffer is detached (teardown).
      return [];
    }
  }

  // The end of the buffered range containing t (consult LIVE buffered — the UA may have
  // evicted a segment we committed to IndexedDB), or t if t isn't buffered.
  private bufferedEndContaining(t: number): number {
    const sb = this.sourceBuffer;
    if (!sb) return t;
    const buffered = sb.buffered;
    for (let i = 0; i < buffered.length; i++) {
      if (buffered.start(i) <= t + EPSILON && buffered.end(i) > t + EPSILON) {
        return buffered.end(i);
      }
    }
    return t;
  }

  private isTimeBuffered(t: number): boolean {
    const sb = this.sourceBuffer;
    if (!sb) return false;
    const buffered = sb.buffered;
    for (let i = 0; i < buffered.length; i++) {
      if (buffered.start(i) <= t + EPSILON && buffered.end(i) > t + EPSILON) {
        return true;
      }
    }
    return false;
  }

  // Clamp a seek target UP to the true start of the bracketing buffered range: the
  // append's first sample can land a hair after the requested time, and seeking into
  // that sub-EPSILON gap hangs the seek (seeking=true forever) on some browsers.
  private clampToBufferedStart(t: number): number {
    const sb = this.sourceBuffer;
    if (!sb) return t;
    const buffered = sb.buffered;
    for (let i = 0; i < buffered.length; i++) {
      const start = buffered.start(i);
      const end = buffered.end(i);
      if (start <= t + EPSILON && end > t + EPSILON) {
        return Math.max(t, start);
      }
    }
    return t;
  }

  // The buffered end of the in-flight segment [start,end] — clamped to its nominal end
  // and scoped to ranges overlapping that segment. The segment start may already be
  // evicted behind the playhead before EOF, so this must not require a range containing
  // `start`.
  private liveEndOf(start: number, end: number): number {
    const sb = this.sourceBuffer;
    if (!sb) return start;
    const buffered = sb.buffered;
    let liveEnd = start;
    for (let i = 0; i < buffered.length; i++) {
      if (buffered.end(i) > start + EPSILON && buffered.start(i) < end - EPSILON) {
        liveEnd = Math.max(liveEnd, Math.min(end, buffered.end(i)));
      }
    }
    return liveEnd;
  }

  private bufferedEnd(): number {
    const sb = this.sourceBuffer;
    if (!sb || sb.buffered.length === 0) return 0;
    return sb.buffered.end(sb.buffered.length - 1);
  }

  private hasBufferedBefore(t: number): boolean {
    const sb = this.sourceBuffer;
    if (!sb) return false;
    const buffered = sb.buffered;
    for (let i = 0; i < buffered.length; i++) {
      if (buffered.start(i) < t - EPSILON) return true;
    }
    return false;
  }

  private abortInflight(): void {
    const inflight = this.inflight;
    if (!inflight) return;
    this.inflight = null;
    // Aborting a fetch can leave the SourceBuffer parser holding a partial MP4 box
    // from the last accepted chunk. Reset the parser before the next stream's init
    // segment is appended, while keeping already-buffered complete frames intact.
    this.enqueueParserReset();
    try {
      inflight.controller.abort();
    } catch {
      // ignore
    }
  }

  private waitForSettledAfterSeek(): Promise<void> {
    const video = this.video;
    if (!video.seeking && video.readyState >= HTMLMediaElement.HAVE_CURRENT_DATA) {
      return Promise.resolve();
    }
    return new Promise((resolve) => {
      const finish = () => {
        video.removeEventListener("seeked", onSeeked);
        video.removeEventListener("canplay", onReady);
        video.removeEventListener("loadeddata", onReady);
        clearTimeout(timer);
        resolve();
      };
      // `seeked` is authoritative: the seek to the target frame is done. canplay/loadeddata
      // can fire for a CONCURRENT fill append while the seek is still in progress — resolving
      // on those would let revealAt unhide the video on the PRE-seek frame, so accept them
      // only once the seek itself has settled. The timeout is the last resort for the (Arc)
      // edge where `seeked` never fires at a buffer boundary.
      const onSeeked = () => finish();
      const onReady = () => {
        if (!video.seeking) finish();
      };
      const timer = setTimeout(finish, SETTLE_TIMEOUT_MS);
      video.addEventListener("seeked", onSeeked);
      video.addEventListener("canplay", onReady);
      video.addEventListener("loadeddata", onReady);
    });
  }

  private waitForCurrentData(): Promise<void> {
    const video = this.video;
    if (video.readyState >= HTMLMediaElement.HAVE_CURRENT_DATA) {
      return Promise.resolve();
    }
    return new Promise((resolve) => {
      const finish = () => {
        clearTimeout(timer);
        video.removeEventListener("loadeddata", finish);
        video.removeEventListener("canplay", finish);
        resolve();
      };
      // Bound the wait: a stuck seek can leave the element at HAVE_METADATA forever.
      const timer = setTimeout(finish, SETTLE_TIMEOUT_MS);
      video.addEventListener("loadeddata", finish, { once: true });
      video.addEventListener("canplay", finish, { once: true });
    });
  }
}

// ---- module helpers -----------------------------------------------------------

function clamp(value: number, lo: number, hi: number): number {
  if (!Number.isFinite(value)) return lo;
  return Math.max(lo, Math.min(value, hi));
}

function selectMimeType(): string {
  for (const mimeType of MP4_MIME_TYPES) {
    if (MediaSource.isTypeSupported(mimeType)) return mimeType;
  }
  throw new Error("MP4 preview streams with FLAC audio are not supported");
}

function waitForSourceOpen(mediaSource: MediaSource): Promise<void> {
  if (mediaSource.readyState === "open") return Promise.resolve();
  return new Promise((resolve, reject) => {
    const onOpen = () => {
      cleanup();
      resolve();
    };
    const onError = () => {
      cleanup();
      reject(new Error("MediaSource failed to open"));
    };
    const cleanup = () => {
      mediaSource.removeEventListener("sourceopen", onOpen);
      mediaSource.removeEventListener("sourceended", onError);
      mediaSource.removeEventListener("sourceclose", onError);
    };
    mediaSource.addEventListener("sourceopen", onOpen);
    mediaSource.addEventListener("sourceended", onError);
    mediaSource.addEventListener("sourceclose", onError);
  });
}

function waitForSourceBufferIdle(sourceBuffer: SourceBuffer): Promise<void> {
  if (!sourceBuffer.updating) return Promise.resolve();
  return new Promise<void>((resolve) => {
    const done = () => {
      sourceBuffer.removeEventListener("updateend", done);
      clearTimeout(timer);
      resolve();
    };
    // Teardown guard: a SourceBuffer detached during dispose never fires updateend, which
    // would otherwise hang the pump (and the streamSegment append it backs) forever.
    const timer = setTimeout(done, SETTLE_TIMEOUT_MS);
    sourceBuffer.addEventListener("updateend", done);
  });
}

function blobToArrayBuffer(blob: Blob): Promise<ArrayBuffer> {
  return blob.arrayBuffer();
}

function mergeRanges(ranges: CacheRange[]): CacheRange[] {
  const sorted = ranges
    .filter((r) => r.end > r.start + EPSILON)
    .sort((a, b) => a.start - b.start);
  const merged: CacheRange[] = [];
  for (const range of sorted) {
    const last = merged[merged.length - 1];
    if (last && range.start <= last.end + EPSILON) {
      last.end = Math.max(last.end, range.end);
    } else {
      merged.push({ ...range });
    }
  }
  return merged;
}

function isAbortError(e: unknown): boolean {
  return e instanceof DOMException && e.name === "AbortError";
}

function isTransientNetworkError(e: unknown): boolean {
  if (e instanceof TypeError) return true;
  const message = e instanceof Error ? e.message : String(e);
  if (/Failed to fetch|NetworkError|Load failed/i.test(message)) return true;
  const statusMatch = /failed:\s*(\d{3})\b/.exec(message);
  if (statusMatch) {
    const status = Number(statusMatch[1]);
    return status >= 500 && status <= 599;
  }
  return false;
}

class SourceBufferAppendError extends Error {
  constructor() {
    super("SourceBuffer append failed");
    this.name = "SourceBufferAppendError";
  }
}

function isSourceBufferAppendError(e: unknown): boolean {
  return e instanceof SourceBufferAppendError;
}

function isQuotaExceeded(e: unknown): boolean {
  return (
    e instanceof DOMException &&
    (e.name === "QuotaExceededError" || e.code === 22)
  );
}

function isInvalidState(e: unknown): boolean {
  return e instanceof DOMException && e.name === "InvalidStateError";
}
