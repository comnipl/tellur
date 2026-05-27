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
import {
  clampTimelineViewport,
  type TimelineViewport,
  type TimelineViewportChange,
} from "./timelineViewport";
import type { ServerInfo, TimelineInfo } from "./types";

const FALLBACK_TIMELINE: TimelineInfo = {
  id: "demo",
  title: "Demo Timeline",
  duration: 150,
};
const INFO_POLL_MS = 200;

export function App() {
  const [info, setInfo] = useState<ServerInfo | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [scale, setScale] = useState(1);
  const [fps, setFps] = useState(30);
  const [timelineViewport, setTimelineViewport] = useState<TimelineViewport>({
    start: 0,
    zoom: 1,
  });
  const [measuredFps, setMeasuredFps] = useState(0);
  const fpsCounterRef = useRef({ frames: 0, last: performance.now() });

  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | null = null;

    const tick = async () => {
      try {
        const next = await fetchInfo();
        if (cancelled) return;
        setInfo((prev) => {
          if (!prev) {
            setFps((current) => Math.max(next.fps || current, 1));
          }
          return next;
        });
        setLoadError(null);
      } catch (e) {
        if (!cancelled) setLoadError(String(e));
      } finally {
        if (!cancelled) timer = setTimeout(tick, INFO_POLL_MS);
      }
    };
    tick();
    return () => {
      cancelled = true;
      if (timer) clearTimeout(timer);
    };
  }, []);

  const timeline = info?.timelines[0] ?? null;

  const preview = usePreview({
    info,
    timeline,
    scale,
    fps,
  });

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

  const aspect = info ? info.width / info.height : 16 / 9;
  const displayTimeline = timeline ?? FALLBACK_TIMELINE;
  const timelineDuration = displayTimeline.duration;
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
    typeof window !== "undefined" && window.location.host
      ? `https://${window.location.host}`
      : "";

  return (
    <div className="app">
      <Header
        projectName="Project Name"
        url={url}
        compileStatus={info?.compileStatus ?? "compiled"}
        compileError={info?.compileError ?? null}
      />
      <div className="workspace">
        <section className="viewer-panel">
          <Preview
            imageSrc={preview.state.imageSrc}
            imageVisible={preview.state.imageVisible}
            videoVisible={preview.state.videoVisible}
            aspect={aspect}
            error={loadError ?? info?.lastError ?? preview.state.error}
            videoRef={preview.videoRef}
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
            scale={scale}
            playing={preview.state.playing}
            onTogglePlay={preview.togglePlay}
            onStep={preview.stepFrame}
            onRewind={preview.rewindToStart}
            onScaleChange={setScale}
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
