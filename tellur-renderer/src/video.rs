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
//! Progress: a three-row display, driven by [`indicatif`]:
//!
//! ```text
//!   Render  ━━━━━━━━━━━━╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌  120/300  ( 40%)  00:01:18 > 00:00:42
//!   Encode  ━━━━━━━━━━╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌  100/300  ( 33%)  00:01:23 > 00:00:42
//!           42.50 MiB @ 3500.2kbits/s
//! ```
//!
//! Row 1 (`Render`) counts frames as we produce them. Row 2 (`Encode`)
//! is updated by parsing `key=value` blocks that `ffmpeg` writes to
//! stdout via `-progress pipe:1`. Row 3 surfaces the running output
//! size and bitrate as reported by ffmpeg. Both bars show
//! `eta > elapsed` in zero-padded `HH:MM:SS` form. The bar fills the
//! terminal width; separators and the total count are dimmed so the
//! live numbers stand out. Disable via [`FfmpegEncoder::progress`].

use std::fmt;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdout, Command, Stdio};
use std::str::FromStr;
use std::thread;
use std::time::{Duration, Instant};

use indicatif::{MultiProgress, ProgressBar, ProgressState, ProgressStyle};
use tellur_core::raster::{PixelFormat, Resolution};
use tellur_core::render_context::RenderContext;
use tellur_core::time::TimelineTime;
use tellur_core::timeline::Timeline;
use tellur_core::timeline_component::ResolvedTimeline;
use thiserror::Error;

use crate::render_context::CachingRenderContext;

/// Fixed audio output rate for the A/V mux (`.sketch/01` ZONE C: one fixed
/// output rate + channel layout at the encoder boundary; leaves resample in).
const AUDIO_RATE: u32 = 48_000;
/// Fixed audio output channel layout for the A/V mux (stereo).
const AUDIO_CHANNELS: u16 = 2;

/// The YUV range the rendered full-range RGB is converted to (and tagged as)
/// when encoding. The conversion and the color tag are always derived from the
/// SAME value, so they cannot disagree — a conversion/tag mismatch is exactly
/// what shifts the decoded colors away from the rendered source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ColorRange {
    /// Limited / "TV" range (luma 16–235). Useful for broad external-player
    /// compatibility when a downstream target expects studio-range video.
    Limited,
    /// Full / "PC" range (luma 0–255). The default because rendered frames and
    /// PNG stills are full-range RGB, so this keeps MP4 preview/export colors
    /// aligned with paused frames.
    #[default]
    Full,
}

impl ColorRange {
    /// A stable user-facing token for CLI, manifest, query, and headers.
    pub fn as_str(self) -> &'static str {
        match self {
            ColorRange::Limited => "limited",
            ColorRange::Full => "full",
        }
    }

    /// The ffmpeg range token used for both the `scale`/`setparams` filter and
    /// the `-color_range` output tag.
    pub fn ffmpeg_token(self) -> &'static str {
        match self {
            ColorRange::Limited => "tv",
            ColorRange::Full => "pc",
        }
    }
}

impl FromStr for ColorRange {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "limited" | "limited-range" | "tv" => Ok(ColorRange::Limited),
            "full" | "full-range" | "pc" => Ok(ColorRange::Full),
            other => Err(format!(
                "invalid color range `{other}`; expected `full` or `limited`"
            )),
        }
    }
}

impl fmt::Display for ColorRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Builder that spawns `ffmpeg` and drives a [`Timeline`] through it.
///
/// The frame size is fixed at construction (`resolution`) and frames are
/// emitted at `fps` Hz. Output-side `ffmpeg` arguments (codec, container,
/// filters, ...) are supplied via [`Self::arg`] / [`Self::args`] and inserted
/// verbatim between the raw-video input and the output path.
///
/// The rendered frames are full-range RGB; the encoder converts them to
/// BT.709 YUV and tags the output accordingly (see [`Self::color_range`]) so a
/// decoder reproduces the rendered colors instead of guessing a matrix/range.
pub struct FfmpegEncoder {
    fps: u32,
    resolution: Resolution,
    args: Vec<String>,
    progress: bool,
    color_range: ColorRange,
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
            color_range: ColorRange::default(),
        }
    }

    /// Selects the YUV range the rendered full-range RGB is converted to AND the
    /// color metadata the output is tagged with. Both are derived from this one
    /// value, so they can never disagree. Defaults to [`ColorRange::Full`] so
    /// MP4 output matches full-range rendered frames and PNG stills; pass
    /// [`ColorRange::Limited`] when a downstream target requires TV range.
    pub fn color_range(mut self, range: ColorRange) -> Self {
        self.color_range = range;
        self
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
        // ceil(duration * fps), saturating to u64.
        let total_frames = (duration * self.fps as f32).ceil().max(0.0) as u64;

        // No audio input for the old path: pass no extra args, so the command
        // is byte-identical to the pre-step-8 behaviour.
        let mut ctx = CachingRenderContext::new();
        self.drive_ffmpeg(&[], total_frames, out, |frame_idx| {
            let t = TimelineTime::new(frame_idx as f32 / self.fps as f32);
            let image = tl.build(t, self.resolution, &mut ctx);
            Ok(ctx.readback(image))
        })
        .map(|_| {
            if self.progress {
                eprint!("{}", ctx.metrics());
            }
        })
    }

    /// A/V encode for the timeline subsystem (`.sketch/01` A.7 / B4 v1). Does NOT
    /// touch the old [`encode`](Self::encode) path.
    ///
    /// Pre-renders the whole mixed audio track to a temp 32-bit float WAV
    /// ([`ResolvedTimeline::render_audio`]), then spawns ffmpeg with the video on
    /// stdin (`-i -`, as today) AND the temp WAV as a SECOND input — the audio
    /// `-i <tmp.wav>` plus the `-c:a aac` / `-map` flags are injected through the
    /// SAME `.args()` path that lands between the input and the output, so the
    /// `FfmpegEncoder` struct is unchanged. Video frames stream from
    /// [`ResolvedTimeline::frame`]; a `None` frame is emitted as transparent.
    ///
    /// The WAV is sized to `ceil(duration * fps) / fps` seconds (the video's
    /// frame-quantized length), NOT `duration`, so a `-shortest` in the caller's
    /// args never tail-clips the audio against a slightly longer video.
    pub fn encode_timeline(
        &self,
        resolved: &ResolvedTimeline,
        out: &Path,
    ) -> Result<(), FfmpegError> {
        if self.fps == 0 {
            return Err(FfmpegError::ZeroFps);
        }
        let duration = resolved.duration();
        if !duration.is_finite() || duration < 0.0 {
            return Err(FfmpegError::InvalidDuration(duration));
        }
        let total_frames = (duration * self.fps as f32).ceil().max(0.0) as u64;
        // Frame-quantized video length: the audio is rendered to exactly this
        // many seconds so the two streams end together.
        let video_seconds = total_frames as f32 / self.fps as f32;

        // Render + write the mixed audio track to a temp WAV.
        let mut mixed = resolved.render_audio(AUDIO_RATE, AUDIO_CHANNELS);
        fit_audio_to_seconds(
            &mut mixed.samples,
            AUDIO_RATE,
            AUDIO_CHANNELS,
            video_seconds,
        );
        let wav_path = unique_temp_wav();
        write_wav_f32le(&wav_path, &mixed.samples, AUDIO_RATE, AUDIO_CHANNELS)
            .map_err(FfmpegError::Spawn)?;

        // The second input + stream maps, injected through the SAME arg slot the
        // user's `.args()` use (between input and output). `-map 0:v -map 1:a`
        // pairs the rawvideo stdin (input 0) with the WAV (input 1); the user's
        // own args (codec/container/-shortest/...) follow.
        let wav_str = wav_path.to_string_lossy().to_string();
        let audio_args = vec![
            "-i".to_string(),
            wav_str,
            "-c:a".to_string(),
            "aac".to_string(),
            "-map".to_string(),
            "0:v:0".to_string(),
            "-map".to_string(),
            "1:a:0".to_string(),
        ];

        let mut ctx = CachingRenderContext::new();
        let result = self.drive_ffmpeg(&audio_args, total_frames, out, |frame_idx| {
            let t = TimelineTime::new(frame_idx as f32 / self.fps as f32);
            let image = resolved
                .frame(t, self.resolution, &mut ctx)
                .unwrap_or_else(|| transparent_frame(self.resolution));
            Ok(ctx.readback(image))
        });

        // Best-effort cleanup of the temp WAV regardless of the encode outcome.
        let _ = std::fs::remove_file(&wav_path);

        result.map(|_| {
            if self.progress {
                eprint!("{}", ctx.metrics());
            }
        })
    }

    /// Shared ffmpeg lifecycle: spawn with the fixed rawvideo input, the
    /// caller's `extra_args` (the audio second input for the A/V path, empty for
    /// the video-only path) followed by `self.args`, then stream `total_frames`
    /// produced by `frame_fn` to stdin while driving the progress display.
    ///
    /// `frame_fn(idx)` returns the readback CPU image for frame `idx`. Splitting
    /// this out lets both [`encode`](Self::encode) and
    /// [`encode_timeline`](Self::encode_timeline) reuse the exact same command
    /// shape, progress machinery, and error handling.
    fn drive_ffmpeg(
        &self,
        extra_args: &[String],
        total_frames: u64,
        out: &Path,
        mut frame_fn: impl FnMut(u64) -> Result<tellur_core::raster::CpuRasterImage, FfmpegError>,
    ) -> Result<(), FfmpegError> {
        let w = self.resolution.width;
        let h = self.resolution.height;
        let expected_bytes = (w as usize) * (h as usize) * 4;

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

        // Convert the rendered FULL-RANGE RGB to BT.709 YUV and tag the stream
        // with that exact matrix/range. `scale` does the RGB→YUV conversion at the
        // chosen range, `setparams` stamps all four color fields onto the frames
        // (so the encoder writes primaries/transfer too, codec-agnostically), and
        // the `-color_*` output options mirror them at the stream level. Both the
        // conversion (`out_range`) and the tag (`-color_range`) come from the SAME
        // token, so they can't drift apart — a mismatch is what would otherwise
        // shift decoded colors away from the rendered source.
        let range = self.color_range.ffmpeg_token();
        let color_vf = format!(
            "scale=out_range={range}:out_color_matrix=bt709,format=yuv420p,\
             setparams=range={range}:color_primaries=bt709:colorspace=bt709:color_trc=bt709"
        );
        let color_args = [
            "-vf",
            &color_vf,
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
            "-colorspace",
            "bt709",
            "-color_range",
            range,
        ];

        // `extra_args` (e.g. the audio `-i <wav>` + maps) declares the second
        // input; the color args and the user's `self.args` are output options that
        // follow. The color args precede `self.args` so an advanced caller can
        // still override them through raw `.args()` if they take full control.
        cmd.args(extra_args)
            .args(color_args)
            .args(&self.args)
            .arg(out)
            .stdin(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn().map_err(FfmpegError::Spawn)?;
        let mut stdin = child.stdin.take().expect("stdin piped");

        // Set up the three-row progress display and a thread that drains
        // ffmpeg's progress output. The `_multi` guard keeps the multi-bar
        // alive for the duration of the encode; dropping it after the bars
        // finish lets indicatif clear/finalize the lines cleanly.
        let (_multi, render_bar, encode_bar, info_bar, progress_thread) = if self.progress {
            let multi = MultiProgress::new();

            let render_bar = multi.add(ProgressBar::new(total_frames));
            render_bar.set_style(make_bar_style("Render", GREEN, total_frames));

            let encode_bar = multi.add(ProgressBar::new(total_frames));
            encode_bar.set_style(make_bar_style("Encode", ORANGE, total_frames));

            // Third row: size @ bitrate text only. We piggyback on a
            // length-1 `ProgressBar` whose template renders just `{msg}`
            // so indicatif owns the line lifecycle (redraw, clear,
            // finish) without painting a bar. Leading indent matches the
            // bar rows' left margin + label width + gap (= 2 + 6 + 2).
            // The size and bitrate values keep the default foreground;
            // only the `@` separator is muted (applied inside `set_message`).
            let info_bar = multi.add(ProgressBar::new(1));
            info_bar.set_style(
                ProgressStyle::with_template("          {msg}").expect("static template parses"),
            );
            info_bar.set_message("-");

            let stdout = child
                .stdout
                .take()
                .expect("stdout piped when progress=true");
            let encode_bar_for_thread = encode_bar.clone();
            let info_bar_for_thread = info_bar.clone();
            let handle = thread::spawn(move || {
                drive_encode_progress(stdout, encode_bar_for_thread, info_bar_for_thread)
            });

            (
                Some(multi),
                Some(render_bar),
                Some(encode_bar),
                Some(info_bar),
                Some(handle),
            )
        } else {
            (None, None, None, None, None)
        };

        // Per-phase timing so we can tell whether wall-clock time is
        // being spent inside `frame_fn` (= our rendering pipeline) or
        // blocked on `stdin.write_all` (= ffmpeg's encoder backpressure).
        let mut build_time = Duration::ZERO;
        let mut write_time = Duration::ZERO;
        let loop_start = Instant::now();

        let write_result = (|| -> Result<(), FfmpegError> {
            for frame_idx in 0..total_frames {
                let build_start = Instant::now();
                let image = frame_fn(frame_idx)?;
                build_time += build_start.elapsed();

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

                let write_start = Instant::now();
                stdin
                    .write_all(&image.pixels)
                    .map_err(|source| FfmpegError::Write {
                        frame: frame_idx,
                        source,
                    })?;
                write_time += write_start.elapsed();

                if let Some(bar) = &render_bar {
                    bar.inc(1);
                }
            }
            Ok(())
        })();
        let loop_elapsed = loop_start.elapsed();

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
        if let Some(bar) = &info_bar {
            bar.finish();
        }

        // Loop-phase timings so it's immediately visible whether our render path
        // or ffmpeg's encoder is the wall-clock bottleneck. The cache summary is
        // printed by the caller (which owns the `CachingRenderContext`).
        if self.progress {
            let other = loop_elapsed.saturating_sub(build_time + write_time);
            eprintln!(
                "Loop  {} total = {} build + {} ffmpeg-write + {} other",
                format_duration_short(loop_elapsed),
                format_duration_short(build_time),
                format_duration_short(write_time),
                format_duration_short(other),
            );
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

// ── A/V mux helpers (step 8) ──────────────────────────────────────────────

/// A fully transparent RGBA frame at `res`. Used when a timeline contributes no
/// visual at a given time (`ResolvedTimeline::frame` ⇒ `None`) so ffmpeg still
/// gets a well-formed rawvideo frame.
fn transparent_frame(res: Resolution) -> tellur_core::raster::RasterImage {
    let count = (res.width as usize) * (res.height as usize) * 4;
    tellur_core::raster::RasterImage::cpu(
        res.width,
        res.height,
        PixelFormat::Rgba8,
        vec![0u8; count],
    )
}

/// A process- and time-unique temp WAV path, so concurrent encodes never clash.
fn unique_temp_wav() -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut path = std::env::temp_dir();
    path.push(format!("tellur_mux_{}_{}.wav", std::process::id(), nanos));
    path
}

/// Pads / truncates an interleaved f32 buffer to exactly `seconds` at `rate` /
/// `channels` so the audio matches the frame-quantized video length (see
/// `encode_timeline`'s doc — this is what stops `-shortest` tail-clipping).
fn fit_audio_to_seconds(samples: &mut Vec<f32>, rate: u32, channels: u16, seconds: f32) {
    let ch = channels.max(1) as usize;
    let target_frames = (seconds.max(0.0) * rate as f32).round() as usize;
    samples.resize(target_frames * ch, 0.0);
}

/// Writes interleaved f32 `samples` as a 32-bit IEEE float WAV.
fn write_wav_f32le(path: &Path, samples: &[f32], rate: u32, channels: u16) -> std::io::Result<()> {
    let bits: u16 = 32;
    let bytes_per_sample = (bits as u32 / 8) as usize;
    let byte_rate = rate * channels as u32 * bytes_per_sample as u32;
    let block_align = channels * bytes_per_sample as u16;
    let data_bytes = (samples.len() * bytes_per_sample) as u32;

    let mut bytes = Vec::with_capacity(44 + samples.len() * bytes_per_sample);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    bytes.extend_from_slice(&3u16.to_le_bytes()); // audio format = IEEE float
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&byte_rate.to_le_bytes());
    bytes.extend_from_slice(&block_align.to_le_bytes());
    bytes.extend_from_slice(&bits.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_bytes.to_le_bytes());
    for &s in samples {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, &bytes)
}

// Drains ffmpeg's `-progress pipe:1` stream and translates it into bar
// updates. The stream is plain text `key=value`, one pair per line, with
// each block terminated by `progress=continue` (or `progress=end` at EOF).
// `frame=` drives the Encode bar's position; `total_size=` / `bitrate=`
// are accumulated and flushed as a single combined message onto the
// info bar (third row) at each block boundary so the displayed string
// is always self-consistent.
fn drive_encode_progress(stdout: ChildStdout, encode_bar: ProgressBar, info_bar: ProgressBar) {
    let reader = BufReader::new(stdout);
    let mut total_size: Option<u64> = None;
    let mut bitrate: Option<String> = None;
    for line in reader.lines().map_while(Result::ok) {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key {
            "frame" => {
                if let Ok(n) = value.parse::<u64>() {
                    encode_bar.set_position(n);
                }
            }
            "total_size" => {
                if let Ok(n) = value.parse::<i64>() {
                    if n >= 0 {
                        total_size = Some(n as u64);
                    }
                }
            }
            "bitrate" if value != "N/A" => {
                bitrate = Some(value.to_string());
            }
            "progress" => {
                let size_str = total_size.map(format_bytes).unwrap_or_else(|| "-".into());
                let br = bitrate.as_deref().unwrap_or("-");
                info_bar.set_message(format!("{size_str} {MUTED}@{RESET} {br}"));
            }
            _ => {}
        }
    }
}

fn format_duration_short(d: Duration) -> String {
    let micros = d.as_micros();
    if micros >= 1_000_000 {
        format!("{:.2}s", d.as_secs_f64())
    } else if micros >= 1_000 {
        format!("{:.2}ms", micros as f64 / 1_000.0)
    } else {
        format!("{micros}µs")
    }
}

fn format_bytes(b: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let bf = b as f64;
    if bf >= GIB {
        format!("{:.2} GiB", bf / GIB)
    } else if bf >= MIB {
        format!("{:.2} MiB", bf / MIB)
    } else if bf >= KIB {
        format!("{:.2} KiB", bf / KIB)
    } else {
        format!("{b} B")
    }
}

// ── progress styling ────────────────────────────────────────────────────
//
// Match the look of the `~/dotfiles` Claude statusline. Three foreground
// tiers, applied consistently across all three rows:
//
// - **Label color** (GREEN for Render, ORANGE for Encode): the label
//   itself, the filled bar segment, and the live counters (`pos`,
//   `percent`). These are the values you actively watch.
// - **Default (white)**: the time values (`eta`, `elapsed`). Important
//   enough to read clearly, but not tied to a specific row's identity.
// - **MUTED grey**: everything else — the unfilled bar, separators
//   (`/`, `(`, `%)`, `>`), the total count, and the `@` between size
//   and bitrate on the third row. De-emphasized so the live numbers
//   stand out. (Size and bitrate themselves use the default color.)
//
// The bar fills the terminal width minus the rest of the template and
// a two-column margin on each side; time is shown as `eta > elapsed`
// (remaining first, since that's the figure you usually want).

const GREEN: &str = "\x1b[38;2;151;201;195m";
const ORANGE: &str = "\x1b[38;2;209;154;102m";
/// Medium grey for de-emphasized text — halfway between the terminal's
/// default foreground and a heavy dim, so the muted parts recede behind
/// the live numbers without disappearing.
const MUTED: &str = "\x1b[38;2;128;128;128m";
const RESET: &str = "\x1b[0m";

/// Smallest bar we'll draw when the terminal is narrow.
const MIN_BAR_WIDTH: usize = 8;
/// Fallback terminal width when we can't query the TTY (piped output etc.).
const FALLBACK_TERM_WIDTH: usize = 80;

fn fmt_hms(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

/// Visible (non-ANSI) length of everything in the template except
/// `{custom_bar}`. Lets the custom bar fill the remaining columns.
///
/// Layout (the spaces are intentional — same shape for both bars):
///
/// ```text
///   LLLLLL  [bar]  PPPPP/NNN  (XXX%)  HH:MM:SS > HH:MM:SS
/// ^^      ^^      ^^         ^^      ^^                  ^^
/// left    label+  bar+        pos/len   percent          right
/// pad     gap     gap                                     pad
/// ```
///
/// Left and right two-column margins keep the bar from butting against
/// the terminal edges.
fn bar_overhead(label_display_len: usize, len_digits: usize) -> usize {
    // left_pad + label + "  " (label gap) + "  " (post-bar gap)
    // + "PPPPP" + "/" + len + "  " + "(" + "XXX" + "%" + ")" + "  "
    // + "HH:MM:SS" + " > " + "HH:MM:SS" + right_pad
    2 + label_display_len + 2 + 2 + 5 + 1 + len_digits + 2 + 1 + 3 + 1 + 1 + 2 + 8 + 3 + 8 + 2
}

fn term_width() -> usize {
    console::Term::stdout()
        .size_checked()
        .map(|(_, cols)| cols as usize)
        .unwrap_or(FALLBACK_TERM_WIDTH)
}

// `label` and `label_color` are static so the captured closures satisfy
// indicatif's `Fn + Send + Sync + 'static` bound on custom keys.
fn make_bar_style(label: &'static str, label_color: &'static str, total: u64) -> ProgressStyle {
    let len_digits = total.to_string().len().max(1);
    let overhead = bar_overhead(label.chars().count(), len_digits);

    let template = format!(
        "  {label_color}{label}{RESET}  {{custom_bar}}  \
         {label_color}{{pos:>5}}{RESET}{MUTED}/{{len}}{RESET}  \
         {MUTED}({RESET}{label_color}{{percent:>3}}{RESET}{MUTED}%){RESET}  \
         {{eta_hms}} {MUTED}>{RESET} {{elapsed_hms}}"
    );
    ProgressStyle::with_template(&template)
        .expect("static template parses")
        .with_key(
            "custom_bar",
            move |state: &ProgressState, w: &mut dyn std::fmt::Write| {
                let bar_width = term_width().saturating_sub(overhead).max(MIN_BAR_WIDTH);
                let frac = state.fraction();
                let filled = ((frac * bar_width as f32).round() as usize).min(bar_width);
                let empty = bar_width - filled;
                let _ = write!(
                    w,
                    "{label_color}{}{MUTED}{}{RESET}",
                    "━".repeat(filled),
                    "╌".repeat(empty),
                );
            },
        )
        .with_key(
            "elapsed_hms",
            |state: &ProgressState, w: &mut dyn std::fmt::Write| {
                let _ = write!(w, "{}", fmt_hms(state.elapsed().as_secs()));
            },
        )
        .with_key(
            "eta_hms",
            |state: &ProgressState, w: &mut dyn std::fmt::Write| {
                let _ = write!(w, "{}", fmt_hms(state.eta().as_secs()));
            },
        )
}

#[cfg(test)]
mod av_mux_tests {
    use super::*;
    use std::path::PathBuf;
    use tellur_core::geometry::{Constraints, Vec2};
    use tellur_core::raster::{PixelFormat, RasterComponent, RasterImage, Resolution};
    use tellur_core::render_context::RenderContext;
    use tellur_core::timeline_component::{resolve, Timed, TimedBuilder};
    use tellur_core::timeline_container::{AudioFile, Timeline};

    // A solid opaque-color visual that fills the target, so the timeline has a
    // real video stream alongside the audio.
    #[derive(PartialEq, Hash)]
    struct Solid;

    impl RasterComponent for Solid {
        fn layout(&self, _c: Constraints) -> Vec2 {
            Vec2(1.0, 1.0)
        }
        fn render(&self, _s: Vec2, t: Resolution, _ctx: &mut dyn RenderContext) -> RasterImage {
            let count = (t.width as usize) * (t.height as usize);
            let mut px = Vec::with_capacity(count * 4);
            for _ in 0..count {
                px.extend_from_slice(&[20, 80, 160, 255]);
            }
            RasterImage::cpu(t.width, t.height, PixelFormat::Rgba8, px)
        }
    }

    // Synthesizes a 1s 440 Hz mono sine WAV (f32le) to a temp path.
    fn sine_wav() -> PathBuf {
        let rate = 48_000u32;
        let frames = rate as usize;
        let mut samples = Vec::with_capacity(frames);
        for i in 0..frames {
            let t = i as f32 / rate as f32;
            let v = (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.5;
            samples.push(v);
        }
        let mut path = std::env::temp_dir();
        path.push(format!("tellur_av_src_{}.wav", std::process::id()));
        write_wav_f32le(&path, &samples, rate, 1).expect("write sine wav");
        path
    }

    #[test]
    fn wav_f32le_preserves_samples_outside_unit_range() {
        let mut path = std::env::temp_dir();
        path.push(format!("tellur_float_wav_{}.wav", std::process::id()));

        let samples = [1.5_f32, -2.0_f32];
        write_wav_f32le(&path, &samples, 48_000, 1).expect("write float wav");
        let bytes = std::fs::read(&path).expect("read float wav");

        assert_eq!(&bytes[20..22], &3u16.to_le_bytes());
        assert_eq!(&bytes[34..36], &32u16.to_le_bytes());
        assert_eq!(&bytes[40..44], &8u32.to_le_bytes());
        assert_eq!(&bytes[44..48], &1.5_f32.to_le_bytes());
        assert_eq!(&bytes[48..52], &(-2.0_f32).to_le_bytes());

        let _ = std::fs::remove_file(&path);
    }

    // Asserts the encoded mp4 has a stream of `kind` ("a" audio / "v" video).
    fn has_stream(path: &Path, kind: &str) -> bool {
        let out = Command::new("ffprobe")
            .args(["-v", "error", "-select_streams", kind])
            .args(["-show_entries", "stream=codec_type"])
            .args(["-of", "csv=p=0"])
            .arg(path)
            .output()
            .expect("run ffprobe");
        let txt = String::from_utf8_lossy(&out.stdout);
        !txt.trim().is_empty()
    }

    // Asserts the encoded mp4 has an audio stream via ffprobe.
    fn has_audio_stream(path: &Path) -> bool {
        has_stream(path, "a")
    }

    // The video stream's four color metadata fields, as
    // "range|space|primaries|transfer".
    fn probe_color(path: &Path) -> String {
        let out = Command::new("ffprobe")
            .args(["-v", "error", "-select_streams", "v:0"])
            .args([
                "-show_entries",
                "stream=color_range,color_space,color_primaries,color_transfer",
            ])
            .args(["-of", "default=noprint_wrappers=1:nokey=1"])
            .arg(path)
            .output()
            .expect("run ffprobe");
        String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join("|")
    }

    /// Writes a short `testsrc` mp4 (a moving color pattern) to a temp path —
    /// the real video BACKGROUND for the end-to-end A/V test.
    fn write_testsrc_mp4(secs: u32, w: u32, h: u32) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("tellur_av_bg_{}.mp4", std::process::id()));
        let lavfi = format!("testsrc=size={w}x{h}:rate=30:duration={secs}");
        let status = Command::new("ffmpeg")
            .args(["-y", "-v", "error"])
            .args(["-f", "lavfi", "-i", &lavfi])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&path)
            .status()
            .expect("spawn ffmpeg testsrc");
        assert!(status.success(), "testsrc fixture write failed");
        path
    }

    #[test]
    #[ignore = "requires ffmpeg + ffprobe on PATH"]
    fn encode_timeline_muxes_audio_stream() {
        let src = sine_wav();

        // A 1s timeline: a solid visual windowed to 1s sets the length; the
        // sine AudioFile mixes underneath it.
        let tl = Timeline::builder()
            .child(Solid.at(0.0..1.0))
            .child(AudioFile::builder().path(src.to_str().unwrap()))
            .build();
        let resolved = resolve(tl).expect("windowed + media-backed");

        let mut out = std::env::temp_dir();
        out.push(format!("tellur_av_out_{}.mp4", std::process::id()));

        let encoder = FfmpegEncoder::new(Resolution::new(64, 64), 24)
            .progress(false)
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-shortest"]);
        encoder
            .encode_timeline(&resolved, &out)
            .expect("ffmpeg A/V mux succeeds");

        assert!(out.exists(), "output mp4 was written");
        assert!(has_audio_stream(&out), "muxed mp4 has an audio stream");

        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&out);
    }

    // The rendered frames are full-range RGB; the encoder must convert them to
    // BT.709 YUV and TAG all four color fields so a decoder reproduces the
    // rendered colors instead of guessing a matrix/range (an untagged stream
    // decoded as the player's default shifts the colors). The range token must be
    // driven by `color_range()` and stay consistent between the conversion and
    // the tag — this guards the regression that made the preview/export colors
    // differ from the paused PNG.
    #[test]
    #[ignore = "requires ffmpeg + ffprobe on PATH"]
    fn encode_timeline_tags_bt709_color_metadata() {
        let tl = Timeline::builder().child(Solid.at(0.0..0.2)).build();
        let resolved = resolve(tl).expect("solid timeline resolves");

        // Default: full-range BT.709, every field tagged.
        let mut full = std::env::temp_dir();
        full.push(format!("tellur_color_full_{}.mp4", std::process::id()));
        FfmpegEncoder::new(Resolution::new(64, 64), 24)
            .progress(false)
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .encode_timeline(&resolved, &full)
            .expect("full-range encode succeeds");
        assert_eq!(
            probe_color(&full),
            "pc|bt709|bt709|bt709",
            "default output is tagged full-range BT.709 on all four fields"
        );

        // Limited range flips ONLY the range; the matrix/primaries/transfer stay
        // BT.709.
        let mut lim = std::env::temp_dir();
        lim.push(format!("tellur_color_lim_{}.mp4", std::process::id()));
        FfmpegEncoder::new(Resolution::new(64, 64), 24)
            .progress(false)
            .color_range(ColorRange::Limited)
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .encode_timeline(&resolved, &lim)
            .expect("limited-range encode succeeds");
        assert_eq!(
            probe_color(&lim),
            "tv|bt709|bt709|bt709",
            "color_range(Limited) tags limited range, keeping the BT.709 matrix"
        );

        let _ = std::fs::remove_file(&full);
        let _ = std::fs::remove_file(&lim);
    }

    // End-to-end A/V: a REAL decoded video background (`testsrc` mp4 via
    // `VideoFile`) + an `AudioFile` + a burned-in caption overlay + a `Subtitle`
    // cue, encoded with `encode_timeline` to an mp4 carrying BOTH a video and an
    // audio stream. This is the full timeline subsystem firing end-to-end.
    #[test]
    #[ignore = "requires ffmpeg + ffprobe on PATH"]
    fn encode_timeline_real_video_audio_caption() {
        use tellur_core::timeline_container::{Subtitle, VideoFile};

        let bg = write_testsrc_mp4(1, 128, 96);
        let src = sine_wav();

        // 1s timeline: the decoded video fills the duration, the sine audio mixes
        // under it, a solid caption overlay burns in over the top half-second,
        // and a Subtitle cue spans the whole clip.
        let tl = Timeline::builder()
            .child(VideoFile::builder().path(bg.to_str().unwrap()).at(0.0..1.0))
            .child(AudioFile::builder().path(src.to_str().unwrap()))
            .child(Solid.at(0.5..1.0))
            .child(Subtitle::builder().text("caption line").fill())
            .build();
        let resolved = resolve(tl).expect("real video + audio + caption resolves");

        // The subtitle cue is collected and spans the clip (the caption channel).
        let cues = resolved.source().cues(0.0);
        let cue = cues
            .iter()
            .find(|c| c.text == "caption line")
            .expect("subtitle cue collected");
        assert!((cue.start - 0.0).abs() < 1e-3 && (cue.end - 1.0).abs() < 0.05);

        let mut out = std::env::temp_dir();
        out.push(format!("tellur_av_full_{}.mp4", std::process::id()));

        let encoder = FfmpegEncoder::new(Resolution::new(128, 96), 24)
            .progress(false)
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-shortest"]);
        encoder
            .encode_timeline(&resolved, &out)
            .expect("ffmpeg A/V mux of a real-video timeline succeeds");

        assert!(out.exists(), "output mp4 was written");
        assert!(has_stream(&out, "v"), "muxed mp4 has a video stream");
        assert!(has_stream(&out, "a"), "muxed mp4 has an audio stream");

        let _ = std::fs::remove_file(&bg);
        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&out);
    }

    // Export-path integrity: unlike the live mux (which paces frames in real time
    // and races a fully-available audio file under `-shortest`), the offline
    // `encode_timeline` pre-fits the audio to the frame-quantized video length, so
    // the output must contain EXACTLY `ceil(duration*fps)` video frames at a
    // perfectly uniform 1/fps cadence — no dropped tail, no startup PTS gap.
    // Exports the real /tmp media fixtures to /tmp/export_check.mp4 (kept for
    // manual inspection) and probes the frame count + PTS spacing.
    #[test]
    #[ignore = "requires ffmpeg + the /tmp media fixtures"]
    fn export_real_media_is_frame_exact_and_evenly_timed() {
        use tellur_core::timeline_container::VideoFile;

        let video = "/tmp/media_in_video.mp4";
        let audio = "/tmp/media_in_audio.wav";
        let dur = 4.0_f32;
        let fps = 30u32;

        let tl = Timeline::builder()
            .child(VideoFile::builder().path(video).at(0.0..dur))
            .child(AudioFile::builder().path(audio))
            .build();
        let resolved = resolve(tl).expect("real media resolves");

        let out = PathBuf::from("/tmp/export_check.mp4");
        FfmpegEncoder::new(Resolution::new(640, 360), fps)
            .progress(false)
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-shortest"])
            .encode_timeline(&resolved, &out)
            .expect("A/V export succeeds");

        // Decoded video frame count must equal ceil(duration * fps).
        let probe = Command::new("ffprobe")
            .args(["-v", "error", "-select_streams", "v:0"])
            .args(["-count_frames", "-show_entries", "stream=nb_read_frames"])
            .args(["-of", "csv=p=0"])
            .arg(&out)
            .output()
            .expect("ffprobe runs");
        let count: usize = String::from_utf8_lossy(&probe.stdout)
            .trim()
            .parse()
            .unwrap_or(0);
        let expected = (dur * fps as f32).ceil() as usize;
        assert_eq!(
            count, expected,
            "export contains every frame (no dropped tail)"
        );

        // PTS spacing must be a uniform 1/fps (no startup gap).
        let pts_out = Command::new("ffprobe")
            .args(["-v", "error", "-select_streams", "v:0"])
            .args(["-show_entries", "frame=pts_time", "-of", "csv=p=0"])
            .arg(&out)
            .output()
            .expect("ffprobe pts runs");
        let mut pts: Vec<f32> = String::from_utf8_lossy(&pts_out.stdout)
            .lines()
            .filter_map(|l| l.split(',').next())
            .filter_map(|s| s.trim().parse().ok())
            .collect();
        pts.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let step = 1.0 / fps as f32;
        for w in pts.windows(2) {
            let d = w[1] - w[0];
            assert!(
                (d - step).abs() < 1e-3,
                "uniform 1/fps cadence; found a {d:.5}s gap (expected {step:.5})"
            );
        }
        eprintln!(
            "EXPORT OK: {count} frames, uniform {step:.5}s cadence -> {}",
            out.display()
        );
    }
}
