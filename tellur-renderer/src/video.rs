//! Video encoding via an `ffmpeg` subprocess.
//!
//! [`FfmpegEncoder`] streams frames of a [`Timeline`] into `ffmpeg`'s stdin
//! as raw RGBA. The output codec, container, pixel format, and any filters
//! are controlled entirely through caller-supplied `args` — the encoder
//! itself only fixes the input side (raw RGBA at a known size/framerate),
//! so any container/codec `ffmpeg` knows about is reachable (mp4, mov +
//! ProRes, webm, image sequences, ...).
//!
//! Frames are produced by repeatedly calling `Timeline::build(t, resolution)`
//! with `t = frame_idx / fps` for `frame_idx` in `0..ceil(duration * fps)`.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use tellur_core::raster::{PixelFormat, Resolution};
use tellur_core::time::TimelineTime;
use tellur_core::timeline::Timeline;
use thiserror::Error;

/// Builder that spawns `ffmpeg` and drives a [`Timeline`] through it.
///
/// The frame size is fixed at construction (`resolution`) and frames are
/// emitted at `fps` Hz. Output-side `ffmpeg` arguments (codec, container,
/// filters, ...) are supplied via [`Self::arg`] / [`Self::args`] and inserted
/// verbatim between the raw-video input and the output path.
pub struct FfmpegEncoder {
    fps: u32,
    resolution: Resolution,
    args: Vec<String>,
}

#[derive(Debug, Error)]
pub enum FfmpegError {
    #[error("failed to spawn ffmpeg (is it on PATH?): {0}")]
    Spawn(std::io::Error),
    #[error("failed to write frame {frame} to ffmpeg stdin: {source}")]
    Write { frame: u64, source: std::io::Error },
    #[error("failed to read ffmpeg stderr: {0}")]
    ReadStderr(std::io::Error),
    #[error("failed to wait for ffmpeg: {0}")]
    Wait(std::io::Error),
    #[error("ffmpeg exited with status {status}:\n{stderr}")]
    NonZeroExit { status: String, stderr: String },
    #[error(
        "frame {frame} produced {actual} bytes, expected {expected} ({width}x{height} RGBA)"
    )]
    FrameSizeMismatch {
        frame: u64,
        expected: usize,
        actual: usize,
        width: u32,
        height: u32,
    },
    #[error("frame {frame} pixel format is {format:?}, only Rgba8 is supported")]
    UnsupportedFormat { frame: u64, format: PixelFormat },
    #[error("fps must be greater than zero")]
    ZeroFps,
    #[error("duration must be finite and non-negative, got {0}")]
    InvalidDuration(f32),
}

impl FfmpegEncoder {
    pub fn new(resolution: Resolution, fps: u32) -> Self {
        Self {
            fps,
            resolution,
            args: Vec::new(),
        }
    }

    pub fn arg(mut self, a: impl Into<String>) -> Self {
        self.args.push(a.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn encode<T: Timeline>(&self, tl: &T, out: &Path) -> Result<(), FfmpegError> {
        if self.fps == 0 {
            return Err(FfmpegError::ZeroFps);
        }
        let duration = tl.duration();
        if !duration.is_finite() || duration < 0.0 {
            return Err(FfmpegError::InvalidDuration(duration));
        }

        let w = self.resolution.width;
        let h = self.resolution.height;
        let expected_bytes = (w as usize) * (h as usize) * 4;
        // ceil(duration * fps), saturating to u64.
        let total_frames = (duration * self.fps as f32).ceil().max(0.0) as u64;

        let size_arg = format!("{w}x{h}");
        let fps_arg = self.fps.to_string();

        let mut cmd = Command::new("ffmpeg");
        cmd.arg("-y")
            .args(["-f", "rawvideo"])
            .args(["-pix_fmt", "rgba"])
            .args(["-s", &size_arg])
            .args(["-r", &fps_arg])
            .args(["-i", "-"])
            .args(&self.args)
            .arg(out)
            .stdin(Stdio::piped())
            // Capture stderr so we can surface ffmpeg's error message on failure.
            // ffmpeg is chatty on stderr even on success, but we only read it
            // when something goes wrong.
            .stderr(Stdio::piped())
            .stdout(Stdio::null());

        let mut child = cmd.spawn().map_err(FfmpegError::Spawn)?;
        let mut stdin = child.stdin.take().expect("stdin piped");

        let write_result = (|| -> Result<(), FfmpegError> {
            for frame_idx in 0..total_frames {
                let t = TimelineTime::new(frame_idx as f32 / self.fps as f32);
                let image = tl.build(t, self.resolution);

                if image.format != PixelFormat::Rgba8 {
                    return Err(FfmpegError::UnsupportedFormat {
                        frame: frame_idx,
                        format: image.format,
                    });
                }
                if image.pixels.len() != expected_bytes {
                    return Err(FfmpegError::FrameSizeMismatch {
                        frame: frame_idx,
                        expected: expected_bytes,
                        actual: image.pixels.len(),
                        width: w,
                        height: h,
                    });
                }

                stdin
                    .write_all(&image.pixels)
                    .map_err(|source| FfmpegError::Write {
                        frame: frame_idx,
                        source,
                    })?;
            }
            Ok(())
        })();

        // Close stdin so ffmpeg can finalize the file, even if frame
        // production errored partway through — that lets ffmpeg shut down
        // cleanly so we can collect its stderr.
        drop(stdin);

        let mut stderr_buf = String::new();
        if let Some(mut stderr) = child.stderr.take() {
            stderr
                .read_to_string(&mut stderr_buf)
                .map_err(FfmpegError::ReadStderr)?;
        }
        let status = child.wait().map_err(FfmpegError::Wait)?;

        write_result?;

        if !status.success() {
            return Err(FfmpegError::NonZeroExit {
                status: status.to_string(),
                stderr: stderr_buf,
            });
        }
        Ok(())
    }
}
