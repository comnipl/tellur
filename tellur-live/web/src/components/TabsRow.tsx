import { useCallback, useEffect, useRef, useState } from "react";
import { CornerUpRight, SquareMenu } from "lucide-react";
import { formatTimelineStart, formatTimecode } from "../formatTime";
import {
  clamp,
  clampTimelineViewport,
  getVisibleDuration,
  type TimelineViewport,
} from "../timelineViewport";
import type { CacheRange, TimelineInfo } from "../types";

interface TabsRowProps {
  timeline: TimelineInfo | null;
  seconds: number;
  fps: number;
  cacheRanges: CacheRange[];
  viewport: TimelineViewport;
  primaryLabel: string;
  secondaryLabel?: string;
  onSeek: (seconds: number) => void;
}

// Compound row that holds the panel tabs on the left and the timeline
// "cursor strip" (playhead chip + green cache bar) on the right. Both
// halves are aligned via the same side-width grid as the timeline body
// below so the cursor strip's pixel coordinates match the clip area.
export function TabsRow(props: TabsRowProps) {
  const {
    timeline,
    seconds,
    fps,
    cacheRanges,
    viewport,
    primaryLabel,
    secondaryLabel,
    onSeek,
  } = props;

  const cursorRef = useRef<HTMLDivElement>(null);
  const draggingSeekRef = useRef(false);
  const [width, setWidth] = useState(0);
  const [draggingSeek, setDraggingSeek] = useState(false);

  useEffect(() => {
    const el = cursorRef.current;
    if (!el) return;
    const observer = new ResizeObserver(() => setWidth(el.clientWidth));
    observer.observe(el);
    setWidth(el.clientWidth);
    return () => observer.disconnect();
  }, []);

  const duration = Math.max(timeline?.duration ?? 0.001, 0.001);
  const normalizedViewport = clampTimelineViewport(viewport, duration);
  const visibleDuration = getVisibleDuration(
    duration,
    normalizedViewport.zoom,
  );
  const viewportEnd = normalizedViewport.start + visibleDuration;
  const x =
    ((seconds - normalizedViewport.start) / visibleDuration) * width;
  const playheadVisible = x >= 0 && x <= width;
  const frame = Math.max(0, Math.round(seconds * fps));
  const viewportStartFrame = Math.max(
    0,
    Math.round(normalizedViewport.start * fps),
  );

  const seekFromClientX = useCallback(
    (clientX: number) => {
      const cursor = cursorRef.current;
      if (!cursor || !timeline) return;
      const rect = cursor.getBoundingClientRect();
      const ratio = clamp((clientX - rect.left) / rect.width, 0, 1);
      onSeek(
        clamp(
          normalizedViewport.start + ratio * visibleDuration,
          0,
          duration,
        ),
      );
    },
    [
      duration,
      normalizedViewport.start,
      onSeek,
      timeline,
      visibleDuration,
    ],
  );

  const handleSeekPointerDown = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      if (e.button !== 0) return;
      e.preventDefault();
      draggingSeekRef.current = true;
      setDraggingSeek(true);
      seekFromClientX(e.clientX);
      e.currentTarget.setPointerCapture(e.pointerId);
    },
    [seekFromClientX],
  );

  const handleSeekPointerMove = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      if (!draggingSeekRef.current) return;
      e.preventDefault();
      seekFromClientX(e.clientX);
    },
    [seekFromClientX],
  );

  const endSeekDrag = useCallback((e: React.PointerEvent<HTMLDivElement>) => {
    if (!draggingSeekRef.current) return;
    draggingSeekRef.current = false;
    setDraggingSeek(false);
    if (e.currentTarget.hasPointerCapture(e.pointerId)) {
      e.currentTarget.releasePointerCapture(e.pointerId);
    }
  }, []);

  return (
    <div className="tabsrow">
      <div className="tabsrow__left">
        <span className="tabsrow__tab tabsrow__tab--primary tabsrow__tab--active">
          {primaryLabel}
        </span>
        {secondaryLabel ? (
          <span className="tabsrow__tab tabsrow__tab--secondary">
            <SquareMenu size={13} strokeWidth={1.8} />
            {secondaryLabel}
          </span>
        ) : null}
      </div>
      <div
        className={
          draggingSeek
            ? "tabsrow__cursor tabsrow__cursor--dragging"
            : "tabsrow__cursor"
        }
        ref={cursorRef}
        onPointerDown={handleSeekPointerDown}
        onPointerMove={handleSeekPointerMove}
        onPointerUp={endSeekDrag}
        onPointerCancel={endSeekDrag}
        onLostPointerCapture={endSeekDrag}
      >
        <div className="tabsrow__cursor-inner">
          <span className="tabsrow__viewport-start">
            <CornerUpRight
              className="tabsrow__viewport-start-icon"
              size={18}
              strokeWidth={1.8}
            />
            <span className="tabsrow__viewport-start-text">
              <span>
                {formatTimelineStart(normalizedViewport.start, fps)}
              </span>
              <span className="tabsrow__viewport-start-frame">
                {viewportStartFrame}F
              </span>
            </span>
          </span>
          {cacheRanges.map((range, i) => {
            const visibleStart = Math.max(
              range.start,
              normalizedViewport.start,
            );
            const visibleEnd = Math.min(range.end, viewportEnd);
            if (visibleEnd <= visibleStart) return null;

            const left =
              ((visibleStart - normalizedViewport.start) /
                visibleDuration) *
              width;
            const w = Math.max(
              2,
              ((visibleEnd - visibleStart) / visibleDuration) * width,
            );
            return (
              <div
                className="tabsrow__cache"
                key={i}
                style={{ left: `${left}px`, width: `${w}px` }}
              />
            );
          })}
          {playheadVisible ? (
            <>
              <span
                className="tabsrow__playhead-line"
                style={{ left: `${x}px` }}
              />
              <span
                className="tabsrow__chip"
                style={{ left: `${x}px` }}
              >
                <span>{formatTimecode(seconds, fps)}</span>
                <span className="tabsrow__chip-sub">{frame}F</span>
              </span>
            </>
          ) : null}
        </div>
      </div>
    </div>
  );
}
