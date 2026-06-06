import type { CacheRange } from "./types";

// Bump whenever the server's encode format changes. "segments-v1" is a fresh
// value that abandons all data from the old fixed-grid "chunks-v1" scheme.
export const ENCODE_FORMAT_VERSION = "segments-v1";

// Small tolerance used in floating-point boundary comparisons.
const EPS = 1e-4;

const SEGMENT_DB_NAME = "tellur-live-segments";
const SEGMENT_DB_VERSION = 1;
const SEGMENT_STORE = "segments";
const LEGACY_MEDIA_CACHE_PREFIX = "tellur-live-media-v1-";

// ---------------------------------------------------------------------------
// Entry shape stored in IndexedDB
// ---------------------------------------------------------------------------

interface SegmentEntry {
  /** Compound key: `${groupKey}\n${start.toFixed(4)}\n${end.toFixed(4)}` */
  id: string;
  groupKey: string;
  pluginKey: string;
  start: number;
  end: number;
  blob: Blob;
  createdAt: number;
}

// ---------------------------------------------------------------------------
// Module-level state
// ---------------------------------------------------------------------------

let dbPromise: Promise<IDBDatabase> | null = null;

// The currently active plugin key. putSegment re-checks this after the await
// and rolls back its write when the plugin changed mid-flight.
let activePluginKey = "";

// One-time legacy cleanup; memoized so it only runs once per page load.
let legacyCleanupPromise: Promise<boolean> | null = null;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

export interface ChunkGroupParams {
  /** Server cacheKey; "" when unknown. */
  pluginKey: string;
  timelineId: string;
  width: number;
  height: number;
  fps: number;
  gop: number;
  crf: number;
}

export interface CachedSegment {
  start: number;
  end: number;
  blob: Blob;
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/**
 * Build a stable group key string from the video parameters.
 * Returns "" if pluginKey or timelineId is empty, so callers can treat ""
 * as "not yet known" without storing anything.
 */
export function groupKeyOf(p: ChunkGroupParams): string {
  if (!p.pluginKey || !p.timelineId) return "";
  return [
    p.pluginKey,
    ENCODE_FORMAT_VERSION,
    p.timelineId,
    `${p.width}x${p.height}`,
    String(p.fps),
    String(p.gop),
    String(p.crf),
  ].join("|");
}

// ---------------------------------------------------------------------------
// Public interface
// ---------------------------------------------------------------------------

export interface MediaCache {
  /**
   * The cached segment whose [start, end) brackets t
   * (start <= t+EPS && end > t+EPS). If several match, the one with the
   * largest start wins. Returns null when none match.
   */
  segmentAt(groupKey: string, t: number): Promise<CachedSegment | null>;
  /**
   * The smallest segment start strictly greater than t (start > t+EPS),
   * or null if none exists.
   */
  nextSegmentStart(groupKey: string, t: number): Promise<number | null>;
  /**
   * Persist a streamed segment [start, end).
   * Returns true iff persisted AND the plugin is still active. Re-checks
   * activePluginKey AFTER the put; if it changed, deletes this entry only
   * and returns false. After a successful put, subsumes any existing segments
   * of the same group fully contained in [start-EPS, end+EPS] so duplicates
   * do not accumulate. Ignores calls where !(end > start + EPS).
   */
  putSegment(
    groupKey: string,
    pluginKey: string,
    start: number,
    end: number,
    blob: Blob,
  ): Promise<boolean>;
  /**
   * Merged, sorted, non-overlapping cached ranges for the green bar.
   * Adjacent segments touching within EPS are coalesced. Returns [] on error.
   */
  cachedRanges(groupKey: string): Promise<CacheRange[]>;
  /**
   * Mark pluginKey active and purge entries of OTHER plugins; also runs the
   * one-time legacy ServiceWorker/CacheStorage cleanup. No-op when "".
   */
  activatePlugin(pluginKey: string): Promise<void>;
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

export function createMediaCache(): MediaCache {
  return {
    segmentAt,
    nextSegmentStart,
    putSegment,
    cachedRanges,
    activatePlugin,
  };
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

async function segmentAt(
  groupKey: string,
  t: number,
): Promise<CachedSegment | null> {
  if (!groupKey || !canUseIndexedDb()) return null;
  try {
    const db = await openSegmentDb();
    return await new Promise<CachedSegment | null>((resolve, reject) => {
      let best: CachedSegment | null = null;
      const tx = db.transaction(SEGMENT_STORE, "readonly");
      const request = tx
        .objectStore(SEGMENT_STORE)
        .index("groupKey")
        .openCursor(IDBKeyRange.only(groupKey));
      request.onerror = () =>
        reject(request.error ?? new Error("IndexedDB cursor failed"));
      request.onsuccess = () => {
        const cursor = request.result;
        if (!cursor) {
          resolve(best);
          return;
        }
        const entry = cursor.value as Partial<SegmentEntry>;
        if (
          typeof entry.start === "number" &&
          typeof entry.end === "number" &&
          entry.blob instanceof Blob &&
          entry.start <= t + EPS &&
          entry.end > t + EPS
        ) {
          if (best === null || entry.start > best.start) {
            best = { start: entry.start, end: entry.end, blob: entry.blob };
          }
        }
        cursor.continue();
      };
    });
  } catch (e) {
    console.warn("tellur-live segment cache segmentAt failed", e);
    return null;
  }
}

async function nextSegmentStart(
  groupKey: string,
  t: number,
): Promise<number | null> {
  if (!groupKey || !canUseIndexedDb()) return null;
  try {
    const db = await openSegmentDb();
    return await new Promise<number | null>((resolve, reject) => {
      let best: number | null = null;
      const tx = db.transaction(SEGMENT_STORE, "readonly");
      const request = tx
        .objectStore(SEGMENT_STORE)
        .index("groupKey")
        .openCursor(IDBKeyRange.only(groupKey));
      request.onerror = () =>
        reject(request.error ?? new Error("IndexedDB cursor failed"));
      request.onsuccess = () => {
        const cursor = request.result;
        if (!cursor) {
          resolve(best);
          return;
        }
        const entry = cursor.value as Partial<SegmentEntry>;
        if (
          typeof entry.start === "number" &&
          entry.start > t + EPS
        ) {
          if (best === null || entry.start < best) {
            best = entry.start;
          }
        }
        cursor.continue();
      };
    });
  } catch (e) {
    console.warn("tellur-live segment cache nextSegmentStart failed", e);
    return null;
  }
}

async function putSegment(
  groupKey: string,
  pluginKey: string,
  start: number,
  end: number,
  blob: Blob,
): Promise<boolean> {
  if (!groupKey || !pluginKey || !canUseIndexedDb()) return false;
  // Ignore degenerate segments.
  if (!(end > start + EPS)) return false;
  try {
    const db = await openSegmentDb();
    const entry: SegmentEntry = {
      id: segmentId(groupKey, start, end),
      groupKey,
      pluginKey,
      start,
      end,
      blob,
      createdAt: Date.now(),
    };
    const tx = db.transaction(SEGMENT_STORE, "readwrite");
    const done = transactionDone(tx);
    await requestToPromise(tx.objectStore(SEGMENT_STORE).put(entry));
    await done;

    // Re-check after the await: if the plugin changed mid-write, roll back ONLY
    // this just-written entry. Bulk-purging the whole (now-inactive) plugin is
    // activatePlugin's job.
    if (activePluginKey && activePluginKey !== pluginKey) {
      await deleteSegmentById(db, entry.id);
      return false;
    }

    // Subsume any existing segments of the same group fully contained within
    // [start-EPS, end+EPS] so duplicates do not accumulate.
    await deleteSubsumedSegments(db, groupKey, start, end, entry.id);
    return true;
  } catch (e) {
    console.warn("tellur-live segment cache write failed", e);
    return false;
  }
}

async function cachedRanges(groupKey: string): Promise<CacheRange[]> {
  if (!groupKey || !canUseIndexedDb()) return [];
  try {
    const db = await openSegmentDb();
    const pairs = await new Promise<{ start: number; end: number }[]>(
      (resolve, reject) => {
        const result: { start: number; end: number }[] = [];
        const tx = db.transaction(SEGMENT_STORE, "readonly");
        const request = tx
          .objectStore(SEGMENT_STORE)
          .index("groupKey")
          .openCursor(IDBKeyRange.only(groupKey));
        request.onerror = () =>
          reject(request.error ?? new Error("IndexedDB cursor failed"));
        request.onsuccess = () => {
          const cursor = request.result;
          if (!cursor) {
            resolve(result);
            return;
          }
          const entry = cursor.value as Partial<SegmentEntry>;
          if (
            typeof entry.start === "number" &&
            typeof entry.end === "number"
          ) {
            result.push({ start: entry.start, end: entry.end });
          }
          cursor.continue();
        };
      },
    );

    if (pairs.length === 0) return [];

    pairs.sort((a, b) => a.start - b.start);

    // Coalesce segments whose boundaries touch within EPS.
    const ranges: CacheRange[] = [];
    let current = { start: pairs[0].start, end: pairs[0].end };
    for (let i = 1; i < pairs.length; i++) {
      const p = pairs[i];
      if (p.start <= current.end + EPS) {
        // Overlapping or adjacent — extend.
        if (p.end > current.end) current.end = p.end;
      } else {
        ranges.push({ start: current.start, end: current.end });
        current = { start: p.start, end: p.end };
      }
    }
    ranges.push({ start: current.start, end: current.end });
    return ranges;
  } catch (e) {
    console.warn("tellur-live segment cache cachedRanges failed", e);
    return [];
  }
}

async function activatePlugin(pluginKey: string): Promise<void> {
  if (!pluginKey) return;
  activePluginKey = pluginKey;
  await Promise.all([
    purgeOtherPlugins(pluginKey),
    cleanupLegacyMediaCaches(),
  ]);
}

// ---------------------------------------------------------------------------
// Internal DB helpers
// ---------------------------------------------------------------------------

/**
 * Delete every segment entry whose pluginKey differs from `keepKey`.
 * Uses a full-store cursor rather than a per-pluginKey index, since we want
 * to remove ALL other plugins, not a single target key.
 */
async function purgeOtherPlugins(keepKey: string): Promise<void> {
  if (!canUseIndexedDb()) return;
  try {
    const db = await openSegmentDb();
    await new Promise<void>((resolve, reject) => {
      const tx = db.transaction(SEGMENT_STORE, "readwrite");
      tx.oncomplete = () => resolve();
      tx.onerror = () =>
        reject(tx.error ?? new Error("IndexedDB purge failed"));
      tx.onabort = () =>
        reject(tx.error ?? new Error("IndexedDB purge aborted"));

      const request = tx.objectStore(SEGMENT_STORE).openCursor();
      request.onerror = () =>
        reject(request.error ?? new Error("IndexedDB cursor failed"));
      request.onsuccess = () => {
        const cursor = request.result;
        if (!cursor) return;
        const entry = cursor.value as Partial<SegmentEntry>;
        if (entry.pluginKey !== keepKey) {
          cursor.delete();
        }
        cursor.continue();
      };
    });
  } catch (e) {
    console.warn("tellur-live segment cache purge failed", e);
  }
}

/** Delete a single segment entry by its id (used to roll back a raced write). */
async function deleteSegmentById(db: IDBDatabase, id: string): Promise<void> {
  try {
    const tx = db.transaction(SEGMENT_STORE, "readwrite");
    const done = transactionDone(tx);
    tx.objectStore(SEGMENT_STORE).delete(id);
    await done;
  } catch (e) {
    console.warn("tellur-live segment cache rollback delete failed", e);
  }
}

/**
 * Delete existing segments of the same group that are fully contained within
 * [start-EPS, end+EPS], excluding the entry we just wrote (by `excludeId`).
 * A segment is subsumed when its start >= start-EPS AND its end <= end+EPS.
 */
async function deleteSubsumedSegments(
  db: IDBDatabase,
  groupKey: string,
  start: number,
  end: number,
  excludeId: string,
): Promise<void> {
  try {
    await new Promise<void>((resolve, reject) => {
      const tx = db.transaction(SEGMENT_STORE, "readwrite");
      tx.oncomplete = () => resolve();
      tx.onerror = () =>
        reject(tx.error ?? new Error("IndexedDB subsume failed"));
      tx.onabort = () =>
        reject(tx.error ?? new Error("IndexedDB subsume aborted"));

      const request = tx
        .objectStore(SEGMENT_STORE)
        .index("groupKey")
        .openCursor(IDBKeyRange.only(groupKey));
      request.onerror = () =>
        reject(request.error ?? new Error("IndexedDB cursor failed"));
      request.onsuccess = () => {
        const cursor = request.result;
        if (!cursor) return;
        const entry = cursor.value as Partial<SegmentEntry>;
        if (
          entry.id !== excludeId &&
          typeof entry.start === "number" &&
          typeof entry.end === "number" &&
          entry.start >= start - EPS &&
          entry.end <= end + EPS
        ) {
          cursor.delete();
        }
        cursor.continue();
      };
    });
  } catch (e) {
    console.warn("tellur-live segment cache subsume failed", e);
  }
}

// ---------------------------------------------------------------------------
// DB open / schema
// ---------------------------------------------------------------------------

function openSegmentDb(): Promise<IDBDatabase> {
  if (dbPromise) return dbPromise;
  dbPromise = new Promise((resolve, reject) => {
    const request = indexedDB.open(SEGMENT_DB_NAME, SEGMENT_DB_VERSION);
    request.onerror = () =>
      reject(request.error ?? new Error("IndexedDB open failed"));
    request.onblocked = () => reject(new Error("IndexedDB upgrade blocked"));
    request.onupgradeneeded = (event) => {
      const db = request.result;
      // Drop + recreate on any version bump — no incremental migration.
      if (
        event.oldVersion > 0 &&
        db.objectStoreNames.contains(SEGMENT_STORE)
      ) {
        db.deleteObjectStore(SEGMENT_STORE);
      }
      const store = db.createObjectStore(SEGMENT_STORE, { keyPath: "id" });
      store.createIndex("pluginKey", "pluginKey", { unique: false });
      store.createIndex("groupKey", "groupKey", { unique: false });
    };
    request.onsuccess = () => {
      const db = request.result;
      // Another tab opened a newer version — close and reset so the next call
      // re-opens with the new schema.
      db.onversionchange = () => {
        db.close();
        dbPromise = null;
      };
      resolve(db);
    };
  });
  // Clear the memoised promise on failure so the next call retries cleanly.
  dbPromise.catch(() => {
    dbPromise = null;
  });
  return dbPromise;
}

// ---------------------------------------------------------------------------
// IndexedDB utility helpers
// ---------------------------------------------------------------------------

function requestToPromise<T>(request: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    request.onerror = () =>
      reject(request.error ?? new Error("IndexedDB request failed"));
    request.onsuccess = () => resolve(request.result);
  });
}

function transactionDone(tx: IDBTransaction): Promise<void> {
  return new Promise((resolve, reject) => {
    tx.oncomplete = () => resolve();
    tx.onerror = () =>
      reject(tx.error ?? new Error("IndexedDB transaction failed"));
    tx.onabort = () =>
      reject(tx.error ?? new Error("IndexedDB transaction aborted"));
  });
}

function canUseIndexedDb(): boolean {
  return typeof indexedDB !== "undefined" && typeof Blob !== "undefined";
}

function segmentId(groupKey: string, start: number, end: number): string {
  return `${groupKey}\n${start.toFixed(4)}\n${end.toFixed(4)}`;
}

// ---------------------------------------------------------------------------
// Legacy cleanup (ported from old chunk-grid scheme)
// ---------------------------------------------------------------------------

/**
 * One-time cleanup of the legacy ServiceWorker-based media cache.
 * Memoised via `legacyCleanupPromise` so it runs at most once per page load,
 * regardless of how many times activatePlugin is called.
 */
async function cleanupLegacyMediaCaches(): Promise<boolean> {
  if (!legacyCleanupPromise) {
    const controllerWasLegacy = isLegacyMediaCacheWorker(
      navigatorWithServiceWorker()?.serviceWorker.controller ?? null,
    );
    legacyCleanupPromise = Promise.all([
      deleteLegacyCacheStorageBuckets(),
      unregisterLegacyMediaCacheWorker(),
    ]).then(([, unregistered]) => controllerWasLegacy || unregistered);
  }
  return legacyCleanupPromise;
}

async function deleteLegacyCacheStorageBuckets(): Promise<void> {
  if (typeof window === "undefined" || !("caches" in window)) return;
  const names = await window.caches.keys();
  await Promise.all(
    names
      .filter((name) => name.startsWith(LEGACY_MEDIA_CACHE_PREFIX))
      .map((name) => window.caches.delete(name)),
  );
}

async function unregisterLegacyMediaCacheWorker(): Promise<boolean> {
  const serviceWorker = navigatorWithServiceWorker()?.serviceWorker;
  if (!serviceWorker) return false;
  const registrations = await serviceWorker.getRegistrations();
  const legacyRegistrations = registrations.filter((registration) =>
    [
      registration.active,
      registration.installing,
      registration.waiting,
    ].some(isLegacyMediaCacheWorker),
  );
  const results = await Promise.all(
    legacyRegistrations.map((registration) => registration.unregister()),
  );
  return results.some(Boolean);
}

function navigatorWithServiceWorker(): Navigator | null {
  return typeof navigator !== "undefined" && "serviceWorker" in navigator
    ? navigator
    : null;
}

function isLegacyMediaCacheWorker(
  worker: ServiceWorker | null | undefined,
): boolean {
  return Boolean(worker?.scriptURL.endsWith("/tellur-live-sw.js"));
}
