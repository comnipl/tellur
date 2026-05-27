import type { CacheRange } from "./types";

const EPSILON = 1e-4;
const RANGE_PREFIX = "tellur-live:cache-ranges:";
const RANGE_SCOPE_VERSION = "video-ranges-v1";
const MEDIA_DB_NAME = "tellur-live-media";
const MEDIA_DB_VERSION = 3;
const MEDIA_STORE = "media";
const LEGACY_MEDIA_CACHE_PREFIX = "tellur-live-media-v1-";

interface MediaCacheEntry {
  id: string;
  cacheKey: string;
  url: string;
  blob: Blob;
  createdAt: number;
  kind?: "exact" | "video-range";
  group?: string;
  start?: number;
  end?: number;
}

export interface CachedMediaObjectUrl {
  objectUrl: string;
  persisted: boolean;
  fromCache: boolean;
}

export interface CachedVideoRange {
  start: number;
  end: number;
  url: string;
}

export interface CachedVideoRangeObjectUrl extends CachedVideoRange {
  objectUrl: string;
}

export interface CachedVideoRangeBlob extends CachedVideoRange {
  blob: Blob;
}

let dbPromise: Promise<IDBDatabase> | null = null;
let activeMediaCacheKey = "";
let legacyCleanupPromise: Promise<boolean> | null = null;

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
  videoSegmentSeconds: number;
}): string {
  return [
    parts.cacheKey,
    RANGE_SCOPE_VERSION,
    parts.timelineId,
    `${parts.width}x${parts.height}`,
    String(parts.fps),
    String(parts.gop),
    String(parts.crf),
    String(parts.videoSegmentSeconds),
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
    const keepPrefix = `${RANGE_PREFIX}${currentCacheKey}|${RANGE_SCOPE_VERSION}|`;
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

export async function loadCachedMediaObjectUrl(
  url: string,
  cacheKey: string,
  signal?: AbortSignal,
): Promise<CachedMediaObjectUrl> {
  if (!cacheKey) throw new Error("media cache key is empty");

  const cached = await getCachedMediaObjectUrl(url, cacheKey);
  if (cached) return cached;

  const response = await fetch(url, { cache: "no-store", signal });
  if (!response.ok) {
    throw new Error(`${url} failed: ${response.status}`);
  }

  const blob = await response.blob();
  let persisted = false;
  if (!activeMediaCacheKey || activeMediaCacheKey === cacheKey) {
    try {
      persisted = await putMediaEntry(url, cacheKey, blob);
      persisted &&= !activeMediaCacheKey || activeMediaCacheKey === cacheKey;
      if (!persisted) {
        await deleteMediaEntriesByCacheKey(cacheKey);
      }
    } catch (e) {
      console.warn("tellur-live IndexedDB media cache write failed", e);
    }
  }

  return {
    objectUrl: URL.createObjectURL(blob),
    persisted,
    fromCache: false,
  };
}

export async function ensureMediaCached(
  url: string,
  cacheKey: string,
  signal?: AbortSignal,
): Promise<boolean> {
  if (!cacheKey) return false;
  if (await getMediaEntry(url, cacheKey)) return true;

  const response = await fetch(url, { cache: "no-store", signal });
  if (!response.ok) {
    throw new Error(`${url} failed: ${response.status}`);
  }

  const blob = await response.blob();
  if (activeMediaCacheKey && activeMediaCacheKey !== cacheKey) {
    return false;
  }
  try {
    const persisted = await putMediaEntry(url, cacheKey, blob);
    if (activeMediaCacheKey && activeMediaCacheKey !== cacheKey) {
      await deleteMediaEntriesByCacheKey(cacheKey);
      return false;
    }
    return persisted;
  } catch (e) {
    console.warn("tellur-live IndexedDB media cache write failed", e);
    return false;
  }
}

export async function ensureVideoRangeCached(
  url: string,
  cacheKey: string,
  group: string,
  start: number,
  end: number,
  signal?: AbortSignal,
): Promise<boolean> {
  return Boolean(
    await ensureVideoRangeCachedObjectUrl(
      url,
      cacheKey,
      group,
      start,
      end,
      signal,
    ),
  );
}

export async function ensureVideoRangeCachedObjectUrl(
  url: string,
  cacheKey: string,
  group: string,
  start: number,
  end: number,
  signal?: AbortSignal,
): Promise<CachedVideoRangeObjectUrl | null> {
  if (!cacheKey || !group || !(end > start)) return null;
  const existing = await getCachedVideoRange(group, cacheKey, start);
  if (existing && existing.end >= end - EPSILON) {
    return getCachedVideoRangeObjectUrl(group, cacheKey, start);
  }

  const response = await fetch(url, { cache: "no-store", signal });
  if (!response.ok) {
    throw new Error(`${url} failed: ${response.status}`);
  }

  const blob = await response.blob();
  if (activeMediaCacheKey && activeMediaCacheKey !== cacheKey) {
    return null;
  }
  try {
    const persisted = await putVideoRangeBlob(
      url,
      cacheKey,
      group,
      start,
      end,
      blob,
    );
    if (activeMediaCacheKey && activeMediaCacheKey !== cacheKey) {
      await deleteMediaEntriesByCacheKey(cacheKey);
      return null;
    }
    if (!persisted) return null;
    return {
      start,
      end,
      url,
      objectUrl: URL.createObjectURL(blob),
    };
  } catch (e) {
    console.warn("tellur-live IndexedDB video range cache write failed", e);
    return null;
  }
}

export async function putVideoRangeBlob(
  url: string,
  cacheKey: string,
  group: string,
  start: number,
  end: number,
  blob: Blob,
): Promise<boolean> {
  if (!cacheKey || !group || !(end > start)) return false;
  const existing = await findVideoRangeEntry(group, cacheKey, start);
  if (existing && Number(existing.end) >= end - EPSILON) {
    return true;
  }
  if (activeMediaCacheKey && activeMediaCacheKey !== cacheKey) {
    return false;
  }
  const id = videoRangeEntryId(cacheKey, group, start, end);
  const persisted = await putVideoRangeEntry(url, cacheKey, group, start, end, blob);
  if (activeMediaCacheKey && activeMediaCacheKey !== cacheKey) {
    await deleteMediaEntriesByCacheKey(cacheKey);
    return false;
  }
  if (persisted) {
    await deleteSubsumedVideoRangeEntries(cacheKey, group, start, end, id);
  }
  return persisted;
}

export async function getCachedMediaObjectUrl(
  url: string,
  cacheKey: string,
): Promise<CachedMediaObjectUrl | null> {
  if (!cacheKey) return null;
  const stored = await getMediaEntry(url, cacheKey);
  if (!stored) return null;
  return {
    objectUrl: URL.createObjectURL(stored.blob),
    persisted: true,
    fromCache: true,
  };
}

export async function getCachedVideoRange(
  group: string,
  cacheKey: string,
  seconds: number,
): Promise<CachedVideoRange | null> {
  const entry = await findVideoRangeEntry(group, cacheKey, seconds);
  if (!entry) return null;
  return {
    start: Number(entry.start),
    end: Number(entry.end),
    url: entry.url,
  };
}

export async function getNextCachedVideoRange(
  group: string,
  cacheKey: string,
  seconds: number,
): Promise<CachedVideoRange | null> {
  const entry = await findNextVideoRangeEntry(group, cacheKey, seconds);
  if (!entry) return null;
  return {
    start: Number(entry.start),
    end: Number(entry.end),
    url: entry.url,
  };
}

export async function getCachedVideoRangeObjectUrl(
  group: string,
  cacheKey: string,
  seconds: number,
): Promise<CachedVideoRangeObjectUrl | null> {
  const entry = await findVideoRangeEntry(group, cacheKey, seconds);
  if (!entry) return null;
  return {
    start: Number(entry.start),
    end: Number(entry.end),
    url: entry.url,
    objectUrl: URL.createObjectURL(entry.blob),
  };
}

export async function getCachedVideoRangeBlob(
  group: string,
  cacheKey: string,
  seconds: number,
): Promise<CachedVideoRangeBlob | null> {
  const entry = await findVideoRangeEntry(group, cacheKey, seconds);
  if (!entry) return null;
  return {
    start: Number(entry.start),
    end: Number(entry.end),
    url: entry.url,
    blob: entry.blob,
  };
}

export async function hasMediaCached(
  url: string,
  cacheKey: string,
): Promise<boolean> {
  return Boolean(await getMediaEntry(url, cacheKey));
}

export async function revokeStaleMediaCacheEntries(
  currentCacheKey: string,
): Promise<void> {
  if (!currentCacheKey) {
    return;
  }
  activeMediaCacheKey = currentCacheKey;
  await Promise.all([
    deleteStaleMediaEntries(currentCacheKey),
    cleanupLegacyMediaCaches(),
  ]);
}

export async function cleanupLegacyMediaCaches(): Promise<boolean> {
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

async function getMediaEntry(
  url: string,
  cacheKey: string,
): Promise<MediaCacheEntry | null> {
  if (!canUseIndexedDb()) return null;
  try {
    const db = await openMediaDb();
    return await requestToPromise<MediaCacheEntry | undefined>(
      db
        .transaction(MEDIA_STORE, "readonly")
        .objectStore(MEDIA_STORE)
        .get(mediaEntryId(url, cacheKey)),
    ).then((entry) =>
      entry && entry.cacheKey === cacheKey ? entry : null,
    );
  } catch (e) {
    console.warn("tellur-live IndexedDB media cache read failed", e);
    return null;
  }
}

async function putMediaEntry(
  url: string,
  cacheKey: string,
  blob: Blob,
): Promise<boolean> {
  if (!canUseIndexedDb()) return false;
  const db = await openMediaDb();
  const entry: MediaCacheEntry = {
    id: mediaEntryId(url, cacheKey),
    cacheKey,
    url,
    blob,
    createdAt: Date.now(),
    kind: "exact",
  };
  const tx = db.transaction(MEDIA_STORE, "readwrite");
  const done = transactionDone(tx);
  await requestToPromise(tx.objectStore(MEDIA_STORE).put(entry));
  await done;
  return true;
}

async function putVideoRangeEntry(
  url: string,
  cacheKey: string,
  group: string,
  start: number,
  end: number,
  blob: Blob,
): Promise<boolean> {
  if (!canUseIndexedDb()) return false;
  const db = await openMediaDb();
  const entry: MediaCacheEntry = {
    id: videoRangeEntryId(cacheKey, group, start, end),
    cacheKey,
    url,
    blob,
    createdAt: Date.now(),
    kind: "video-range",
    group,
    start,
    end,
  };
  const tx = db.transaction(MEDIA_STORE, "readwrite");
  const done = transactionDone(tx);
  await requestToPromise(tx.objectStore(MEDIA_STORE).put(entry));
  await done;
  return true;
}

async function findVideoRangeEntry(
  group: string,
  cacheKey: string,
  seconds: number,
): Promise<MediaCacheEntry | null> {
  if (!group || !cacheKey || !Number.isFinite(seconds) || !canUseIndexedDb()) {
    return null;
  }
  try {
    const db = await openMediaDb();
    return await new Promise<MediaCacheEntry | null>((resolve, reject) => {
      let best: MediaCacheEntry | null = null;
      const tx = db.transaction(MEDIA_STORE, "readonly");
      const request = tx
        .objectStore(MEDIA_STORE)
        .index("cacheKey")
        .openCursor(IDBKeyRange.only(cacheKey));
      request.onerror = () =>
        reject(request.error ?? new Error("IndexedDB cursor failed"));
      request.onsuccess = () => {
        const cursor = request.result;
        if (!cursor) {
          resolve(best);
          return;
        }
        const entry = cursor.value as Partial<MediaCacheEntry>;
        const start = Number(entry.start);
        const end = Number(entry.end);
        if (
          entry.kind === "video-range" &&
          entry.group === group &&
          Number.isFinite(start) &&
          Number.isFinite(end) &&
          start <= seconds + EPSILON &&
          end > seconds + EPSILON &&
          (!best || start > Number(best.start))
        ) {
          best = cursor.value as MediaCacheEntry;
        }
        cursor.continue();
      };
    });
  } catch (e) {
    console.warn("tellur-live IndexedDB video range cache read failed", e);
    return null;
  }
}

async function findNextVideoRangeEntry(
  group: string,
  cacheKey: string,
  seconds: number,
): Promise<MediaCacheEntry | null> {
  if (!group || !cacheKey || !Number.isFinite(seconds) || !canUseIndexedDb()) {
    return null;
  }
  try {
    const db = await openMediaDb();
    return await new Promise<MediaCacheEntry | null>((resolve, reject) => {
      let best: MediaCacheEntry | null = null;
      const tx = db.transaction(MEDIA_STORE, "readonly");
      const request = tx
        .objectStore(MEDIA_STORE)
        .index("cacheKey")
        .openCursor(IDBKeyRange.only(cacheKey));
      request.onerror = () =>
        reject(request.error ?? new Error("IndexedDB cursor failed"));
      request.onsuccess = () => {
        const cursor = request.result;
        if (!cursor) {
          resolve(best);
          return;
        }
        const entry = cursor.value as Partial<MediaCacheEntry>;
        const start = Number(entry.start);
        const end = Number(entry.end);
        if (
          entry.kind === "video-range" &&
          entry.group === group &&
          Number.isFinite(start) &&
          Number.isFinite(end) &&
          end > seconds + EPSILON &&
          start > seconds + EPSILON &&
          (!best || start < Number(best.start))
        ) {
          best = cursor.value as MediaCacheEntry;
        }
        cursor.continue();
      };
    });
  } catch (e) {
    console.warn("tellur-live IndexedDB next video range read failed", e);
    return null;
  }
}

async function deleteStaleMediaEntries(keepCacheKey: string): Promise<void> {
  if (!canUseIndexedDb()) return;
  const db = await openMediaDb();
  await new Promise<void>((resolve, reject) => {
    const tx = db.transaction(MEDIA_STORE, "readwrite");
    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(tx.error ?? new Error("IndexedDB delete failed"));
    tx.onabort = () => reject(tx.error ?? new Error("IndexedDB delete aborted"));

    const cursorRequest = tx.objectStore(MEDIA_STORE).openCursor();
    cursorRequest.onerror = () =>
      reject(cursorRequest.error ?? new Error("IndexedDB cursor failed"));
    cursorRequest.onsuccess = () => {
      const cursor = cursorRequest.result;
      if (!cursor) return;
      const entry = cursor.value as Partial<MediaCacheEntry>;
      if (entry.cacheKey !== keepCacheKey || entry.kind !== "video-range") {
        cursor.delete();
      }
      cursor.continue();
    };
  });
}

async function deleteMediaEntriesByCacheKey(cacheKey: string): Promise<void> {
  if (!cacheKey || !canUseIndexedDb()) return;
  const db = await openMediaDb();
  await new Promise<void>((resolve, reject) => {
    const tx = db.transaction(MEDIA_STORE, "readwrite");
    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(tx.error ?? new Error("IndexedDB delete failed"));
    tx.onabort = () => reject(tx.error ?? new Error("IndexedDB delete aborted"));

    const cursorRequest = tx
      .objectStore(MEDIA_STORE)
      .index("cacheKey")
      .openCursor(IDBKeyRange.only(cacheKey));
    cursorRequest.onerror = () =>
      reject(cursorRequest.error ?? new Error("IndexedDB cursor failed"));
    cursorRequest.onsuccess = () => {
      const cursor = cursorRequest.result;
      if (!cursor) return;
      cursor.delete();
      cursor.continue();
    };
  });
}

async function deleteSubsumedVideoRangeEntries(
  cacheKey: string,
  group: string,
  start: number,
  end: number,
  keepId: string,
): Promise<void> {
  if (!cacheKey || !group || !canUseIndexedDb()) return;
  const db = await openMediaDb();
  await new Promise<void>((resolve, reject) => {
    const tx = db.transaction(MEDIA_STORE, "readwrite");
    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(tx.error ?? new Error("IndexedDB delete failed"));
    tx.onabort = () => reject(tx.error ?? new Error("IndexedDB delete aborted"));

    const cursorRequest = tx
      .objectStore(MEDIA_STORE)
      .index("cacheKey")
      .openCursor(IDBKeyRange.only(cacheKey));
    cursorRequest.onerror = () =>
      reject(cursorRequest.error ?? new Error("IndexedDB cursor failed"));
    cursorRequest.onsuccess = () => {
      const cursor = cursorRequest.result;
      if (!cursor) return;
      const entry = cursor.value as Partial<MediaCacheEntry>;
      const entryStart = Number(entry.start);
      const entryEnd = Number(entry.end);
      if (
        entry.id !== keepId &&
        entry.kind === "video-range" &&
        entry.group === group &&
        Number.isFinite(entryStart) &&
        Number.isFinite(entryEnd) &&
        entryStart >= start - EPSILON &&
        entryEnd <= end + EPSILON
      ) {
        cursor.delete();
      }
      cursor.continue();
    };
  });
}

function openMediaDb(): Promise<IDBDatabase> {
  if (dbPromise) return dbPromise;
  dbPromise = new Promise((resolve, reject) => {
    const request = indexedDB.open(MEDIA_DB_NAME, MEDIA_DB_VERSION);
    request.onerror = () =>
      reject(request.error ?? new Error("IndexedDB open failed"));
    request.onblocked = () => reject(new Error("IndexedDB upgrade blocked"));
    request.onupgradeneeded = (event) => {
      const db = request.result;
      const oldVersion = event.oldVersion;
      if (
        oldVersion > 0 &&
        oldVersion < MEDIA_DB_VERSION &&
        db.objectStoreNames.contains(MEDIA_STORE)
      ) {
        db.deleteObjectStore(MEDIA_STORE);
      }
      const store = db.objectStoreNames.contains(MEDIA_STORE)
        ? request.transaction!.objectStore(MEDIA_STORE)
        : db.createObjectStore(MEDIA_STORE, { keyPath: "id" });
      if (!store.indexNames.contains("cacheKey")) {
        store.createIndex("cacheKey", "cacheKey", { unique: false });
      }
      if (!store.indexNames.contains("group")) {
        store.createIndex("group", "group", { unique: false });
      }
    };
    request.onsuccess = () => {
      const db = request.result;
      db.onversionchange = () => {
        db.close();
        dbPromise = null;
      };
      resolve(db);
    };
  });
  dbPromise.catch(() => {
    dbPromise = null;
  });
  return dbPromise;
}

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
    tx.onerror = () => reject(tx.error ?? new Error("IndexedDB transaction failed"));
    tx.onabort = () => reject(tx.error ?? new Error("IndexedDB transaction aborted"));
  });
}

function canUseIndexedDb(): boolean {
  return typeof indexedDB !== "undefined" && typeof Blob !== "undefined";
}

function mediaEntryId(url: string, cacheKey: string): string {
  return `${cacheKey}\n${url}`;
}

function videoRangeEntryId(
  cacheKey: string,
  group: string,
  start: number,
  end: number,
): string {
  return `${cacheKey}\nvideo-range\n${group}\n${start.toFixed(4)}\n${end.toFixed(4)}`;
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
