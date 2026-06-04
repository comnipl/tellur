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

// A single placed segment on a lane. `id` is the stable dotted DFS path of the
// originating node (or that path for a collapsed-summary clip). `collapsedNode`
// is set only for a collapsed-container summary clip so the clip can render a
// re-expand affordance and toggle that node id.
interface Clip {
  id: string;
  start: number;
  end: number;
  name: string;
  kind: NodeKind;
  triggers: number[];
  trim: [number, number] | null;
  collapsedNode: string | null;
}

// One horizontal row in the timeline. A "header lane" carries `header` (the
// chevron + name on the rail) and no clips; a "content lane" carries one or
// more time-packed clips and no header. Rail and grid both iterate `Lane[]` in
// lockstep, so `depth` drives the rail indent for both.
interface Lane {
  id: string;
  depth: number;
  header: { nodeId: string; name: string } | null;
  clips: Clip[];
}

// Packing scratch lane: a `Lane` plus the time after which it is free to accept
// the next unit. `lastEnd` is mutated during greedy first-fit placement and
// dropped before the lanes are handed to React.
interface WorkLane extends Lane {
  lastEnd: number;
}

// EPS keeps abutting clips (e.g. Dialogue[0,3] then Dialogue[3,6]) from being
// treated as overlapping when checking whether a lane is free at a start time.
const PACK_EPS = 1e-4;

function capitalize(s: string): string {
  return s.charAt(0).toUpperCase() + s.slice(1);
}

// Display label for a node: explicit `name`, else its `label`, else the
// capitalized kind. Used for both rail headers and clip labels.
function displayName(node: Arrangement): string {
  if (node.name != null && node.name !== "") return node.name;
  if (node.label !== "") return node.label;
  return capitalize(node.kind);
}

function isContainer(node: Arrangement): boolean {
  return node.children.length > 0;
}

// A "grouping" container has at least one container child, so it earns a
// collapsible rail header row. A container whose children are all leaves is a
// "leaf container": transparent (no row), its leaves packed into the parent.
function isGrouping(node: Arrangement): boolean {
  return isContainer(node) && node.children.some(isContainer);
}

function leafClip(node: Arrangement, path: string): Clip {
  return {
    id: path,
    start: node.start,
    end: node.end,
    name: displayName(node),
    kind: node.kind,
    triggers: node.triggers,
    trim: node.trim,
    collapsedNode: null,
  };
}

// Lanes for `node`'s subtree. The four cases mirror the swimlane spec:
// leaf -> one content lane; collapsed container -> one summary lane; grouping
// container -> header lane + packed children; leaf container -> transparent
// (its children packed at the same depth, no header).
function layout(
  node: Arrangement,
  depth: number,
  path: string,
  collapsed: Set<string>,
): Lane[] {
  if (!isContainer(node)) {
    return [{ id: path, depth, header: null, clips: [leafClip(node, path)] }];
  }

  if (collapsed.has(path)) {
    // Collapsed summary: a single clip spanning the container's full extent.
    // `collapsedNode` makes the clip (and rail chevron) toggle this node open.
    const clip: Clip = {
      id: path,
      start: node.start,
      end: node.end,
      name: displayName(node),
      kind: node.kind,
      triggers: node.triggers,
      trim: node.trim,
      collapsedNode: path,
    };
    return [
      {
        id: path,
        depth,
        header: { nodeId: path, name: displayName(node) },
        clips: [clip],
      },
    ];
  }

  if (isGrouping(node)) {
    const headerLane: Lane = {
      id: `${path}#header`,
      depth,
      header: { nodeId: path, name: displayName(node) },
      clips: [],
    };
    return [headerLane, ...packChildren(node.children, depth + 1, path, collapsed)];
  }

  // Leaf container, expanded: transparent. Pack its leaf children at the same
  // depth with no header row of its own.
  return packChildren(node.children, depth, path, collapsed);
}

// A packing unit: the consecutive lanes contributed by one child subtree plus
// the time extent used to test lane availability. `extent` is the child's own
// [start, end] (or, for a transparent leaf-container's leaf, that leaf's span).
interface PackUnit {
  lanes: Lane[];
  start: number;
  end: number;
  order: number;
}

// 2D greedy first-fit packing of child subtrees by time. Transparent leaf
// containers are flattened inline: each of their leaf children becomes its own
// unit so leaves from different siblings (the per-Dialogue Captions/Subtitles)
// pack across siblings into shared lanes.
function packChildren(
  children: Arrangement[],
  depth: number,
  parentPath: string,
  collapsed: Set<string>,
): Lane[] {
  const units: PackUnit[] = [];
  let order = 0;

  children.forEach((child, i) => {
    const childPath = `${parentPath}.${i}`;
    const transparent =
      isContainer(child) && !isGrouping(child) && !collapsed.has(childPath);

    if (transparent) {
      // Flatten the transparent leaf container: each leaf child is a unit
      // whose lanes/extent come from that leaf alone.
      child.children.forEach((leaf, j) => {
        const leafPath = `${childPath}.${j}`;
        units.push({
          lanes: layout(leaf, depth, leafPath, collapsed),
          start: leaf.start,
          end: leaf.end,
          order: order++,
        });
      });
      return;
    }

    // Leaf, grouping container, or collapsed container: one unit whose lanes
    // are the child's own layout and whose extent is the child's span.
    units.push({
      lanes: layout(child, depth, childPath, collapsed),
      start: child.start,
      end: child.end,
      order: order++,
    });
  });

  // Stable sort by start ascending: equal starts keep authored order, so a
  // Caption placed before a Subtitle at the same start stays first.
  units.sort((a, b) => (a.start === b.start ? a.order - b.order : a.start - b.start));

  const result: WorkLane[] = [];

  for (const unit of units) {
    const height = unit.lanes.length;
    // Lowest index L where lanes [L .. L+height-1] are all free at unit.start
    // (missing lanes count as free). The block must be contiguous.
    let placeAt = 0;
    for (let l = 0; ; l++) {
      let fits = true;
      for (let k = 0; k < height; k++) {
        const lane = result[l + k];
        if (lane && lane.lastEnd > unit.start + PACK_EPS) {
          fits = false;
          break;
        }
      }
      if (fits) {
        placeAt = l;
        break;
      }
    }

    for (let k = 0; k < height; k++) {
      const src = unit.lanes[k];
      const idx = placeAt + k;
      if (!result[idx]) {
        result[idx] = {
          id: src.id,
          depth: src.depth,
          header: src.header,
          clips: [...src.clips],
          lastEnd: -Infinity,
        };
      } else {
        // Merge into an existing lane: keep its header if it already had one,
        // otherwise adopt the incoming header (e.g. a packed grouping unit).
        const lane = result[idx];
        lane.clips.push(...src.clips);
        if (!lane.header && src.header) lane.header = src.header;
      }
      result[idx].lastEnd = Math.max(result[idx].lastEnd, unit.end);
    }
  }

  return result.map(({ lastEnd: _lastEnd, ...lane }) => lane);
}

// Build the full lane list for the arrangement, re-keying each lane with a
// render-stable id derived from its position so React reconciles cleanly.
function computeLanes(root: Arrangement, collapsed: Set<string>): Lane[] {
  const lanes = layout(root, 0, "0", collapsed);
  return lanes.map((lane, index) => ({
    ...lane,
    id: lane.header
      ? `${lane.header.nodeId}#${index}`
      : lane.clips[0]
        ? `${lane.clips[0].id}#${index}`
        : `lane#${index}`,
  }));
}

// Rail / clip label for a content lane: the shared display name when every clip
// agrees, else the shared capitalized kind, else nothing.
function contentLaneLabel(lane: Lane): string {
  if (lane.clips.length === 0) return "";
  const firstName = lane.clips[0].name;
  if (lane.clips.every((c) => c.name === firstName)) return firstName;
  const firstKind = lane.clips[0].kind;
  if (lane.clips.every((c) => c.kind === firstKind)) return capitalize(firstKind);
  return "";
}

export function Timeline(props: TimelineProps) {
  const { timeline, seconds, viewport, onSeek, onViewportChange } = props;
  const duration = Math.max(timeline?.duration ?? 0.001, 0.001);

  const bodyRef = useRef<HTMLDivElement>(null);
  const draggingSeekRef = useRef(false);
  const [bodyWidth, setBodyWidth] = useState(0);
  const [draggingSeek, setDraggingSeek] = useState(false);
  const [arrangement, setArrangement] = useState<Arrangement | null>(null);
  // Set of collapsed grouping-container node ids (dotted DFS paths). Empty by
  // default, so the tree renders fully expanded.
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());

  // Immutable toggle: collapsing/expanding swaps in a fresh Set so React sees a
  // new reference and re-derives the lanes.
  const toggleCollapsed = useCallback((nodeId: string) => {
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(nodeId)) next.delete(nodeId);
      else next.add(nodeId);
      return next;
    });
  }, []);

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

  const lanes = arrangement ? computeLanes(arrangement, collapsed) : [];

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
        {lanes.length > 0 ? (
          lanes.map((lane) => {
            const isCollapsed = lane.header
              ? collapsed.has(lane.header.nodeId)
              : false;
            const railLabel = lane.header
              ? lane.header.name
              : contentLaneLabel(lane);
            return (
              <div
                className={
                  lane.header ? "track-head track-head--group" : "track-head"
                }
                key={lane.id}
                style={{ paddingLeft: `${10 + lane.depth * 14}px` }}
              >
                {lane.header ? (
                  <button
                    type="button"
                    className="track-head__chevron"
                    aria-expanded={!isCollapsed}
                    title={isCollapsed ? "Expand" : "Collapse"}
                    onPointerDown={(e) => e.stopPropagation()}
                    onClick={(e) => {
                      e.stopPropagation();
                      toggleCollapsed(lane.header!.nodeId);
                    }}
                  >
                    {isCollapsed ? "▸" : "▾"}
                  </button>
                ) : (
                  <span className="track-head__chevron-spacer" />
                )}
                <span className="track-head__name" title={railLabel}>
                  {railLabel}
                </span>
              </div>
            );
          })
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
          {lanes.length > 0
            ? lanes.map((lane) => {
                if (lane.header && lane.clips.length === 0) {
                  // Grouping-container header row: no clip, just a faint
                  // full-row hairline so the group band reads as a section.
                  return (
                    <div key={lane.id} className="timeline__track">
                      <div className="timeline__group-line" />
                    </div>
                  );
                }
                return (
                  <div key={lane.id} className="timeline__track">
                    {lane.clips.map((clip) => {
                      const left = (clamp(clip.start, 0, duration) / duration) *
                        innerWidth;
                      const right = (clamp(clip.end, 0, duration) / duration) *
                        innerWidth;
                      const width = Math.max(right - left, 2);
                      const summary = clip.collapsedNode != null;
                      return (
                        <div
                          key={clip.id}
                          className={
                            summary
                              ? "timeline__clip timeline__clip--summary"
                              : "timeline__clip"
                          }
                          style={{ left: `${left}px`, width: `${width}px` }}
                          onPointerDown={
                            summary ? (e) => e.stopPropagation() : undefined
                          }
                          onClick={
                            summary
                              ? (e) => {
                                  e.stopPropagation();
                                  toggleCollapsed(clip.collapsedNode!);
                                }
                              : undefined
                          }
                        >
                          <span className="timeline__clip-label">
                            {summary ? `▸ ${clip.name}` : clip.name}
                          </span>
                        </div>
                      );
                    })}
                    {/* Triggers sit at track level (full-width coords) so their
                        diamond heads are not clipped by the clip's overflow. */}
                    {lane.clips.flatMap((clip) =>
                      clip.triggers.map((t, index) => (
                        <div
                          key={`${clip.id}:${index}`}
                          className="timeline__trigger"
                          title={`Event @ ${t.toFixed(3)}s`}
                          style={{
                            left: `${(clamp(t, 0, duration) / duration) *
                              innerWidth}px`,
                          }}
                        />
                      )),
                    )}
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
