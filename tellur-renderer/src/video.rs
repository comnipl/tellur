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
//!
//! Progress: a two-row progress display (`Render` / `Encode`) is shown by
//! default, driven by [`indicatif`]. The Render row counts frames as we
//! produce them; the Encode row is updated by parsing `frame=N` lines that
//! `ffmpeg` writes to stdout via `-progress pipe:1`. Disable via
//! [`FfmpegEncoder::progress`].

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
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
    progress: bool,
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
    #[error("frame {frame} produced {actual} bytes, expected {expected} ({width}x{height} RGBA)")]
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
            progress: true,
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

    /// Toggle the two-row progress display. Default is on. When the output
    /// is not a TTY, `indicatif` automatically renders nothing, so leaving
    /// this on is safe for piped/redirected runs.
    pub fn progress(mut self, enabled: bool) -> Self {
        self.progress = enabled;
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
            .args(["-i", "-"]);

        // -progress pipe:1 makes ffmpeg emit machine-readable key=value lines
        // to stdout; we parse `frame=N` off the stream to drive the Encode bar.
        // Without progress, we just discard stdout.
        if self.progress {
            cmd.args(["-progress", "pipe:1"]).stdout(Stdio::piped());
        } else {
            cmd.stdout(Stdio::null());
        }

        cmd.args(&self.args)
            .arg(out)
            .stdin(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn().map_err(FfmpegError::Spawn)?;
        let mut stdin = child.stdin.take().expect("stdin piped");

        // Set up the two-row progress display and a thread that drains
        // ffmpeg's progress output. The `_multi` guard keeps the multi-bar
        // alive for the duration of the encode; dropping it after both bars
        // finish lets indicatif clear/finalize the lines cleanly.
        let (_multi, render_bar, encode_bar, progress_thread) = if self.progress {
            let multi = MultiProgress::new();
            let style = ProgressStyle::with_template(
                "{msg:7} {bar:40.cyan/blue} {pos:>5}/{len} ({percent:>3}%) {elapsed_precise}",
            )
            .expect("static template parses")
            .progress_chars("##-");

            let render_bar = multi.add(ProgressBar::new(total_frames));
            render_bar.set_style(style.clone());
            render_bar.set_message("Render");

            let encode_bar = multi.add(ProgressBar::new(total_frames));
            encode_bar.set_style(style);
            encode_bar.set_message("Encode");

            let stdout = child
                .stdout
                .take()
                .expect("stdout piped when progress=true");
            let encode_bar_for_thread = encode_bar.clone();
            let handle = thread::spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines().map_while(Result::ok) {
                    if let Some(rest) = line.strip_prefix("frame=") {
                        if let Ok(n) = rest.trim().parse::<u64>() {
                            encode_bar_for_thread.set_position(n);
                        }
                    }
                }
            });

            (
                Some(multi),
                Some(render_bar),
                Some(encode_bar),
                Some(handle),
            )
        } else {
            (None, None, None, None)
        };

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

                if let Some(bar) = &render_bar {
                    bar.inc(1);
                }
            }
            Ok(())
        })();

        // Close stdin so ffmpeg can finalize the file, even if frame
        // production errored partway through — that lets ffmpeg shut down
        // cleanly so we can collect its stderr.
        drop(stdin);

        // Render side is done; the encoder thread continues until ffmpeg
        // closes stdout (which it does on exit).
        if let Some(bar) = &render_bar {
            bar.finish();
        }

        if let Some(handle) = progress_thread {
            // Join is best-effort: a panic in the parsing thread should not
            // shadow the actual ffmpeg outcome below.
            let _ = handle.join();
        }

        let mut stderr_buf = String::new();
        if let Some(mut stderr) = child.stderr.take() {
            stderr
                .read_to_string(&mut stderr_buf)
                .map_err(FfmpegError::ReadStderr)?;
        }
        let status = child.wait().map_err(FfmpegError::Wait)?;

        if let Some(bar) = &encode_bar {
            // ffmpeg's last `frame=` might lag the actual final count; snap to
            // total so the bar reads 100% on completion when it really finished.
            if status.success() {
                bar.set_position(total_frames);
            }
            bar.finish();
        }

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
