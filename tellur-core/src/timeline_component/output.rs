//! Channel / output types: audio buffers, subtitle cues, and the
//! [`Arrangement`] tree the live UI draws.

// ── Channels / output types (`.sketch/01` A.7) ──────────────────────────────

/// Interleaved f32 samples + rate. A minimal skeleton; the encoder fixes one
/// output rate + channel layout and leaves resample into it (`.sketch/01` A.7).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioBuffer {
    /// Interleaved f32 PCM samples.
    pub samples: Vec<f32>,
    /// Sample rate in Hz.
    pub rate: u32,
    /// Number of interleaved channels.
    pub channels: u16,
}

impl AudioBuffer {
    /// An empty buffer at the given rate / channel layout.
    pub fn empty(rate: u32, channels: u16) -> Self {
        Self {
            samples: Vec::new(),
            rate,
            channels,
        }
    }
}

/// One subtitle interval, absolute on the timeline (after [`cues`](super::TimelineComponent::cues)).
#[derive(Debug, Clone, PartialEq)]
pub struct Cue {
    pub start: f32,
    pub end: f32,
    pub text: String,
}

/// One resolved [`Event`](super::Event) marker on a node: the absolute time it fires, plus the
/// event's optional display name (from [`Event::named`](super::Event::named)) so the live UI can
/// label the marker. The name is owned (`String`) so the arrangement is a
/// self-contained, serializable snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct TriggerMark {
    pub time: f32,
    pub name: Option<String>,
}

/// The CALL-SITE source location of a node: the `file:line` of the `.child(...)`
/// call that placed the component into its container. Surfaced so the live UI
/// can jump a clicked node back to its authoring line. Owned (`String`) so the
/// arrangement is a self-contained, serializable snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceLoc {
    pub file: String,
    pub line: u32,
}

/// What the live UI draws — the resolved arrangement of a node and its
/// children. Built by walking the RESOLVED tree (`.sketch/01` A.7 / B.4).
///
/// `trim` carries the source crop separately so the UI can show both the placed
/// bar and the source crop; `triggers` surfaces where [`Event`](super::Event)s fire (each a
/// [`TriggerMark`] carrying the time and the event's optional name); `source` is
/// the `.child(...)` call site that placed the node (see [`SourceLoc`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Arrangement {
    pub kind: NodeKind,
    pub label: String,
    /// Human-readable DISPLAY NAME for a `#[component(...)]` node, distinct from
    /// [`label`](Self::label) (which carries body-specific content such as a
    /// caption's text or a clip's source path). Auto-derived from the component's
    /// PascalCase name, or set from an explicit `name = "..."` template; `None`
    /// for nodes that have no enclosing named component.
    pub name: Option<String>,
    /// The `.child(...)` CALL SITE that placed this node, captured by the
    /// generated container setter via `#[track_caller]`; `None` for the root and
    /// for nodes built outside a tracked setter.
    pub source: Option<SourceLoc>,
    pub start: f32,
    pub end: f32,
    pub trim: Option<(f32, f32)>,
    pub triggers: Vec<TriggerMark>,
    pub children: Vec<Arrangement>,
}

/// The kind of node the live UI renders.
///
/// The display side collapses to three TRACK kinds — `Video` (映像: every
/// rasterized/timeless visual, including backdrops, captions, and reveals),
/// `Audio` (音声), and `Subtitle` (字幕) — plus the two structural containers
/// (`Timeline` / `Sequence`). There is intentionally no separate caption kind:
/// a styled `Text` telop is a visual and lives on the video track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeKind {
    Video,
    Audio,
    Subtitle,
    Timeline,
    Sequence,
}
