export interface TimelineInfo {
  id: string;
  title: string;
  duration: number;
}

export interface ServerInfo {
  width: number;
  height: number;
  fps: number;
  lastError: string | null;
  timelines: TimelineInfo[];
}

export interface CacheRange {
  start: number;
  end: number;
}
