import { forwardRef, type CSSProperties } from "react";

interface PreviewProps {
  imageSrc: string | null;
  imageVisible: boolean;
  aspect: number;
  error: string | null;
  // Soft notice shown when the segment cache can't persist (vs `error`, which is a hard
  // failure). Playback still works; this just explains why the green bar isn't sticking.
  cacheNotice?: string | null;
}

interface PreviewRefs {
  videoRef: React.RefObject<HTMLVideoElement>;
  imgRef: React.RefObject<HTMLImageElement>;
}

// The <video> is ALWAYS rendered (never `.hidden`) so power-managed browsers (Arc /
// battery-saver Chromium) keep decoding it — a visibility:hidden video stalls at
// HAVE_METADATA and its seeks never complete. The PNG still <img> is stacked ON TOP
// (same grid cell, later in DOM) and toggles via `.hidden`: shown it covers the video
// during paused/seek rebuffer; hidden it reveals the playing video underneath.
export const Preview = forwardRef<HTMLDivElement, PreviewProps & PreviewRefs>(
  function Preview(
    { imageSrc, imageVisible, aspect, error, cacheNotice, videoRef, imgRef },
    ref,
  ) {
    const safeAspect = Number.isFinite(aspect) && aspect > 0 ? aspect : 16 / 9;
    const previewStyle: CSSProperties & { "--preview-aspect": string } = {
      "--preview-aspect": String(safeAspect),
    };

    return (
      <section className="preview" ref={ref} style={previewStyle}>
        <div className="preview__frame">
          <div className="preview__media">
            <video ref={videoRef} playsInline preload="auto" />
            <img
              ref={imgRef}
              className={imageVisible ? "" : "hidden"}
              alt=""
              src={imageSrc ?? undefined}
            />
          </div>
          {error ? <div className="preview__error">{error}</div> : null}
          {!error && cacheNotice ? (
            <div
              // Small, unobtrusive corner hint — caching is degraded but playback is fine.
              style={{
                position: "absolute",
                top: 8,
                right: 8,
                padding: "3px 8px",
                fontSize: 11,
                lineHeight: 1.3,
                borderRadius: 4,
                background: "rgba(40, 24, 0, 0.78)",
                color: "#f4c77a",
                pointerEvents: "none",
                maxWidth: "70%",
              }}
            >
              {cacheNotice}
            </div>
          ) : null}
        </div>
      </section>
    );
  },
);
