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
  // At zoom < 1 the visible window exceeds the content, so maxStart is 0 and
  // `start` clamps to 0 — the content is left-aligned with no pan room.
  const maxStart = Math.max(0, safeDuration - visibleDuration);

  return {
    start: clamp(viewport.start, 0, maxStart),
    zoom,
  };
}

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
