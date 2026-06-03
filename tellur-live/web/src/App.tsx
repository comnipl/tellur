import { useCallback, useEffect, useRef, useState } from "react";
import { fetchInfo } from "./api";
import { Header } from "./components/Header";
import { Preview } from "./components/Preview";
import { PreviewScrubber } from "./components/PreviewScrubber";
import { TabsRow } from "./components/TabsRow";
import { Timeline } from "./components/Timeline";
import { TimelineViewportBar } from "./components/TimelineViewportBar";
import { Transport } from "./components/Transport";
import { usePreview } from "./preview/usePreview";
import { cleanupLegacyMediaCaches } from "./cache";
import {
  clampTimelineViewport,
  type TimelineViewport,
  type TimelineViewportChange,
} from "./timelineViewport";
import type { PreviewResolution, ServerInfo, TimelineInfo } from "./types";

const FALLBACK_TIMELINE: TimelineInfo = {
  id: "demo",
  title: "Demo Timeline",
  duration: 150,
  error: null,
};
const INFO_FALLBACK_POLL_MS = 2000;
const LEGACY_CACHE_RELOAD_KEY = "tellur-live:legacy-cache-reloaded";
const DEFAULT_PREVIEW_RESOLUTION: PreviewResolution = {
  width: 1280,
  height: 720,
};
const PREVIEW_RESOLUTION_OPTIONS = [
  { width: 3840, height: 2160, label: "3840 × 2160" },
  { width: 1920, height: 1080, label: "1920 × 1080" },
  { width: 1280, height: 720, label: "1280 × 720" },
  { width: 854, height: 480, label: "854 × 480" },
  { width: 640, height: 360, label: "640 × 360" },
  { width: 426, height: 240, label: "426 × 240" },
  { width: 2160, height: 3840, label: "2160 × 3840" },
  { width: 1080, height: 1920, label: "1080 × 1920" },
  { width: 720, height: 1280, label: "720 × 1280" },
  { width: 480, height: 854, label: "480 × 854" },
  { width: 360, height: 640, label: "360 × 640" },
  { width: 240, height: 426, label: "240 × 426" },
];

export function App() {
  const [info, setInfo] = useState<ServerInfo | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [resolution, setResolution] = useState<PreviewResolution>(
    DEFAULT_PREVIEW_RESOLUTION,
  );
  const [fps, setFps] = useState(30);
  const [timelineViewport, setTimelineViewport] = useState<TimelineViewport>({
    start: 0,
    zoom: 1,
  });
  const [measuredFps, setMeasuredFps] = useState(0);
  const fpsCounterRef = useRef({ frames: 0, last: performance.now() });
  const userSelectedResolutionRef = useRef(false);

  useEffect(() => {
    cleanupLegacyMediaCaches()
      .then((needsReload) => {
        if (
          !needsReload ||
          typeof window === "undefined" ||
          window.sessionStorage.getItem(LEGACY_CACHE_RELOAD_KEY)
        ) {
          return;
        }
        window.sessionStorage.setItem(LEGACY_CACHE_RELOAD_KEY, "1");
        window.location.reload();
      })
      .catch((e) => {
        console.warn("tellur-live legacy media cache cleanup failed", e);
      });
  }, []);

  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | null = null;
    let source: EventSource | null = null;

    const applyInfo = (next: ServerInfo) => {
      if (cancelled) return;
      setInfo((prev) => {
        if (!prev) {
          setFps((current) => Math.max(next.fps || current, 1));
          if (!userSelectedResolutionRef.current) {
            setResolution({ width: next.width, height: next.height });
          }
        }
        return next;
      });
      setLoadError(null);
    };

    const tick = async () => {
      try {
        applyInfo(await fetchInfo());
      } catch (e) {
        if (!cancelled) setLoadError(String(e));
      } finally {
        if (!cancelled) timer = setTimeout(tick, INFO_FALLBACK_POLL_MS);
      }
    };

    if ("EventSource" in window) {
      source = new EventSource("/api/events");
      source.addEventListener("info", (event: MessageEvent) => {
        try {
          applyInfo(JSON.parse(event.data) as ServerInfo);
        } catch (e) {
          if (!cancelled) setLoadError(String(e));
        }
      });
      source.onerror = () => {
        if (cancelled) return;
        source?.close();
        source = null;
        tick();
      };
    } else {
      tick();
    }

    return () => {
      cancelled = true;
      if (timer) clearTimeout(timer);
      source?.close();
    };
  }, []);

  const timeline = info?.timelines[0] ?? null;

  const preview = usePreview({
    info,
    timeline,
    resolution,
    fps,
  });

  useEffect(() => {
    const isInteractiveTarget = (target: EventTarget | null) => {
      if (!(target instanceof HTMLElement)) return false;
      return Boolean(
        target.closest(
          'button, input, textarea, select, a[href], [contenteditable="true"], [role="textbox"]',
        ),
      );
    };

    const handleKeyDown = (event: KeyboardEvent) => {
      if (
        event.defaultPrevented ||
        event.altKey ||
        event.ctrlKey ||
        event.metaKey ||
        isInteractiveTarget(event.target)
      ) {
        return;
      }

      if (
        event.code === "Space" ||
        event.key === " " ||
        event.key === "Spacebar"
      ) {
        event.preventDefault();
        if (!event.repeat) preview.togglePlay();
        return;
      }

      if (event.key === "ArrowLeft") {
        event.preventDefault();
        preview.stepFrame(-1);
        return;
      }

      if (event.key === "ArrowRight") {
        event.preventDefault();
        preview.stepFrame(1);
      }
    };

    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [preview.stepFrame, preview.togglePlay]);

  useEffect(() => {
    const counter = fpsCounterRef.current;
    if (!preview.state.playing) {
      counter.frames = 0;
      counter.last = performance.now();
      return;
    }
    counter.frames += 1;
    const now = performance.now();
    const dt = now - counter.last;
    if (dt > 500) {
      setMeasuredFps((counter.frames / dt) * 1000);
      counter.frames = 0;
      counter.last = now;
    }
  }, [preview.state.seconds, preview.state.playing]);

  const aspect = resolution.width / resolution.height;
  const displayTimeline = timeline ?? FALLBACK_TIMELINE;
  const timelineDuration = displayTimeline.duration;
  const resolutionOptions = previewResolutionOptions(resolution);
  const changeResolution = useCallback((next: PreviewResolution) => {
    userSelectedResolutionRef.current = true;
    setResolution(next);
  }, []);
  const updateTimelineViewport = useCallback(
    (next: TimelineViewportChange) => {
      setTimelineViewport((current) => {
        const nextViewport =
          typeof next === "function" ? next(current) : next;
        return clampTimelineViewport(nextViewport, timelineDuration);
      });
    },
    [timelineDuration],
  );

  useEffect(() => {
    setTimelineViewport((current) =>
      clampTimelineViewport(current, timelineDuration),
    );
  }, [timelineDuration]);

  const url =
    typeof window !== "undefined" && window.location.origin
      ? window.location.origin
      : "";

  return (
    <div className="app">
      <Header
        projectName="Project Name"
        url={url}
        compileStatus={
          loadError ? "disconnected" : info?.compileStatus ?? "compiled"
        }
        compileError={loadError ?? info?.compileError ?? null}
      />
      <div className="workspace">
        <section className="viewer-panel">
          <Preview
            imageSrc={preview.state.imageSrc}
            imageVisible={preview.state.imageVisible}
            videoVisible={preview.state.videoVisible}
            activeVideoSlot={preview.state.activeVideoSlot}
            aspect={aspect}
            error={loadError ?? info?.lastError ?? preview.state.error}
            videoRefs={preview.videoRefs}
            imgRef={preview.imgRef}
          />
          <PreviewScrubber
            seconds={preview.state.seconds}
            duration={displayTimeline.duration}
            onSeek={preview.setSeconds}
          />
          <Transport
            seconds={preview.state.seconds}
            duration={displayTimeline.duration}
            fps={fps}
            measuredFps={preview.state.playing ? measuredFps : fps}
            resolution={resolution}
            resolutionOptions={resolutionOptions}
            playing={preview.state.playing}
            onTogglePlay={preview.togglePlay}
            onStep={preview.stepFrame}
            onRewind={preview.rewindToStart}
            onResolutionChange={changeResolution}
            onFpsChange={setFps}
          />
        </section>
        <section className="timeline-panel">
          <TabsRow
            timeline={displayTimeline}
            seconds={preview.state.seconds}
            fps={fps}
            cacheRanges={preview.state.cacheRanges}
            viewport={timelineViewport}
            primaryLabel="Timeline"
            secondaryLabel={displayTimeline.title}
            onSeek={preview.setSeconds}
          />
          <Timeline
            timeline={displayTimeline}
            seconds={preview.state.seconds}
            viewport={timelineViewport}
            onSeek={preview.setSeconds}
            onViewportChange={updateTimelineViewport}
          />
          <TimelineViewportBar
            duration={timelineDuration}
            viewport={timelineViewport}
            onViewportChange={updateTimelineViewport}
          />
        </section>
      </div>
    </div>
  );
}

interface PreviewResolutionOption extends PreviewResolution {
  label: string;
}

function previewResolutionOptions(
  resolution: PreviewResolution,
): PreviewResolutionOption[] {
  if (
    PREVIEW_RESOLUTION_OPTIONS.some((option) =>
      sameResolution(option, resolution),
    )
  ) {
    return PREVIEW_RESOLUTION_OPTIONS;
  }

  return [
    {
      ...resolution,
      label: `${resolution.width} × ${resolution.height}`,
    },
    ...PREVIEW_RESOLUTION_OPTIONS,
  ];
}

function sameResolution(
  a: PreviewResolution,
  b: PreviewResolution,
): boolean {
  return a.width === b.width && a.height === b.height;
}
