import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { RefObject } from "react";
import { frameUrl, videoUrl } from "../api";
import { createMediaCache, groupKeyOf, type MediaCache } from "../mediaCache";
import { TimelinePlayer } from "./TimelinePlayer";
import type {
  CacheRange,
  PreviewResolution,
  ServerInfo,
  TimelineInfo,
} from "../types";

const EPSILON = 0.001;
const CRF = 23;

export interface PreviewState {
  seconds: number;
  playing: boolean;
  cacheRanges: CacheRange[];
  error: string | null;
  imageSrc: string | null;
  imageVisible: boolean;
}

export interface PreviewControls {
  state: PreviewState;
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

// Thin React shell over TimelinePlayer (which owns all MSE/cache logic). The hook's
// only jobs are: own the stable <video>/<img> elements, recreate the player when the
// plugin/resolution/fps/timeline "group" changes, mirror the player's events into
// React state, and fetch the paused/seek PNG still the player asks it to display.
export function usePreview(settings: PreviewSettings): PreviewControls {
  const { info, timeline, resolution, fps } = settings;
  const pluginKey = info?.cacheKey ?? "";
  const timelineId = timeline?.id ?? "";
  const duration = timeline?.duration ?? 0;
  const width = Math.max(1, Math.round(resolution.width));
  const height = Math.max(1, Math.round(resolution.height));
  const gop = Math.max(1, Math.floor(fps / 4));

  const groupKey = useMemo(
    () =>
      groupKeyOf({ pluginKey, timelineId, width, height, fps, gop, crf: CRF }),
    [pluginKey, timelineId, width, height, fps, gop],
  );

  const videoRef = useRef<HTMLVideoElement>(null);
  const imgRef = useRef<HTMLImageElement>(null);

  const [seconds, setSecondsState] = useState(0);
  const [playing, setPlaying] = useState(false);
  const [cacheRanges, setCacheRanges] = useState<CacheRange[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [imageSrc, setImageSrc] = useState<string | null>(null);
  const [imageVisible, setImageVisible] = useState(true);

  const secondsRef = useRef(0);
  const playingRef = useRef(false);
  const playerRef = useRef<TimelinePlayer | null>(null);

  const cacheRef = useRef<MediaCache | null>(null);
  if (!cacheRef.current) cacheRef.current = createMediaCache();

  // PNG still-frame state. A monotonic token coalesces rapid scrub requests to the
  // latest, and the object URL is revoked only after the next one is swapped in.
  const stillTokenRef = useRef(0);
  const stillUrlRef = useRef<string | null>(null);

  const setSeconds = useCallback((value: number) => {
    secondsRef.current = value;
    setSecondsState(value);
  }, []);

  // (Re)create the player whenever the group identity changes. groupKey folds the
  // plugin cacheKey, timeline, resolution, fps and encode params — so an unchanged
  // plugin reload (same cacheKey string from the SSE churn) does NOT recreate it,
  // while a real plugin/resolution/fps change tears down + rebuilds MSE.
  useEffect(() => {
    const video = videoRef.current;
    if (!video || !groupKey || !pluginKey || !timelineId || duration <= 0) {
      return;
    }
    let cancelled = false;
    const cache = cacheRef.current!;

    const buildVideoUrl = (start: number, end: number): string => {
      const segmentDuration =
        end < duration - EPSILON ? Math.max(0, end - start) : undefined;
      return videoUrl({
        timelineId,
        time: start,
        width,
        height,
        fps,
        gop,
        crf: CRF,
        duration: segmentDuration,
        cacheKey: pluginKey,
      });
    };

    const requestStill = (time: number) => {
      const clamped = Math.min(
        Math.max(0, time),
        Math.max(0, duration - EPSILON),
      );
      const url = frameUrl({
        timelineId,
        time: clamped,
        width,
        height,
        fps,
        cacheKey: pluginKey,
      });
      const token = ++stillTokenRef.current;
      void fetch(url, { cache: "no-store" })
        .then((response) => {
          if (!response.ok) throw new Error(`${url} failed: ${response.status}`);
          return response.blob();
        })
        .then((blob) => {
          if (token !== stillTokenRef.current || cancelled) return;
          const objectUrl = URL.createObjectURL(blob);
          // Decode in a throwaway Image first so a slow/broken frame never clobbers a
          // newer one and we never flash a half-decoded image.
          const image = new Image();
          image.onload = () => {
            if (token !== stillTokenRef.current || cancelled) {
              URL.revokeObjectURL(objectUrl);
              return;
            }
            const previous = stillUrlRef.current;
            stillUrlRef.current = objectUrl;
            setImageSrc(objectUrl);
            if (previous && previous !== objectUrl) URL.revokeObjectURL(previous);
          };
          image.onerror = () => URL.revokeObjectURL(objectUrl);
          image.src = objectUrl;
        })
        .catch(() => {
          // A failed still leaves the previous frame up; the player surfaces real errors.
        });
    };

    void cache.activatePlugin(pluginKey).then(() => {
      if (cancelled) return;
      const player = new TimelinePlayer(
        video,
        {
          groupKey,
          pluginKey,
          duration,
          initialPosition: secondsRef.current,
          videoUrl: buildVideoUrl,
          cache,
        },
        {
          onTime: (s) => setSeconds(s),
          onRanges: (ranges) => setCacheRanges(ranges),
          onPlaying: (value) => {
            playingRef.current = value;
            setPlaying(value);
          },
          onError: (message) => setError(message),
          onEnded: () => {
            // The player already parked the playhead + emitted the still; nothing to do.
          },
          onDisplayMode: (mode, stillTime) => {
            if (mode === "video") {
              setImageVisible(false);
            } else {
              requestStill(stillTime);
              setImageVisible(true);
            }
          },
        },
      );
      playerRef.current = player;
    });

    return () => {
      cancelled = true;
      const player = playerRef.current;
      playerRef.current = null;
      void player?.dispose();
    };
  }, [groupKey, pluginKey, timelineId, duration, width, height, fps, gop, setSeconds]);

  // Revoke the last still URL on unmount.
  useEffect(
    () => () => {
      if (stillUrlRef.current) {
        URL.revokeObjectURL(stillUrlRef.current);
        stillUrlRef.current = null;
      }
    },
    [],
  );

  const seekTo = useCallback(
    (value: number) => {
      const target = clamp(value, duration);
      const player = playerRef.current;
      if (player) {
        player.seek(target);
      } else {
        setSeconds(target);
      }
    },
    [duration, setSeconds],
  );

  const togglePlay = useCallback(() => {
    const player = playerRef.current;
    if (!player) return;
    // Runs inside the click/keydown gesture so play() can unmute for the autoplay policy.
    if (playingRef.current) player.pause();
    else player.play();
  }, []);

  const stepFrame = useCallback(
    (delta: number) => {
      if (delta === 0 || duration <= 0) return;
      const step = 1 / Math.max(fps, 1);
      const cur = secondsRef.current;
      // Snap to the frame grid in the press direction: from a scrubbed time the first
      // press lands on the nearest boundary; from an aligned time it advances one frame.
      const frames =
        delta > 0
          ? Math.floor(cur / step + 1e-6) + delta
          : Math.ceil(cur / step - 1e-6) + delta;
      seekTo(frames * step);
    },
    [duration, fps, seekTo],
  );

  const rewindToStart = useCallback(() => seekTo(0), [seekTo]);

  return {
    state: {
      seconds,
      playing,
      cacheRanges,
      error,
      imageSrc,
      imageVisible,
    },
    videoRef,
    imgRef,
    setSeconds: seekTo,
    togglePlay,
    stepFrame,
    rewindToStart,
  };
}

function clamp(value: number, duration: number): number {
  return Math.max(0, Math.min(Number.isFinite(value) ? value : 0, duration));
}
