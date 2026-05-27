export const MIN_TIMELINE_ZOOM = 1;
export const MAX_TIMELINE_ZOOM = 20;

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
