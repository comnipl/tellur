interface PreviewScrubberProps {
  seconds: number;
  duration: number;
  onSeek: (seconds: number) => void;
}

export function PreviewScrubber(props: PreviewScrubberProps) {
  const { seconds, duration, onSeek } = props;
  const safeDuration = Math.max(duration, 0.001);
  const progress = Math.max(0, Math.min(1, seconds / safeDuration));

  return (
    <div
      className="preview-scrubber"
      onClick={(e) => {
        if (!duration) return;
        const rect = e.currentTarget.getBoundingClientRect();
        const ratio = (e.clientX - rect.left) / rect.width;
        onSeek(Math.max(0, Math.min(duration, ratio * duration)));
      }}
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
