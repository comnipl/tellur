import { useCallback, useRef, useState } from "react";

interface PreviewScrubberProps {
  seconds: number;
  duration: number;
  onSeek: (seconds: number) => void;
}

export function PreviewScrubber(props: PreviewScrubberProps) {
  const { seconds, duration, onSeek } = props;
  const scrubberRef = useRef<HTMLDivElement>(null);
  const draggingRef = useRef(false);
  const [dragging, setDragging] = useState(false);
  const safeDuration = Math.max(duration, 0.001);
  const progress = Math.max(0, Math.min(1, seconds / safeDuration));

  const seekFromClientX = useCallback(
    (clientX: number) => {
      const scrubber = scrubberRef.current;
      if (!scrubber || !duration) return;
      const rect = scrubber.getBoundingClientRect();
      const ratio = Math.max(0, Math.min(1, (clientX - rect.left) / rect.width));
      onSeek(Math.max(0, Math.min(duration, ratio * duration)));
    },
    [duration, onSeek],
  );

  const beginDrag = useCallback((e: React.PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    e.preventDefault();
    draggingRef.current = true;
    setDragging(true);
    seekFromClientX(e.clientX);
    e.currentTarget.setPointerCapture(e.pointerId);
  }, [seekFromClientX]);

  const handlePointerMove = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      if (!draggingRef.current) return;
      e.preventDefault();
      seekFromClientX(e.clientX);
    },
    [seekFromClientX],
  );

  const endDrag = useCallback((e: React.PointerEvent<HTMLDivElement>) => {
    if (!draggingRef.current) return;
    draggingRef.current = false;
    setDragging(false);
    if (e.currentTarget.hasPointerCapture(e.pointerId)) {
      e.currentTarget.releasePointerCapture(e.pointerId);
    }
  }, []);

  return (
    <div
      className={
        dragging
          ? "preview-scrubber preview-scrubber--dragging"
          : "preview-scrubber"
      }
      ref={scrubberRef}
      onPointerDown={beginDrag}
      onPointerMove={handlePointerMove}
      onPointerUp={endDrag}
      onPointerCancel={endDrag}
      onLostPointerCapture={endDrag}
    >
      <div className="preview-scrubber__rail">
        <span
          className="preview-scrubber__progress"
          style={{ width: `${progress * 100}%` }}
        />
        <span className="preview-scrubber__cap preview-scrubber__cap--start" />
        <span
          className="preview-scrubber__thumb"
          style={{ left: `${progress * 100}%` }}
        />
        <span className="preview-scrubber__cap preview-scrubber__cap--end" />
      </div>
    </div>
  );
}
