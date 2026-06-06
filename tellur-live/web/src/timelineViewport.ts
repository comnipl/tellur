// Zoom < 1 shrinks the whole timeline NARROWER than the viewport (content
// left-aligned, empty space to the right); zoom 1 fits it exactly.
export const MIN_TIMELINE_ZOOM = 0.2;
export const MAX_TIMELINE_ZOOM = 20;

// Initial zoom on load: innerWidth = bodyWidth * zoom, so 2/3 makes the full
// duration span ~2/3 of the panel width — left-aligned with ~1/3 empty room to
// its right (pulled back from a fit-to-width zoom of 1).
export const DEFAULT_TIMELINE_ZOOM = 2 / 3;

export interface TimelineViewport {
  start: number;
  zoom: number;
}

export type TimelineViewportChange =
  | TimelineViewport
  | ((current: TimelineViewport) => TimelineViewport);

export function clampTimelineViewport(
  viewport: TimelineViewport,
  duration: number,
): TimelineViewport {
  const safeDuration = getSafeDuration(duration);
  const zoom = clamp(
    viewport.zoom,
    MIN_TIMELINE_ZOOM,
    MAX_TIMELINE_ZOOM,
  );
  const visibleDuration = getVisibleDuration(safeDuration, zoom);
  // Start that brings the content end flush to the viewport's right edge. At
  // zoom <= 1 the visible window meets/exceeds the content, so this is <= 0 (no
  // pan room; content stays left-aligned).
  const contentEndStart = safeDuration - visibleDuration;
  // When zoomed IN (content wider than the viewport) allow trailing overscroll
  // of ~one viewport width past the content end, so the user can scroll into the
  // empty space after the last clip. When zoomed out, there's no pan room and no
  // overscroll — start stays clamped to 0 (unchanged behavior).
  const maxStart =
    contentEndStart > 0
      ? contentEndStart + visibleDuration * TRAILING_OVERSCROLL
      : 0;

  return {
    start: clamp(viewport.start, 0, maxStart),
    zoom,
  };
}

// Trailing overscroll past the content end, in units of the visible window
// (1 = one full viewport width of empty space after the last clip). Exported so
// Timeline.tsx widens its body translate clamp by the same amount, keeping body
// + ruler in lock-step through the overscroll region.
export const TRAILING_OVERSCROLL = 1;

export function getVisibleDuration(duration: number, zoom: number): number {
  const safeDuration = getSafeDuration(duration);
  const safeZoom = clamp(zoom, MIN_TIMELINE_ZOOM, MAX_TIMELINE_ZOOM);
  return safeDuration / safeZoom;
}

export function getSafeDuration(duration: number): number {
  return Math.max(duration, 0.001);
}

export function clamp(value: number, min: number, max: number): number {
  if (!Number.isFinite(value)) return min;
  return Math.min(max, Math.max(min, value));
}
