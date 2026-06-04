import { useCallback, useEffect, useRef, useState } from "react";
import {
  AnimatePresence,
  animate,
  motion,
  useMotionValue,
  useReducedMotion,
} from "motion/react";
import { CornerUpRight, SquareMenu } from "lucide-react";
import { formatTimelineStart, formatTimecode } from "../formatTime";
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
  // Left-edge readout shows the TARGET start (not the per-frame tween value), so
  // the time/frame text cross-fades to the final value instead of flickering
  // through every intermediate during the tween.
  const viewportStartFrame = Math.max(0, Math.round(targetStart * fps));
  const viewportStartLabel = formatTimelineStart(targetStart, fps);

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
            {/* The time/frame readout cross-fades when its value changes, so the
                number swaps smoothly instead of snapping. `mode="popLayout"`
                lets the outgoing copy fade out atop the incoming one in place;
                keying on the value drives the swap. Under reduced-motion the
                durations collapse to 0 (an instant swap). */}
            <span className="tabsrow__viewport-start-text">
              <AnimatePresence mode="popLayout" initial={false}>
                <motion.span
                  key={viewportStartLabel}
                  initial={{ opacity: reduceMotion ? 1 : 0 }}
                  animate={{ opacity: 1 }}
                  exit={{ opacity: 0 }}
                  transition={{ duration: reduceMotion ? 0 : 0.18 }}
                >
                  {viewportStartLabel}
                </motion.span>
              </AnimatePresence>
              <span className="tabsrow__viewport-start-frame">
                <AnimatePresence mode="popLayout" initial={false}>
                  <motion.span
                    key={viewportStartFrame}
                    initial={{ opacity: reduceMotion ? 1 : 0 }}
                    animate={{ opacity: 1 }}
                    exit={{ opacity: 0 }}
                    transition={{ duration: reduceMotion ? 0 : 0.18 }}
                  >
                    {viewportStartFrame}F
                  </motion.span>
                </AnimatePresence>
              </span>
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
          {playheadVisible ? (
            <>
              <span
                className="tabsrow__playhead-line"
                style={{ left: `${x}px` }}
              />
              <span
                className="tabsrow__chip"
                style={{ left: `${x}px` }}
              >
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
