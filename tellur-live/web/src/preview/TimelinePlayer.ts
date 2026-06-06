import type { CacheRange } from "../types";
import {
  CHUNK_SECONDS,
  chunkCount,
  chunkIndexAt,
  chunkRange,
  indicesToRanges,
  type MediaCache,
} from "../mediaCache";

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
// Paused scrubbing fires many seeks; defer the actual chunk FETCH until the playhead
// has been quiet this long so a drag issues ~1 fetch at its resting point, not 120.
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

// A/V-with-FLAC variants FIRST: the preview stream ALWAYS carries a FLAC audio track
// (silent when the timeline has no audio), so the SourceBuffer must declare the audio
// codec or appended segments fail to parse. FLAC (not AAC) because AAC's per-encode
// priming left an audible click at every fresh-encode cache seam; FLAC has zero
// encoder delay so segments concatenate gaplessly. The three avc1 profiles cover
// browsers that support different H.264 levels; video-only and bare fallbacks last.
const MP4_MIME_TYPES = [
  'video/mp4; codecs="avc1.42E01E, flac"',
  'video/mp4; codecs="avc1.4D401E, flac"',
  'video/mp4; codecs="avc1.64001E, flac"',
  'video/mp4; codecs="avc1.42E01E"',
  'video/mp4; codecs="avc1.4D401E"',
  'video/mp4; codecs="avc1.64001E"',
  "video/mp4",
];

export type DisplayMode = "video" | "still";

export interface TimelinePlayerEvents {
  onTime?: (seconds: number) => void;
  onRanges?: (ranges: CacheRange[]) => void;
  onPlaying?: (playing: boolean) => void;
  onDisplayMode?: (mode: DisplayMode, stillTime: number) => void;
  onEnded?: () => void;
  onError?: (message: string | null) => void;
}

export interface TimelinePlayerConfig {
  groupKey: string;
  pluginKey: string;
  duration: number;
  initialPosition: number;
  // Builds the /api/video.mp4 URL for the half-open chunk [start, end). When
  // end >= duration the caller should omit the `duration` param (stream to the tail);
  // this closure handles that.
  videoUrl: (start: number, end: number) => string;
  cache: MediaCache;
}

type PlayMode = "idle" | "priming" | "playing";

interface InFlight {
  index: number;
  controller: AbortController;
  fillGen: number;
  received: ArrayBuffer[];
  liveEnd: number;
}

interface AppendJob {
  index: number;
  data: ArrayBuffer;
  fillGen: number;
}

interface RemoveJob {
  start: number;
  end: number;
}

type QueuedOp =
  | ({ kind: "append" } & AppendJob & { settle: () => void })
  | ({ kind: "remove" } & RemoveJob & { settle: () => void });

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

  // Committed cache = the set of chunk indices durably stored in IndexedDB. This — NOT
  // sourceBuffer.buffered — is the persistent green bar, so UA/eviction never shrinks it.
  private readonly committed = new Set<number>();
  private inflight: InFlight | null = null;

  // Serialized op queue: the ONLY code that mutates the SourceBuffer.
  private readonly opQueue: QueuedOp[] = [];
  private pumping = false;
  private lastAppliedIndex = -1;

  private fillActiveGen = -1;
  private lastEvictedTo = 0;
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
    void this.loadCommitted();
    // Cover the element with the paused still immediately; prime ahead from the start
    // so play() is instant (requirement 1).
    this.setDisplayMode("still", this.position);
    this.emitTime(this.position);
    void this.ready.then(() => {
      if (!this.disposed) this.kickFill();
    });
  }

  // ---- public API -------------------------------------------------------------

  get currentTime(): number {
    return this.currentPlaybackSeconds();
  }

  seek(seconds: number): void {
    if (this.disposed) return;
    const target = clamp(seconds, 0, this.config.duration);
    this.position = target;
    this.fillGen++;
    this.lastEvictedTo = 0;
    this.abortInflight();
    this.clearSeekDebounce();
    this.emitTime(target);

    if (this.mode === "playing") {
      // Keep playing through the seek. If the target is already buffered, reposition
      // instantly; else cover with the still and reveal once its chunk arrives.
      if (this.isTimeBuffered(target) && !this.video.paused) {
        this.pendingReveal = null;
        this.video.currentTime = this.clampToBufferedStart(target);
        this.setDisplayMode("video", target);
        this.kickFill();
      } else {
        this.setDisplayMode("still", target);
        this.pendingReveal = { target, fillGen: this.fillGen };
        this.kickFill();
      }
    } else {
      // Paused: show the crisp still and prime ahead. The fetch is debounced so a drag
      // doesn't storm the server; the display already updated above.
      this.mode = "priming";
      this.pendingReveal = null;
      this.setDisplayMode("still", target);
      this.scheduleDebouncedFill();
    }
  }

  // Must be called inside the user gesture (it unmutes for the autoplay policy).
  play(): void {
    if (this.disposed) return;
    // Unmute synchronously within the gesture so the (now A/V) stream plays with sound.
    // React does not re-apply the `muted` attribute after mount, so set the property.
    this.video.muted = false;
    // Play-from-the-end replays from the start: at duration the cursor loop has no work
    // and playback would silently never begin.
    if (this.position >= this.config.duration - EPSILON) {
      this.position = 0;
      this.emitTime(0);
    }
    this.fillGen++;
    this.lastEvictedTo = 0;
    this.abortInflight();
    this.clearSeekDebounce();
    this.mode = "playing";
    this.events.onPlaying?.(true);
    this.pendingReveal = { target: this.position, fillGen: this.fillGen };
    this.setDisplayMode("still", this.position);
    this.tryResolveReveal();
    this.kickFill();
  }

  pause(): void {
    if (this.disposed) return;
    this.position = this.currentPlaybackSeconds();
    this.fillGen++;
    this.video.pause();
    this.stopTicker();
    this.mode = "priming";
    this.pendingReveal = null;
    this.events.onPlaying?.(false);
    this.emitTime(this.position);
    this.setDisplayMode("still", this.position);
    // Keep the cursor where it is so already-fetched-ahead work is retained (req 7).
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
    for (const op of this.opQueue.splice(0)) op.settle();
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
    }
  }

  private async loadCommitted(): Promise<void> {
    const indices = await this.config.cache.cachedIndices(this.config.groupKey);
    if (this.disposed) return;
    for (const i of indices) this.committed.add(i);
    this.emitRanges();
  }

  // ---- fill loop --------------------------------------------------------------

  private kickFill(): void {
    if (this.disposed) return;
    if (!this.sourceBuffer) {
      void this.ready.then(() => {
        if (!this.disposed) this.kickFill();
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
    const { duration } = this.config;
    const total = chunkCount(duration);
    while (!this.disposed && gen === this.fillGen) {
      const lookahead =
        this.mode === "playing"
          ? PLAY_AHEAD
          : this.mode === "priming"
            ? PRIME_AHEAD
            : 0;
      if (lookahead <= 0) return;

      const from = chunkIndexAt(this.position);
      const targetTime = Math.min(duration, this.position + lookahead);
      const to = chunkIndexAt(Math.max(0, targetTime - EPSILON));

      let next = -1;
      for (let i = from; i <= to && i < total; i++) {
        if (!this.isChunkBuffered(i)) {
          next = i;
          break;
        }
      }
      if (next < 0) {
        // The whole lookahead window is buffered. If we're playing and the window
        // reaches the timeline end, finalize so native `ended` can fire.
        if (this.mode === "playing" && to >= total - 1) {
          this.endOfStreamIfOpen();
        }
        return;
      }

      const blob = await this.config.cache.getChunk(this.config.groupKey, next);
      if (this.disposed || gen !== this.fillGen) return;
      if (blob) {
        await this.appendChunk(next, await blobToArrayBuffer(blob), gen);
        if (this.disposed || gen !== this.fillGen) return;
        // If the append could not land (buffer full and nothing is evictable behind a
        // paused playhead), stop filling instead of re-trying the same chunk in a tight
        // loop. The chunk stays green (it IS in IndexedDB) and re-appends from cache
        // once the playhead advances and eviction frees room.
        if (!this.isChunkBuffered(next)) return;
        this.committed.add(next);
        this.emitRanges();
        this.tryResolveReveal();
      } else {
        await this.streamChunk(next, gen);
        if (this.disposed || gen !== this.fillGen) return;
      }
    }
  }

  private async streamChunk(index: number, gen: number): Promise<void> {
    const { start, end } = chunkRange(index, this.config.duration);
    const controller = new AbortController();
    const inflight: InFlight = {
      index,
      controller,
      fillGen: gen,
      received: [],
      liveEnd: start,
    };
    this.inflight = inflight;
    const url = this.config.videoUrl(start, end);
    try {
      const response = await fetch(url, { cache: "no-store", signal: controller.signal });
      if (this.disposed || gen !== this.fillGen) return;
      if (!response.ok) throw new Error(`${url} failed: ${response.status}`);
      const reader = response.body?.getReader();
      if (!reader) throw new Error("video stream has no body");
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        if (this.disposed || gen !== this.fillGen) return;
        if (!value) continue;
        // Copy exactly the view's region; the reader reuses one backing buffer, so
        // appending value.buffer directly would splice in bytes from other reads.
        const slice = value.buffer.slice(
          value.byteOffset,
          value.byteOffset + value.byteLength,
        ) as ArrayBuffer;
        inflight.received.push(slice);
        await this.appendChunk(index, slice.slice(0), gen);
        if (this.disposed || gen !== this.fillGen) return;
        // The live green edge is the buffered end of THIS chunk only — never bytes
        // received (which over-report before parse) and never the global buffered end
        // (contaminated by lookahead / eviction). buffered advances per GOP fragment.
        inflight.liveEnd = this.liveEndOf(index);
        this.emitRanges();
        this.tryResolveReveal();
      }
      // Natural EOF only: persist the WHOLE chunk. A partial (aborted) fetch is never
      // persisted — a truncated fMP4 isn't independently decodable and would poison
      // the cache. The accumulated bytes are byte-identical to what played, so the
      // cached blob == the stream (requirement 7, no waste on a completed chunk).
      const blob = new Blob(inflight.received, { type: "video/mp4" });
      const persisted = await this.config.cache.putChunk(
        this.config.groupKey,
        this.config.pluginKey,
        index,
        blob,
      );
      if (this.disposed) return;
      if (this.inflight === inflight) this.inflight = null;
      if (persisted) this.committed.add(index);
      this.emitRanges();
    } catch (e) {
      if (this.inflight === inflight) this.inflight = null;
      if (isAbortError(e)) {
        // Intentional interruption (seek/pause/dispose): drop the partial, recede the
        // live edge to the last committed boundary.
        this.emitRanges();
        return;
      }
      if (this.disposed || gen !== this.fillGen) return;
      this.events.onError?.(String(e));
    }
  }

  // Enqueue an append for a chunk slice and resolve once it has drained (backpressure
  // so the reader can't outrun the SourceBuffer). The op carries fillGen so a stale
  // cursor's slices are dropped by the pump rather than polluting buffered ranges.
  private appendChunk(index: number, data: ArrayBuffer, fillGen: number): Promise<void> {
    return new Promise<void>((resolve) => {
      this.opQueue.push({ kind: "append", index, data, fillGen, settle: resolve });
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
          op.settle(); // stale cursor — drop, but settle so the awaiter continues
          continue;
        }
        try {
          if (op.kind === "append") {
            await this.appendWithQuota(sb, op.index, op.data);
          } else {
            await this.removeRange(sb, op.start, op.end);
          }
        } catch (e) {
          if (isInvalidState(e)) {
            // MediaSource detached mid-op (teardown) — stop draining.
            op.settle();
            break;
          }
          if (
            op.kind === "append" &&
            !this.disposed &&
            op.fillGen === this.fillGen &&
            !isAbortError(e)
          ) {
            this.events.onError?.(String(e));
          }
        }
        op.settle();
      }
    } finally {
      this.pumping = false;
    }
  }

  private async appendWithQuota(
    sb: SourceBuffer,
    index: number,
    data: ArrayBuffer,
  ): Promise<void> {
    for (let attempt = 0; ; attempt++) {
      try {
        await this.appendOne(sb, index, data);
        return;
      } catch (e) {
        // QuotaExceededError is thrown synchronously when the buffer is full. Evict the
        // oldest data well behind the playhead and retry; this is the normal path during
        // long playback, not an edge case.
        if (isQuotaExceeded(e) && attempt < 4) {
          const evictEnd = this.currentPlaybackSeconds() - KEEP_BEHIND;
          if (evictEnd > EPSILON && this.hasBufferedBefore(evictEnd)) {
            try {
              await this.removeRange(sb, 0, evictEnd);
              continue;
            } catch {
              // fall through to give up
            }
          }
          // Nothing evictable (everything is near the playhead): give up this append;
          // it will be retried as the playhead advances and frees room.
          return;
        }
        throw e;
      }
    }
  }

  private async appendOne(
    sb: SourceBuffer,
    index: number,
    data: ArrayBuffer,
  ): Promise<void> {
    await waitForSourceBufferIdle(sb);
    // "ended" is fine here — setting timestampOffset / appendBuffer transitions the
    // MediaSource back to "open". Bail only when it's "closed" (detached on teardown).
    if (this.disposed || this.mediaSource.readyState === "closed") return;
    if (index !== this.lastAppliedIndex) {
      // Position this chunk so buffered-time == timeline-time. Each chunk is a
      // self-contained encode with its own IDR and an exact integer number of FLAC
      // packets, so leaving appendWindow at its defaults (open) butts adjacent chunks
      // up sample-exact with no overlap and no dropped boundary frame.
      sb.timestampOffset = index * CHUNK_SECONDS;
      this.lastAppliedIndex = index;
    }
    await new Promise<void>((resolve, reject) => {
      const onEnd = () => {
        cleanup();
        resolve();
      };
      const onErr = () => {
        cleanup();
        reject(new Error("SourceBuffer append failed"));
      };
      const cleanup = () => {
        sb.removeEventListener("updateend", onEnd);
        sb.removeEventListener("error", onErr);
      };
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

  private async removeRange(sb: SourceBuffer, start: number, end: number): Promise<void> {
    if (!(end > start + EPSILON)) return;
    await waitForSourceBufferIdle(sb);
    if (this.disposed || this.mediaSource.readyState !== "open") return;
    await new Promise<void>((resolve) => {
      const onEnd = () => {
        sb.removeEventListener("updateend", onEnd);
        resolve();
      };
      sb.addEventListener("updateend", onEnd);
      try {
        sb.remove(start, end);
      } catch {
        sb.removeEventListener("updateend", onEnd);
        resolve();
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
      settle: () => {},
    });
    void this.pump();
  }

  // ---- playback ticker --------------------------------------------------------

  private async tryResolveReveal(): Promise<void> {
    const pending = this.pendingReveal;
    if (!pending || this.disposed || pending.fillGen !== this.fillGen) return;
    if (!this.isTimeBuffered(pending.target)) return;
    this.pendingReveal = null;
    await this.revealAt(pending.target, this.mode === "playing", pending.fillGen);
  }

  private async revealAt(
    target: number,
    startPlaying: boolean,
    gen: number,
  ): Promise<void> {
    this.video.currentTime = this.clampToBufferedStart(target);
    await this.waitForSettledAfterSeek();
    if (this.disposed || gen !== this.fillGen) return;
    await this.waitForCurrentData();
    if (this.disposed || gen !== this.fillGen) return;
    if (startPlaying) {
      try {
        await this.video.play();
      } catch (e) {
        if (!this.disposed && gen === this.fillGen) this.events.onError?.(String(e));
        return;
      }
      if (this.disposed || gen !== this.fillGen) return;
      this.startTicker(gen);
    }
    this.setDisplayMode("video", target);
  }

  private startTicker(gen: number): void {
    this.stopTicker();
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

  private settleAtEnd(): void {
    if (this.disposed || this.mode !== "playing") return;
    this.stopTicker();
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
    // duration-EPSILON (exact =duration returns 500).
    this.setDisplayMode("still", Math.max(0, this.config.duration - EPSILON));
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
    if (this.mode === "playing") {
      return clamp(this.video.currentTime, 0, this.config.duration);
    }
    return this.position;
  }

  private emitTime(seconds: number): void {
    this.events.onTime?.(seconds);
  }

  private setDisplayMode(mode: DisplayMode, stillTime: number): void {
    this.events.onDisplayMode?.(mode, stillTime);
  }

  private emitRanges(): void {
    const ranges = indicesToRanges(this.committed, this.config.duration);
    const inflight = this.inflight;
    if (inflight) {
      const { start } = chunkRange(inflight.index, this.config.duration);
      if (inflight.liveEnd > start + EPSILON) {
        ranges.push({ start, end: inflight.liveEnd });
      }
    }
    this.events.onRanges?.(mergeRanges(ranges));
  }

  // Whether the chunk's full range is currently present in MSE (consult LIVE buffered,
  // because the UA may have evicted a chunk we committed to IndexedDB).
  private isChunkBuffered(index: number): boolean {
    const { start, end } = chunkRange(index, this.config.duration);
    return this.isRangeBuffered(start, end);
  }

  private isRangeBuffered(start: number, end: number): boolean {
    const sb = this.sourceBuffer;
    if (!sb) return false;
    const buffered = sb.buffered;
    for (let i = 0; i < buffered.length; i++) {
      if (
        buffered.start(i) <= start + EPSILON &&
        buffered.end(i) >= end - EPSILON
      ) {
        return true;
      }
    }
    return false;
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

  private liveEndOf(index: number): number {
    const { start, end } = chunkRange(index, this.config.duration);
    const sb = this.sourceBuffer;
    if (!sb) return start;
    const buffered = sb.buffered;
    for (let i = 0; i < buffered.length; i++) {
      if (buffered.start(i) <= start + EPSILON && buffered.end(i) > start + EPSILON) {
        return Math.min(end, buffered.end(i));
      }
    }
    return start;
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
        video.removeEventListener("seeked", finish);
        video.removeEventListener("canplay", finish);
        video.removeEventListener("loadeddata", finish);
        clearTimeout(timer);
        resolve();
      };
      const timer = setTimeout(finish, SETTLE_TIMEOUT_MS);
      video.addEventListener("seeked", finish, { once: true });
      video.addEventListener("canplay", finish, { once: true });
      video.addEventListener("loadeddata", finish, { once: true });
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
  return "video/mp4";
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
    sourceBuffer.addEventListener("updateend", () => resolve(), { once: true });
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

function isQuotaExceeded(e: unknown): boolean {
  return (
    e instanceof DOMException &&
    (e.name === "QuotaExceededError" || e.code === 22)
  );
}

function isInvalidState(e: unknown): boolean {
  return e instanceof DOMException && e.name === "InvalidStateError";
}
