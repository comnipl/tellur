import { useCallback, useEffect, useRef, useState } from "react";
import {
  AnimatePresence,
  animate,
  motion,
  useMotionValue,
  useReducedMotion,
} from "motion/react";
import { ChevronRight, Component, Flag } from "lucide-react";
import { fetchArrangement } from "../api";
import {
  MIN_TIMELINE_ZOOM,
  MAX_TIMELINE_ZOOM,
  TRAILING_OVERSCROLL,
  clamp,
  clampTimelineViewport,
  getVisibleDuration,
  type TimelineViewport,
  type TimelineViewportChange,
} from "../timelineViewport";
import type { SelectedNode } from "./Inspector";
import type { Arrangement, NodeKind, TimelineInfo } from "../types";

interface TimelineProps {
  timeline: TimelineInfo | null;
  seconds: number;
  viewport: TimelineViewport;
  // The id of the currently-selected node (dotted DFS path), driving the
  // `.timeline__clip--selected` highlight. Selection state is lifted to App so
  // the sibling Inspector can read the full node; App passes the id back here.
  selectedId: string | null;
  // Bumped by the server on every hot reload (the `cacheKey`). The arrangement
  // fetch keys off this so reloading refreshes the lanes even though the timeline
  // id is unchanged. `null` while server info hasn't loaded yet.
  reloadKey: string | null;
  onSeek: (seconds: number) => void;
  onViewportChange: (next: TimelineViewportChange) => void;
  // Called when a clip is clicked (with the clicked node's data) or the
  // selection is cleared (null). App owns the selection state.
  onSelect: (node: SelectedNode | null) => void;
}

// A single placed segment on a lane. `id` is the stable dotted DFS path of the
// originating node. `collapsedNode` is set on any clip that toggles a node's
// collapsed state on click (a leaf-container title bar — collapsed OR expanded —
// and a collapsed grouping-container summary). `isTitle` marks a leaf-container
// title bar so it renders with the thick window-title look + a chevron.
// `frameOwner` is the expanded leaf-container path this clip belongs to (set on
// the title clip and every descendant clip), so window frames are derived from
// clip ownership rather than lane tags — two time-disjoint expanded windows can
// then share the same lanes yet still each draw their own box.
// A timeline Event firing: an absolute time and an optional name. Carried on the
// emitting clip so the flag can be anchored directly above that clip.
interface TimelineEvent {
  time: number;
  name: string | null;
}

interface Clip {
  id: string;
  start: number;
  end: number;
  name: string;
  kind: NodeKind;
  trim: [number, number] | null;
  // Events fired by this clip's node, drawn as a flag above the clip's top edge.
  triggers: TimelineEvent[];
  collapsedNode: string | null;
  isTitle: boolean;
  frameOwner: string | null;
  // True when the node is a user `#[component]` (the macro sets a non-null
  // `name`). A component is green if it COMPOSES children (a container), or blue
  // if its body is a single raster (no children); raw nodes use their kind color.
  isComponent: boolean;
  // True when the node has children. Distinguishes container components (green)
  // from single-raster leaf components (blue).
  hasChildren: boolean;
  // The node's authoring call site (file + line), or null (e.g. the root).
  // Shown in the top-right readout when the clip is selected.
  source: { file: string; line: number } | null;
}

// One horizontal row in the timeline. A "header lane" carries `header` (the
// chevron + name on the rail) and no clips; a "content lane" carries one or
// more time-packed clips. A leaf-container title lane carries BOTH a `header`
// (so the rail shows its chevron) and a single title-bar clip. `frame` tags the
// lane as belonging to an expanded leaf-container's window block (its dotted
// path) so the overlay can draw one rounded box around the block. Rail and grid
// both iterate `Lane[]` in lockstep, so `depth` drives the rail indent for both.
interface Lane {
  id: string;
  depth: number;
  header: { nodeId: string; name: string } | null;
  clips: Clip[];
  frame: string | null;
}

// A rounded "window" drawn around an expanded leaf-container's title + child
// lanes. Positioned absolutely over `.timeline__tracks`: x/width from the time
// projection of [start, end]; y/height from the contiguous lane block. `kind`
// (the container's NodeKind) colors the frame border to match its title bar.
interface Frame {
  path: string;
  kind: NodeKind;
  // Mirror of the owner title clip's flag, so the border picks green for a
  // component window (e.g. Dialogue) regardless of its inner kind.
  isComponent: boolean;
  start: number;
  end: number;
  laneStart: number;
  laneCount: number;
}

// Row height in px, matching `--track-h`. Tracks stack with no vertical gap, so
// frame top/height are exact multiples of this.
const TRACK_H = 28;

// Min px a sticky clip label keeps inside the clip's right edge, so it never
// slides off the end of a partly-scrolled clip.
const LABEL_RESERVE = 24;

// Per-NodeKind colors. Containers (timeline/sequence) are green ("components");
// subtitle a warm orange, video blue, audio red. Light fills get dark text; the
// darker blue/red fills get light text — readability first. `tint` is the fill at
// low alpha, used for the expanded window's interior so a Dialogue (timeline=
// green) window reads as a soft green box around its children. Captions/telops
// have no kind of their own — they arrive as video and take the video color.
const KIND_COLOR: Record<
  NodeKind,
  { fill: string; text: string; border: string; tint: string }
> = {
  timeline: { fill: "#6fcf97", text: "#0e1f15", border: "#8fdcab", tint: "rgba(111, 207, 151, 0.14)" },
  sequence: { fill: "#6fcf97", text: "#0e1f15", border: "#8fdcab", tint: "rgba(111, 207, 151, 0.14)" },
  subtitle: { fill: "#e0944a", text: "#2a1606", border: "#eaa869", tint: "rgba(224, 148, 74, 0.14)" },
  video: { fill: "#5a7fd6", text: "#f2f5fc", border: "#7d9be1", tint: "rgba(90, 127, 214, 0.14)" },
  audio: { fill: "#d65a6b", text: "#fdf2f4", border: "#e17d8b", tint: "rgba(214, 90, 107, 0.14)" },
};

// Color rule, distinguishing container components from single-raster leaf
// components: a component that composes children (e.g. Dialogue) is green; a
// component whose body is a single raster (e.g. Backdrop/Reveal/FadingCaption,
// no children) is blue — regardless of the kind its body rasterizes to. Raw
// nodes (null name) use their own kind color.
function colorFor(kind: NodeKind, isComponent: boolean, hasChildren: boolean) {
  if (isComponent) {
    return hasChildren ? KIND_COLOR.timeline : KIND_COLOR.video;
  }
  return KIND_COLOR[kind];
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
    trim: node.trim,
    triggers: node.triggers,
    collapsedNode: null,
    isTitle: false,
    frameOwner: null,
    isComponent: node.name != null,
    hasChildren: node.children.length > 0,
    source: node.source,
  };
}

// A summary clip for a grouping container (one that owns nested container
// children). It stays visible both collapsed and expanded so the container's own
// span remains selectable/toggleable while its children are open.
function summaryClip(node: Arrangement, path: string): Clip {
  return {
    id: path,
    start: node.start,
    end: node.end,
    name: displayName(node),
    kind: node.kind,
    trim: node.trim,
    triggers: node.triggers,
    collapsedNode: path,
    isTitle: false,
    frameOwner: null,
    isComponent: node.name != null,
    hasChildren: node.children.length > 0,
    source: node.source,
  };
}

// A title-bar clip for a leaf-container, spanning its full [start, end]. Clicking
// it toggles `path`; `isTitle` drives the thick window-title styling. `frameOwner`
// is left null here and set by the expanded leaf-container branch — a COLLAPSED
// title bar owns no frame.
function titleClip(node: Arrangement, path: string): Clip {
  return {
    id: path,
    start: node.start,
    end: node.end,
    name: displayName(node),
    kind: node.kind,
    trim: node.trim,
    triggers: node.triggers,
    collapsedNode: path,
    isTitle: true,
    frameOwner: null,
    isComponent: node.name != null,
    hasChildren: node.children.length > 0,
    source: node.source,
  };
}

// Lanes for `node`'s subtree:
//  - leaf -> one content lane.
//  - grouping container (has a container child): summary lane + packed children
//    at depth+1. Collapsed grouping -> the summary lane only.
//  - leaf container (all children are leaves): a COLLAPSIBLE WINDOW. Collapsed
//    -> one title-bar lane (no frame, so siblings pack onto a shared lane).
//    Expanded -> a title lane + packed children at depth+1, every lane tagged
//    `frame = path` so the overlay draws a rounded box around the block.
function layout(
  node: Arrangement,
  depth: number,
  path: string,
  collapsed: Set<string>,
): Lane[] {
  if (!isContainer(node)) {
    return [
      { id: path, depth, header: null, clips: [leafClip(node, path)], frame: null },
    ];
  }

  if (collapsed.has(path)) {
    // Collapsed container: a single clip spanning the container's full extent.
    // For a grouping container it is a plain summary; for a leaf container it is
    // a title bar (so collapsed Dialogues read as `Dialogue | Dialogue | ...`).
    const grouping = isGrouping(node);
    const clip: Clip = grouping ? summaryClip(node, path) : titleClip(node, path);
    return [
      {
        id: path,
        depth,
        header: { nodeId: path, name: displayName(node) },
        clips: [clip],
        frame: null,
      },
    ];
  }

  if (isGrouping(node)) {
    const summaryLane: Lane = {
      id: `${path}#header`,
      depth,
      header: { nodeId: path, name: displayName(node) },
      clips: [summaryClip(node, path)],
      frame: null,
    };
    return [summaryLane, ...packChildren(node.children, depth + 1, path, collapsed)];
  }

  // Leaf container, expanded: a window. Title lane on top, child lanes below.
  // Stamp `frameOwner = path` on the title clip and every descendant clip so the
  // frame is derived from clip ownership; this stays correct even when another
  // time-disjoint expanded window shares these same lanes.
  const titleLane: Lane = {
    id: `${path}#title`,
    depth,
    header: { nodeId: path, name: displayName(node) },
    clips: [{ ...titleClip(node, path), frameOwner: path }],
    frame: path,
  };
  const childLanes = packChildren(node.children, depth + 1, path, collapsed).map(
    (lane) => ({
      ...lane,
      frame: path,
      clips: lane.clips.map((c) => ({ ...c, frameOwner: path })),
    }),
  );
  return [titleLane, ...childLanes];
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

// 2D greedy first-fit packing of child subtrees by time. Each child is one unit
// whose lanes come from `layout` and whose time extent is the child's span; a
// unit occupies a contiguous block of result lanes equal to its lane count
// (1 for a leaf or a collapsed container; 1+childLanes for an expanded window).
// Collapsed leaf-container siblings (height 1) therefore pack onto one shared
// lane, while an expanded leaf-container's contiguous block stays intact.
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
          frame: src.frame,
          lastEnd: -Infinity,
        };
      } else {
        // Merge into an existing lane: keep its header if it already had one,
        // otherwise adopt the incoming header (e.g. a packed grouping unit).
        // `frame` is preserved from the existing lane; only same-frame (or
        // frameless) units ever share a result lane, so no tag is lost.
        const lane = result[idx];
        lane.clips.push(...src.clips);
        if (!lane.header && src.header) lane.header = src.header;
      }
      result[idx].lastEnd = Math.max(result[idx].lastEnd, unit.end);
    }
  }

  return result.map(({ lastEnd: _lastEnd, ...lane }) => lane);
}

// Build the full lane list for the arrangement plus the window frames to draw
// over them. Each lane is re-keyed with a render-stable id from its position so
// React reconciles cleanly. Frames are derived from CLIP ownership: every clip
// carrying a `frameOwner` path votes for that owner's lane span. Two time-
// disjoint expanded windows can share the same lanes (the packer places their
// blocks side by side), yet each draws its own box because each owns its clips.
function computeLanes(
  root: Arrangement,
  collapsed: Set<string>,
): { lanes: Lane[]; frames: Frame[] } {
  const lanes = layout(root, 0, "0", collapsed).map((lane, index) => ({
    ...lane,
    id: lane.header
      ? `${lane.header.nodeId}#${index}`
      : lane.clips[0]
        ? `${lane.clips[0].id}#${index}`
        : `lane#${index}`,
  }));

  // Per owner: min/max lane index its clips touch, plus the owner's title clip
  // (the one with isTitle) which carries the window's [start, end].
  interface FrameAcc {
    minLane: number;
    maxLane: number;
    title: Clip | null;
  }
  const accByOwner = new Map<string, FrameAcc>();
  const order: string[] = [];
  lanes.forEach((lane, index) => {
    for (const clip of lane.clips) {
      const owner = clip.frameOwner;
      if (owner == null) continue;
      let acc = accByOwner.get(owner);
      if (!acc) {
        acc = { minLane: index, maxLane: index, title: null };
        accByOwner.set(owner, acc);
        order.push(owner);
      } else {
        if (index < acc.minLane) acc.minLane = index;
        if (index > acc.maxLane) acc.maxLane = index;
      }
      if (clip.isTitle && clip.frameOwner === owner) acc.title = clip;
    }
  });

  const frames: Frame[] = order.map((owner) => {
    const acc = accByOwner.get(owner)!;
    return {
      path: owner,
      // Border color follows the owner: green for a component window (Dialogue),
      // else the kind color. `kind`/`isComponent` mirror the owner title clip.
      kind: acc.title ? acc.title.kind : "timeline",
      isComponent: acc.title ? acc.title.isComponent : false,
      start: acc.title ? acc.title.start : 0,
      end: acc.title ? acc.title.end : 0,
      laneStart: acc.minLane,
      laneCount: acc.maxLane - acc.minLane + 1,
    };
  });

  return { lanes, frames };
}

// Walk the tree collecting the dotted paths of every leaf-container (a container
// whose children are all leaves). These default to COLLAPSED. Paths use the same
// dotted DFS scheme as `layout`/`packChildren`, so they match the lane ids.
function collectLeafContainerPaths(
  node: Arrangement,
  path: string,
  out: string[],
): void {
  if (isContainer(node) && !isGrouping(node)) {
    out.push(path);
    return; // its children are leaves; nothing collapsible deeper
  }
  node.children.forEach((child, i) =>
    collectLeafContainerPaths(child, `${path}.${i}`, out),
  );
}

// The distinct trigger times across all currently-rendered clips, so the full-
// height reference lines are drawn once per time (deduped) even when several
// clips fire at the same instant — each clip still gets its own flag.
function eventLineTimes(lanes: Lane[]): number[] {
  const times = new Set<number>();
  for (const lane of lanes) {
    for (const clip of lane.clips) {
      for (const t of clip.triggers) times.add(t.time);
    }
  }
  return [...times];
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
  const {
    timeline,
    seconds,
    viewport,
    selectedId,
    reloadKey,
    onSeek,
    onViewportChange,
    onSelect,
  } = props;
  const duration = Math.max(timeline?.duration ?? 0.001, 0.001);

  const bodyRef = useRef<HTMLDivElement>(null);
  const draggingSeekRef = useRef(false);
  const [bodyWidth, setBodyWidth] = useState(0);
  // Last body width the geometry tween saw, so a resize (including the first
  // measurement) snaps instead of animating — a layout change is not a pan/zoom.
  const prevBodyWidthRef = useRef(0);
  const [draggingSeek, setDraggingSeek] = useState(false);
  const [arrangement, setArrangement] = useState<Arrangement | null>(null);
  // Set of collapsed node ids (dotted DFS paths). Seeded with every leaf
  // container when an arrangement first loads (windows default to COLLAPSED);
  // grouping containers stay expanded. On a hot-reload refetch the user's manual
  // open/closed state is PRESERVED for paths that still exist (see the seeding
  // effect); only brand-new leaf containers default to collapsed.
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  // Leaf-container paths seen on the previous arrangement, so the seeding effect
  // can tell a genuinely NEW container (default it collapsed) from one the user
  // already expanded (keep it expanded across a reload).
  const prevLeafContainersRef = useRef<Set<string>>(new Set());

  // Honor the OS "reduce motion" preference: when set, expand/collapse snaps
  // instead of tweening (zero-duration transitions, no enter/exit offset).
  const reduceMotion = useReducedMotion();

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

  // Refetch the resolved tree whenever the active timeline changes OR the server
  // hot-reloads (`reloadKey` = the info cacheKey). The id is stable across a
  // reload, so without `reloadKey` the lanes would go stale; `/api/arrangement`
  // re-resolves per request, so a refetch always returns the latest tree. `null`
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
  }, [timelineId, reloadKey]);

  // Seed collapsed state when the tree changes. First load (or after a failed
  // resolve) defaults every leaf container to collapsed. On a hot-reload refetch
  // the user's toggles are preserved: a container that still exists keeps its
  // prior open/closed state, paths that vanished are dropped, and only brand-new
  // leaf containers default to collapsed. Uses `prevLeafContainersRef` to tell
  // "user expanded this" from "this didn't exist before".
  useEffect(() => {
    if (!arrangement) {
      setCollapsed(new Set());
      prevLeafContainersRef.current = new Set();
      return;
    }
    const leafContainers: string[] = [];
    collectLeafContainerPaths(arrangement, "0", leafContainers);
    const prevLeaf = prevLeafContainersRef.current;
    setCollapsed((prev) => {
      const next = new Set<string>();
      for (const path of leafContainers) {
        const isNew = !prevLeaf.has(path);
        // New container -> default collapsed; existing -> keep prior state.
        if (isNew || prev.has(path)) next.add(path);
      }
      return next;
    });
    prevLeafContainersRef.current = new Set(leafContainers);
  }, [arrangement]);

  const { lanes, frames } = arrangement
    ? computeLanes(arrangement, collapsed)
    : { lanes: [], frames: [] };

  // Distinct trigger times → one deduped full-height reference line each.
  const eventTimes = eventLineTimes(lanes);

  const normalizedViewport = clampTimelineViewport(viewport, duration);
  const visibleDuration = getVisibleDuration(
    duration,
    normalizedViewport.zoom,
  );
  // TARGET geometry: the projection the viewport prop maps to right now. Pointer
  // input (seek/wheel) is computed against these so it stays exact and instant.
  // No viewport-width floor: at zoom < 1 the content shrinks NARROWER than the
  // viewport (left-aligned, empty space to the right). A tiny absolute floor
  // keeps projections finite.
  const targetInnerWidth = Math.max(bodyWidth * normalizedViewport.zoom, 16);
  // When the content is narrower than the viewport there is nothing to pan, so
  // `innerWidth - bodyWidth` is negative and the floor below clamps to 0
  // (left-aligned). When zoomed in, allow the body to translate one extra
  // viewport width past the content end (TRAILING_OVERSCROLL) — the SAME amount
  // clampTimelineViewport lets `start` overscroll — so the clips slide left in
  // lock-step with the ruler into the trailing blank, instead of stalling at the
  // content end. `start` is already overscroll-clamped, so this just stops the
  // body translate from re-capping it short.
  const maxViewportX =
    Math.max(0, targetInnerWidth - bodyWidth) + bodyWidth * TRAILING_OVERSCROLL;
  const targetViewportX = clamp(
    (normalizedViewport.start / duration) * targetInnerWidth,
    0,
    maxViewportX,
  );

  // ANIMATED geometry: `innerWidth`/`viewportX` glide to the target on an
  // ease-out tween so pan/zoom feels smooth instead of jumping. Everything DRAWN
  // (clip left/width, frames, flags, event lines, playhead, the tracks layer's
  // width + translate) reads these animated values; only pointer math above uses
  // the targets. Held both as motion values (the source of truth driven by
  // `animate`) and mirrored to state so the projections recompute each frame.
  const innerWidthMV = useMotionValue(targetInnerWidth);
  const viewportXMV = useMotionValue(targetViewportX);
  const [innerWidth, setInnerWidth] = useState(targetInnerWidth);
  const [viewportX, setViewportX] = useState(targetViewportX);

  // Tween the animated geometry toward the target whenever it changes. Under
  // reduced-motion (or before a width is measured) snap instantly. The ease
  // mirrors the expand/collapse choreography (easeOutQuint) for a consistent
  // feel; pan and zoom share one tween so a combined zoom-at-pointer move glides
  // as a single gesture.
  useEffect(() => {
    // Snap (no tween) when reduced-motion is on, before a width is measured, or
    // on a resize — a layout change must not read as a pan/zoom gesture.
    const resized = prevBodyWidthRef.current !== bodyWidth;
    prevBodyWidthRef.current = bodyWidth;
    if (reduceMotion || bodyWidth <= 0 || resized) {
      innerWidthMV.set(targetInnerWidth);
      viewportXMV.set(targetViewportX);
      setInnerWidth(targetInnerWidth);
      setViewportX(targetViewportX);
      return;
    }
    const ease = [0.22, 1, 0.36, 1] as const;
    const controls = [
      animate(innerWidthMV, targetInnerWidth, {
        duration: 0.28,
        ease,
        onUpdate: setInnerWidth,
      }),
      animate(viewportXMV, targetViewportX, {
        duration: 0.28,
        ease,
        onUpdate: setViewportX,
      }),
    ];
    return () => controls.forEach((c) => c.stop());
  }, [
    targetInnerWidth,
    targetViewportX,
    reduceMotion,
    bodyWidth,
    innerWidthMV,
    viewportXMV,
  ]);

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
      // A press on the empty body (clips stop propagation) clears the selection.
      onSelect(null);
      draggingSeekRef.current = true;
      setDraggingSeek(true);
      seekFromClientX(e.clientX);
      e.currentTarget.setPointerCapture(e.pointerId);
    },
    [onSelect, seekFromClientX],
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
    (e: WheelEvent) => {
      if (!e.shiftKey) return;
      const body = bodyRef.current;
      if (!body) return;
      const rect = body.getBoundingClientRect();
      if (rect.width <= 0) return;

      e.preventDefault();
      e.stopPropagation();

      // Pointer position as a 0..1 ratio across the body, measured against the
      // body's OWN rect (not `bodyWidth`, which can lag/round) so the anchor x is
      // exact.
      const pointerRatio = clamp((e.clientX - rect.left) / rect.width, 0, 1);
      const delta = normalizeWheelDelta(e, rect.width);
      const zoomGesture = e.metaKey || e.ctrlKey;

      // Functional update so EACH wheel event computes from the LATEST viewport,
      // not a value captured in this closure — back-to-back wheel events fire
      // faster than React re-renders the `viewport` prop, and using the stale
      // prop would drift the anchor / fail to accumulate zoom. `current` is the
      // up-to-date viewport; recompute its visible window from it.
      onViewportChange((current) => {
        const view = clampTimelineViewport(current, duration);
        const curVisible = getVisibleDuration(duration, view.zoom);

        if (zoomGesture) {
          // Keep the time under the cursor pinned: the seconds at the pointer
          // before the zoom must land back under the pointer after it.
          const anchorSeconds = view.start + pointerRatio * curVisible;
          const nextZoom = clamp(
            view.zoom * Math.exp(-delta * 0.0025),
            MIN_TIMELINE_ZOOM,
            MAX_TIMELINE_ZOOM,
          );
          const nextVisible = getVisibleDuration(duration, nextZoom);
          return {
            start: anchorSeconds - pointerRatio * nextVisible,
            zoom: nextZoom,
          };
        }

        // Plain shift-wheel pans horizontally at the current zoom.
        return {
          start: view.start + delta * (curVisible / rect.width),
          zoom: view.zoom,
        };
      });
    },
    [duration, onViewportChange],
  );

  useEffect(() => {
    const body = bodyRef.current;
    if (!body) return;
    body.addEventListener("wheel", handleWheel, { passive: false });
    return () => body.removeEventListener("wheel", handleWheel);
  }, [handleWheel]);

  // Motion transitions for the expand/collapse choreography. Snappy but clean:
  // position/size/scaleY ride an ease-out tween (easeOutQuint) that decelerates
  // to the target with NO overshoot/bounce; fades are short tweens. Under
  // reduced-motion every transition is instant. `nestedDelay` lightly staggers a
  // window's child bars so the box "unfolds" without dragging.
  const layoutEase = reduceMotion
    ? { duration: 0 }
    : ({ type: "tween", duration: 0.2, ease: [0.22, 1, 0.36, 1] } as const);
  const fadeIn = reduceMotion
    ? { duration: 0 }
    : ({ duration: 0.12, ease: [0.22, 1, 0.36, 1] } as const);
  const fadeOut = reduceMotion
    ? { duration: 0 }
    : ({ duration: 0.1, ease: "easeIn" } as const);
  const nestedDelay = (laneIndex: number) =>
    reduceMotion ? 0 : Math.min(laneIndex * 0.015, 0.05);
  // Collapse-handle chevron spin: rotates between collapsed/expanded on the same
  // ease-out curve; instant under reduced-motion.
  const chevronSpin = reduceMotion
    ? { duration: 0 }
    : ({ type: "tween", duration: 0.18, ease: [0.22, 1, 0.36, 1] } as const);

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
            // A header lane with no clips is a de-emphasized section fallback.
            // A header lane with clips is a container lane (grouping summary or
            // leaf-container title), where the body clip carries the main label.
            const isSectionHeader = lane.header != null && lane.clips.length === 0;
            return (
              <div
                className={
                  isSectionHeader
                    ? "track-head track-head--group"
                    : lane.header
                      ? "track-head track-head--title"
                      : "track-head"
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
                      // Blur so focus doesn't stay on the chevron and swallow
                      // the global Space/arrow keyboard shortcuts.
                      e.currentTarget.blur();
                    }}
                  >
                    {/* One chevron that ROTATES between states (0deg collapsed →
                        90deg expanded) instead of swapping glyphs, so the handle
                        turns smoothly on toggle. Snap under reduced-motion. */}
                    <motion.span
                      className="track-head__chevron-icon"
                      animate={{ rotate: isCollapsed ? 0 : 90 }}
                      transition={chevronSpin}
                    >
                      <ChevronRight size={13} strokeWidth={2} />
                    </motion.span>
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
            ? lanes.map((lane, laneIndex) => {
                if (lane.header && lane.clips.length === 0) {
                  // Empty section row: no clip, just a faint full-row hairline.
                  return (
                    <div key={lane.id} className="timeline__track">
                      <div className="timeline__group-line" />
                    </div>
                  );
                }
                return (
                  <div key={lane.id} className="timeline__track">
                    {/* AnimatePresence lets a window's child/title bars fade +
                        rise in on expand and out on collapse. Horizontal
                        position (left/width) is a plain style so pan/zoom is
                        instant; only opacity/y animate. */}
                    <AnimatePresence initial={false}>
                      {lane.clips.map((clip) => {
                        // Exact time->x projection — NO horizontal inset, so a
                        // nested child bar is flush to the same edges as its
                        // window frame (the horizontal axis is faithful).
                        const left =
                          (clamp(clip.start, 0, duration) / duration) *
                          innerWidth;
                        const right =
                          (clamp(clip.end, 0, duration) / duration) *
                          innerWidth;
                        const width = Math.max(right - left, 2);
                        // A clip with `collapsedNode` toggles a node on click: a
                        // leaf-container title bar (collapsed or expanded) or a
                        // collapsed grouping summary. `isTitle` selects the
                        // window-title look; the handle reflects collapsed state.
                        const toggles = clip.collapsedNode != null;
                        const clipCollapsed =
                          toggles && collapsed.has(clip.collapsedNode!);
                        const nested =
                          clip.frameOwner != null && !clip.isTitle;
                        const color = colorFor(
                          clip.kind,
                          clip.isComponent,
                          clip.hasChildren,
                        );
                        const classes = ["timeline__clip"];
                        if (clip.isTitle)
                          classes.push("timeline__clip--title");
                        else if (toggles)
                          classes.push("timeline__clip--summary");
                        if (nested) classes.push("timeline__clip--nested");
                        if (clip.id === selectedId)
                          classes.push("timeline__clip--selected");
                        // Sticky label: the track is translated by -viewportX, so
                        // a clip at inline `left` shows at screen-x `left -
                        // viewportX`. When its start is scrolled off the left
                        // (left < viewportX), push the label right so it stays at
                        // the viewport edge, but never past `width - LABEL_RESERVE`
                        // so it stops at the clip's right end.
                        const stickyOffset = clamp(
                          viewportX - left,
                          0,
                          Math.max(0, width - LABEL_RESERVE),
                        );
                        return (
                          <motion.div
                            key={clip.id}
                            className={classes.join(" ")}
                            // `left`/`width` are the time->x projection: plain
                            // inline styles (NOT animated) so pan/zoom updates
                            // them instantly with no catch-up tween. Only the
                            // open/close props (opacity, y) animate.
                            style={{
                              left,
                              width,
                              background: color.fill,
                              color: color.text,
                            }}
                            initial={{ opacity: 0, y: reduceMotion ? 0 : -6 }}
                            animate={{ opacity: 1, y: 0 }}
                            exit={{
                              opacity: 0,
                              y: reduceMotion ? 0 : -6,
                              transition: fadeOut,
                            }}
                            transition={{
                              y: layoutEase,
                              // Stagger nested bars so the window unfolds.
                              opacity: nested
                                ? { ...fadeIn, delay: nestedDelay(laneIndex) }
                                : fadeIn,
                            }}
                            // Clicking a clip SELECTS it (not seek): stop the
                            // pointer so the body's seek/scrub handler doesn't
                            // fire, then report the clicked node to App (which
                            // feeds the Inspector). The collapse chevron below
                            // stops propagation itself, so chevron clicks toggle
                            // without bubbling up to select here.
                            //
                            // Second click on an ALREADY-SELECTED container bar
                            // (one with `collapsedNode`, i.e. a collapsible
                            // window/summary) toggles its collapse instead of
                            // re-selecting — so a user can open/close it by
                            // clicking the bar twice, not just the chevron.
                            // Selection is kept (Inspector stays put). Leaf bars
                            // have no `collapsedNode`, so they always just select.
                            onPointerDown={(e) => e.stopPropagation()}
                            onClick={(e) => {
                              e.stopPropagation();
                              if (
                                clip.id === selectedId &&
                                clip.collapsedNode != null
                              ) {
                                toggleCollapsed(clip.collapsedNode);
                                return;
                              }
                              onSelect({
                                id: clip.id,
                                name: clip.name,
                                kind: clip.kind,
                                source: clip.source,
                                start: clip.start,
                                end: clip.end,
                                triggers: clip.triggers,
                              });
                            }}
                          >
                            {/* Container bars get a toggle handle + a component
                                type icon; plain leaf clips get neither. The
                                handle is its own button that toggles collapse and
                                stops propagation, so clicking it does NOT bubble
                                up to select the clip (or seek). */}
                            {toggles ? (
                              <button
                                type="button"
                                className="timeline__clip-toggle"
                                aria-label={
                                  clipCollapsed ? "Expand" : "Collapse"
                                }
                                // Sticky like the label: the toggle/icon precede
                                // the label in flex order, so they get the same
                                // offset to stay together at the viewport edge.
                                style={{
                                  transform: `translateX(${stickyOffset}px)`,
                                }}
                                onPointerDown={(e) => e.stopPropagation()}
                                onClick={(e) => {
                                  e.stopPropagation();
                                  toggleCollapsed(clip.collapsedNode!);
                                  // Blur so focus doesn't stay on the toggle and
                                  // swallow the global Space/arrow shortcuts.
                                  e.currentTarget.blur();
                                }}
                              >
                                {/* Rotate one chevron (0deg collapsed → 90deg
                                    expanded) rather than swapping glyphs. The
                                    rotation lives on this inner span, NOT the
                                    button (the button keeps its sticky
                                    translateX), so #3's sticky offset is intact.
                                    Snap under reduced-motion. */}
                                <motion.span
                                  className="timeline__clip-toggle-icon"
                                  animate={{ rotate: clipCollapsed ? 0 : 90 }}
                                  transition={chevronSpin}
                                >
                                  <ChevronRight size={13} strokeWidth={2} />
                                </motion.span>
                              </button>
                            ) : null}
                            {toggles ? (
                              <span
                                className="timeline__clip-icon"
                                aria-hidden="true"
                                style={{
                                  transform: `translateX(${stickyOffset}px)`,
                                }}
                              >
                                <Component size={13} strokeWidth={2} />
                              </span>
                            ) : null}
                            <span
                              className="timeline__clip-label"
                              style={{
                                transform: `translateX(${stickyOffset}px)`,
                              }}
                            >
                              {clip.name}
                            </span>
                          </motion.div>
                        );
                      })}
                    </AnimatePresence>
                    {/* Per-clip flags, rendered at TRACK level (not inside the
                        clip, whose overflow:hidden would clip them) so each flag
                        sticks UP above its emitting clip's top edge at the
                        trigger's x. The track is full-width, so the flag's left
                        is the absolute trigger projection; it tracks the clip on
                        pan/zoom since the whole tracks layer is translated. */}
                    {lane.clips.flatMap((clip) =>
                      clip.triggers.map((trigger, i) => {
                        const triggerX =
                          (clamp(trigger.time, 0, duration) / duration) *
                          innerWidth;
                        const flagLabel = trigger.name ?? "Event";
                        return (
                          <span
                            key={`flag:${clip.id}:${i}`}
                            className="timeline__event-flag"
                            title={`Event "${flagLabel}" @ ${trigger.time.toFixed(3)}s`}
                            style={{ left: `${triggerX}px` }}
                          >
                            <Flag size={12} strokeWidth={2} />
                            <span className="timeline__event-name">
                              {flagLabel}
                            </span>
                          </span>
                        );
                      }),
                    )}
                  </div>
                );
              })
            : timeline ? (
                <div className="timeline__track">
                  <div
                    className="timeline__clip timeline__clip--fallback"
                    style={{ left: 0, width: `${innerWidth}px` }}
                  >
                    <span className="timeline__clip-label">
                      {timeline.title}
                    </span>
                  </div>
                </div>
              ) : null}
          {/* Window frames: one rounded box per expanded leaf-container, drawn
              under the clips (pointer-events:none) so clicks still reach the
              title-bar chevron and child clips. AnimatePresence grows the box
              in from the title bar (transform-origin top) on expand and shrinks
              it out on collapse; only top/height/scaleY (the vertical grow) glide
              via layoutEase — left/width are instant for pan/zoom. */}
          <AnimatePresence initial={false}>
            {frames.map((frame) => {
              const left = (clamp(frame.start, 0, duration) / duration) *
                innerWidth;
              const right = (clamp(frame.end, 0, duration) / duration) *
                innerWidth;
              const width = Math.max(right - left, 2);
              const top = frame.laneStart * TRACK_H;
              const height = frame.laneCount * TRACK_H;
              // Border matches the owner color (green for a component Dialogue);
              // the interior is that color at low alpha (light-green tint). A
              // window owner is always a container, so hasChildren is true — a
              // component window never goes blue.
              const color = colorFor(frame.kind, frame.isComponent, true);
              return (
                <motion.div
                  key={`frame:${frame.path}`}
                  className="timeline__window"
                  // `left`/`width` are the time->x projection: plain inline
                  // styles (NOT animated) so pan/zoom updates them instantly.
                  // Only the open/close props (top, height, scaleY, opacity —
                  // the vertical grow/reflow) animate via layoutEase/fades.
                  style={{
                    left,
                    width,
                    borderColor: color.border,
                    background: color.tint,
                    transformOrigin: "top",
                  }}
                  initial={{
                    top,
                    height,
                    opacity: 0,
                    scaleY: reduceMotion ? 1 : 0.4,
                  }}
                  animate={{ top, height, opacity: 1, scaleY: 1 }}
                  exit={{
                    opacity: 0,
                    scaleY: reduceMotion ? 1 : 0.4,
                    transition: fadeOut,
                  }}
                  transition={{
                    top: layoutEase,
                    height: layoutEase,
                    scaleY: layoutEase,
                    opacity: fadeIn,
                    default: fadeIn,
                  }}
                />
              );
            })}
          </AnimatePresence>
          {/* Event reference lines: one full-height cyan line per DISTINCT
              trigger time (deduped so coincident clips don't double-draw the
              line). Each emitting clip still gets its own flag above it (above).
              Rendered inside `.timeline__tracks` so they pan/zoom with the
              playhead; the cyan accent stays distinct from the pink playhead. */}
          {eventTimes.map((time) => {
            const x = (clamp(time, 0, duration) / duration) * innerWidth;
            return (
              <div
                key={`event-line:${time}`}
                className="timeline__event"
                style={{ left: `${x}px` }}
              />
            );
          })}
          <div
            className="timeline__playhead"
            style={{ left: `${playheadX}px` }}
          />
        </div>
      </div>
    </section>
  );
}

function normalizeWheelDelta(e: WheelEvent, pageSize: number): number {
  const rawDelta =
    Math.abs(e.deltaX) > Math.abs(e.deltaY) ? e.deltaX : e.deltaY;

  if (e.deltaMode === 1) return rawDelta * 16;
  if (e.deltaMode === 2) return rawDelta * pageSize;
  return rawDelta;
}
