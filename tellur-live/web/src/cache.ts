import type { CacheRange } from "./types";

const EPSILON = 1e-4;

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
