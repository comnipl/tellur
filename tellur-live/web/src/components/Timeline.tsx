import { useCallback, useEffect, useRef, useState } from "react";
import { fetchArrangement } from "../api";
import {
  MIN_TIMELINE_ZOOM,
  MAX_TIMELINE_ZOOM,
  clamp,
  clampTimelineViewport,
  getVisibleDuration,
  type TimelineViewport,
  type TimelineViewportChange,
} from "../timelineViewport";
import type { Arrangement, NodeKind, TimelineInfo } from "../types";

interface TimelineProps {
  timeline: TimelineInfo | null;
  seconds: number;
  viewport: TimelineViewport;
  onSeek: (seconds: number) => void;
  onViewportChange: (next: TimelineViewportChange) => void;
}

// One flattened arrangement node, paired with its depth so the side heads can
// indent and the lanes can color/position each clip on the shared time axis.
interface ArrangementRow {
  id: string;
  depth: number;
  kind: NodeKind;
  label: string;
  start: number;
  end: number;
  triggers: number[];
}

// Per-kind clip color. Containers (timeline/sequence) read as group bands;
// leaves (caption/subtitle/video/audio) get distinct hues so the hierarchy is
// legible at a glance.
const KIND_COLOR: Record<NodeKind, string> = {
  timeline: "#5c6b8a",
  sequence: "#7292e8",
  video: "#4f9d8a",
  audio: "#c08457",
  caption: "#b06fd0",
  subtitle: "#d0a24a",
};

// Depth-first flatten: root first, then children in order. Stable ids let React
// keep rows across re-fetches and keep the side heads aligned with the lanes.
function flattenArrangement(
  node: Arrangement,
  depth = 0,
  path = "0",
): ArrangementRow[] {
  const row: ArrangementRow = {
    id: path,
    depth,
    kind: node.kind,
    label: node.label,
    start: node.start,
    end: node.end,
    triggers: node.triggers,
  };
  const rows = [row];
  node.children.forEach((child, index) => {
    rows.push(...flattenArrangement(child, depth + 1, `${path}.${index}`));
  });
  return rows;
}

function rowHeadLabel(row: ArrangementRow): string {
  const kind = row.kind.charAt(0).toUpperCase() + row.kind.slice(1);
  return row.label ? `${kind} · ${row.label}` : kind;
}

export function Timeline(props: TimelineProps) {
  const { timeline, seconds, viewport, onSeek, onViewportChange } = props;
  const duration = Math.max(timeline?.duration ?? 0.001, 0.001);

  const bodyRef = useRef<HTMLDivElement>(null);
  const draggingSeekRef = useRef(false);
  const [bodyWidth, setBodyWidth] = useState(0);
  const [draggingSeek, setDraggingSeek] = useState(false);
  const [arrangement, setArrangement] = useState<Arrangement | null>(null);

  useEffect(() => {
    const el = bodyRef.current;
    if (!el) return;
    const observer = new ResizeObserver(() => setBodyWidth(el.clientWidth));
    observer.observe(el);
    setBodyWidth(el.clientWidth);
    return () => observer.disconnect();
  }, []);

  // Refetch the resolved tree whenever the active timeline changes. `null`
  // (failed resolve / legacy adapter) leaves us in the flat fallback below.
  const timelineId = timeline?.id ?? null;
  useEffect(() => {
    if (!timelineId) {
      setArrangement(null);
      return;
    }
    const controller = new AbortController();
    fetchArrangement(timelineId, controller.signal)
      .then((next) => setArrangement(next))
      .catch((e) => {
        if (controller.signal.aborted) return;
        console.warn("tellur-live arrangement fetch failed", e);
        setArrangement(null);
      });
    return () => controller.abort();
  }, [timelineId]);

  const rows = arrangement ? flattenArrangement(arrangement) : [];

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

  const seekFromClientX = useCallback(
    (clientX: number) => {
      const body = bodyRef.current;
      if (!body || bodyWidth <= 0) return;
      const rect = body.getBoundingClientRect();
      const x = clientX - rect.left;
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
        {rows.length > 0 ? (
          rows.map((row) => (
            <div
              className="track-head"
              key={row.id}
              style={{ paddingLeft: `${10 + row.depth * 12}px` }}
            >
              <span
                className="track-head__name"
                title={rowHeadLabel(row)}
              >
                {rowHeadLabel(row)}
              </span>
              <span
                className="track-head__color"
                style={{ background: KIND_COLOR[row.kind] }}
              />
            </div>
          ))
        ) : (
          <div className="track-head track-head--empty" />
        )}
      </aside>
      <div
        className={
          draggingSeek
            ? "timeline__body timeline__body--dragging"
            : "timeline__body"
        }
        ref={bodyRef}
        onWheel={handleWheel}
        onPointerDown={handleSeekPointerDown}
        onPointerMove={handleSeekPointerMove}
        onPointerUp={endSeekDrag}
        onPointerCancel={endSeekDrag}
        onLostPointerCapture={endSeekDrag}
      >
        <div
          className="timeline__tracks"
          style={{
            width: `${innerWidth}px`,
            transform: `translateX(${-viewportX}px)`,
          }}
        >
          {rows.length > 0
            ? rows.map((row) => {
                const left = (clamp(row.start, 0, duration) / duration) *
                  innerWidth;
                const right = (clamp(row.end, 0, duration) / duration) *
                  innerWidth;
                const width = Math.max(right - left, 2);
                const color = KIND_COLOR[row.kind];
                return (
                  <div key={row.id} className="timeline__track">
                    <div
                      className="timeline__clip"
                      style={{
                        left: `${left}px`,
                        width: `${width}px`,
                        background: color,
                      }}
                    >
                      <span className="timeline__clip-label">
                        {rowHeadLabel(row)}
                      </span>
                    </div>
                    {row.triggers.map((t, index) => (
                      <div
                        key={index}
                        className="timeline__trigger"
                        title={`Event @ ${t.toFixed(3)}s`}
                        style={{
                          left: `${(clamp(t, 0, duration) / duration) *
                            innerWidth}px`,
                        }}
                      />
                    ))}
                  </div>
                );
              })
            : timeline ? (
                <div className="timeline__track">
                  <div
                    className="timeline__clip"
                    style={{ left: 0, width: `${innerWidth}px` }}
                  >
                    <span className="timeline__clip-label">
                      {timeline.title}
                    </span>
                  </div>
                </div>
              ) : null}
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
