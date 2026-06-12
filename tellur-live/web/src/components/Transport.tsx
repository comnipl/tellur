import { Pause, Play, SkipBack, SkipForward } from "lucide-react";
import { formatTimecode } from "../formatTime";
import type { PreviewResolution } from "../types";

interface ResolutionOption extends PreviewResolution {
  label: string;
}

interface TransportProps {
  seconds: number;
  duration: number;
  fps: number;
  measuredFps: number;
  resolution: PreviewResolution;
  resolutionOptions: ResolutionOption[];
  motionBlur: boolean;
  playing: boolean;
  onTogglePlay: () => void;
  onStep: (delta: number) => void;
  onRewind: () => void;
  onResolutionChange: (resolution: PreviewResolution) => void;
  onFpsChange: (fps: number) => void;
  onMotionBlurChange: (motionBlur: boolean) => void;
}

const FPS_OPTIONS = [60, 30, 24, 15, 12];

export function Transport(props: TransportProps) {
  const {
    seconds,
    duration,
    fps,
    measuredFps,
    resolution,
    resolutionOptions,
    motionBlur,
    playing,
    onTogglePlay,
    onStep,
    onRewind,
    onResolutionChange,
    onFpsChange,
    onMotionBlurChange,
  } = props;
  const selectedResolutionKey = resolutionKey(resolution);

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
          // Blur after activation so focus doesn't stay on the button and
          // swallow the global Space/arrow keyboard shortcuts.
          onClick={(e) => {
            onRewind();
            e.currentTarget.blur();
          }}
        >
          <SkipBack size={14} strokeWidth={1.6} />
        </button>
        <button
          type="button"
          className="transport__btn transport__btn--play"
          aria-label={playing ? "Pause" : "Play"}
          onClick={(e) => {
            onTogglePlay();
            e.currentTarget.blur();
          }}
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
          onClick={(e) => {
            onStep(1);
            e.currentTarget.blur();
          }}
        >
          <SkipForward size={14} strokeWidth={1.6} />
        </button>
      </div>
      <div className="transport__right">
        <button
          type="button"
          className={
            motionBlur
              ? "transport__toggle transport__toggle--on"
              : "transport__toggle"
          }
          aria-pressed={motionBlur}
          aria-label={motionBlur ? "Disable motion blur" : "Enable motion blur"}
          // Blur after activation so focus doesn't stay on the button and
          // swallow the global Space/arrow keyboard shortcuts.
          onClick={(e) => {
            onMotionBlurChange(!motionBlur);
            e.currentTarget.blur();
          }}
        >
          <span className="transport__toggle-dot" />
          Motion Blur
        </button>
        <label className="transport__control">
          <select
            value={fps}
            // Blur after the value is committed so focus leaves the <select>
            // and the global Space/arrow shortcuts keep working.
            onChange={(e) => {
              onFpsChange(Number(e.target.value));
              e.currentTarget.blur();
            }}
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
            value={selectedResolutionKey}
            onChange={(e) => {
              const option = resolutionOptions.find(
                (candidate) => resolutionKey(candidate) === e.target.value,
              );
              if (option) {
                onResolutionChange({
                  width: option.width,
                  height: option.height,
                });
              }
              // Blur after the value is committed so focus leaves the <select>
              // and the global Space/arrow shortcuts keep working.
              e.currentTarget.blur();
            }}
          >
            {resolutionOptions.map((option) => (
              <option key={resolutionKey(option)} value={resolutionKey(option)}>
                {option.label}
              </option>
            ))}
          </select>
        </label>
      </div>
    </div>
  );
}

function resolutionKey(resolution: PreviewResolution): string {
  return `${resolution.width}x${resolution.height}`;
}
