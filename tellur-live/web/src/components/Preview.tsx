import { forwardRef } from "react";

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
    const frameStyle: React.CSSProperties = {
      aspectRatio: String(aspect || 16 / 9),
    };
    return (
      <section className="preview" ref={ref}>
        <div className="preview__frame" style={frameStyle}>
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
