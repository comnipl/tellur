import { Pause, Play, SkipBack, SkipForward } from "lucide-react";
import { formatTimecode } from "../formatTime";

interface TransportProps {
  seconds: number;
  duration: number;
  fps: number;
  measuredFps: number;
  scale: number;
  playing: boolean;
  onTogglePlay: () => void;
  onStep: (delta: number) => void;
  onRewind: () => void;
  onScaleChange: (scale: number) => void;
  onFpsChange: (fps: number) => void;
}

const SCALE_OPTIONS = [
  { value: 1, label: "1920 × 1080" },
  { value: 0.75, label: "1440 × 810" },
  { value: 0.5, label: "960 × 540" },
  { value: 0.25, label: "480 × 270" },
];

const FPS_OPTIONS = [60, 30, 24, 15, 12];

export function Transport(props: TransportProps) {
  const {
    seconds,
    duration,
    fps,
    measuredFps,
    scale,
    playing,
    onTogglePlay,
    onStep,
    onRewind,
    onScaleChange,
    onFpsChange,
  } = props;

  return (
    <div className="transport">
      <div className="transport__left">
        <span className="transport__timecode">
          <span className="transport__timecode-cur">
            {formatTimecode(seconds, fps)}
          </span>
          <span className="transport__timecode-sep">/</span>
          <span className="transport__timecode-total">
            {formatTimecode(duration, fps)}
          </span>
        </span>
        <span className="transport__fps-readout">
          {measuredFps.toFixed(2)} fps
        </span>
      </div>
      <div className="transport__center">
        <button
          type="button"
          className="transport__btn"
          aria-label="Rewind"
          onClick={onRewind}
        >
          <SkipBack size={14} strokeWidth={1.6} />
        </button>
        <button
          type="button"
          className="transport__btn transport__btn--play"
          aria-label={playing ? "Pause" : "Play"}
          onClick={onTogglePlay}
        >
          {playing ? (
            <Pause size={18} strokeWidth={1.4} fill="currentColor" />
          ) : (
            <Play size={18} strokeWidth={1.4} fill="currentColor" />
          )}
        </button>
        <button
          type="button"
          className="transport__btn"
          aria-label="Step forward"
          onClick={() => onStep(1)}
        >
          <SkipForward size={14} strokeWidth={1.6} />
        </button>
      </div>
      <div className="transport__right">
        <label className="transport__control">
          <select
            value={fps}
            onChange={(e) => onFpsChange(Number(e.target.value))}
          >
            {FPS_OPTIONS.map((value) => (
              <option key={value} value={value}>
                {value} fps
              </option>
            ))}
          </select>
        </label>
        <label className="transport__control">
          <select
            value={scale}
            onChange={(e) => onScaleChange(Number(e.target.value))}
          >
            {SCALE_OPTIONS.map((option) => (
              <option key={option.value} value={option.value}>
                {option.label}
              </option>
            ))}
          </select>
        </label>
      </div>
    </div>
  );
}
