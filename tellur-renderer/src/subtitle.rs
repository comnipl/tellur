//! Subtitle sidecar export (`.sketch/01` A.7).
//!
//! [`write_subtitles`] is a FREE FUNCTION (not a trait method) so the
//! [`TimelineComponent`](tellur_core::timeline_component::TimelineComponent)
//! trait stays render-focused: sidecars are explicit. It collects the resolved
//! cues once via `resolved.source().cues(0.0)` and formats them as `.srt` or
//! `.vtt` chosen by the output file extension.

use std::io;
use std::path::Path;

use tellur_core::timeline_component::{Cue, ResolvedTimeline};

/// Writes the timeline's subtitle cues to `path`, picking the format from the
/// extension: `.vtt` → WebVTT, anything else (incl. `.srt`) → SubRip.
pub fn write_subtitles(resolved: &ResolvedTimeline, path: &Path) -> io::Result<()> {
    let cues = resolved.source().cues(0.0);
    let is_vtt = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("vtt"))
        .unwrap_or(false);
    let body = if is_vtt {
        format_vtt(&cues)
    } else {
        format_srt(&cues)
    };
    std::fs::write(path, body)
}

/// Formats cues as SubRip (`.srt`): 1-based index, `HH:MM:SS,mmm` timestamps
/// separated by ` --> `, the text, then a blank line.
fn format_srt(cues: &[Cue]) -> String {
    let mut out = String::new();
    for (i, cue) in cues.iter().enumerate() {
        out.push_str(&(i + 1).to_string());
        out.push('\n');
        out.push_str(&fmt_ts(cue.start, ','));
        out.push_str(" --> ");
        out.push_str(&fmt_ts(cue.end, ','));
        out.push('\n');
        out.push_str(&cue.text);
        out.push_str("\n\n");
    }
    out
}

/// Formats cues as WebVTT (`.vtt`): a `WEBVTT` header then `HH:MM:SS.mmm`
/// timestamped blocks.
fn format_vtt(cues: &[Cue]) -> String {
    let mut out = String::from("WEBVTT\n\n");
    for cue in cues {
        out.push_str(&fmt_ts(cue.start, '.'));
        out.push_str(" --> ");
        out.push_str(&fmt_ts(cue.end, '.'));
        out.push('\n');
        out.push_str(&cue.text);
        out.push_str("\n\n");
    }
    out
}

/// Formats `seconds` as `HH:MM:SS<sep>mmm`, where `sep` is `,` for SubRip and
/// `.` for WebVTT.
fn fmt_ts(seconds: f64, sep: char) -> String {
    let total_ms = (seconds.max(0.0) * 1000.0).round() as u64;
    let ms = total_ms % 1000;
    let total_s = total_ms / 1000;
    let s = total_s % 60;
    let m = (total_s / 60) % 60;
    let h = total_s / 3600;
    format!("{h:02}:{m:02}:{s:02}{sep}{ms:03}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cue(start: f64, end: f64, text: &str) -> Cue {
        Cue {
            start,
            end,
            text: text.to_string(),
        }
    }

    #[test]
    fn srt_formats_index_timestamps_and_text() {
        let cues = vec![cue(0.0, 1.5, "hello"), cue(2.0, 3.25, "world")];
        let out = format_srt(&cues);
        assert_eq!(
            out,
            "1\n00:00:00,000 --> 00:00:01,500\nhello\n\n\
             2\n00:00:02,000 --> 00:00:03,250\nworld\n\n"
        );
    }

    #[test]
    fn vtt_has_header_and_dot_separator() {
        let out = format_vtt(&[cue(61.0, 62.0, "late")]);
        assert!(out.starts_with("WEBVTT\n\n"));
        assert!(out.contains("00:01:01.000 --> 00:01:02.000\nlate"));
    }
}
