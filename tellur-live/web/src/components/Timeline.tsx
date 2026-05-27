import { useCallback, useEffect, useRef, useState } from "react";
import {
  MIN_TIMELINE_ZOOM,
  MAX_TIMELINE_ZOOM,
  clamp,
  clampTimelineViewport,
  getVisibleDuration,
  type TimelineViewport,
  type TimelineViewportChange,
} from "../timelineViewport";
import type { TimelineInfo } from "../types";

interface TimelineProps {
  timeline: TimelineInfo | null;
  seconds: number;
  viewport: TimelineViewport;
  onSeek: (seconds: number) => void;
  onViewportChange: (next: TimelineViewportChange) => void;
}

interface TrackDef {
  id: string;
  name: string;
  alt: boolean;
  muteActive?: boolean;
  color?: string;
}

const TRACKS: TrackDef[] = [
  { id: "video-1", name: "Video 1", alt: false, color: "#7292e8" },
  { id: "extra-1", name: "", alt: true },
];

export function Timeline(props: TimelineProps) {
  const { timeline, seconds, viewport, onSeek, onViewportChange } = props;
  const duration = Math.max(timeline?.duration ?? 0.001, 0.001);

  const bodyRef = useRef<HTMLDivElement>(null);
  const [bodyWidth, setBodyWidth] = useState(0);

  useEffect(() => {
    const el = bodyRef.current;
    if (!el) return;
    const observer = new ResizeObserver(() => setBodyWidth(el.clientWidth));
    observer.observe(el);
    setBodyWidth(el.clientWidth);
    return () => observer.disconnect();
  }, []);

  const normalizedViewport = clampTimelineViewport(viewport, duration);
  const visibleDuration = getVisibleDuration(
    duration,
    normalizedViewport.zoom,
  );
  const innerWidth = Math.max(
    bodyWidth * normalizedViewport.zoom,
    bodyWidth,
  );
  const viewportX = clamp(
    (normalizedViewport.start / duration) * innerWidth,
    0,
    Math.max(0, innerWidth - bodyWidth),
  );
  const playheadX = Math.max(
    0,
    Math.min(innerWidth, (seconds / duration) * innerWidth),
  );

  const handleTrackClick = useCallback(
    (e: React.MouseEvent<HTMLDivElement>) => {
      const body = bodyRef.current;
      if (!body || bodyWidth <= 0) return;
      const rect = body.getBoundingClientRect();
      const x = e.clientX - rect.left;
      onSeek(
        clamp(
          normalizedViewport.start + (x / bodyWidth) * visibleDuration,
          0,
          duration,
        ),
      );
    },
    [
      bodyWidth,
      duration,
      normalizedViewport.start,
      onSeek,
      visibleDuration,
    ],
  );

  const handleWheel = useCallback(
    (e: React.WheelEvent<HTMLDivElement>) => {
      if (!e.shiftKey || bodyWidth <= 0) return;

      e.preventDefault();
      e.stopPropagation();

      const rect = e.currentTarget.getBoundingClientRect();
      const pointerRatio = clamp((e.clientX - rect.left) / bodyWidth, 0, 1);
      const delta = normalizeWheelDelta(e, bodyWidth);

      if (e.metaKey || e.ctrlKey) {
        const anchorSeconds =
          normalizedViewport.start + pointerRatio * visibleDuration;
        const nextZoom = clamp(
          normalizedViewport.zoom * Math.exp(-delta * 0.0025),
          MIN_TIMELINE_ZOOM,
          MAX_TIMELINE_ZOOM,
        );
        const nextVisibleDuration = getVisibleDuration(duration, nextZoom);

        onViewportChange({
          start: anchorSeconds - pointerRatio * nextVisibleDuration,
          zoom: nextZoom,
        });
        return;
      }

      onViewportChange({
        start:
          normalizedViewport.start + delta * (visibleDuration / bodyWidth),
        zoom: normalizedViewport.zoom,
      });
    },
    [
      bodyWidth,
      duration,
      normalizedViewport.start,
      normalizedViewport.zoom,
      onViewportChange,
      visibleDuration,
    ],
  );

  return (
    <section className="timeline">
      <aside className="timeline__side">
        {TRACKS.map((track) =>
          track.name ? (
            <div className="track-head" key={track.id}>
              <span className="track-head__name">{track.name}</span>
              <button
                className={
                  track.muteActive
                    ? "track-head__btn track-head__btn--active"
                    : "track-head__btn"
                }
                type="button"
                title="Mute"
              >
                M
              </button>
              <button
                className="track-head__btn"
                type="button"
                title="Solo"
              >
                S
              </button>
              <span
                className="track-head__color"
                style={{ background: track.color }}
              />
            </div>
          ) : (
            <div
              key={track.id}
              className="track-head track-head--empty"
            />
          ),
        )}
      </aside>
      <div
        className="timeline__body"
        ref={bodyRef}
        onWheel={handleWheel}
      >
        <div
          className="timeline__tracks"
          style={{
            width: `${innerWidth}px`,
            transform: `translateX(${-viewportX}px)`,
          }}
        >
          {TRACKS.map((track) => (
            <div
              key={track.id}
              className={
                track.alt
                  ? "timeline__track timeline__track--alt"
                  : "timeline__track"
              }
              onClick={handleTrackClick}
            >
              {timeline && track.id === "video-1" ? (
                <div
                  className="timeline__clip"
                  style={{ left: 0, width: `${innerWidth}px` }}
                >
                  <span className="timeline__clip-label">
                    {timeline.title}
                  </span>
                </div>
              ) : null}
            </div>
          ))}
          <div
            className="timeline__playhead"
            style={{ left: `${playheadX}px` }}
          />
        </div>
      </div>
    </section>
  );
}

function normalizeWheelDelta(
  e: React.WheelEvent<HTMLDivElement>,
  pageSize: number,
): number {
  const rawDelta =
    Math.abs(e.deltaX) > Math.abs(e.deltaY) ? e.deltaX : e.deltaY;

  if (e.deltaMode === 1) return rawDelta * 16;
  if (e.deltaMode === 2) return rawDelta * pageSize;
  return rawDelta;
}
