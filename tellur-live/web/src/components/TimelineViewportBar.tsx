import { useRef, useState } from "react";
import {
  MAX_TIMELINE_ZOOM,
  clamp,
  clampTimelineViewport,
  getSafeDuration,
  getVisibleDuration,
  type TimelineViewport,
  type TimelineViewportChange,
} from "../timelineViewport";

interface TimelineViewportBarProps {
  duration: number;
  viewport: TimelineViewport;
  onViewportChange: (next: TimelineViewportChange) => void;
}

type DragKind = "move" | "start" | "end";

interface DragState {
  kind: DragKind;
  grabRatio: number;
}

const MIN_WINDOW_RATIO = 1 / MAX_TIMELINE_ZOOM;

export function TimelineViewportBar(props: TimelineViewportBarProps) {
  const { duration, viewport, onViewportChange } = props;
  const railRef = useRef<HTMLDivElement>(null);
  const dragRef = useRef<DragState | null>(null);
  const [dragging, setDragging] = useState<DragKind | null>(null);

  const safeDuration = getSafeDuration(duration);
  const normalizedViewport = clampTimelineViewport(viewport, safeDuration);
  const visibleDuration = getVisibleDuration(
    safeDuration,
    normalizedViewport.zoom,
  );
  const windowRatio = visibleDuration / safeDuration;
  const startRatio = normalizedViewport.start / safeDuration;
  const endRatio = Math.min(1, startRatio + windowRatio);

  const ratioFromEvent = (e: React.PointerEvent<HTMLDivElement>) => {
    const rail = railRef.current;
    if (!rail) return 0;
    const rect = rail.getBoundingClientRect();
    return clamp((e.clientX - rect.left) / rect.width, 0, 1);
  };

  const beginDrag = (
    kind: DragKind,
    e: React.PointerEvent<HTMLDivElement>,
  ) => {
    e.preventDefault();
    e.stopPropagation();

    const ratio = ratioFromEvent(e);
    dragRef.current = {
      kind,
      grabRatio:
        kind === "move" ? clamp(ratio - startRatio, 0, windowRatio) : 0,
    };
    setDragging(kind);
    railRef.current?.setPointerCapture(e.pointerId);
  };

  const handleRailPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    e.preventDefault();
    const ratio = ratioFromEvent(e);
    const nextStartRatio = clamp(ratio - windowRatio / 2, 0, 1 - windowRatio);

    onViewportChange({
      start: nextStartRatio * safeDuration,
      zoom: normalizedViewport.zoom,
    });

    dragRef.current = { kind: "move", grabRatio: windowRatio / 2 };
    setDragging("move");
    e.currentTarget.setPointerCapture(e.pointerId);
  };

  const handlePointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    const drag = dragRef.current;
    if (!drag) return;
    e.preventDefault();

    const ratio = ratioFromEvent(e);

    if (drag.kind === "move") {
      const nextStartRatio = clamp(
        ratio - drag.grabRatio,
        0,
        1 - windowRatio,
      );
      onViewportChange({
        start: nextStartRatio * safeDuration,
        zoom: normalizedViewport.zoom,
      });
      return;
    }

    if (drag.kind === "start") {
      const nextStartRatio = clamp(ratio, 0, endRatio - MIN_WINDOW_RATIO);
      const nextWindowRatio = endRatio - nextStartRatio;
      onViewportChange({
        start: nextStartRatio * safeDuration,
        zoom: 1 / nextWindowRatio,
      });
      return;
    }

    const nextEndRatio = clamp(
      ratio,
      startRatio + MIN_WINDOW_RATIO,
      1,
    );
    const nextWindowRatio = nextEndRatio - startRatio;
    onViewportChange({
      start: normalizedViewport.start,
      zoom: 1 / nextWindowRatio,
    });
  };

  const endDrag = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!dragRef.current) return;
    dragRef.current = null;
    setDragging(null);
    if (e.currentTarget.hasPointerCapture(e.pointerId)) {
      e.currentTarget.releasePointerCapture(e.pointerId);
    }
  };

  return (
    <div className="zoom-bar">
      <div
        className={
          dragging
            ? "zoom-bar__rail zoom-bar__rail--dragging"
            : "zoom-bar__rail"
        }
        ref={railRef}
        onPointerDown={handleRailPointerDown}
        onPointerMove={handlePointerMove}
        onPointerUp={endDrag}
        onPointerCancel={endDrag}
      >
        <div
          className={
            dragging === "move"
              ? "zoom-bar__viewport zoom-bar__viewport--dragging"
              : "zoom-bar__viewport"
          }
          style={{
            left: `${startRatio * 100}%`,
            width: `${windowRatio * 100}%`,
          }}
          onPointerDown={(e) => beginDrag("move", e)}
        >
          <div
            className="zoom-bar__handle zoom-bar__handle--start"
            onPointerDown={(e) => beginDrag("start", e)}
          />
          <div
            className="zoom-bar__handle zoom-bar__handle--end"
            onPointerDown={(e) => beginDrag("end", e)}
          />
        </div>
      </div>
    </div>
  );
}
