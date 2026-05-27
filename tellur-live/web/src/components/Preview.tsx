import { forwardRef, type CSSProperties } from "react";

interface PreviewProps {
  imageSrc: string | null;
  imageVisible: boolean;
  videoVisible: boolean;
  activeVideoSlot: 0 | 1;
  aspect: number;
  error: string | null;
}

interface PreviewRefs {
  videoRefs: [
    React.RefObject<HTMLVideoElement>,
    React.RefObject<HTMLVideoElement>,
  ];
  imgRef: React.RefObject<HTMLImageElement>;
}

export const Preview = forwardRef<HTMLDivElement, PreviewProps & PreviewRefs>(
  function Preview(
    {
      imageSrc,
      imageVisible,
      videoVisible,
      activeVideoSlot,
      aspect,
      error,
      videoRefs,
      imgRef,
    },
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
            {videoRefs.map((videoRef, slot) => (
              <video
                key={slot}
                ref={videoRef}
                className={
                  videoVisible && activeVideoSlot === slot ? "" : "hidden"
                }
                muted
                playsInline
                preload="auto"
              />
            ))}
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
