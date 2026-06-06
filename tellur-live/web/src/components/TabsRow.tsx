import { useCallback, useEffect, useRef, useState } from "react";
import { animate, useMotionValue, useReducedMotion } from "motion/react";
import { CornerUpRight, SquareMenu } from "lucide-react";
import { formatTimelineStart, formatTimecode } from "../formatTime";
import { RollingNumber } from "./RollingNumber";
import {
  clamp,
  clampTimelineViewport,
  getVisibleDuration,
  type TimelineViewport,
} from "../timelineViewport";
import type { CacheRange, TimelineInfo } from "../types";

interface TabsRowProps {
  timeline: TimelineInfo | null;
  seconds: number;
  fps: number;
  cacheRanges: CacheRange[];
  viewport: TimelineViewport;
  primaryLabel: string;
  secondaryLabel?: string;
  onSeek: (seconds: number) => void;
}

// Compound row that holds the panel tabs on the left and the timeline
// "cursor strip" (playhead chip + green cache bar) on the right. Both
// halves are aligned via the same side-width grid as the timeline body
// below so the cursor strip's pixel coordinates match the clip area.
export function TabsRow(props: TabsRowProps) {
  const {
    timeline,
    seconds,
    fps,
    cacheRanges,
    viewport,
    primaryLabel,
    secondaryLabel,
    onSeek,
  } = props;

  const cursorRef = useRef<HTMLDivElement>(null);
  const draggingSeekRef = useRef(false);
  const [width, setWidth] = useState(0);
  // Last width the geometry tween saw, so a resize (including the first
  // measurement) snaps instead of animating — a layout change is not a pan/zoom.
  const prevWidthRef = useRef(0);
  const [draggingSeek, setDraggingSeek] = useState(false);

  // Honor the OS "reduce motion" preference: snap instead of tweening when set.
  const reduceMotion = useReducedMotion();

  useEffect(() => {
    const el = cursorRef.current;
    if (!el) return;
    const observer = new ResizeObserver(() => setWidth(el.clientWidth));
    observer.observe(el);
    setWidth(el.clientWidth);
    return () => observer.disconnect();
  }, []);

  const duration = Math.max(timeline?.duration ?? 0.001, 0.001);
  const normalizedViewport = clampTimelineViewport(viewport, duration);
  // TARGET projection: the viewport the prop maps to right now. Pointer input
  // (seek/scrub) is computed against these so it stays exact and instant. The
  // top strip's x = (t - start) / visibleDuration * width, which is the SAME
  // projection the body uses ((t/duration)*innerWidth - viewportX), so tweening
  // start + visibleDuration here keeps the strip in lock-step with the body.
  const targetStart = normalizedViewport.start;
  const targetVisibleDuration = getVisibleDuration(
    duration,
    normalizedViewport.zoom,
  );

  // ANIMATED projection: `start`/`visibleDuration` glide to the target on the
  // same ease-out tween as the body (easeOutQuint, 0.28s) so the strip's
  // playhead/cache geometry slides in step instead of jumping. Held as motion
  // values (driven by `animate`) mirrored to state so the projection recomputes
  // each frame. Only the DRAWN geometry reads these; pointer math uses TARGET.
  const startMV = useMotionValue(targetStart);
  const visibleDurationMV = useMotionValue(targetVisibleDuration);
  const [animStart, setAnimStart] = useState(targetStart);
  const [animVisibleDuration, setAnimVisibleDuration] = useState(
    targetVisibleDuration,
  );

  useEffect(() => {
    // Snap (no tween) under reduced-motion, before a width is measured, or on a
    // resize — a layout change must not read as a pan/zoom gesture.
    const resized = prevWidthRef.current !== width;
    prevWidthRef.current = width;
    if (reduceMotion || width <= 0 || resized) {
      startMV.set(targetStart);
      visibleDurationMV.set(targetVisibleDuration);
      setAnimStart(targetStart);
      setAnimVisibleDuration(targetVisibleDuration);
      return;
    }
    const ease = [0.22, 1, 0.36, 1] as const;
    const controls = [
      animate(startMV, targetStart, {
        duration: 0.28,
        ease,
        onUpdate: setAnimStart,
      }),
      animate(visibleDurationMV, targetVisibleDuration, {
        duration: 0.28,
        ease,
        onUpdate: setAnimVisibleDuration,
      }),
    ];
    return () => controls.forEach((c) => c.stop());
  }, [
    targetStart,
    targetVisibleDuration,
    reduceMotion,
    width,
    startMV,
    visibleDurationMV,
  ]);

  const viewportEnd = animStart + animVisibleDuration;
  const x = ((seconds - animStart) / animVisibleDuration) * width;
  const playheadVisible = x >= 0 && x <= width;
  const frame = Math.max(0, Math.round(seconds * fps));

  // Ruler ticks. The STEP (seconds between ticks) is chosen from the TARGET
  // projection so the tick COUNT stays stable through the pan/zoom tween (only
  // their positions glide); deriving it from the animated value would make ticks
  // pop in/out mid-tween. Each tick's x then reads the ANIMATED projection so the
  // ruler eases in lock-step with the body + cursor strip.
  const ticks =
    width > 0
      ? buildRulerTicks(
          animStart,
          animVisibleDuration,
          targetVisibleDuration,
          width,
        )
      : [];
  // Left-edge readout (the pinned leading tick): formatted from the TARGET start
  // (the final viewport start, not the per-frame geometry tween, which would
  // jitter). The slot-machine roll is handled per-digit inside <RollingNumber>,
  // so here we just hand it the final formatted strings; each digit column rolls
  // to its new value on pan/scroll.
  const viewportStartLabel = formatTimelineStart(targetStart, fps);
  const viewportStartFrame = `${Math.max(0, Math.round(targetStart * fps))}F`;

  const seekFromClientX = useCallback(
    (clientX: number) => {
      const cursor = cursorRef.current;
      if (!cursor || !timeline) return;
      const rect = cursor.getBoundingClientRect();
      const ratio = clamp((clientX - rect.left) / rect.width, 0, 1);
      onSeek(
        clamp(
          targetStart + ratio * targetVisibleDuration,
          0,
          duration,
        ),
      );
    },
    [
      duration,
      targetStart,
      targetVisibleDuration,
      onSeek,
      timeline,
    ],
  );

  const handleSeekPointerDown = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      if (e.button !== 0) return;
      e.preventDefault();
      draggingSeekRef.current = true;
      setDraggingSeek(true);
      seekFromClientX(e.clientX);
      e.currentTarget.setPointerCapture(e.pointerId);
    },
    [seekFromClientX],
  );

  const handleSeekPointerMove = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      if (!draggingSeekRef.current) return;
      e.preventDefault();
      seekFromClientX(e.clientX);
    },
    [seekFromClientX],
  );

  const endSeekDrag = useCallback((e: React.PointerEvent<HTMLDivElement>) => {
    if (!draggingSeekRef.current) return;
    draggingSeekRef.current = false;
    setDraggingSeek(false);
    if (e.currentTarget.hasPointerCapture(e.pointerId)) {
      e.currentTarget.releasePointerCapture(e.pointerId);
    }
  }, []);

  return (
    <div className="tabsrow">
      <div className="tabsrow__left">
        <span className="tabsrow__tab tabsrow__tab--primary tabsrow__tab--active">
          {primaryLabel}
        </span>
        {secondaryLabel ? (
          <span className="tabsrow__tab tabsrow__tab--secondary">
            <SquareMenu size={13} strokeWidth={1.8} />
            {secondaryLabel}
          </span>
        ) : null}
      </div>
      <div
        className={
          draggingSeek
            ? "tabsrow__cursor tabsrow__cursor--dragging"
            : "tabsrow__cursor"
        }
        ref={cursorRef}
        onPointerDown={handleSeekPointerDown}
        onPointerMove={handleSeekPointerMove}
        onPointerUp={endSeekDrag}
        onPointerCancel={endSeekDrag}
        onLostPointerCapture={endSeekDrag}
      >
        <div className="tabsrow__cursor-inner">
          <span className="tabsrow__viewport-start">
            <CornerUpRight
              className="tabsrow__viewport-start-icon"
              size={18}
              strokeWidth={1.8}
            />
            {/* Slot-machine readout: each digit column rolls vertically to its
                new value as the viewport start changes; colons / "." / "F" stay
                fixed. Driven by the TARGET start so digits settle on the final
                value. */}
            <span className="tabsrow__viewport-start-text">
              <RollingNumber value={viewportStartLabel} />
              <RollingNumber
                className="tabsrow__viewport-start-frame"
                value={viewportStartFrame}
              />
            </span>
          </span>
          {cacheRanges.map((range, i) => {
            const visibleStart = Math.max(range.start, animStart);
            const visibleEnd = Math.min(range.end, viewportEnd);
            if (visibleEnd <= visibleStart) return null;

            const left =
              ((visibleStart - animStart) / animVisibleDuration) * width;
            const w = Math.max(
              2,
              ((visibleEnd - visibleStart) / animVisibleDuration) * width,
            );
            return (
              <div
                className="tabsrow__cache"
                key={i}
                style={{ left: `${left}px`, width: `${w}px` }}
              />
            );
          })}
          {/* Ruler: a tick per chosen interval, each labeled with time (main)
              and frame number (sub), sharing the row with the pinned left-edge
              readout. Positions read the animated projection so the ruler eases
              in lock-step. A moving tick whose x falls under the pinned readout
              is dropped so the two don't visually collide. */}
          {ticks.map((tick) => {
            const left =
              ((tick.time - animStart) / animVisibleDuration) * width;
            if (left < LEFT_READOUT_RESERVE) return null;
            return (
              <div
                className="tabsrow__tick"
                key={tick.time}
                style={{ left: `${left}px` }}
              >
                <span className="tabsrow__tick-line" />
                <span className="tabsrow__tick-label">
                  <span className="tabsrow__tick-time">
                    {formatTimelineStart(tick.time, fps)}
                  </span>
                  <span className="tabsrow__tick-frame">
                    {Math.round(tick.time * fps)}F
                  </span>
                </span>
              </div>
            );
          })}
          {playheadVisible ? (
            <>
              <span
                className="tabsrow__playhead-line"
                style={{ left: `${x}px` }}
              />
              <span className="tabsrow__chip" style={{ left: `${x}px` }}>
                <span>{formatTimecode(seconds, fps)}</span>
                <span className="tabsrow__chip-sub">{frame}F</span>
              </span>
            </>
          ) : null}
        </div>
      </div>
    </div>
  );
}

// Pick a tick interval from a 1-2-5 series (…0.1, 0.2, 0.5, 1, 2, 5, 10…s) such
// that adjacent ticks are at least MIN_TICK_PX apart, then emit one tick per
// interval boundary across the visible window (plus a small margin so ticks
// entering from the edges aren't missing during a pan). `pxVisibleDuration` is
// the TARGET visible duration used to size the step (stable through the tween);
// the caller positions each tick with the ANIMATED projection.
interface RulerTick {
  time: number;
}

const MIN_TICK_PX = 64;
const TICK_STEPS_PER_DECADE = [1, 2, 5];
// Horizontal span (px) reserved at the left for the pinned viewport-start
// readout. Moving ticks landing inside this band are dropped so they don't
// collide with the fixed leading entry.
const LEFT_READOUT_RESERVE = 72;

function buildRulerTicks(
  animStart: number,
  animVisibleDuration: number,
  pxVisibleDuration: number,
  width: number,
): RulerTick[] {
  if (width <= 0 || pxVisibleDuration <= 0) return [];

  // Minimum interval (seconds) that keeps labels MIN_TICK_PX apart.
  const minStep = (MIN_TICK_PX / width) * pxVisibleDuration;
  const step = niceStep(minStep);
  if (!Number.isFinite(step) || step <= 0) return [];

  // Visible window from the ANIMATED projection, padded by one step so ticks
  // sliding in at the edges are present mid-tween. The window end is NOT capped
  // at `duration`: in the trailing overscroll region (past the content end) the
  // ruler keeps emitting ticks for times beyond duration, so the graticule
  // continues across the blank area. Only the start is floored at 0 (no negative
  // times).
  const windowStart = animStart - step;
  const windowEnd = animStart + animVisibleDuration + step;
  const first = Math.max(0, Math.ceil(windowStart / step) * step);
  const last = windowEnd;

  const ticks: RulerTick[] = [];
  // Guard against pathological counts (e.g. a degenerate width during layout).
  for (let t = first, guard = 0; t <= last && guard < 2000; t += step, guard++) {
    // Snap to the step grid to avoid floating-point drift accumulating.
    ticks.push({ time: Math.round(t / step) * step });
  }
  return ticks;
}

function niceStep(minStep: number): number {
  const decade = Math.pow(10, Math.floor(Math.log10(minStep)));
  for (const mult of TICK_STEPS_PER_DECADE) {
    const candidate = mult * decade;
    if (candidate >= minStep) return candidate;
  }
  return 10 * decade;
}
