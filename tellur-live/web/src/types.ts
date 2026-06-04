export interface TimelineInfo {
  id: string;
  title: string;
  duration: number;
  error: string | null;
}

// Lowercased discriminants, matching `tellur_core::timeline_component::NodeKind`
// and the server's `node_kind_str`. The display side collapses to three track
// kinds — video (映像: every rasterized visual, including backdrops, telops, and
// reveals), audio (音声), subtitle (字幕) — plus the two structural containers.
// There is no separate caption kind: a styled text telop is a visual on the
// video track.
export type NodeKind =
  | "video"
  | "audio"
  | "subtitle"
  | "timeline"
  | "sequence";

// Mirror of `tellur_core::timeline_component::Arrangement` (see server.rs
// `arrangement_json`). `trim` is the source crop `[a, b]` or null; `triggers`
// are absolute times where Events fire; `children` nests recursively.
export interface Arrangement {
  kind: NodeKind;
  label: string;
  name?: string | null;
  source: { file: string; line: number } | null;
  start: number;
  end: number;
  trim: [number, number] | null;
  triggers: { time: number; name: string | null }[];
  children: Arrangement[];
}

export interface PreviewResolution {
  width: number;
  height: number;
}

export interface ServerInfo {
  width: number;
  height: number;
  fps: number;
  lastError: string | null;
  cacheKey: string;
  compileStatus: "compiled" | "compiling" | "failed";
  compileError: string | null;
  timelines: TimelineInfo[];
}

export interface CacheRange {
  start: number;
  end: number;
}
