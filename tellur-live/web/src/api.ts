import type { Arrangement, ServerInfo } from "./types";

export async function fetchInfo(signal?: AbortSignal): Promise<ServerInfo> {
  const response = await fetch("/api/info", { cache: "no-store", signal });
  if (!response.ok) {
    throw new Error(`/api/info failed: ${response.status}`);
  }
  return (await response.json()) as ServerInfo;
}

// Fetches the resolved arrangement tree for `timelineId`. The server returns
// `null` (200 or 404) when no tree resolved — a failed resolve or a not-yet
// migrated collection — in which case the UI falls back to a flat view.
export async function fetchArrangement(
  timelineId: string,
  signal?: AbortSignal,
): Promise<Arrangement | null> {
  const query = new URLSearchParams({ timeline: timelineId });
  const response = await fetch(`/api/arrangement?${query}`, {
    cache: "no-store",
    signal,
  });
  if (response.status === 404) return null;
  if (!response.ok) {
    throw new Error(`/api/arrangement failed: ${response.status}`);
  }
  return (await response.json()) as Arrangement | null;
}

export interface FrameRequestParams {
  timelineId: string;
  time: number;
  width: number;
  height: number;
  fps: number;
  cacheKey: string;
}

export function frameUrl(params: FrameRequestParams): string {
  const query = new URLSearchParams({
    timeline: params.timelineId,
    time: params.time.toFixed(4),
    width: String(params.width),
    height: String(params.height),
    fps: String(params.fps),
    format: "png",
    v: params.cacheKey,
  });
  return `/api/frame?${query}`;
}

export interface VideoRequestParams extends FrameRequestParams {
  gop: number;
  crf: number;
  duration?: number;
}

export function videoUrl(params: VideoRequestParams): string {
  const query = new URLSearchParams({
    timeline: params.timelineId,
    time: params.time.toFixed(4),
    width: String(params.width),
    height: String(params.height),
    fps: String(params.fps),
    gop: String(params.gop),
    crf: String(params.crf),
    v: params.cacheKey,
  });
  if (params.duration != null) {
    query.set("duration", params.duration.toFixed(4));
  }
  return `/api/video.mp4?${query}`;
}
