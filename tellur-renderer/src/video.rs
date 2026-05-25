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

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{ChildStdout, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use indicatif::{MultiProgress, ProgressBar, ProgressState, ProgressStyle};
use tellur_core::raster::{PixelFormat, Resolution};
use tellur_core::time::TimelineTime;
use tellur_core::timeline::Timeline;
use thiserror::Error;

use crate::render_context::CachingRenderContext;

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

        // One context for the whole encode so memoization survives across
        // frames — that's what makes static subtrees (e.g. a DropShadow
        // wrapping a time-invariant child) only re-render once.
        let mut ctx = CachingRenderContext::new();

        // Per-phase timing so we can tell whether wall-clock time is
        // being spent inside `tl.build` (= our rendering pipeline) or
        // blocked on `stdin.write_all` (= ffmpeg's encoder backpressure).
        let mut build_time = Duration::ZERO;
        let mut write_time = Duration::ZERO;
        let loop_start = Instant::now();

        let write_result = (|| -> Result<(), FfmpegError> {
            for frame_idx in 0..total_frames {
                let t = TimelineTime::new(frame_idx as f32 / self.fps as f32);

                let build_start = Instant::now();
                let image = tl.build(t, self.resolution, &mut ctx);
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

        // Dump cache metrics so users can confirm memoization is firing
        // (and diagnose why it isn't when hit_rate stays low). The
        // breakdown-by-type rows are particularly useful for spotting
        // which component types are not benefiting from the cache.
        // The loop-phase timings sit above the cache summary so it's
        // immediately visible whether our render path or ffmpeg's
        // encoder is the wall-clock bottleneck.
        if self.progress {
            let other = loop_elapsed.saturating_sub(build_time + write_time);
            eprintln!(
                "Loop  {} total = {} build + {} ffmpeg-write + {} other",
                format_duration_short(loop_elapsed),
                format_duration_short(build_time),
                format_duration_short(write_time),
                format_duration_short(other),
            );
            eprint!("{}", ctx.metrics());
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
