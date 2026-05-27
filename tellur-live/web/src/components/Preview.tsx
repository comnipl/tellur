import { forwardRef } from "react";

interface PreviewProps {
  imageSrc: string | null;
  imageVisible: boolean;
  videoVisible: boolean;
  aspect: number;
  error: string | null;
}

interface PreviewRefs {
  videoRef: React.RefObject<HTMLVideoElement>;
  imgRef: React.RefObject<HTMLImageElement>;
}

export const Preview = forwardRef<HTMLDivElement, PreviewProps & PreviewRefs>(
  function Preview(
    { imageSrc, imageVisible, videoVisible, aspect, error, videoRef, imgRef },
    ref,
  ) {
    const frameStyle: React.CSSProperties = {
      aspectRatio: String(aspect || 16 / 9),
    };
    return (
      <section className="preview" ref={ref}>
        <div className="preview__frame" style={frameStyle}>
          <div className="preview__media">
            <video
              ref={videoRef}
              className={videoVisible ? "" : "hidden"}
              muted
              playsInline
              preload="auto"
            />
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
