import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { RefObject } from "react";
import { frameUrl, videoUrl } from "../api";
import {
  cacheScopeKey,
  getCachedVideoRangeBlob,
  getCachedVideoRange,
  getNextCachedVideoRange,
  loadCacheRanges,
  mergeCacheRange,
  putVideoRangeBlob,
  revokeStaleCacheRanges,
  revokeStaleMediaCacheEntries,
  saveCacheRanges,
} from "../cache";
import type { CachedVideoRangeBlob } from "../cache";
import type {
  CacheRange,
  PreviewResolution,
  ServerInfo,
  TimelineInfo,
} from "../types";

const EPSILON = 0.001;
const PRELOAD_DELAY_MS = 260;
const STOPPED_STREAM_CACHE_SECONDS = 3;
// End-of-playback stall detection (see the tickers): only treat a buffer-end
// stall as "reached the end" when the buffered end is within this window of the
// timeline duration, so a mid-clip buffering wait never snaps to the end.
const END_STALL_TAIL_SECONDS = 0.5;
// Consecutive RAF ticks with no forward progress (stuck at the buffer end)
// required before we conclude playback has ended.
const END_STALL_TICKS = 6;
const DEBUG_STORAGE_KEY = "tellur-live:debug";
const MP4_MIME_TYPES = [
  // A/V variants first: the preview stream always carries a FLAC audio track
  // (silent when the timeline has no audio), so the SourceBuffer must declare
  // the audio codec or appended segments with an audio track fail to parse.
  // FLAC is used instead of AAC because it has zero encoder delay, so the audio
  // concatenates gaplessly across cache-segment boundaries — AAC's per-encode
  // priming left an audible click at each seam.
  'video/mp4; codecs="avc1.42E01E, flac"',
  'video/mp4; codecs="avc1.4D401E, flac"',
  'video/mp4; codecs="avc1.64001E, flac"',
  // Video-only fallbacks for environments without FLAC-in-MP4 support.
  'video/mp4; codecs="avc1.42E01E"',
  'video/mp4; codecs="avc1.4D401E"',
  'video/mp4; codecs="avc1.64001E"',
  "video/mp4",
];

type VideoSlot = 0 | 1;
type StreamingMode = "stopped" | "playback";
type StreamCacheOwner =
  | { kind: "session"; id: number }
  | { kind: "pipeline"; id: number };

interface PlaybackState {
  kind: "cache" | "stream" | "pipeline";
  slot: VideoSlot;
  start: number;
}

interface PlaybackPipeline {
  id: number;
  token: number;
  slot: VideoSlot;
  start: number;
  group: string;
  cacheKey: string;
  mediaSource: MediaSource;
  controller: AbortController;
  sourceBuffer: SourceBuffer | null;
  appendChain: Promise<void>;
  started: boolean;
  starting: Promise<void> | null;
  saveOnAbort: boolean;
  liveSegment: LivePipelineSegment | null;
}

interface LivePipelineSegment {
  start: number;
  end: number;
  url: string;
  chunks: ArrayBuffer[];
  bufferedEnd: number;
  saved: boolean;
}

interface StreamingSession {
  id: number;
  token: number;
  mode: StreamingMode;
  slot: VideoSlot;
  start: number;
  end: number;
  stoppedTargetEnd: number | null;
  url: string;
  cacheKey: string;
  group: string;
  mediaSource: MediaSource;
  controller: AbortController;
  chunks: ArrayBuffer[];
  sourceBuffer: SourceBuffer | null;
  appendChain: Promise<void>;
  opened: Promise<void>;
  bufferedSeconds: number;
  saveOnFinalize: boolean;
  clearOnFinalize: boolean;
}

export interface PreviewState {
  seconds: number;
  playing: boolean;
  cacheRanges: CacheRange[];
  error: string | null;
  imageSrc: string | null;
  imageVisible: boolean;
  videoVisible: boolean;
  activeVideoSlot: VideoSlot;
}

export interface PreviewControls {
  state: PreviewState;
  videoRefs: [RefObject<HTMLVideoElement>, RefObject<HTMLVideoElement>];
  videoRef: RefObject<HTMLVideoElement>;
  imgRef: RefObject<HTMLImageElement>;
  setSeconds: (s: number) => void;
  togglePlay: () => void;
  stepFrame: (delta: number) => void;
  rewindToStart: () => void;
}

export interface PreviewSettings {
  info: ServerInfo | null;
  timeline: TimelineInfo | null;
  resolution: PreviewResolution;
  fps: number;
}

export function usePreview(settings: PreviewSettings): PreviewControls {
  const { info, timeline, resolution, fps } = settings;
  const hasServerInfo = info != null;
  const timelineId = timeline?.id ?? "";
  const timelineDuration = timeline?.duration ?? 0;
  const pluginCacheKey = info?.cacheKey ?? "";

  const videoARef = useRef<HTMLVideoElement>(null);
  const videoBRef = useRef<HTMLVideoElement>(null);
  const imgRef = useRef<HTMLImageElement>(null);

  const [seconds, setSecondsState] = useState(0);
  const [playing, setPlaying] = useState(false);
  const [cacheRanges, setCacheRanges] = useState<CacheRange[]>([]);
  const [streamCacheRanges, setStreamCacheRanges] = useState<CacheRange[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [imageSrc, setImageSrc] = useState<string | null>(null);
  const [imageVisible, setImageVisible] = useState(true);
  const [videoVisible, setVideoVisible] = useState(false);
  const [activeVideoSlot, setActiveVideoSlotState] = useState<VideoSlot>(0);

  const secondsRef = useRef(0);
  const playingRef = useRef(false);
  const displayTokenRef = useRef(0);
  const pngTokenRef = useRef(0);
  const pendingPngRef = useRef(false);
  const pendingPngUrlRef = useRef<string | null>(null);
  const queuedPngRef = useRef(false);
  const preloadTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const playbackTokenRef = useRef(0);
  const rafRef = useRef<number | null>(null);
  // Handle for the deferred `startVideoPlayback` rAF scheduled in `togglePlay`.
  // Tracked separately from `rafRef` (which only holds the playback tickers) so a
  // pause/seek landing inside the one-frame window before it fires can cancel it
  // — otherwise the start would run after the user already paused (phantom
  // playback) and bump `displayTokenRef`, discarding the pause-edge PNG.
  const playStartRafRef = useRef<number | null>(null);
  const cacheScopeRef = useRef("");
  const lastCacheKeyRef = useRef("");
  const immediatePreloadRef = useRef(false);
  const imageObjectUrlRef = useRef<string | null>(null);
  const videoObjectUrlsRef = useRef<[string | null, string | null]>([
    null,
    null,
  ]);
  const activeVideoSlotRef = useRef<VideoSlot>(0);
  const heldVideoSlotRef = useRef<VideoSlot | null>(null);
  const playbackRef = useRef<PlaybackState | null>(null);
  const streamingSessionRef = useRef<StreamingSession | null>(null);
  const playbackPipelineRef = useRef<PlaybackPipeline | null>(null);
  const streamCacheOwnerRef = useRef<StreamCacheOwner | null>(null);
  const streamIdRef = useRef(0);
  const pipelineIdRef = useRef(0);
  const startStoppedStreamRef = useRef<
    (fromSeconds: number, targetEnd?: number) => void
  >(() => {});

  const resolvedResolution = useCallback(() => {
    return {
      width: Math.max(1, Math.round(resolution.width)),
      height: Math.max(1, Math.round(resolution.height)),
    };
  }, [resolution.height, resolution.width]);

  const videoGop = useCallback(() => Math.max(1, Math.floor(fps / 4)), [fps]);

  const currentCacheScope = useCallback((): string => {
    if (!timelineId || !pluginCacheKey) return "";
    const r = resolvedResolution();
    return cacheScopeKey({
      cacheKey: pluginCacheKey,
      timelineId,
      width: r.width,
      height: r.height,
      fps,
      gop: videoGop(),
      crf: 23,
      videoSegmentSeconds: 0,
    });
  }, [timelineId, pluginCacheKey, resolvedResolution, fps, videoGop]);

  const videoRangeGroup = useCallback((): string => {
    if (!timelineId) return "";
    const r = resolvedResolution();
    return [
      // Bump this version (and cache.ts RANGE_SCOPE_VERSION) on any server-side
      // encode-format change so old, format-incompatible cached blobs are never
      // re-appended into MSE. v2 retired the `frag_every_frame` blobs; v3 retired
      // the AAC-audio blobs now that the stream uses FLAC; v4 retired the
      // untagged-color blobs once the video was color-tagged; v5 switches that
      // tag to limited-range BT.709 to match the offline export default; v6 aligns
      // the FLAC block size to the video-frame grid for gapless segment seams.
      "range-v6",
      timelineId,
      `${r.width}x${r.height}`,
      String(fps),
      String(videoGop()),
      "23",
    ].join("|");
  }, [timelineId, resolvedResolution, fps, videoGop]);

  const cachedVideoUrl = useCallback(
    (start: number, end?: number): string => {
      if (!timelineId || !pluginCacheKey) return "";
      const r = resolvedResolution();
      const duration =
        end != null && end < timelineDuration - EPSILON
          ? Math.max(0, end - start)
          : undefined;
      return videoUrl({
        timelineId,
        time: start,
        width: r.width,
        height: r.height,
        fps,
        gop: videoGop(),
        crf: 23,
        duration,
        cacheKey: pluginCacheKey,
      });
    },
    [
      timelineId,
      pluginCacheKey,
      timelineDuration,
      resolvedResolution,
      fps,
      videoGop,
    ],
  );

  const liveVideoUrl = useCallback(
    (start: number, end?: number): string => {
      if (!timelineId) return "";
      const r = resolvedResolution();
      const duration =
        end != null && end < timelineDuration - EPSILON
          ? Math.max(0, end - start)
          : undefined;
      return videoUrl({
        timelineId,
        time: start,
        width: r.width,
        height: r.height,
        fps,
        gop: videoGop(),
        crf: 23,
        duration,
        cacheKey: "",
      });
    },
    [timelineId, timelineDuration, fps, resolvedResolution, videoGop],
  );

  const videoElement = useCallback(
    (slot: VideoSlot): HTMLVideoElement | null =>
      slot === 0 ? videoARef.current : videoBRef.current,
    [],
  );

  const setActiveVideoSlot = useCallback((slot: VideoSlot) => {
    activeVideoSlotRef.current = slot;
    setActiveVideoSlotState(slot);
  }, []);

  const clampSeconds = useCallback(
    (value: number) =>
      Math.max(0, Math.min(Number.isFinite(value) ? value : 0, timelineDuration)),
    [timelineDuration],
  );

  const setPreviewSecondsState = useCallback(
    (value: number) => {
      const next = clampSeconds(value);
      secondsRef.current = next;
      setSecondsState(next);
    },
    [clampSeconds],
  );

  const setPreviewPlayingState = useCallback((value: boolean) => {
    playingRef.current = value;
    setPlaying(value);
    previewDebug("playing", {
      value,
      seconds: secondsRef.current,
      displayToken: displayTokenRef.current,
    });
  }, []);

  const streamCacheOwnerMatches = useCallback((owner: StreamCacheOwner) => {
    const current = streamCacheOwnerRef.current;
    return current?.kind === owner.kind && current.id === owner.id;
  }, []);

  const setOwnedStreamCacheRange = useCallback(
    (owner: StreamCacheOwner, range: CacheRange) => {
      streamCacheOwnerRef.current = owner;
      setStreamCacheRanges([range]);
      previewDebug("stream-cache:set", { owner, range });
    },
    [],
  );

  const clearOwnedStreamCacheRange = useCallback(
    (owner: StreamCacheOwner) => {
      if (!streamCacheOwnerMatches(owner)) {
        previewDebug("stream-cache:clear:ignored", {
          owner,
          current: streamCacheOwnerRef.current,
        });
        return;
      }
      streamCacheOwnerRef.current = null;
      setStreamCacheRanges([]);
      previewDebug("stream-cache:clear", { owner });
    },
    [streamCacheOwnerMatches],
  );

  const recordCacheRanges = useCallback((ranges: CacheRange[]) => {
    if (ranges.length === 0) return;
    setCacheRanges((prev) => {
      let next = prev;
      for (const range of ranges) {
        next = mergeCacheRange(next, range.start, range.end);
      }
      saveCacheRanges(cacheScopeRef.current, next);
      previewDebug("cache-ranges:record", { ranges, next });
      return next;
    });
  }, []);

  const recordCacheRange = useCallback(
    (start: number, end: number) => {
      recordCacheRanges([{ start, end }]);
    },
    [recordCacheRanges],
  );

  const visibleCacheRanges = useMemo(() => {
    let next = cacheRanges;
    for (const range of streamCacheRanges) {
      next = mergeCacheRange(next, range.start, range.end);
    }
    return next;
  }, [cacheRanges, streamCacheRanges]);

  const setImageObjectUrl = useCallback((url: string) => {
    const previous = imageObjectUrlRef.current;
    imageObjectUrlRef.current = url;
    setImageSrc(url);
    if (previous && previous !== url) {
      URL.revokeObjectURL(previous);
    }
  }, []);

  const clearVideoSlot = useCallback(
    (slot: VideoSlot) => {
      previewDebug("video-slot:clear", {
        slot,
        held: heldVideoSlotRef.current,
        active: activeVideoSlotRef.current,
      });
      if (heldVideoSlotRef.current === slot) {
        heldVideoSlotRef.current = null;
      }
      const video = videoElement(slot);
      if (video) {
        video.pause();
        video.onloadedmetadata = null;
        video.onloadeddata = null;
        video.onerror = null;
        video.onended = null;
        video.removeAttribute("src");
      }
      const previous = videoObjectUrlsRef.current[slot];
      videoObjectUrlsRef.current[slot] = null;
      if (previous) URL.revokeObjectURL(previous);
      if (video) {
        try {
          video.load();
        } catch {
          // ignore
        }
      }
    },
    [videoElement],
  );

  const releaseHeldVideoSlotSoon = useCallback(() => {
    const slot = heldVideoSlotRef.current;
    if (slot == null) return;
    heldVideoSlotRef.current = null;
    requestAnimationFrame(() => {
      if (playbackRef.current?.slot === slot) return;
      if (streamingSessionRef.current?.slot === slot) return;
      clearVideoSlot(slot);
    });
  }, [clearVideoSlot]);

  const setVideoObjectUrl = useCallback(
    (slot: VideoSlot, url: string) => {
      const video = videoElement(slot);
      if (!video) return;
      const previous = videoObjectUrlsRef.current[slot];
      videoObjectUrlsRef.current[slot] = url;
      video.src = url;
      previewDebug("video-slot:set-url", {
        slot,
        replaced: Boolean(previous && previous !== url),
      });
      if (previous && previous !== url) {
        URL.revokeObjectURL(previous);
      }
    },
    [videoElement],
  );

  const stopRaf = useCallback(() => {
    if (rafRef.current != null) {
      cancelAnimationFrame(rafRef.current);
      rafRef.current = null;
    }
  }, []);

  const abortStreamingSession = useCallback(
    (save: boolean, clear: boolean = true) => {
      const session = streamingSessionRef.current;
      if (!session) return;
      previewDebug("session:abort", {
        id: session.id,
        mode: session.mode,
        start: session.start,
        end: session.end,
        bufferedSeconds: session.bufferedSeconds,
        save,
        clear,
      });
      session.saveOnFinalize = save;
      streamingSessionRef.current = null;
      session.controller.abort();
      if (clear) {
        session.clearOnFinalize = false;
        clearVideoSlot(session.slot);
      } else {
        session.clearOnFinalize = false;
      }
      if (!save) clearOwnedStreamCacheRange({ kind: "session", id: session.id });
    },
    [clearOwnedStreamCacheRange, clearVideoSlot],
  );

  const savePipelineLiveSegment = useCallback(
    async (
      pipeline: PlaybackPipeline,
      live: LivePipelineSegment,
    ): Promise<void> => {
      if (live.saved || live.chunks.length === 0) return;
      const videoSeconds = videoElement(pipeline.slot)?.currentTime ?? 0;
      const playedEnd =
        videoSeconds >= live.start - EPSILON && videoSeconds <= live.end + EPSILON
          ? videoSeconds
          : live.bufferedEnd;
      const end = Math.min(live.end, Math.max(live.bufferedEnd, playedEnd));
      if (end <= live.start + EPSILON) return;
      const cacheUrl = cachedVideoUrl(live.start, end);
      if (!cacheUrl) return;
      live.saved = true;
      previewDebug("pipeline:live-save", {
        id: pipeline.id,
        liveStart: live.start,
        liveEnd: live.end,
        bufferedEnd: live.bufferedEnd,
        playedEnd,
        saveEnd: end,
        chunks: live.chunks.length,
        aborted: pipeline.controller.signal.aborted,
      });
      const blob = new Blob(live.chunks, { type: "video/mp4" });
      const persisted = await putVideoRangeBlob(
        cacheUrl,
        pipeline.cacheKey,
        pipeline.group,
        live.start,
        end,
        blob,
      );
      if (persisted) recordCacheRange(live.start, end);
    },
    [cachedVideoUrl, recordCacheRange, videoElement],
  );

  const abortPlaybackPipeline = useCallback(
    (save: boolean, clear: boolean = true) => {
      const pipeline = playbackPipelineRef.current;
      if (!pipeline) return;
      previewDebug("pipeline:abort", {
        id: pipeline.id,
        start: pipeline.start,
        live: pipeline.liveSegment
          ? {
              start: pipeline.liveSegment.start,
              end: pipeline.liveSegment.end,
              bufferedEnd: pipeline.liveSegment.bufferedEnd,
            }
          : null,
        save,
        clear,
      });
      pipeline.saveOnAbort = save;
      playbackPipelineRef.current = null;
      pipeline.controller.abort();
      if (clear) clearVideoSlot(pipeline.slot);
      if (!save) {
        clearOwnedStreamCacheRange({ kind: "pipeline", id: pipeline.id });
      }
    },
    [clearOwnedStreamCacheRange, clearVideoSlot],
  );

  const clearAllVideo = useCallback(() => {
    heldVideoSlotRef.current = null;
    abortPlaybackPipeline(false);
    abortStreamingSession(false);
    playbackRef.current = null;
    clearVideoSlot(0);
    clearVideoSlot(1);
  }, [abortPlaybackPipeline, abortStreamingSession, clearVideoSlot]);

  const findFirstCacheGap = useCallback(
    async (fromSeconds: number): Promise<number> => {
      const group = videoRangeGroup();
      if (!group || !pluginCacheKey) return clampSeconds(fromSeconds);
      let gap = clampSeconds(fromSeconds);
      while (gap < timelineDuration - EPSILON) {
        const cached = await getCachedVideoRange(group, pluginCacheKey, gap);
        if (!cached || cached.end <= gap + EPSILON) break;
        gap = Math.min(cached.end, timelineDuration);
      }
      return gap;
    },
    [videoRangeGroup, pluginCacheKey, timelineDuration, clampSeconds],
  );

  const findNextCacheStart = useCallback(
    async (fromSeconds: number): Promise<number> => {
      const group = videoRangeGroup();
      if (!group || !pluginCacheKey) return timelineDuration;
      const next = await getNextCachedVideoRange(
        group,
        pluginCacheKey,
        clampSeconds(fromSeconds),
      );
      return next ? Math.min(next.start, timelineDuration) : timelineDuration;
    },
    [videoRangeGroup, pluginCacheKey, timelineDuration, clampSeconds],
  );

  const startStreamingSession = useCallback(
    (
      startSeconds: number,
      endSeconds: number,
      slot: VideoSlot,
      mode: StreamingMode,
      token: number,
      stoppedTargetEnd: number | null = null,
    ): StreamingSession | null => {
      const video = videoElement(slot);
      const group = videoRangeGroup();
      const sessionEnd = mode === "stopped" ? timelineDuration : endSeconds;
      const url =
        mode === "stopped"
          ? liveVideoUrl(startSeconds)
          : liveVideoUrl(startSeconds, endSeconds);
      if (
        !video ||
        !group ||
        !pluginCacheKey ||
        !url ||
        sessionEnd <= startSeconds + EPSILON ||
        typeof MediaSource === "undefined"
      ) {
        previewDebug("session:start:skipped", {
          hasVideo: Boolean(video),
          hasGroup: Boolean(group),
          hasCacheKey: Boolean(pluginCacheKey),
          hasUrl: Boolean(url),
          start: startSeconds,
          end: sessionEnd,
          mediaSource: typeof MediaSource !== "undefined",
        });
        return null;
      }

      const controller = new AbortController();
      const mediaSource = new MediaSource();
      const objectUrl = URL.createObjectURL(mediaSource);
      let resolveOpened!: () => void;
      let rejectOpened!: (reason?: unknown) => void;
      const opened = new Promise<void>((resolve, reject) => {
        resolveOpened = resolve;
        rejectOpened = reject;
      });
      const session: StreamingSession = {
        id: ++streamIdRef.current,
        token,
        mode,
        slot,
        start: startSeconds,
        end: sessionEnd,
        stoppedTargetEnd,
        url,
        cacheKey: pluginCacheKey,
        group,
        mediaSource,
        controller,
        chunks: [],
        sourceBuffer: null,
        appendChain: Promise.resolve(),
        opened,
        bufferedSeconds: 0,
        saveOnFinalize: true,
        clearOnFinalize: mode === "stopped",
      };
      previewDebug("session:start", {
        id: session.id,
        mode,
        slot,
        start: startSeconds,
        end: sessionEnd,
        stoppedTargetEnd,
        token,
      });

      setVideoObjectUrl(slot, objectUrl);
      try {
        video.load();
      } catch {
        // ignore
      }

      void (async () => {
        try {
          await waitForMediaSourceOpen(mediaSource);
          if (controller.signal.aborted) {
            rejectOpened(new DOMException("Aborted", "AbortError"));
            return;
          }
          const sourceBuffer = mediaSource.addSourceBuffer(selectMp4MimeType());
          session.sourceBuffer = sourceBuffer;
          resolveOpened();
          previewDebug("session:opened", {
            id: session.id,
            mode: session.mode,
            readyState: mediaSource.readyState,
          });

          const response = await fetch(url, {
            cache: "no-store",
            signal: controller.signal,
          });
          if (!response.ok) {
            throw new Error(`${url} failed: ${response.status}`);
          }
          const reader = response.body?.getReader();
          if (!reader) throw new Error("video stream has no body");

          for (;;) {
            const { done, value } = await reader.read();
            if (done || !value) break;
            const chunk = value.buffer.slice(
              value.byteOffset,
              value.byteOffset + value.byteLength,
            ) as ArrayBuffer;
            session.chunks.push(chunk);
            session.appendChain = session.appendChain.then(() =>
              appendSourceBuffer(sourceBuffer, chunk.slice(0)),
            );
            await session.appendChain;
            session.bufferedSeconds = Math.max(
              session.bufferedSeconds,
              sourceBufferEnd(sourceBuffer),
            );
            setOwnedStreamCacheRange(
              { kind: "session", id: session.id },
              {
                start: session.start,
                end: Math.min(
                  session.end,
                  session.start + session.bufferedSeconds,
                ),
              },
            );
            if (
              session.mode === "stopped" &&
              session.stoppedTargetEnd != null &&
              session.start + session.bufferedSeconds >=
                session.stoppedTargetEnd - EPSILON
            ) {
              previewDebug("session:stopped-target-hit", {
                id: session.id,
                start: session.start,
                bufferedSeconds: session.bufferedSeconds,
                targetEnd: session.stoppedTargetEnd,
              });
              controller.abort();
              break;
            }
          }
        } catch (e) {
          if (!controller.signal.aborted && !isAbortError(e)) {
            previewDebug("session:error", { id: session.id, error: String(e) });
            rejectOpened(e);
            if (session.token === displayTokenRef.current) setError(String(e));
          }
        } finally {
          try {
            await session.appendChain;
          } catch {
            // The source may already have been detached by a user action.
          }
          if (mediaSource.readyState === "open") {
            try {
              mediaSource.endOfStream();
            } catch {
              // ignore
            }
          }

          const videoDuration = videoElement(slot)?.currentTime ?? 0;
          const duration = Math.max(session.bufferedSeconds, videoDuration);
          const end = Math.min(session.end, session.start + duration);
          previewDebug("session:finalize", {
            id: session.id,
            mode: session.mode,
            start: session.start,
            end,
            sessionEnd: session.end,
            bufferedSeconds: session.bufferedSeconds,
            videoDuration,
            chunks: session.chunks.length,
            save: session.saveOnFinalize,
            clear: session.clearOnFinalize,
            aborted: session.controller.signal.aborted,
            token: session.token,
            currentToken: displayTokenRef.current,
          });
          if (
            session.saveOnFinalize &&
            session.chunks.length > 0 &&
            end > session.start + EPSILON
          ) {
            const cacheUrl = cachedVideoUrl(session.start, end);
            if (cacheUrl) {
              const blob = new Blob(session.chunks, { type: "video/mp4" });
              const persisted = await putVideoRangeBlob(
                cacheUrl,
                session.cacheKey,
                session.group,
                session.start,
                end,
                blob,
              );
              if (persisted) recordCacheRange(session.start, end);
            }
          }

          clearOwnedStreamCacheRange({ kind: "session", id: session.id });
          if (streamingSessionRef.current?.id === session.id) {
            streamingSessionRef.current = null;
          }
          // Don't tear down the slot if it was just promoted to active playback:
          // `promoteStoppedStream` synchronously claims `playbackRef.slot` before
          // awaiting the element's seek/current-data. If this finalize (e.g. the
          // preroll hitting its `stoppedTargetEnd`) cleared that same slot, the
          // promote's `waitForVideoSeeked`/`waitForVideoCurrentData` would hang
          // forever (a removed `src` fires `emptied`/`abort`, never `seeked`/
          // `error`) and playback would silently never start.
          if (session.clearOnFinalize && playbackRef.current?.slot !== slot) {
            clearVideoSlot(slot);
          }
          if (
            session.mode === "stopped" &&
            session.stoppedTargetEnd != null &&
            !session.controller.signal.aborted &&
            end < session.stoppedTargetEnd - EPSILON &&
            session.token === displayTokenRef.current
          ) {
            startStoppedStreamRef.current(end, session.stoppedTargetEnd);
          }
        }
      })();

      return session;
    },
    [
      cachedVideoUrl,
      clearVideoSlot,
      clearOwnedStreamCacheRange,
      liveVideoUrl,
      pluginCacheKey,
      recordCacheRange,
      setOwnedStreamCacheRange,
      setVideoObjectUrl,
      timelineDuration,
      videoElement,
      videoRangeGroup,
    ],
  );

  const startStoppedStream = useCallback(
    (
      fromSeconds: number,
      targetEnd: number = Math.min(
        fromSeconds + STOPPED_STREAM_CACHE_SECONDS,
        timelineDuration,
      ),
    ) => {
      if (!timelineId || !hasServerInfo || !pluginCacheKey || playingRef.current) {
        previewDebug("stopped-stream:skip-sync", {
          fromSeconds,
          targetEnd,
          hasTimeline: Boolean(timelineId),
          hasServerInfo,
          hasCacheKey: Boolean(pluginCacheKey),
          playing: playingRef.current,
        });
        return;
      }
      const token = displayTokenRef.current;
      previewDebug("stopped-stream:plan", { fromSeconds, targetEnd, token });
      void (async () => {
        const cappedTargetEnd = Math.min(targetEnd, timelineDuration);
        const gap = await findFirstCacheGap(fromSeconds);
        const nextCacheStart = await findNextCacheStart(gap);
        const streamEnd = Math.min(cappedTargetEnd, nextCacheStart);
        if (
          token !== displayTokenRef.current ||
          playingRef.current ||
          gap >= cappedTargetEnd - EPSILON ||
          streamEnd <= gap + EPSILON
        ) {
          previewDebug("stopped-stream:skip-async", {
            fromSeconds,
            targetEnd,
            cappedTargetEnd,
            gap,
            nextCacheStart,
            streamEnd,
            token,
            currentToken: displayTokenRef.current,
            playing: playingRef.current,
          });
          return;
        }
        const current = streamingSessionRef.current;
        if (
          current &&
          current.mode === "stopped" &&
          !current.controller.signal.aborted
        ) {
          const currentBufferedEnd = current.start + current.bufferedSeconds;
          if (
            Math.abs(current.start - gap) <= EPSILON ||
            (current.start <= gap + EPSILON &&
              currentBufferedEnd >= gap - EPSILON)
          ) {
            if (current.stoppedTargetEnd != null) {
              current.stoppedTargetEnd = Math.max(
                current.stoppedTargetEnd,
                cappedTargetEnd,
              );
            }
            previewDebug("stopped-stream:reuse", {
              currentId: current.id,
              currentStart: current.start,
              currentBufferedEnd,
              gap,
              cappedTargetEnd,
            });
            return;
          }
        }
        previewDebug("stopped-stream:start", {
          fromSeconds,
          gap,
          streamEnd,
          cappedTargetEnd,
          nextCacheStart,
          slot: activeVideoSlotRef.current,
          abortCurrent: Boolean(streamingSessionRef.current),
        });
        abortStreamingSession(false);
        const session = startStreamingSession(
          gap,
          streamEnd,
          activeVideoSlotRef.current,
          "stopped",
          token,
          cappedTargetEnd,
        );
        if (session) streamingSessionRef.current = session;
      })();
    },
    [
      timelineId,
      timelineDuration,
      hasServerInfo,
      pluginCacheKey,
      findFirstCacheGap,
      findNextCacheStart,
      abortStreamingSession,
      startStreamingSession,
    ],
  );
  startStoppedStreamRef.current = startStoppedStream;

  useEffect(() => {
    const scope = currentCacheScope();
    const cacheKeyChanged =
      Boolean(pluginCacheKey) && lastCacheKeyRef.current !== pluginCacheKey;
    lastCacheKeyRef.current = pluginCacheKey;
    immediatePreloadRef.current = cacheKeyChanged;
    cacheScopeRef.current = scope;
    setCacheRanges(scope ? loadCacheRanges(scope) : []);
    streamCacheOwnerRef.current = null;
    setStreamCacheRanges([]);
    revokeStaleCacheRanges(pluginCacheKey);
    revokeStaleMediaCacheEntries(pluginCacheKey).catch((e) => {
      console.warn("tellur-live media cache revoke failed", e);
    });
    clearAllVideo();
  }, [currentCacheScope, pluginCacheKey, clearAllVideo]);

  const schedulePreload = useCallback(
    (delay: number = PRELOAD_DELAY_MS) => {
      if (preloadTimerRef.current) clearTimeout(preloadTimerRef.current);
      if (!timelineId || !hasServerInfo || !pluginCacheKey || playingRef.current) {
        previewDebug("preload:skip", {
          delay,
          hasTimeline: Boolean(timelineId),
          hasServerInfo,
          hasCacheKey: Boolean(pluginCacheKey),
          playing: playingRef.current,
        });
        return;
      }
      previewDebug("preload:schedule", { delay, seconds: secondsRef.current });
      preloadTimerRef.current = setTimeout(() => {
        preloadTimerRef.current = null;
        const currentSeconds = secondsRef.current;
        previewDebug("preload:fire", { seconds: currentSeconds });
        startStoppedStream(currentSeconds);
      }, delay);
    },
    [
      timelineId,
      hasServerInfo,
      pluginCacheKey,
      startStoppedStream,
    ],
  );

  const requestPngFrame = useCallback(
    (force: boolean = false, atSeconds?: number) => {
      if (!timelineId || !hasServerInfo || !pluginCacheKey) return;
      const res = resolvedResolution();
      // The timeline is a half-open range [0, duration): at exactly `duration`
      // every clip is inactive, so `/api/frame` returns None -> 500. Never ask
      // for the very end — pull back by EPSILON so the LAST representable frame is
      // requested instead (this is what the playhead shows when it sticks at the
      // end). Applies to the explicit end-of-playback requests too.
      const frameSeconds = Math.min(
        clampSeconds(atSeconds ?? secondsRef.current),
        Math.max(0, timelineDuration - EPSILON),
      );
      const url = frameUrl({
        timelineId,
        time: frameSeconds,
        width: res.width,
        height: res.height,
        fps,
        cacheKey: pluginCacheKey,
      });
      if (pendingPngRef.current) {
        if (pendingPngUrlRef.current === url && !queuedPngRef.current) {
          if (preloadTimerRef.current) clearTimeout(preloadTimerRef.current);
          return;
        }
        queuedPngRef.current = true;
        if (force) displayTokenRef.current += 1;
        if (preloadTimerRef.current) clearTimeout(preloadTimerRef.current);
        return;
      }
      pendingPngRef.current = true;
      queuedPngRef.current = false;
      if (preloadTimerRef.current) clearTimeout(preloadTimerRef.current);
      const id = ++displayTokenRef.current;
      const pngId = ++pngTokenRef.current;
      pendingPngUrlRef.current = url;

      loadUncachedMediaObjectUrl(url)
        .then((objectUrl) => {
          const img = new Image();
          img.onload = () => {
            if (id === displayTokenRef.current) {
              setImageObjectUrl(objectUrl);
              setImageVisible(true);
              setVideoVisible(false);
              releaseHeldVideoSlotSoon();
              setError(null);
            } else {
              URL.revokeObjectURL(objectUrl);
            }
            finish();
          };
          img.onerror = () => {
            URL.revokeObjectURL(objectUrl);
            if (id === displayTokenRef.current) {
              setError("frame request failed");
            }
            finish();
          };
          img.src = objectUrl;
        })
        .catch((e) => {
          if (id === displayTokenRef.current) {
            setError(String(e));
          }
          finish();
        });

      function finish() {
        if (pngId === pngTokenRef.current) {
          pendingPngRef.current = false;
          pendingPngUrlRef.current = null;
        }
        if (queuedPngRef.current && !playingRef.current) {
          queuedPngRef.current = false;
          requestPngFrame(true);
        } else if (!playingRef.current) {
          const delay = force || immediatePreloadRef.current ? 0 : undefined;
          immediatePreloadRef.current = false;
          schedulePreload(delay);
        }
      }
    },
    [
      timelineId,
      hasServerInfo,
      pluginCacheKey,
      resolvedResolution,
      clampSeconds,
      fps,
      setImageObjectUrl,
      schedulePreload,
      releaseHeldVideoSlotSoon,
    ],
  );

  const currentPlaybackSeconds = useCallback(() => {
    const playback = playbackRef.current;
    if (!playback) return secondsRef.current;
    const video = videoElement(playback.slot);
    const videoSeconds = video?.currentTime;
    if (playback.kind === "pipeline") {
      return clampSeconds(videoSeconds ?? secondsRef.current);
    }
    return clampSeconds(
      videoSeconds == null ? secondsRef.current : playback.start + videoSeconds,
    );
  }, [clampSeconds, videoElement]);

  const stopPlayback = useCallback(
    (saveStream: boolean, keepCurrentFrame: boolean = false) => {
      stopRaf();
      // Cancel a deferred `startVideoPlayback` that hasn't fired yet, so pausing
      // (or seeking) within the rAF window can't resurrect playback afterwards.
      if (playStartRafRef.current != null) {
        cancelAnimationFrame(playStartRafRef.current);
        playStartRafRef.current = null;
      }
      playbackTokenRef.current += 1;
      const playback = playbackRef.current;
      previewDebug("playback:stop", {
        playback,
        saveStream,
        keepCurrentFrame,
        seconds: secondsRef.current,
      });
      playbackRef.current = null;
      if (playback?.kind === "cache") {
        const video = videoElement(playback.slot);
        video?.pause();
        if (keepCurrentFrame) {
          heldVideoSlotRef.current = playback.slot;
          setActiveVideoSlot(playback.slot);
        } else {
          clearVideoSlot(playback.slot);
        }
        const session = streamingSessionRef.current;
        if (session?.mode === "playback") {
          abortStreamingSession(saveStream);
        }
      } else if (playback?.kind === "stream") {
        const video = videoElement(playback.slot);
        video?.pause();
        if (keepCurrentFrame) {
          heldVideoSlotRef.current = playback.slot;
          setActiveVideoSlot(playback.slot);
        }
        abortStreamingSession(saveStream, !keepCurrentFrame);
      } else if (playback?.kind === "pipeline") {
        const video = videoElement(playback.slot);
        video?.pause();
        if (keepCurrentFrame) {
          heldVideoSlotRef.current = playback.slot;
          setActiveVideoSlot(playback.slot);
        }
        abortPlaybackPipeline(saveStream, !keepCurrentFrame);
      }
      if (keepCurrentFrame && playback) {
        setVideoVisible(true);
        setImageVisible(false);
      } else {
        setVideoVisible(false);
        setImageVisible(true);
      }
    },
    [
      abortStreamingSession,
      abortPlaybackPipeline,
      clearVideoSlot,
      setActiveVideoSlot,
      stopRaf,
      videoElement,
    ],
  );

  const startVideoPlayback = useCallback(() => {
    // This runs from a deferred rAF scheduled in `togglePlay`. If the user paused
    // (or seeked) inside that one-frame window, `playingRef` is already false —
    // bail so we don't start phantom playback or bump `displayTokenRef` (which
    // would discard the freshly requested paused PNG).
    playStartRafRef.current = null;
    if (!playingRef.current) {
      previewDebug("playback:start:not-playing", { seconds: secondsRef.current });
      return;
    }
    if (!timelineId || !hasServerInfo || !pluginCacheKey) {
      previewDebug("playback:start:skipped", {
        hasTimeline: Boolean(timelineId),
        hasServerInfo,
        hasCacheKey: Boolean(pluginCacheKey),
      });
      return;
    }
    const group = videoRangeGroup();
    const slot = activeVideoSlotRef.current;
    const video = videoElement(slot);
    if (!group || !video || typeof MediaSource === "undefined") {
      previewDebug("playback:start:skipped", {
        hasGroup: Boolean(group),
        hasVideo: Boolean(video),
        mediaSource: typeof MediaSource !== "undefined",
      });
      return;
    }
    if (preloadTimerRef.current) {
      clearTimeout(preloadTimerRef.current);
      preloadTimerRef.current = null;
    }
    stopRaf();
    const token = ++displayTokenRef.current;
    heldVideoSlotRef.current = null;
    let startSeconds = clampSeconds(secondsRef.current);
    // Pressing play with the head parked at the very end (after playback ran to
    // the end, or a full-right scrub) would start at `timelineDuration`, where the
    // pipeline cursor loop (`cursor < timelineDuration - EPSILON`) skips entirely
    // and `maybeStart` never runs — playback would silently never begin and no
    // error would surface. Treat play-from-the-end as a replay from the start.
    if (startSeconds >= timelineDuration - EPSILON) {
      startSeconds = 0;
      setPreviewSecondsState(0);
    }
    previewDebug("playback:start", {
      startSeconds,
      token,
      slot,
      stoppedSession: streamingSessionRef.current
        ? {
            id: streamingSessionRef.current.id,
            mode: streamingSessionRef.current.mode,
            start: streamingSessionRef.current.start,
            end: streamingSessionRef.current.end,
            bufferedSeconds: streamingSessionRef.current.bufferedSeconds,
            aborted: streamingSessionRef.current.controller.signal.aborted,
          }
        : null,
      pipeline: playbackPipelineRef.current?.id ?? null,
    });

    const fail = (e: unknown) => {
      if (token !== displayTokenRef.current) return;
      previewDebug("playback:fail", { token, error: String(e) });
      setPreviewPlayingState(false);
      setError(String(e));
      stopPlayback(true);
    };

    // Force the end-of-playback state. The browser's `ended` event doesn't fire
    // if the stream's buffered end falls a few frames short of the duration, so
    // the tickers also call this when they detect an end-of-timeline stall (see
    // below). Runs at most once per playback (guarded by the ticker's `settled`).
    const settleAtTimelineEnd = () => {
      stopRaf();
      setPreviewSecondsState(timelineDuration);
      setPreviewPlayingState(false);
      // #31 clamps the request to duration - EPSILON, so this fetches the last
      // representable frame rather than the out-of-range exact end.
      requestPngFrame(true, timelineDuration);
    };

    // True when `bufferedEnd` (absolute) is within END_STALL_TAIL of the timeline
    // end AND playback is stuck at that buffered end — i.e. the stream ended a few
    // frames short and `ended` won't fire. The duration-proximity check is what
    // prevents a mid-clip buffering wait from being mistaken for the end.
    const isEndStall = (current: HTMLVideoElement, absoluteEnd: number) => {
      const buffered = current.buffered;
      if (buffered.length === 0) return false;
      const bufferedEnd = buffered.end(buffered.length - 1);
      const atBufferEnd = current.currentTime >= bufferedEnd - EPSILON;
      const nearTimelineEnd =
        absoluteEnd >= timelineDuration - END_STALL_TAIL_SECONDS;
      return atBufferEnd && nearTimelineEnd;
    };

    const startTicker = () => {
      const playbackToken = ++playbackTokenRef.current;
      stopRaf();
      let lastTime = -1;
      let stalledTicks = 0;
      let settled = false;
      const tick = () => {
        if (playbackToken !== playbackTokenRef.current) return;
        const current = videoElement(slot);
        if (!current) return;
        // Monotonic guard: near the end the video clock can briefly report a
        // value BELOW where we already are, which would flash the playhead back
        // to the start. Skip the backwards frame (don't update state) but keep
        // the RAF running. Forward progress and the end-clamp (onended) still
        // apply; explicit seeks go through `setSeconds`, not this ticker.
        if (current.currentTime >= secondsRef.current - EPSILON) {
          setPreviewSecondsState(current.currentTime);
        }
        // End-of-timeline stall: when the buffered end is a hair short of the
        // duration, `ended` never fires and the monotonic guard would freeze the
        // playhead a few frames early. Detect a sustained stall AT the buffer end
        // (and only when that end is near the duration) and snap to the end once.
        if (!settled) {
          stalledTicks =
            Math.abs(current.currentTime - lastTime) < EPSILON
              ? stalledTicks + 1
              : 0;
          lastTime = current.currentTime;
          if (
            stalledTicks >= END_STALL_TICKS &&
            isEndStall(current, current.currentTime)
          ) {
            settled = true;
            settleAtTimelineEnd();
            return;
          }
        }
        rafRef.current = requestAnimationFrame(tick);
      };
      rafRef.current = requestAnimationFrame(tick);
    };

    const startOffsetTicker = (slot: VideoSlot, baseSeconds: number) => {
      const playbackToken = ++playbackTokenRef.current;
      stopRaf();
      let lastTime = -1;
      let stalledTicks = 0;
      let settled = false;
      const tick = () => {
        if (playbackToken !== playbackTokenRef.current) return;
        const current = videoElement(slot);
        if (!current) return;
        // Same monotonic guard as startTicker (see there): never let a momentary
        // backwards clock reading rewind the playhead to the start.
        const nextSeconds = baseSeconds + current.currentTime;
        if (nextSeconds >= secondsRef.current - EPSILON) {
          setPreviewSecondsState(nextSeconds);
        }
        // End-of-timeline stall detection (see startTicker). Here the absolute
        // end is `baseSeconds + bufferedEnd`, since this ticker plays an offset
        // stream segment.
        if (!settled) {
          stalledTicks =
            Math.abs(current.currentTime - lastTime) < EPSILON
              ? stalledTicks + 1
              : 0;
          lastTime = current.currentTime;
          if (
            stalledTicks >= END_STALL_TICKS &&
            isEndStall(current, baseSeconds + current.currentTime)
          ) {
            settled = true;
            settleAtTimelineEnd();
            return;
          }
        }
        rafRef.current = requestAnimationFrame(tick);
      };
      rafRef.current = requestAnimationFrame(tick);
    };

    const promoteStoppedStream = (session: StreamingSession): boolean => {
      const streamVideo = videoElement(session.slot);
      if (
        !streamVideo ||
        session.mode !== "stopped" ||
        session.start > startSeconds + EPSILON ||
        session.end <= startSeconds + EPSILON ||
        session.controller.signal.aborted
      ) {
        previewDebug("session:promote:reject", {
          id: session.id,
          hasVideo: Boolean(streamVideo),
          mode: session.mode,
          sessionStart: session.start,
          sessionEnd: session.end,
          startSeconds,
          aborted: session.controller.signal.aborted,
        });
        return false;
      }

      previewDebug("session:promote", {
        id: session.id,
        sessionStart: session.start,
        sessionEnd: session.end,
        bufferedSeconds: session.bufferedSeconds,
        startSeconds,
        offset: Math.max(0, startSeconds - session.start),
      });
      session.mode = "playback";
      session.saveOnFinalize = true;
      session.clearOnFinalize = true;
      session.token = token;
      playbackRef.current = {
        kind: "stream",
        slot: session.slot,
        start: session.start,
      };
      const offset = Math.max(0, startSeconds - session.start);
      streamVideo.onerror = () => fail("video stream failed");
      streamVideo.onended = () => {
        if (token !== displayTokenRef.current) return;
        stopRaf();
        const end = clampSeconds(session.start + streamVideo.currentTime);
        previewDebug("session:playback-ended", {
          id: session.id,
          end,
          videoCurrentTime: streamVideo.currentTime,
        });
        playbackRef.current = null;
        if (end >= timelineDuration - EPSILON) {
          setPreviewPlayingState(false);
          setPreviewSecondsState(timelineDuration);
          heldVideoSlotRef.current = session.slot;
          setActiveVideoSlot(session.slot);
          setVideoVisible(true);
          setImageVisible(false);
          requestPngFrame(true, timelineDuration);
        } else {
          setPreviewSecondsState(end);
        }
      };
      session.opened
        .then(async () => {
          while (
            token === displayTokenRef.current &&
            !session.controller.signal.aborted &&
            session.bufferedSeconds + EPSILON < offset
          ) {
            await delay(16);
          }
          if (token !== displayTokenRef.current || session.controller.signal.aborted) {
            return;
          }
          await waitForVideoMetadata(streamVideo);
          if (offset > EPSILON) {
            const range = bufferedRangeContaining(streamVideo.buffered, offset);
            streamVideo.currentTime = range
              ? Math.max(offset, range.start)
              : offset;
            // Resilient wait (see waitForVideoSettledAfterSeek): a `seeked` that
            // never fires at the buffer edge must not strand the promoted stream.
            await waitForVideoSettledAfterSeek(streamVideo);
          }
          await waitForVideoCurrentData(streamVideo);
          if (token !== displayTokenRef.current || session.controller.signal.aborted) {
            return;
          }
          await streamVideo.play();
          if (token !== displayTokenRef.current || session.controller.signal.aborted) {
            return;
          }
          previewDebug("session:promote-playing", {
            id: session.id,
            currentTime: streamVideo.currentTime,
            start: session.start,
          });
          setActiveVideoSlot(session.slot);
          setImageVisible(false);
          setVideoVisible(true);
          startOffsetTicker(session.slot, session.start);
        })
        .catch(fail);
      return true;
    };

    const stoppedSession = streamingSessionRef.current;
    if (stoppedSession && promoteStoppedStream(stoppedSession)) {
      abortPlaybackPipeline(false);
      return;
    }

    // Promotion was rejected. If the rejected preroll is a still-buffering AHEAD
    // gap (it began past the playhead because the playhead's region was already
    // cached), finalize it with save=true so its buffered bytes are persisted
    // into the cache instead of discarded — the pipeline below then reuses that
    // gap rather than re-streaming it, and the green bar doesn't blink away the
    // segment it just built.
    const reusablePreroll =
      stoppedSession != null &&
      stoppedSession.mode === "stopped" &&
      !stoppedSession.controller.signal.aborted &&
      stoppedSession.bufferedSeconds > EPSILON &&
      stoppedSession.start > startSeconds + EPSILON;
    abortStreamingSession(reusablePreroll);
    abortPlaybackPipeline(false);
    clearVideoSlot(slot);

    const mediaSource = new MediaSource();
    const objectUrl = URL.createObjectURL(mediaSource);
    const controller = new AbortController();
    const pipeline: PlaybackPipeline = {
      id: ++pipelineIdRef.current,
      token,
      slot,
      start: startSeconds,
      group,
      cacheKey: pluginCacheKey,
      mediaSource,
      controller,
      sourceBuffer: null,
      appendChain: Promise.resolve(),
      started: false,
      starting: null,
      saveOnAbort: true,
      liveSegment: null,
    };
    playbackPipelineRef.current = pipeline;
    playbackRef.current = { kind: "pipeline", slot, start: 0 };
    previewDebug("pipeline:start", { id: pipeline.id, startSeconds, slot });
    setVideoObjectUrl(slot, objectUrl);

    const maybeStart = async () => {
      const sourceBuffer = pipeline.sourceBuffer;
      const contains = sourceBuffer
        ? bufferedContains(sourceBuffer.buffered, startSeconds)
        : false;
      if (pipeline.started || pipeline.starting || !sourceBuffer || !contains) {
        previewDebug("pipeline:maybe-start:skip", {
          id: pipeline.id,
          started: pipeline.started,
          starting: Boolean(pipeline.starting),
          hasSourceBuffer: Boolean(sourceBuffer),
          bufferedContainsStart: contains,
          startSeconds,
          buffered: sourceBuffer ? bufferedDebug(sourceBuffer.buffered) : [],
        });
        return pipeline.starting ?? Promise.resolve();
      }
      pipeline.starting = (async () => {
        previewDebug("pipeline:maybe-start:begin", {
          id: pipeline.id,
          startSeconds,
          currentTime: video.currentTime,
          readyState: video.readyState,
          buffered: bufferedDebug(sourceBuffer.buffered),
        });
        await waitForVideoMetadata(video);
        if (controller.signal.aborted || token !== displayTokenRef.current) return;
        previewDebug("pipeline:maybe-start:metadata", {
          id: pipeline.id,
          readyState: video.readyState,
        });
        if (Math.abs(video.currentTime - startSeconds) > EPSILON) {
          // Clamp the seek target up to the true start of the buffered range that
          // brackets startSeconds: the append's first sample can land a hair after
          // startSeconds (within EPSILON, so bufferedContains still passes), and
          // seeking into that sub-EPSILON gap hangs the seek on some browsers.
          const range = bufferedRangeContaining(sourceBuffer.buffered, startSeconds);
          const target = range ? Math.max(startSeconds, range.start) : startSeconds;
          video.currentTime = target;
          previewDebug("pipeline:maybe-start:seek", {
            id: pipeline.id,
            to: target,
            startSeconds,
            rangeStart: range?.start ?? null,
            rangeEnd: range?.end ?? null,
            seeking: video.seeking,
          });
          await waitForVideoSettledAfterSeek(video);
          previewDebug("pipeline:maybe-start:seeked", {
            id: pipeline.id,
            currentTime: video.currentTime,
            readyState: video.readyState,
            seeking: video.seeking,
          });
        }
        await waitForVideoCurrentData(video);
        if (controller.signal.aborted || token !== displayTokenRef.current) return;
        previewDebug("pipeline:maybe-start:current-data", {
          id: pipeline.id,
          readyState: video.readyState,
        });
        await video.play();
        if (controller.signal.aborted || token !== displayTokenRef.current) return;
        pipeline.started = true;
        previewDebug("pipeline:playing", {
          id: pipeline.id,
          videoCurrentTime: video.currentTime,
          startSeconds,
        });
        setActiveVideoSlot(slot);
        setImageVisible(false);
        setVideoVisible(true);
        startTicker();
      })()
        .catch((e) => {
          previewDebug("pipeline:maybe-start:error", {
            id: pipeline.id,
            error: String(e),
          });
          throw e;
        })
        .finally(() => {
          pipeline.starting = null;
        });
      return pipeline.starting;
    };

    const appendCachedRange = async (range: CachedVideoRangeBlob) => {
      const sourceBuffer = pipeline.sourceBuffer;
      if (!sourceBuffer) return;
      previewDebug("pipeline:cache-append:start", {
        id: pipeline.id,
        range: { start: range.start, end: range.end },
        cursorContainsStart: range.start <= startSeconds && range.end > startSeconds,
      });
      const data = await range.blob.arrayBuffer();
      pipeline.appendChain = pipeline.appendChain.then(() =>
        appendTimestampedSourceBuffer(sourceBuffer, range.start, range.end, data),
      );
      await pipeline.appendChain;
      recordCacheRange(range.start, range.end);
      previewDebug("pipeline:cache-append:end", {
        id: pipeline.id,
        range: { start: range.start, end: range.end },
        buffered: bufferedDebug(sourceBuffer.buffered),
      });
      await maybeStart();
    };

    const saveLiveSegment = async (live: LivePipelineSegment) => {
      if (pipeline.saveOnAbort || !controller.signal.aborted) {
        await savePipelineLiveSegment(pipeline, live);
      }
    };

    const appendLiveRange = async (start: number, end: number) => {
      const sourceBuffer = pipeline.sourceBuffer;
      if (!sourceBuffer || end <= start + EPSILON) return;
      const url = liveVideoUrl(start, end);
      if (!url) return;
      previewDebug("pipeline:live:start", {
        id: pipeline.id,
        start,
        end,
        currentTime: video.currentTime,
      });
      const live: LivePipelineSegment = {
        start,
        end,
        url,
        chunks: [],
        bufferedEnd: start,
        saved: false,
      };
      pipeline.liveSegment = live;
      pipeline.appendChain = pipeline.appendChain.then(() =>
        prepareSourceBufferSegment(sourceBuffer, start, end),
      );
      await pipeline.appendChain;
      try {
        const response = await fetch(url, {
          cache: "no-store",
          signal: controller.signal,
        });
        if (!response.ok) {
          throw new Error(`${url} failed: ${response.status}`);
        }
        const reader = response.body?.getReader();
        if (!reader) throw new Error("video stream has no body");
        for (;;) {
          const { done, value } = await reader.read();
          if (done || !value) break;
          const chunk = value.buffer.slice(
            value.byteOffset,
            value.byteOffset + value.byteLength,
          ) as ArrayBuffer;
          live.chunks.push(chunk);
          pipeline.appendChain = pipeline.appendChain.then(() =>
            appendSourceBuffer(sourceBuffer, chunk.slice(0)),
          );
          await pipeline.appendChain;
          live.bufferedEnd = Math.min(end, sourceBufferEnd(sourceBuffer));
          setOwnedStreamCacheRange(
            { kind: "pipeline", id: pipeline.id },
            { start: live.start, end: live.bufferedEnd },
          );
          await maybeStart();
        }
      } finally {
        await pipeline.appendChain.catch(() => undefined);
        previewDebug("pipeline:live:finalize", {
          id: pipeline.id,
          start: live.start,
          end: live.end,
          bufferedEnd: live.bufferedEnd,
          chunks: live.chunks.length,
          aborted: controller.signal.aborted,
          saveOnAbort: pipeline.saveOnAbort,
        });
        await saveLiveSegment(live);
        if (pipeline.liveSegment === live) pipeline.liveSegment = null;
        clearOwnedStreamCacheRange({ kind: "pipeline", id: pipeline.id });
      }
    };

    const run = async () => {
      try {
        setError(null);
        video.onerror = () => fail("video pipeline failed");
        video.onended = () => {
          if (token !== displayTokenRef.current) return;
          stopRaf();
          previewDebug("pipeline:ended", {
            id: pipeline.id,
            currentTime: video.currentTime,
          });
          setPreviewSecondsState(timelineDuration);
          setPreviewPlayingState(false);
          playbackRef.current = null;
          playbackPipelineRef.current = null;
          heldVideoSlotRef.current = slot;
          setActiveVideoSlot(slot);
          setVideoVisible(true);
          setImageVisible(false);
          requestPngFrame(true, timelineDuration);
        };
        setVideoObjectUrl(slot, objectUrl);
        try {
          video.load();
        } catch {
          // ignore
        }
        await waitForMediaSourceOpen(mediaSource);
        if (controller.signal.aborted || token !== displayTokenRef.current) return;
        const sourceBuffer = mediaSource.addSourceBuffer(selectMp4MimeType());
        try {
          sourceBuffer.mode = "segments";
        } catch {
          // Some browsers expose mode as readonly for this SourceBuffer.
        }
        pipeline.sourceBuffer = sourceBuffer;
        mediaSource.duration = timelineDuration;
        previewDebug("pipeline:opened", {
          id: pipeline.id,
          duration: timelineDuration,
        });

        let cursor = startSeconds;
        while (
          cursor < timelineDuration - EPSILON &&
          !controller.signal.aborted &&
          token === displayTokenRef.current
        ) {
          const cached = await getCachedVideoRangeBlob(
            group,
            pluginCacheKey,
            cursor,
          );
          if (cached && cached.end > cursor + EPSILON) {
            previewDebug("pipeline:cursor:cache-hit", {
              id: pipeline.id,
              cursor,
              cached: { start: cached.start, end: cached.end },
            });
            await appendCachedRange(cached);
            cursor = Math.min(cached.end, timelineDuration);
            continue;
          }

          const nextCacheStart = await findNextCacheStart(cursor);
          const liveEnd = Math.min(nextCacheStart, timelineDuration);
          previewDebug("pipeline:cursor:cache-miss", {
            id: pipeline.id,
            cursor,
            nextCacheStart,
            liveEnd,
          });
          if (liveEnd <= cursor + EPSILON) {
            cursor = Math.min(liveEnd, timelineDuration);
            continue;
          }
          await appendLiveRange(cursor, liveEnd);
          cursor = liveEnd;
        }

        await pipeline.appendChain;
        if (mediaSource.readyState === "open") {
          try {
            mediaSource.endOfStream();
          } catch {
            // ignore
          }
        }
      } catch (e) {
        if (!controller.signal.aborted && token === displayTokenRef.current) {
          fail(e);
        }
      } finally {
        previewDebug("pipeline:run-finalize", {
          id: pipeline.id,
          currentPipeline: playbackPipelineRef.current?.id ?? null,
          aborted: controller.signal.aborted,
          token,
          currentToken: displayTokenRef.current,
        });
        if (playbackPipelineRef.current?.id === pipeline.id) {
          playbackPipelineRef.current = null;
        }
      }
    };

    void run();
  }, [
    timelineId,
    timelineDuration,
    hasServerInfo,
    pluginCacheKey,
    clampSeconds,
    videoRangeGroup,
    videoElement,
    stopRaf,
    stopPlayback,
    findNextCacheStart,
    abortStreamingSession,
    abortPlaybackPipeline,
    setActiveVideoSlot,
    clearVideoSlot,
    clearOwnedStreamCacheRange,
    setVideoObjectUrl,
    recordCacheRange,
    liveVideoUrl,
    savePipelineLiveSegment,
    setOwnedStreamCacheRange,
    setPreviewPlayingState,
    setPreviewSecondsState,
    requestPngFrame,
  ]);

  useEffect(() => {
    if (!timelineId || !hasServerInfo) return;
    if (playing) return;
    requestPngFrame(true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    timelineId,
    hasServerInfo,
    info?.width,
    info?.height,
    info?.cacheKey,
    resolution.width,
    resolution.height,
    fps,
    seconds,
    playing,
  ]);

  useEffect(
    () => () => {
      stopRaf();
      if (playStartRafRef.current != null) {
        cancelAnimationFrame(playStartRafRef.current);
        playStartRafRef.current = null;
      }
      if (preloadTimerRef.current) clearTimeout(preloadTimerRef.current);
      clearAllVideo();
      if (imageObjectUrlRef.current) {
        URL.revokeObjectURL(imageObjectUrlRef.current);
        imageObjectUrlRef.current = null;
      }
    },
    [clearAllVideo, stopRaf],
  );

  const setSeconds = useCallback(
    (s: number) => {
      const next = clampSeconds(s);
      previewDebug("seek", {
        requested: s,
        next,
        wasPlaying: playingRef.current,
        previousSeconds: secondsRef.current,
        streamingSession: streamingSessionRef.current
          ? {
              id: streamingSessionRef.current.id,
              mode: streamingSessionRef.current.mode,
              start: streamingSessionRef.current.start,
              end: streamingSessionRef.current.end,
              bufferedSeconds: streamingSessionRef.current.bufferedSeconds,
              aborted: streamingSessionRef.current.controller.signal.aborted,
            }
          : null,
      });
      if (playingRef.current) {
        const stoppedSeconds = currentPlaybackSeconds();
        setPreviewSecondsState(stoppedSeconds);
        setPreviewPlayingState(false);
        stopPlayback(true);
      } else {
        const session = streamingSessionRef.current;
        const keepStoppedStream =
          session?.mode === "stopped" &&
          !session.controller.signal.aborted &&
          session.start <= next + EPSILON &&
          session.start + session.bufferedSeconds >= next - EPSILON;
        if (keepStoppedStream) {
          if (session.stoppedTargetEnd != null) {
            session.stoppedTargetEnd = Math.max(
              session.stoppedTargetEnd,
              clampSeconds(next + STOPPED_STREAM_CACHE_SECONDS),
            );
          }
          clearVideoSlot(session.slot === 0 ? 1 : 0);
        } else {
          abortStreamingSession(false);
          clearVideoSlot(0);
          clearVideoSlot(1);
        }
      }
      setPreviewSecondsState(next);
      immediatePreloadRef.current = true;
    },
    [
      clampSeconds,
      currentPlaybackSeconds,
      stopPlayback,
      abortStreamingSession,
      clearVideoSlot,
      setPreviewPlayingState,
      setPreviewSecondsState,
    ],
  );

  const togglePlay = useCallback(() => {
    if (!timelineId || !hasServerInfo) return;
    previewDebug("toggle-play", {
      playing: playingRef.current,
      seconds: secondsRef.current,
      playback: playbackRef.current,
      stream: streamingSessionRef.current
        ? {
            id: streamingSessionRef.current.id,
            mode: streamingSessionRef.current.mode,
            start: streamingSessionRef.current.start,
            end: streamingSessionRef.current.end,
            bufferedSeconds: streamingSessionRef.current.bufferedSeconds,
            aborted: streamingSessionRef.current.controller.signal.aborted,
          }
        : null,
      pipeline: playbackPipelineRef.current?.id ?? null,
    });
    if (!playingRef.current) {
      displayTokenRef.current += 1;
      if (preloadTimerRef.current) {
        clearTimeout(preloadTimerRef.current);
        preloadTimerRef.current = null;
      }
      // Unmute within the play gesture so the browser's autoplay policy lets the
      // (now A/V) stream play with sound. The `<video>` mounts `muted` for safe
      // gesture-less buffering; React doesn't re-apply that attribute after
      // mount, so flipping it here sticks for the rest of the session.
      if (videoARef.current) videoARef.current.muted = false;
      if (videoBRef.current) videoBRef.current.muted = false;
      setPreviewPlayingState(true);
      if (playStartRafRef.current != null) {
        cancelAnimationFrame(playStartRafRef.current);
      }
      playStartRafRef.current = requestAnimationFrame(() => startVideoPlayback());
    } else {
      const stoppedSeconds = currentPlaybackSeconds();
      setPreviewSecondsState(stoppedSeconds);
      setPreviewPlayingState(false);
      stopPlayback(true, true);
      immediatePreloadRef.current = true;
    }
  }, [
    timelineId,
    hasServerInfo,
    startVideoPlayback,
    currentPlaybackSeconds,
    stopPlayback,
    setPreviewPlayingState,
    setPreviewSecondsState,
  ]);

  const stepFrame = useCallback(
    (delta: number) => {
      if (!timelineId || delta === 0) return;
      const step = 1 / Math.max(fps, 1);
      const cur = secondsRef.current;
      // Snap to the frame grid for the current fps in the press direction: from
      // a non-aligned (scrubbed) time, the first press lands on the nearest
      // boundary; from an aligned time it advances exactly one frame. The small
      // epsilon absorbs float error so an already-aligned position isn't treated
      // as just-past/just-before a boundary. Bounds are handled by setSeconds.
      const frames =
        delta > 0
          ? Math.floor(cur / step + 1e-6) + delta
          : Math.ceil(cur / step - 1e-6) + delta;
      setSeconds(frames * step);
    },
    [timelineId, fps, setSeconds],
  );

  const rewindToStart = useCallback(() => {
    setSeconds(0);
  }, [setSeconds]);

  return {
    state: {
      seconds,
      playing,
      cacheRanges: visibleCacheRanges,
      error,
      imageSrc,
      imageVisible,
      videoVisible,
      activeVideoSlot,
    },
    videoRefs: [videoARef, videoBRef],
    videoRef: videoARef,
    imgRef,
    setSeconds,
    togglePlay,
    stepFrame,
    rewindToStart,
  };
}

async function loadUncachedMediaObjectUrl(url: string): Promise<string> {
  const response = await fetch(url, { cache: "no-store" });
  if (!response.ok) {
    throw new Error(`${url} failed: ${response.status}`);
  }
  return URL.createObjectURL(await response.blob());
}

function selectMp4MimeType(): string {
  for (const mimeType of MP4_MIME_TYPES) {
    if (MediaSource.isTypeSupported(mimeType)) return mimeType;
  }
  return "video/mp4";
}

function waitForMediaSourceOpen(mediaSource: MediaSource): Promise<void> {
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

function waitForVideoMetadata(video: HTMLVideoElement): Promise<void> {
  if (video.readyState >= HTMLMediaElement.HAVE_METADATA) {
    return Promise.resolve();
  }
  return new Promise((resolve, reject) => {
    const onLoadedMetadata = () => {
      cleanup();
      resolve();
    };
    const onError = () => {
      cleanup();
      reject(new Error("video metadata failed"));
    };
    const cleanup = () => {
      video.removeEventListener("loadedmetadata", onLoadedMetadata);
      video.removeEventListener("error", onError);
    };
    video.addEventListener("loadedmetadata", onLoadedMetadata);
    video.addEventListener("error", onError);
  });
}

function waitForVideoCurrentData(video: HTMLVideoElement): Promise<void> {
  if (video.readyState >= HTMLMediaElement.HAVE_CURRENT_DATA) {
    return Promise.resolve();
  }
  return new Promise((resolve, reject) => {
    const onReady = () => {
      cleanup();
      resolve();
    };
    const onError = () => {
      cleanup();
      reject(new Error("video frame failed"));
    };
    // Bound the wait: if neither `loadeddata`/`canplay` nor `error` arrives (a
    // stuck seek can leave the element at HAVE_METADATA indefinitely), proceed to
    // the play() attempt rather than stranding the start sequence forever.
    const timer = setTimeout(onReady, 1500);
    const cleanup = () => {
      clearTimeout(timer);
      video.removeEventListener("loadeddata", onReady);
      video.removeEventListener("canplay", onReady);
      video.removeEventListener("error", onError);
    };
    video.addEventListener("loadeddata", onReady, { once: true });
    video.addEventListener("canplay", onReady, { once: true });
    video.addEventListener("error", onError, { once: true });
  });
}

function bufferedContains(buffered: TimeRanges, seconds: number): boolean {
  for (let i = 0; i < buffered.length; i++) {
    if (buffered.start(i) <= seconds + EPSILON && buffered.end(i) > seconds + EPSILON) {
      return true;
    }
  }
  return false;
}

// The buffered range that brackets `seconds` (within EPSILON), or null. Used to
// clamp a seek target up to the range's true start: `bufferedContains` accepts a
// target up to EPSILON before the range, but actually seeking a hair before the
// first buffered sample makes some browsers (Arc / older Chromium) sit at
// `seeking = true` forever, waiting for data that isn't there — `seeked` never
// fires and playback never starts.
function bufferedRangeContaining(
  buffered: TimeRanges,
  seconds: number,
): { start: number; end: number } | null {
  for (let i = 0; i < buffered.length; i++) {
    const start = buffered.start(i);
    const end = buffered.end(i);
    if (start <= seconds + EPSILON && end > seconds + EPSILON) {
      return { start, end };
    }
  }
  return null;
}

// Resolve once the element has settled after a seek. `seeked` is the happy path,
// but it can never arrive when the seek target sits at the very edge of a
// freshly-appended MSE buffer (observed in Arc); `canplay`/`loadeddata` (data is
// ready at the new position) and a hard timeout keep that from stranding the
// start sequence forever. Never rejects — a missing `seeked` should degrade to
// "try to play anyway", not a dead playback.
function waitForVideoSettledAfterSeek(video: HTMLVideoElement): Promise<void> {
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
    const timer = setTimeout(finish, 1500);
    video.addEventListener("seeked", finish, { once: true });
    video.addEventListener("canplay", finish, { once: true });
    video.addEventListener("loadeddata", finish, { once: true });
  });
}

async function waitForSourceBufferIdle(sourceBuffer: SourceBuffer): Promise<void> {
  if (!sourceBuffer.updating) return;
  await new Promise<void>((resolve) => {
    sourceBuffer.addEventListener("updateend", () => resolve(), { once: true });
  });
}

async function prepareSourceBufferSegment(
  sourceBuffer: SourceBuffer,
  start: number,
  end: number,
): Promise<void> {
  await waitForSourceBufferIdle(sourceBuffer);
  sourceBuffer.timestampOffset = start;
  sourceBuffer.appendWindowStart = Math.max(0, start - EPSILON);
  sourceBuffer.appendWindowEnd = end + EPSILON;
}

async function appendTimestampedSourceBuffer(
  sourceBuffer: SourceBuffer,
  start: number,
  end: number,
  data: ArrayBuffer,
): Promise<void> {
  await prepareSourceBufferSegment(sourceBuffer, start, end);
  await appendSourceBuffer(sourceBuffer, data);
}

function appendSourceBuffer(
  sourceBuffer: SourceBuffer,
  data: ArrayBuffer,
): Promise<void> {
  if (sourceBuffer.updating) {
    return new Promise<void>((resolve) => {
      sourceBuffer.addEventListener("updateend", () => resolve(), {
        once: true,
      });
    }).then(() => appendSourceBuffer(sourceBuffer, data));
  }
  return new Promise((resolve, reject) => {
    const cleanup = () => {
      sourceBuffer.removeEventListener("updateend", onUpdateEnd);
      sourceBuffer.removeEventListener("error", onError);
    };
    const onUpdateEnd = () => {
      cleanup();
      resolve();
    };
    const onError = () => {
      cleanup();
      reject(new Error("SourceBuffer append failed"));
    };
    sourceBuffer.addEventListener("updateend", onUpdateEnd);
    sourceBuffer.addEventListener("error", onError);
    try {
      sourceBuffer.appendBuffer(data);
    } catch (e) {
      cleanup();
      reject(e);
    }
  });
}

function sourceBufferEnd(sourceBuffer: SourceBuffer): number {
  const buffered = sourceBuffer.buffered;
  if (buffered.length === 0) return 0;
  return buffered.end(buffered.length - 1);
}

function isAbortError(e: unknown): boolean {
  return e instanceof DOMException && e.name === "AbortError";
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => window.setTimeout(resolve, ms));
}

function previewDebug(
  event: string,
  fields: Record<string, unknown> = {},
): void {
  if (!previewDebugEnabled()) return;
  const at =
    typeof performance !== "undefined"
      ? Math.round(performance.now() * 10) / 10
      : Date.now();
  console.debug(`[tellur-live preview] ${event}`, { at, ...fields });
}

function previewDebugEnabled(): boolean {
  if (typeof window === "undefined") return false;
  try {
    const params = new URLSearchParams(window.location.search);
    const value =
      params.get("debug") ??
      params.get("previewDebug") ??
      params.get("tellurDebug");
    if (value === "1" || value === "true" || value === "preview") {
      return true;
    }
    return window.localStorage.getItem(DEBUG_STORAGE_KEY) === "1";
  } catch {
    return false;
  }
}

function bufferedDebug(buffered: TimeRanges): CacheRange[] {
  const ranges: CacheRange[] = [];
  for (let i = 0; i < buffered.length; i++) {
    ranges.push({
      start: buffered.start(i),
      end: buffered.end(i),
    });
  }
  return ranges;
}
