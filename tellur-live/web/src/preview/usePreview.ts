import { useCallback, useEffect, useRef, useState } from "react";
import { frameUrl, videoUrl } from "../api";
import { mergeCacheRange } from "../cache";
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

  const resolvedResolution = useCallback(() => {
    if (!info) return { width: 1, height: 1 };
    const s = Math.max(scale, 0.01);
    return {
      width: Math.max(1, Math.round(info.width * s)),
      height: Math.max(1, Math.round(info.height * s)),
    };
  }, [info, scale]);

  const videoGop = useCallback(() => Math.max(1, Math.floor(fps / 4)), [fps]);

  const videoKey = useCallback(
    (t: number): string => {
      if (!timeline || !info) return "";
      const r = resolvedResolution();
      return [
        timeline.id,
        t.toFixed(4),
        `${r.width}x${r.height}`,
        String(fps),
        String(videoGop()),
        "23",
      ].join("|");
    },
    [timeline, info, fps, resolvedResolution, videoGop],
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
      setCacheRanges((prev) => {
        let next = prev;
        for (let i = 0; i < video.buffered.length; i++) {
          const start = video.buffered.start(i);
          const end = video.buffered.end(i);
          if (Number.isFinite(start) && Number.isFinite(end) && end > start) {
            next = mergeCacheRange(next, base + start, base + end);
          }
        }
        return next;
      });
    };
    video.addEventListener("progress", onProgress);
    video.addEventListener("loadeddata", onProgress);
    return () => {
      video.removeEventListener("progress", onProgress);
      video.removeEventListener("loadeddata", onProgress);
    };
  }, []);

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

  const requestPngFrame = useCallback(
    (force: boolean = false) => {
      if (!timeline || !info) return;
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
      const url = frameUrl(
        {
          timelineId: timeline.id,
          time: seconds,
          width: res.width,
          height: res.height,
          fps,
        },
        id,
      );
      const img = new Image();
      img.onload = () => {
        if (id === displayTokenRef.current) {
          setImageSrc(url);
          setImageVisible(true);
          setVideoVisible(false);
          setError(null);
          setCacheRanges((prev) =>
            mergeCacheRange(prev, seconds, seconds + 1 / Math.max(fps, 1)),
          );
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
          schedulePreload();
        }
      }
    },
    [timeline, info, resolvedResolution, seconds, fps, playing],
  );

  const schedulePreload = useCallback(
    (delay: number = PRELOAD_DELAY_MS) => {
      if (preloadTimerRef.current) clearTimeout(preloadTimerRef.current);
      if (!timeline || !info || playing) return;
      preloadTimerRef.current = setTimeout(() => {
        const video = videoRef.current;
        if (!video || !timeline || !info || playing) return;
        const key = videoKey(seconds);
        if (key && key === preloadedKeyRef.current && video.src) return;
        const token = ++preloadTokenRef.current;
        preloadedKeyRef.current = key;
        const r = resolvedResolution();
        video.pause();
        video.onloadeddata = null;
        video.onerror = null;
        video.onended = null;
        videoBaseSecondsRef.current = seconds;
        video.src = videoUrl(
          {
            timelineId: timeline.id,
            time: seconds,
            width: r.width,
            height: r.height,
            fps,
            gop: videoGop(),
            crf: 23,
          },
          token,
        );
        try {
          video.load();
        } catch {
          // ignore
        }
      }, delay);
    },
    [timeline, info, playing, videoKey, resolvedResolution, seconds, fps, videoGop],
  );

  const startVideoPlayback = useCallback(() => {
    const video = videoRef.current;
    if (!video || !timeline || !info) return;
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
    const url = videoUrl(
      {
        timelineId: timeline.id,
        time: startSeconds,
        width: r.width,
        height: r.height,
        fps,
        gop: videoGop(),
        crf: 23,
      },
      token,
    );

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
      const dur = timeline.duration;
      const end = Math.min(startSeconds + video.currentTime, dur);
      setSecondsState(end);
      setVideoVisible(false);
      setImageVisible(true);
      setCacheRanges((prev) => mergeCacheRange(prev, startSeconds, end));
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
      const dur = timeline.duration;
      const next = Math.min(startSeconds + v.currentTime, dur);
      setSecondsState(next);
      setCacheRanges((prev) => mergeCacheRange(prev, startSeconds, next));
      rafRef.current = requestAnimationFrame(tick);
    };
    rafRef.current = requestAnimationFrame(tick);
  }, [timeline, info, seconds, videoKey, resolvedResolution, fps, videoGop, stopRaf]);

  // Re-render the preview whenever seconds / scale / fps / timeline change
  // and we're not actively playing. During playback the <video> element
  // owns the display, so we skip PNG refetches.
  useEffect(() => {
    if (!timeline || !info) return;
    if (playing) return;
    requestPngFrame(true);
    // We deliberately leave requestPngFrame out of deps — it captures
    // seconds and we re-trigger via the explicit dep list below.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [timeline?.id, info?.width, info?.height, scale, fps, seconds, playing]);

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
    if (!timeline || !info) return;
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
          timeline.duration,
        );
        setSecondsState(end);
        video.pause();
      }
      setVideoVisible(false);
      setImageVisible(true);
    }
  }, [timeline, info, playing, startVideoPlayback, stopRaf]);

  const stepFrame = useCallback(
    (delta: number) => {
      if (!timeline) return;
      const step = 1 / Math.max(fps, 1);
      const next = Math.max(
        0,
        Math.min(timeline.duration, seconds + delta * step),
      );
      setSeconds(next);
    },
    [timeline, fps, seconds, setSeconds],
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
