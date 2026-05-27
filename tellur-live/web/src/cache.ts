import type { CacheRange } from "./types";

const EPSILON = 1e-4;
const RANGE_PREFIX = "tellur-live:cache-ranges:";

// Merge a new [start, end] range into an existing sorted, disjoint list.
// Adjacent or overlapping ranges (within EPSILON) are coalesced so the
// rendered green bar stays continuous when playback walks forward frame
// by frame.
export function mergeCacheRange(
  ranges: CacheRange[],
  start: number,
  end: number,
): CacheRange[] {
  if (!(end > start)) return ranges;
  const next: CacheRange[] = [];
  let cur: CacheRange = { start, end };
  let inserted = false;
  for (const range of ranges) {
    if (range.end < cur.start - EPSILON) {
      next.push(range);
      continue;
    }
    if (range.start > cur.end + EPSILON) {
      if (!inserted) {
        next.push(cur);
        inserted = true;
      }
      next.push(range);
      continue;
    }
    cur = {
      start: Math.min(cur.start, range.start),
      end: Math.max(cur.end, range.end),
    };
  }
  if (!inserted) next.push(cur);
  return next;
}

export function cacheScopeKey(parts: {
  cacheKey: string;
  timelineId: string;
  width: number;
  height: number;
  fps: number;
  gop: number;
  crf: number;
}): string {
  return [
    parts.cacheKey,
    parts.timelineId,
    `${parts.width}x${parts.height}`,
    String(parts.fps),
    String(parts.gop),
    String(parts.crf),
  ].join("|");
}

export function loadCacheRanges(scope: string): CacheRange[] {
  if (!scope || typeof window === "undefined") return [];
  try {
    const raw = window.localStorage.getItem(`${RANGE_PREFIX}${scope}`);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed
      .map((range) => ({
        start: Number(range?.start),
        end: Number(range?.end),
      }))
      .filter(
        (range) =>
          Number.isFinite(range.start) &&
          Number.isFinite(range.end) &&
          range.end > range.start,
      );
  } catch {
    return [];
  }
}

export function saveCacheRanges(scope: string, ranges: CacheRange[]): void {
  if (!scope || typeof window === "undefined") return;
  try {
    window.localStorage.setItem(`${RANGE_PREFIX}${scope}`, JSON.stringify(ranges));
  } catch {
    // Browser storage is opportunistic; the in-memory cache display still works.
  }
}

export function revokeStaleCacheRanges(currentCacheKey: string): void {
  if (!currentCacheKey || typeof window === "undefined") return;
  try {
    const keepPrefix = `${RANGE_PREFIX}${currentCacheKey}|`;
    for (let i = window.localStorage.length - 1; i >= 0; i--) {
      const key = window.localStorage.key(i);
      if (key?.startsWith(RANGE_PREFIX) && !key.startsWith(keepPrefix)) {
        window.localStorage.removeItem(key);
      }
    }
  } catch {
    // ignore
  }
}
