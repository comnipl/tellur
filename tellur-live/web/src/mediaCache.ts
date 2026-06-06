import type { CacheRange } from "./types";

// Fixed chunk duration in seconds. Every cached unit is exactly this long
// (the last chunk of a timeline may be shorter — see chunkRange).
export const CHUNK_SECONDS = 2;

// Bump this whenever the server's encode format changes, even if the plugin
// .so (and thus pluginKey) is unchanged. The chunk-grid scheme is brand new,
// so all old arbitrary-range blobs are incompatible; start at a fresh value.
export const ENCODE_FORMAT_VERSION = "chunks-v1";

// Small tolerance used in floating-point boundary comparisons.
const EPS = 1e-4;

const CHUNK_DB_NAME = "tellur-live-chunks";
const CHUNK_DB_VERSION = 1;
const CHUNK_STORE = "chunks";
const LEGACY_MEDIA_CACHE_PREFIX = "tellur-live-media-v1-";

// ---------------------------------------------------------------------------
// Entry shape stored in IndexedDB
// ---------------------------------------------------------------------------

interface ChunkEntry {
  /** Compound key: `${groupKey}\n${index}` */
  id: string;
  groupKey: string;
  pluginKey: string;
  index: number;
  blob: Blob;
  createdAt: number;
}

// ---------------------------------------------------------------------------
// Module-level state
// ---------------------------------------------------------------------------

let dbPromise: Promise<IDBDatabase> | null = null;

// The currently active plugin key. putChunk re-checks this after the await
// and rolls back its write when the plugin changed mid-flight (same pattern
// as activeMediaCacheKey in cache.ts).
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

/** Zero-based chunk index for a given timeline position in seconds. */
export function chunkIndexAt(t: number): number {
  return Math.floor(Math.max(0, t) / CHUNK_SECONDS);
}

/**
 * Total number of chunks needed to cover a timeline of the given duration.
 * Returns 0 when duration is non-positive.
 */
export function chunkCount(duration: number): number {
  if (duration <= 0) return 0;
  return Math.max(0, Math.ceil((duration - EPS) / CHUNK_SECONDS));
}

/**
 * Half-open time interval [start, end) for chunk at `index`, clamped so that
 * the last chunk does not exceed `duration`.
 */
export function chunkRange(
  index: number,
  duration: number,
): { start: number; end: number } {
  const start = index * CHUNK_SECONDS;
  const end = Math.min((index + 1) * CHUNK_SECONDS, duration);
  return { start, end };
}

/**
 * Convert a set of cached chunk indices into sorted, contiguous half-open
 * [start, end) ranges (in seconds), clamped by `duration`.
 * Consecutive indices (e.g. 0, 1, 2) merge into one range.
 * Used by the green progress-bar overlay.
 */
export function indicesToRanges(
  indices: Iterable<number>,
  duration: number,
): CacheRange[] {
  const total = chunkCount(duration);
  if (total === 0) return [];

  // Sort a copy of the indices so we can walk them in order.
  const sorted = Array.from(indices)
    .map((i) => Math.floor(i))
    .filter((i) => i >= 0 && i < total)
    .sort((a, b) => a - b);

  if (sorted.length === 0) return [];

  const result: CacheRange[] = [];
  let runStart = sorted[0];
  let runEnd = sorted[0];

  for (let k = 1; k < sorted.length; k++) {
    const idx = sorted[k];
    if (idx === runEnd + 1) {
      // Extend the current run.
      runEnd = idx;
    } else {
      result.push(makeRange(runStart, runEnd, duration));
      runStart = idx;
      runEnd = idx;
    }
  }
  result.push(makeRange(runStart, runEnd, duration));
  return result;
}

function makeRange(
  firstIdx: number,
  lastIdx: number,
  duration: number,
): CacheRange {
  return {
    start: firstIdx * CHUNK_SECONDS,
    end: Math.min((lastIdx + 1) * CHUNK_SECONDS, duration),
  };
}

// ---------------------------------------------------------------------------
// Public interface
// ---------------------------------------------------------------------------

export interface MediaCache {
  getChunk(groupKey: string, index: number): Promise<Blob | null>;
  /**
   * Persist a fully-received chunk blob.
   * Returns true iff persisted AND the plugin is still active.
   * Re-checks the module-level activePluginKey AFTER the put resolves; if it
   * changed mid-write, deletes the entry and returns false.
   */
  putChunk(
    groupKey: string,
    pluginKey: string,
    index: number,
    blob: Blob,
  ): Promise<boolean>;
  /** Returns an empty Set on unavailable/error. */
  cachedIndices(groupKey: string): Promise<Set<number>>;
  /**
   * Mark pluginKey active and purge entries belonging to OTHER plugins.
   * Also runs the one-time legacy ServiceWorker/CacheStorage cleanup.
   * Safe to call repeatedly; no-op when pluginKey is "".
   */
  activatePlugin(pluginKey: string): Promise<void>;
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

export function createMediaCache(): MediaCache {
  return {
    getChunk,
    putChunk,
    cachedIndices,
    activatePlugin,
  };
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

async function getChunk(groupKey: string, index: number): Promise<Blob | null> {
  if (!groupKey || !canUseIndexedDb()) return null;
  try {
    const db = await openChunkDb();
    const id = chunkId(groupKey, index);
    const entry = await requestToPromise<ChunkEntry | undefined>(
      db.transaction(CHUNK_STORE, "readonly").objectStore(CHUNK_STORE).get(id),
    );
    return entry?.blob ?? null;
  } catch (e) {
    console.warn("tellur-live chunk cache read failed", e);
    return null;
  }
}

async function putChunk(
  groupKey: string,
  pluginKey: string,
  index: number,
  blob: Blob,
): Promise<boolean> {
  if (!groupKey || !pluginKey || !canUseIndexedDb()) return false;
  try {
    const db = await openChunkDb();
    const entry: ChunkEntry = {
      id: chunkId(groupKey, index),
      groupKey,
      pluginKey,
      index,
      blob,
      createdAt: Date.now(),
    };
    const tx = db.transaction(CHUNK_STORE, "readwrite");
    const done = transactionDone(tx);
    await requestToPromise(tx.objectStore(CHUNK_STORE).put(entry));
    await done;

    // Re-check after the await: if the plugin changed mid-write, roll back ONLY this
    // just-written entry. Bulk-purging the whole (now-inactive) plugin is
    // activatePlugin's job; doing it here would wipe entries an A->B->A flip may still
    // want, and any genuinely stale entries are already covered by that purge.
    if (activePluginKey && activePluginKey !== pluginKey) {
      await deleteChunkById(db, entry.id);
      return false;
    }
    return true;
  } catch (e) {
    console.warn("tellur-live chunk cache write failed", e);
    return false;
  }
}

async function cachedIndices(groupKey: string): Promise<Set<number>> {
  if (!groupKey || !canUseIndexedDb()) return new Set();
  try {
    const db = await openChunkDb();
    return await new Promise<Set<number>>((resolve, reject) => {
      const result = new Set<number>();
      const tx = db.transaction(CHUNK_STORE, "readonly");
      const request = tx
        .objectStore(CHUNK_STORE)
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
        const entry = cursor.value as Partial<ChunkEntry>;
        if (typeof entry.index === "number") {
          result.add(entry.index);
        }
        cursor.continue();
      };
    });
  } catch (e) {
    console.warn("tellur-live chunk cachedIndices failed", e);
    return new Set();
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
 * Delete every chunk entry whose pluginKey differs from `keepKey`.
 * Uses a full-store cursor rather than a per-pluginKey index, since we want
 * to remove ALL other plugins, not a single target key.
 */
async function purgeOtherPlugins(keepKey: string): Promise<void> {
  if (!canUseIndexedDb()) return;
  try {
    const db = await openChunkDb();
    await new Promise<void>((resolve, reject) => {
      const tx = db.transaction(CHUNK_STORE, "readwrite");
      tx.oncomplete = () => resolve();
      tx.onerror = () =>
        reject(tx.error ?? new Error("IndexedDB purge failed"));
      tx.onabort = () =>
        reject(tx.error ?? new Error("IndexedDB purge aborted"));

      const request = tx.objectStore(CHUNK_STORE).openCursor();
      request.onerror = () =>
        reject(request.error ?? new Error("IndexedDB cursor failed"));
      request.onsuccess = () => {
        const cursor = request.result;
        if (!cursor) return;
        const entry = cursor.value as Partial<ChunkEntry>;
        if (entry.pluginKey !== keepKey) {
          cursor.delete();
        }
        cursor.continue();
      };
    });
  } catch (e) {
    console.warn("tellur-live chunk cache purge failed", e);
  }
}

/** Delete a single chunk entry by its id (used to roll back a raced write). */
async function deleteChunkById(db: IDBDatabase, id: string): Promise<void> {
  try {
    const tx = db.transaction(CHUNK_STORE, "readwrite");
    const done = transactionDone(tx);
    tx.objectStore(CHUNK_STORE).delete(id);
    await done;
  } catch (e) {
    console.warn("tellur-live chunk cache rollback delete failed", e);
  }
}

// ---------------------------------------------------------------------------
// DB open / schema
// ---------------------------------------------------------------------------

function openChunkDb(): Promise<IDBDatabase> {
  if (dbPromise) return dbPromise;
  dbPromise = new Promise((resolve, reject) => {
    const request = indexedDB.open(CHUNK_DB_NAME, CHUNK_DB_VERSION);
    request.onerror = () =>
      reject(request.error ?? new Error("IndexedDB open failed"));
    request.onblocked = () => reject(new Error("IndexedDB upgrade blocked"));
    request.onupgradeneeded = (event) => {
      const db = request.result;
      // Drop + recreate on any version bump — no incremental migration.
      if (event.oldVersion > 0 && db.objectStoreNames.contains(CHUNK_STORE)) {
        db.deleteObjectStore(CHUNK_STORE);
      }
      const store = db.createObjectStore(CHUNK_STORE, { keyPath: "id" });
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
// IndexedDB utility helpers (mirror of cache.ts)
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

function chunkId(groupKey: string, index: number): string {
  return `${groupKey}\n${index}`;
}

// ---------------------------------------------------------------------------
// Legacy cleanup (ported from cache.ts)
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
