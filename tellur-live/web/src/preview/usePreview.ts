import { useCallback, useEffect, useRef, useState } from "react";
import { frameUrl, videoUrl } from "../api";
import {
  cacheScopeKey,
  loadCacheRanges,
  mergeCacheRange,
  revokeStaleCacheRanges,
  saveCacheRanges,
} from "../cache";
import type { CacheRange, ServerInfo, TimelineInfo } from "../types";

const PRELOAD_DELAY_MS = 260;

export interface PreviewState {
  seconds: number;
  playing: boolean;
  cacheRanges: CacheRange[];
  error: string | null;
  imageSrc: string | null;
  imageVisible: boolean;
  videoVisible: boolean;
}

export interface PreviewControls {
  state: PreviewState;
  videoRef: React.RefObject<HTMLVideoElement>;
  imgRef: React.RefObject<HTMLImageElement>;
  setSeconds: (s: number) => void;
  togglePlay: () => void;
  stepFrame: (delta: number) => void;
  rewindToStart: () => void;
}

export interface PreviewSettings {
  info: ServerInfo | null;
  timeline: TimelineInfo | null;
  scale: number;
  fps: number;
}

// Drives the preview pane: PNG fetches for stills/seek, fragmented MP4 for
// playback, plus client-side bookkeeping of the cache range visualised on
// the timeline ruler.
export function usePreview(settings: PreviewSettings): PreviewControls {
  const { info, timeline, scale, fps } = settings;
  const hasServerInfo = info != null;
  const timelineId = timeline?.id ?? "";
  const timelineDuration = timeline?.duration ?? 0;
  const sourceWidth = info?.width ?? 1;
  const sourceHeight = info?.height ?? 1;
  const pluginCacheKey = info?.cacheKey ?? "";

  const videoRef = useRef<HTMLVideoElement>(null);
  const imgRef = useRef<HTMLImageElement>(null);

  const [seconds, setSecondsState] = useState(0);
  const [playing, setPlaying] = useState(false);
  const [cacheRanges, setCacheRanges] = useState<CacheRange[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [imageSrc, setImageSrc] = useState<string | null>(null);
  const [imageVisible, setImageVisible] = useState(true);
  const [videoVisible, setVideoVisible] = useState(false);

  const displayTokenRef = useRef(0);
  const pngTokenRef = useRef(0);
  const preloadTokenRef = useRef(0);
  const pendingPngRef = useRef(false);
  const queuedPngRef = useRef(false);
  const preloadTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const preloadedKeyRef = useRef("");
  const playbackStartRef = useRef(0);
  const playbackTokenRef = useRef(0);
  const rafRef = useRef<number | null>(null);
  const videoBaseSecondsRef = useRef(0);
  const cacheScopeRef = useRef("");
  const lastCacheKeyRef = useRef("");
  const immediatePreloadRef = useRef(false);

  const resolvedResolution = useCallback(() => {
    const s = Math.max(scale, 0.01);
    return {
      width: Math.max(1, Math.round(sourceWidth * s)),
      height: Math.max(1, Math.round(sourceHeight * s)),
    };
  }, [sourceWidth, sourceHeight, scale]);

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
    });
  }, [timelineId, pluginCacheKey, resolvedResolution, fps, videoGop]);

  const videoKey = useCallback(
    (t: number): string => {
      if (!timelineId || !pluginCacheKey) return "";
      const r = resolvedResolution();
      return [
        pluginCacheKey,
        timelineId,
        t.toFixed(4),
        `${r.width}x${r.height}`,
        String(fps),
        String(videoGop()),
        "23",
      ].join("|");
    },
    [timelineId, pluginCacheKey, fps, resolvedResolution, videoGop],
  );

  const recordCacheRanges = useCallback((ranges: CacheRange[]) => {
    if (ranges.length === 0) return;
    setCacheRanges((prev) => {
      let next = prev;
      for (const range of ranges) {
        next = mergeCacheRange(next, range.start, range.end);
      }
      saveCacheRanges(cacheScopeRef.current, next);
      return next;
    });
  }, []);

  const recordCacheRange = useCallback(
    (start: number, end: number) => {
      recordCacheRanges([{ start, end }]);
    },
    [recordCacheRanges],
  );

  // Track <video>.buffered.end(0) and merge it into cacheRanges so the
  // green strip on the cursor row mirrors what the browser actually has
  // ready for playback (preload or in-progress stream).
  useEffect(() => {
    const video = videoRef.current;
    if (!video) return;
    const onProgress = () => {
      if (video.buffered.length === 0) return;
      const base = videoBaseSecondsRef.current;
      const ranges: CacheRange[] = [];
      for (let i = 0; i < video.buffered.length; i++) {
        const start = video.buffered.start(i);
        const end = video.buffered.end(i);
        if (Number.isFinite(start) && Number.isFinite(end) && end > start) {
          ranges.push({ start: base + start, end: base + end });
        }
      }
      recordCacheRanges(ranges);
    };
    video.addEventListener("progress", onProgress);
    video.addEventListener("loadeddata", onProgress);
    return () => {
      video.removeEventListener("progress", onProgress);
      video.removeEventListener("loadeddata", onProgress);
    };
  }, [recordCacheRanges]);

  const clearVideoPreload = useCallback(() => {
    if (preloadTimerRef.current) {
      clearTimeout(preloadTimerRef.current);
      preloadTimerRef.current = null;
    }
    const video = videoRef.current;
    if (!video) return;
    if (!video.src && !preloadedKeyRef.current) return;
    video.pause();
    video.removeAttribute("src");
    try {
      video.load();
    } catch {
      // ignore
    }
    preloadedKeyRef.current = "";
    preloadTokenRef.current += 1;
  }, []);

  const stopRaf = useCallback(() => {
    if (rafRef.current != null) {
      cancelAnimationFrame(rafRef.current);
      rafRef.current = null;
    }
  }, []);

  useEffect(() => {
    const scope = currentCacheScope();
    const cacheKeyChanged =
      Boolean(pluginCacheKey) && lastCacheKeyRef.current !== pluginCacheKey;
    lastCacheKeyRef.current = pluginCacheKey;
    immediatePreloadRef.current = cacheKeyChanged;
    cacheScopeRef.current = scope;
    setCacheRanges(scope ? loadCacheRanges(scope) : []);
    revokeStaleCacheRanges(pluginCacheKey);
    clearVideoPreload();
  }, [currentCacheScope, pluginCacheKey, clearVideoPreload]);

  const requestPngFrame = useCallback(
    (force: boolean = false) => {
      if (!timelineId || !hasServerInfo || !pluginCacheKey) return;
      if (pendingPngRef.current) {
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
      const res = resolvedResolution();
      const url = frameUrl({
        timelineId,
        time: seconds,
        width: res.width,
        height: res.height,
        fps,
        cacheKey: pluginCacheKey,
      });
      const img = new Image();
      img.onload = () => {
        if (id === displayTokenRef.current) {
          setImageSrc(url);
          setImageVisible(true);
          setVideoVisible(false);
          setError(null);
          recordCacheRange(seconds, seconds + 1 / Math.max(fps, 1));
        }
        finish();
      };
      img.onerror = () => {
        if (id === displayTokenRef.current) {
          setError("frame request failed");
        }
        finish();
      };
      img.src = url;

      function finish() {
        if (pngId === pngTokenRef.current) pendingPngRef.current = false;
        if (queuedPngRef.current && !playing) {
          queuedPngRef.current = false;
          requestPngFrame(true);
        } else if (!playing) {
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
      seconds,
      fps,
      playing,
      recordCacheRange,
    ],
  );

  const schedulePreload = useCallback(
    (delay: number = PRELOAD_DELAY_MS) => {
      if (preloadTimerRef.current) clearTimeout(preloadTimerRef.current);
      if (!timelineId || !hasServerInfo || !pluginCacheKey || playing) return;
      preloadTimerRef.current = setTimeout(() => {
        const video = videoRef.current;
        if (!video || !timelineId || !hasServerInfo || !pluginCacheKey || playing) return;
        const key = videoKey(seconds);
        if (key && key === preloadedKeyRef.current && video.src) return;
        preloadTokenRef.current += 1;
        preloadedKeyRef.current = key;
        const r = resolvedResolution();
        video.pause();
        video.onloadeddata = null;
        video.onerror = null;
        video.onended = null;
        videoBaseSecondsRef.current = seconds;
        video.src = videoUrl({
          timelineId,
          time: seconds,
          width: r.width,
          height: r.height,
          fps,
          gop: videoGop(),
          crf: 23,
          cacheKey: pluginCacheKey,
        });
        try {
          video.load();
        } catch {
          // ignore
        }
      }, delay);
    },
    [
      timelineId,
      hasServerInfo,
      pluginCacheKey,
      playing,
      videoKey,
      resolvedResolution,
      seconds,
      fps,
      videoGop,
    ],
  );

  const startVideoPlayback = useCallback(() => {
    const video = videoRef.current;
    if (!video || !timelineId || !hasServerInfo || !pluginCacheKey) return;
    stopRaf();
    if (preloadTimerRef.current) {
      clearTimeout(preloadTimerRef.current);
      preloadTimerRef.current = null;
    }
    const token = ++displayTokenRef.current;
    const playbackToken = ++playbackTokenRef.current;
    const startSeconds = seconds;
    playbackStartRef.current = startSeconds;
    videoBaseSecondsRef.current = startSeconds;
    const key = videoKey(startSeconds);
    const r = resolvedResolution();
    const url = videoUrl({
      timelineId,
      time: startSeconds,
      width: r.width,
      height: r.height,
      fps,
      gop: videoGop(),
      crf: 23,
      cacheKey: pluginCacheKey,
    });

    video.onloadeddata = () => {
      if (token === displayTokenRef.current) {
        setImageVisible(false);
        setVideoVisible(true);
      }
    };
    video.onerror = () => {
      if (token === displayTokenRef.current) setError("video stream failed");
    };
    video.onended = () => {
      if (token !== displayTokenRef.current) return;
      setPlaying(false);
      const dur = timelineDuration;
      const end = Math.min(startSeconds + video.currentTime, dur);
      setSecondsState(end);
      setVideoVisible(false);
      setImageVisible(true);
      recordCacheRange(startSeconds, end);
    };

    if (key !== preloadedKeyRef.current || !video.src || video.error) {
      preloadedKeyRef.current = key;
      video.src = url;
    }
    if (video.readyState >= HTMLMediaElement.HAVE_CURRENT_DATA) {
      setImageVisible(false);
      setVideoVisible(true);
    }
    video.play().catch((e) => {
      if (token === displayTokenRef.current) setError(String(e));
    });

    const tick = () => {
      if (playbackToken !== playbackTokenRef.current) return;
      const v = videoRef.current;
      if (!v) return;
      const dur = timelineDuration;
      const next = Math.min(startSeconds + v.currentTime, dur);
      setSecondsState(next);
      recordCacheRange(startSeconds, next);
      rafRef.current = requestAnimationFrame(tick);
    };
    rafRef.current = requestAnimationFrame(tick);
  }, [
    timelineId,
    timelineDuration,
    hasServerInfo,
    pluginCacheKey,
    seconds,
    videoKey,
    resolvedResolution,
    fps,
    videoGop,
    stopRaf,
    recordCacheRange,
  ]);

  // Re-render the preview whenever seconds / scale / fps / timeline change
  // and we're not actively playing. During playback the <video> element
  // owns the display, so we skip PNG refetches.
  useEffect(() => {
    if (!timelineId || !hasServerInfo) return;
    if (playing) return;
    requestPngFrame(true);
    // We deliberately leave requestPngFrame out of deps — it captures
    // seconds and we re-trigger via the explicit dep list below.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    timelineId,
    hasServerInfo,
    info?.width,
    info?.height,
    info?.cacheKey,
    scale,
    fps,
    seconds,
    playing,
  ]);

  // Stop RAF on unmount.
  useEffect(() => () => stopRaf(), [stopRaf]);

  const setSeconds = useCallback((s: number) => {
    setSecondsState(s);
    if (playing) {
      setPlaying(false);
      stopRaf();
      const video = videoRef.current;
      if (video) video.pause();
    }
    clearVideoPreload();
  }, [playing, clearVideoPreload, stopRaf]);

  const togglePlay = useCallback(() => {
    if (!timelineId || !hasServerInfo) return;
    if (!playing) {
      setPlaying(true);
      // start playback after state flush
      requestAnimationFrame(() => startVideoPlayback());
    } else {
      setPlaying(false);
      stopRaf();
      const video = videoRef.current;
      if (video) {
        const end = Math.min(
          playbackStartRef.current + video.currentTime,
          timelineDuration,
        );
        setSecondsState(end);
        recordCacheRange(playbackStartRef.current, end);
        video.pause();
      }
      setVideoVisible(false);
      setImageVisible(true);
    }
  }, [
    timelineId,
    timelineDuration,
    hasServerInfo,
    playing,
    startVideoPlayback,
    stopRaf,
    recordCacheRange,
  ]);

  const stepFrame = useCallback(
    (delta: number) => {
      if (!timelineId) return;
      const step = 1 / Math.max(fps, 1);
      const next = Math.max(
        0,
        Math.min(timelineDuration, seconds + delta * step),
      );
      setSeconds(next);
    },
    [timelineId, timelineDuration, fps, seconds, setSeconds],
  );

  const rewindToStart = useCallback(() => {
    setSeconds(0);
  }, [setSeconds]);

  return {
    state: {
      seconds,
      playing,
      cacheRanges,
      error,
      imageSrc,
      imageVisible,
      videoVisible,
    },
    videoRef: videoRef as React.RefObject<HTMLVideoElement>,
    imgRef: imgRef as React.RefObject<HTMLImageElement>,
    setSeconds,
    togglePlay,
    stepFrame,
    rewindToStart,
  };
}
