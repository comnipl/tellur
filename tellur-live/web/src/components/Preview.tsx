import { forwardRef, type CSSProperties } from "react";

interface PreviewProps {
  imageSrc: string | null;
  imageVisible: boolean;
  aspect: number;
  error: string | null;
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
  function Preview({ imageSrc, imageVisible, aspect, error, videoRef, imgRef }, ref) {
    const safeAspect = Number.isFinite(aspect) && aspect > 0 ? aspect : 16 / 9;
    const previewStyle: CSSProperties & { "--preview-aspect": string } = {
      "--preview-aspect": String(safeAspect),
    };

    return (
      <section className="preview" ref={ref} style={previewStyle}>
        <div className="preview__frame">
          <div className="preview__media">
            <video ref={videoRef} muted playsInline preload="auto" />
            <img
              ref={imgRef}
              className={imageVisible ? "" : "hidden"}
              alt=""
              src={imageSrc ?? undefined}
            />
          </div>
          {error ? <div className="preview__error">{error}</div> : null}
        </div>
      </section>
    );
  },
);
