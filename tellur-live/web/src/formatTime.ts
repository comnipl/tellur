// Format seconds as HH:MM:SS.FF where FF is centi-frames (subseconds * 100).
// Mirrors the readout in the reference UI (e.g. "00:00:07.18").
export function formatTimecode(seconds: number, fps: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) seconds = 0;
  const totalFrames = Math.round(seconds * fps);
  const ff = totalFrames % fps;
  const totalSec = Math.floor(totalFrames / fps);
  const ss = totalSec % 60;
  const mm = Math.floor(totalSec / 60) % 60;
  const hh = Math.floor(totalSec / 3600);
  return `${pad(hh)}:${pad(mm)}:${pad(ss)}.${pad(ff)}`;
}

// Same as formatTimecode but uses a `:` between seconds and frame index
// — matches the chip label "00:00:07:18" sitting on the playhead.
export function formatTimecodeColon(seconds: number, fps: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) seconds = 0;
  const totalFrames = Math.round(seconds * fps);
  const ff = totalFrames % fps;
  const totalSec = Math.floor(totalFrames / fps);
  const ss = totalSec % 60;
  const mm = Math.floor(totalSec / 60) % 60;
  const hh = Math.floor(totalSec / 3600);
  return `${pad(hh)}:${pad(mm)}:${pad(ss)}:${pad(ff)}`;
}

export function formatTimelineStart(seconds: number, fps: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) seconds = 0;
  const totalFrames = Math.round(seconds * fps);
  const ff = totalFrames % fps;
  const totalSec = Math.floor(totalFrames / fps);
  const ss = totalSec % 60;
  const mm = Math.floor(totalSec / 60) % 60;
  const hh = Math.floor(totalSec / 3600);

  if (hh > 0) return `${pad(hh)}:${pad(mm)}:${pad(ss)}.${pad(ff)}`;
  return `${pad(mm)}:${pad(ss)}.${pad(ff)}`;
}

function pad(n: number): string {
  return n.toString().padStart(2, "0");
}
