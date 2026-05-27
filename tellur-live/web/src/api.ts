import type { ServerInfo } from "./types";

export async function fetchInfo(signal?: AbortSignal): Promise<ServerInfo> {
  const response = await fetch("/api/info", { cache: "no-store", signal });
  if (!response.ok) {
    throw new Error(`/api/info failed: ${response.status}`);
  }
  return (await response.json()) as ServerInfo;
}

export interface FrameRequestParams {
  timelineId: string;
  time: number;
  width: number;
  height: number;
  fps: number;
}

export function frameUrl(params: FrameRequestParams, token: number): string {
  const query = new URLSearchParams({
    timeline: params.timelineId,
    time: params.time.toFixed(4),
    width: String(params.width),
    height: String(params.height),
    fps: String(params.fps),
    format: "png",
    _: String(token),
  });
  return `/api/frame?${query}`;
}

export interface VideoRequestParams extends FrameRequestParams {
  gop: number;
  crf: number;
}

export function videoUrl(params: VideoRequestParams, token: number): string {
  const query = new URLSearchParams({
    timeline: params.timelineId,
    time: params.time.toFixed(4),
    width: String(params.width),
    height: String(params.height),
    fps: String(params.fps),
    gop: String(params.gop),
    crf: String(params.crf),
    _: String(token),
  });
  return `/api/video.mp4?${query}`;
}
