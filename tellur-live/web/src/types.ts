export interface TimelineInfo {
  id: string;
  title: string;
  duration: number;
  error: string | null;
}

// Lowercased discriminants, matching `tellur_core::timeline_component::NodeKind`
// and the server's `node_kind_str`.
export type NodeKind =
  | "video"
  | "audio"
  | "caption"
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
