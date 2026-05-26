//! Text rendering as a vector component.
//!
//! [`Text`] shapes a sequence of [`TextSpan`]s with rustybuzz, lays the
//! resulting glyph runs out left-to-right, and emits one filled
//! [`Path`](crate::vector::Path) per glyph through the existing
//! [`VectorGraphic`] pipeline. The base `Text { font, size, fill }`
//! provides defaults that each `TextSpan` may override on a per-field
//! basis: a `Some(_)` value on a span replaces the base; `None` inherits
//! it. Coloring a substring red is therefore just "insert a `TextSpan`
//! whose `fill: Some(Paint::Solid(red))` between plain spans".
//!
//! Single-line only for now: `\n` is not interpreted; multi-line layout
//! and line breaking are deferred to a follow-up.

use std::hash::{Hash, Hasher};
use std::path::Path as FsPath;
use std::sync::{Arc, LazyLock};

use rustybuzz::{ttf_parser, UnicodeBuffer};
use thiserror::Error;

use crate::dyn_compare::hash_f32;
use crate::geometry::{Constraints, Rect, Transform, Vec2};
use crate::placement::Placed;
use crate::vector::{
    Fill, Group, Node, Paint, Path as VPath, PathCommand, VectorComponent, VectorGraphic,
};

#[derive(Debug, Error)]
pub enum FontError {
    #[error("failed to parse font data")]
    Parse,
    #[error("failed to read font file: {0}")]
    Io(#[from] std::io::Error),
    #[error("no system font found matching {name:?}")]
    NotFound { name: String },
    #[error("fontconfig is not available on this system")]
    FontconfigUnavailable,
}

/// The system's default sans-serif font, resolved once via fontconfig
/// on first access and shared thereafter. Use `SANS_SERIF.clone()` to
/// get an `Arc<Font>` cheap to hand to `Text::font`. The first access
/// panics if no sans-serif family is resolvable on this system; use
/// [`Font::sans_serif`] if you need to handle that case yourself.
pub static SANS_SERIF: LazyLock<Arc<Font>> =
    LazyLock::new(|| Arc::new(Font::sans_serif().expect("resolve system sans-serif font")));

/// The system's default serif font. See [`SANS_SERIF`] for sharing
/// semantics.
pub static SERIF: LazyLock<Arc<Font>> =
    LazyLock::new(|| Arc::new(Font::serif().expect("resolve system serif font")));

/// The system's default monospace font. See [`SANS_SERIF`].
pub static MONOSPACE: LazyLock<Arc<Font>> =
    LazyLock::new(|| Arc::new(Font::monospace().expect("resolve system monospace font")));

/// The system's default cursive font. See [`SANS_SERIF`].
pub static CURSIVE: LazyLock<Arc<Font>> =
    LazyLock::new(|| Arc::new(Font::cursive().expect("resolve system cursive font")));

/// The system's default fantasy font. See [`SANS_SERIF`].
pub static FANTASY: LazyLock<Arc<Font>> =
    LazyLock::new(|| Arc::new(Font::fantasy().expect("resolve system fantasy font")));

/// An owned font, cheaply shareable via `Arc<Font>` across components.
///
/// The byte buffer is reference-counted; a fresh `rustybuzz::Face` is
/// constructed per shaping/outlining call. The parse is not free but is
/// inexpensive relative to a full text render, and this avoids the
/// self-referential storage that would be needed to cache the face
/// alongside its backing bytes.
pub struct Font {
    data: Arc<Vec<u8>>,
    face_index: u32,
}

impl Font {
    /// Constructs a `Font` from owned font bytes (the first face in the
    /// file). The bytes are parsed once for validation; `FontError::Parse`
    /// is returned if they do not represent a valid font face.
    pub fn from_bytes(bytes: impl Into<Arc<Vec<u8>>>) -> Result<Self, FontError> {
        Self::from_bytes_indexed(bytes, 0)
    }

    /// Like [`Font::from_bytes`] but selects `face_index` from inside the
    /// container (used for font collections; .ttc files carry several
    /// faces in one blob).
    pub fn from_bytes_indexed(
        bytes: impl Into<Arc<Vec<u8>>>,
        face_index: u32,
    ) -> Result<Self, FontError> {
        let data: Arc<Vec<u8>> = bytes.into();
        rustybuzz::Face::from_slice(data.as_ref(), face_index).ok_or(FontError::Parse)?;
        Ok(Self { data, face_index })
    }

    /// Reads `path` into memory and constructs a `Font` from the bytes
    /// (first face only).
    pub fn load_file(path: impl AsRef<FsPath>) -> Result<Self, FontError> {
        let bytes = std::fs::read(path)?;
        Self::from_bytes(bytes)
    }

    /// Resolves the system's default sans-serif font through fontconfig
    /// (e.g. "Noto Sans" or "DejaVu Sans" on Linux, "Helvetica" on
    /// macOS).
    pub fn sans_serif() -> Result<Self, FontError> {
        Self::find_generic("sans-serif")
    }

    /// Resolves the system's default serif font through fontconfig.
    pub fn serif() -> Result<Self, FontError> {
        Self::find_generic("serif")
    }

    /// Resolves the system's default monospace font through fontconfig.
    pub fn monospace() -> Result<Self, FontError> {
        Self::find_generic("monospace")
    }

    /// Resolves the system's default cursive font through fontconfig.
    pub fn cursive() -> Result<Self, FontError> {
        Self::find_generic("cursive")
    }

    /// Resolves the system's default fantasy font through fontconfig.
    pub fn fantasy() -> Result<Self, FontError> {
        Self::find_generic("fantasy")
    }

    /// Shared backend for the generic-family lookups. Delegates to
    /// fontconfig's `FcMatch` (the same machinery `fc-match` uses), so
    /// the result reflects the user's actual system configuration —
    /// including any per-language or per-script overrides — rather than
    /// any hardcoded fallback list.
    fn find_generic(family: &str) -> Result<Self, FontError> {
        let fc = fontconfig::Fontconfig::new().ok_or(FontError::FontconfigUnavailable)?;
        let resolved = fc.find(family, None).ok_or_else(|| FontError::NotFound {
            name: family.to_owned(),
        })?;
        Self::load_file(resolved.path)
    }

    /// Resolves a font by family name through the system font database
    /// (e.g. `"DejaVu Sans"`, `"Helvetica"`). The system fonts are scanned
    /// on each call — fine for one-off setup, not for tight loops; cache
    /// the returned `Font` in an `Arc` for reuse.
    pub fn find_by_name(name: &str) -> Result<Self, FontError> {
        Self::find_by_name_with_weight(name, Weight::NORMAL)
    }

    /// Like [`Font::find_by_name`] but selects a specific weight. Useful
    /// for picking the actual "Bold" file out of a family that ships
    /// each weight as a separate `.ttf` (e.g. `DejaVu Sans` →
    /// `DejaVuSans-Bold.ttf`).
    pub fn find_by_name_with_weight(name: &str, weight: Weight) -> Result<Self, FontError> {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        let query = fontdb::Query {
            families: &[fontdb::Family::Name(name)],
            weight: fontdb::Weight(weight.0),
            ..fontdb::Query::default()
        };
        let not_found = || FontError::NotFound {
            name: format!("{} (weight {})", name, weight.0),
        };
        let id = db.query(&query).ok_or_else(not_found)?;
        let face = db.face(id).ok_or_else(not_found)?;
        let face_index = face.index;
        match &face.source {
            fontdb::Source::File(path) => {
                let bytes = std::fs::read(path)?;
                Self::from_bytes_indexed(bytes, face_index)
            }
            fontdb::Source::Binary(arc) | fontdb::Source::SharedFile(_, arc) => {
                let bytes: Vec<u8> = arc.as_ref().as_ref().to_vec();
                Self::from_bytes_indexed(bytes, face_index)
            }
        }
    }

    fn face(&self) -> rustybuzz::Face<'_> {
        rustybuzz::Face::from_slice(self.data.as_ref(), self.face_index)
            .expect("font data validated in Font constructors")
    }
}

// `PartialEq`/`Hash` use `Arc` pointer identity, so two `Font`s
// referring to the same buffer compare equal cheaply. Loading the same
// file twice yields distinct `Font`s, which is intentional — render
// caches should miss on independent loads rather than re-validate that
// two buffers contain identical bytes.
impl PartialEq for Font {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.data, &other.data) && self.face_index == other.face_index
    }
}

impl Hash for Font {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (Arc::as_ptr(&self.data) as usize).hash(state);
        self.face_index.hash(state);
    }
}

/// CSS-style weight value (100 = Thin, 400 = Normal, 700 = Bold, ...).
///
/// Applied as the OpenType `wght` variation axis. Has visible effect
/// only on Variable Fonts that expose the `wght` axis; on a non-VF font
/// the value is silently ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Weight(pub u16);

impl Weight {
    pub const THIN: Self = Self(100);
    pub const EXTRA_LIGHT: Self = Self(200);
    pub const LIGHT: Self = Self(300);
    pub const NORMAL: Self = Self(400);
    pub const MEDIUM: Self = Self(500);
    pub const SEMIBOLD: Self = Self(600);
    pub const BOLD: Self = Self(700);
    pub const EXTRA_BOLD: Self = Self(800);
    pub const BLACK: Self = Self(900);
}

impl Default for Weight {
    fn default() -> Self {
        Self::NORMAL
    }
}

/// A run of text with optional per-field style overrides.
///
/// Any `None` field inherits the value from the enclosing [`Text`]'s
/// base. To color a substring, insert a `TextSpan` whose `fill` is
/// `Some(Paint::Solid(color))` between plain spans.
#[derive(Default, Clone)]
pub struct TextSpan {
    pub text: String,
    pub fill: Option<Paint>,
    pub font: Option<Arc<Font>>,
    pub size: Option<f32>,
    pub weight: Option<Weight>,
}

impl TextSpan {
    /// A span carrying only text — fill, font, and size inherit from
    /// the enclosing [`Text`].
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }
}

impl PartialEq for TextSpan {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text
            && self.fill == other.fill
            && match (&self.font, &other.font) {
                (Some(a), Some(b)) => a == b,
                (None, None) => true,
                _ => false,
            }
            && self.size.map(f32::to_bits) == other.size.map(f32::to_bits)
            && self.weight == other.weight
    }
}

impl Hash for TextSpan {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.text.hash(state);
        self.fill.hash(state);
        match &self.font {
            None => 0u8.hash(state),
            Some(f) => {
                1u8.hash(state);
                f.as_ref().hash(state);
            }
        }
        match self.size {
            None => 0u8.hash(state),
            Some(v) => {
                1u8.hash(state);
                hash_f32(v, state);
            }
        }
        self.weight.hash(state);
    }
}

/// A single line of styled text.
///
/// `font`, `size`, `weight`, and `fill` are the defaults used by every
/// `TextSpan` that does not override them; `spans` carries the actual
/// content and any per-region styling.
#[derive(Clone)]
pub struct Text {
    pub spans: Vec<TextSpan>,
    pub font: Arc<Font>,
    pub size: f32,
    pub weight: Weight,
    pub fill: Paint,
}

impl PartialEq for Text {
    fn eq(&self, other: &Self) -> bool {
        self.spans == other.spans
            && self.font == other.font
            && self.size.to_bits() == other.size.to_bits()
            && self.weight == other.weight
            && self.fill == other.fill
    }
}

impl Hash for Text {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.spans.hash(state);
        self.font.as_ref().hash(state);
        hash_f32(self.size, state);
        self.weight.hash(state);
        self.fill.hash(state);
    }
}

impl Text {
    fn effective_fill(&self, span: &TextSpan) -> Paint {
        span.fill.clone().unwrap_or_else(|| self.fill.clone())
    }

    fn effective_font<'a>(&'a self, span: &'a TextSpan) -> &'a Arc<Font> {
        span.font.as_ref().unwrap_or(&self.font)
    }

    fn effective_size(&self, span: &TextSpan) -> f32 {
        span.size.unwrap_or(self.size)
    }

    fn effective_weight(&self, span: &TextSpan) -> Weight {
        span.weight.unwrap_or(self.weight)
    }

    /// Vertical metrics of the base font at `self.size`, returned as
    /// `(ascent, line_height)`.
    fn line_metrics(&self) -> (f32, f32) {
        let base_face = self.font.face();
        let base_upem = base_face.units_per_em() as f32;
        let base_scale = self.size / base_upem;
        let ascent = base_face.ascender() as f32 * base_scale;
        // `descender` is conventionally negative (below baseline).
        let descent = base_face.descender() as f32 * base_scale;
        let line_gap = base_face.line_gap() as f32 * base_scale;
        let line_height = ascent - descent + line_gap;
        (ascent, line_height)
    }

    /// Shapes each span and returns one [`ShapedSpan`] per input
    /// [`TextSpan`], with paths in local (span-relative) coordinates so
    /// each span graphic can be used as an independent placeable
    /// component. The line height comes from the base font; spans that
    /// override `font`/`size` may visually paint past the line box for
    /// now.
    fn shape_per_span(&self) -> Vec<ShapedSpan> {
        let (ascent, _) = self.line_metrics();
        let baseline_y = ascent;

        let mut out = Vec::with_capacity(self.spans.len());
        let mut pen_x: f32 = 0.0;

        for span in &self.spans {
            let span_start_x = pen_x;
            let mut span_paths: Vec<(Vec<PathCommand>, Paint)> = Vec::new();

            if !span.text.is_empty() {
                let font = self.effective_font(span);
                let size = self.effective_size(span);
                let fill = self.effective_fill(span);
                let weight = self.effective_weight(span);

                let mut face = font.face();
                // Apply the OpenType `wght` axis. No effect on fonts
                // without a `wght` axis; the call returns `None` and
                // we just keep going.
                face.set_variations(&[rustybuzz::Variation {
                    tag: ttf_parser::Tag::from_bytes(b"wght"),
                    value: weight.0 as f32,
                }]);
                let upem = face.units_per_em() as f32;
                let scale = size / upem;

                let mut buffer = UnicodeBuffer::new();
                buffer.push_str(&span.text);
                let glyph_buffer = rustybuzz::shape(&face, &[], buffer);

                for (info, pos) in glyph_buffer
                    .glyph_infos()
                    .iter()
                    .zip(glyph_buffer.glyph_positions().iter())
                {
                    let glyph_id = ttf_parser::GlyphId(info.glyph_id as u16);
                    let x_off = pos.x_offset as f32 * scale;
                    let y_off = pos.y_offset as f32 * scale;
                    // Local x: span starts at 0 in its own space.
                    let glyph_x = (pen_x - span_start_x) + x_off;
                    // Font Y points up; flipping by subtracting `y_off`
                    // from the Y-down baseline puts the glyph in our
                    // space.
                    let glyph_y = baseline_y - y_off;

                    let mut builder = OutlinePathBuilder {
                        commands: Vec::new(),
                        scale,
                        origin_x: glyph_x,
                        origin_y: glyph_y,
                    };
                    face.outline_glyph(glyph_id, &mut builder);
                    if !builder.commands.is_empty() {
                        span_paths.push((builder.commands, fill.clone()));
                    }

                    pen_x += pos.x_advance as f32 * scale;
                    // y_advance is typically 0 for horizontal text.
                }
            }

            let width = pen_x - span_start_x;
            out.push(ShapedSpan {
                start_x: span_start_x,
                width,
                paths: span_paths,
            });
        }

        out
    }

    /// Shapes every span and returns `(glyph paths, intrinsic size)`,
    /// with paths in the line's global coordinates (all spans
    /// concatenated left-to-right).
    fn shape_and_layout(&self) -> (Vec<(Vec<PathCommand>, Paint)>, Vec2) {
        let (_, line_height) = self.line_metrics();
        let spans = self.shape_per_span();
        let total_width = spans
            .last()
            .map(|s| s.start_x + s.width)
            .unwrap_or(0.0);

        let mut all_paths: Vec<(Vec<PathCommand>, Paint)> = Vec::new();
        for span in spans {
            let delta = Vec2(span.start_x, 0.0);
            for (commands, fill) in span.paths {
                let shifted: Vec<PathCommand> = commands
                    .into_iter()
                    .map(|c| translate_command(c, delta))
                    .collect();
                all_paths.push((shifted, fill));
            }
        }

        (all_paths, Vec2(total_width, line_height))
    }

    /// Decompose the text into per-span graphics, each placed at the
    /// position where its first glyph would land in the line. The
    /// returned `Vec` has exactly one entry per input
    /// [`TextSpan`] (in the same order), and entries for empty spans
    /// are zero-width placeholders so positional indexing matches.
    ///
    /// Useful for attaching per-span effects (transforms, drop shadows
    /// on the rasterized form, outlines, ...) by manipulating each
    /// [`Placed<TextSpanGraphic>`] before composing them back into a
    /// layer:
    ///
    /// ```ignore
    /// let [hello, world, bang]: [Placed<TextSpanGraphic>; 3] =
    ///     Text { ... }.into_spans().try_into().unwrap();
    ///
    /// let layer = VectorLayer {
    ///     size: None,                 // auto-fit
    ///     children: vec![hello.into(), world.into(), bang.into()],
    /// };
    /// ```
    pub fn into_spans(self) -> Vec<Placed<TextSpanGraphic>> {
        let (_, line_height) = self.line_metrics();
        self.shape_per_span()
            .into_iter()
            .map(|s| Placed {
                position: Vec2(s.start_x, 0.0),
                child: Box::new(TextSpanGraphic {
                    paths: s.paths,
                    size: Vec2(s.width, line_height),
                }),
            })
            .collect()
    }
}

/// One shaped span produced by [`Text::shape_per_span`].
struct ShapedSpan {
    start_x: f32,
    width: f32,
    /// Paths in local (span-relative) coordinates: the span starts at
    /// `x = 0` in its own space; the line's baseline is at `y = ascent`.
    paths: Vec<(Vec<PathCommand>, Paint)>,
}

/// Translates every coordinate in a path command by `delta`. Used when
/// stitching per-span paths back into the line's global coordinates.
fn translate_command(cmd: PathCommand, delta: Vec2) -> PathCommand {
    match cmd {
        PathCommand::MoveTo(p) => PathCommand::MoveTo(p + delta),
        PathCommand::LineTo(p) => PathCommand::LineTo(p + delta),
        PathCommand::QuadTo { control, to } => PathCommand::QuadTo {
            control: control + delta,
            to: to + delta,
        },
        PathCommand::CubicTo { c1, c2, to } => PathCommand::CubicTo {
            c1: c1 + delta,
            c2: c2 + delta,
            to: to + delta,
        },
        PathCommand::Close => PathCommand::Close,
    }
}

/// An independently placeable vector graphic of one shaped text span,
/// produced by [`Text::into_spans`]. Implements [`VectorComponent`] so
/// the span can be wrapped in any of the existing layout / decoration
/// containers (e.g. dropped into a [`VectorLayer`](crate::layer::VectorLayer))
/// and used for per-span effects.
///
/// The local coordinate space puts the span's leftmost glyph origin at
/// `x = 0` and the line baseline at `y = ascent`; the layout/view_box
/// size is `(span_width, line_height)`.
#[derive(Clone, PartialEq, Hash)]
pub struct TextSpanGraphic {
    paths: Vec<(Vec<PathCommand>, Paint)>,
    size: Vec2,
}

impl VectorComponent for TextSpanGraphic {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        constraints.constrain(self.size)
    }

    fn render(&self, size: Vec2) -> VectorGraphic {
        let children: Vec<Node> = self
            .paths
            .iter()
            .map(|(commands, fill)| {
                Node::Path(VPath {
                    commands: commands.clone(),
                    fill: Some(Fill { paint: fill.clone() }),
                    stroke: None,
                    transform: Transform::IDENTITY,
                })
            })
            .collect();
        VectorGraphic {
            view_box: Rect {
                origin: Vec2::ZERO,
                size,
            },
            root: Node::Group(Group {
                transform: Transform::IDENTITY,
                opacity: 1.0,
                children,
            }),
        }
    }
}

impl VectorComponent for Text {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        let (_paths, size) = self.shape_and_layout();
        constraints.constrain(size)
    }

    fn render(&self, size: Vec2) -> VectorGraphic {
        let (paths, _intrinsic) = self.shape_and_layout();
        let nodes: Vec<Node> = paths
            .into_iter()
            .map(|(commands, fill)| {
                Node::Path(VPath {
                    commands,
                    fill: Some(Fill { paint: fill }),
                    stroke: None,
                    transform: Transform::IDENTITY,
                })
            })
            .collect();
        VectorGraphic {
            view_box: Rect {
                origin: Vec2::ZERO,
                size,
            },
            root: Node::Group(Group {
                transform: Transform::IDENTITY,
                opacity: 1.0,
                children: nodes,
            }),
        }
    }
}

/// Adapter from `ttf_parser`'s Y-up font-unit space to our Y-down
/// logical space, used while pulling glyph outlines out of a face.
struct OutlinePathBuilder {
    commands: Vec<PathCommand>,
    scale: f32,
    origin_x: f32,
    origin_y: f32,
}

impl OutlinePathBuilder {
    fn map(&self, x: f32, y: f32) -> Vec2 {
        Vec2(
            self.origin_x + x * self.scale,
            self.origin_y - y * self.scale,
        )
    }
}

impl ttf_parser::OutlineBuilder for OutlinePathBuilder {
    fn move_to(&mut self, x: f32, y: f32) {
        self.commands.push(PathCommand::MoveTo(self.map(x, y)));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.commands.push(PathCommand::LineTo(self.map(x, y)));
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.commands.push(PathCommand::QuadTo {
            control: self.map(x1, y1),
            to: self.map(x, y),
        });
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.commands.push(PathCommand::CubicTo {
            c1: self.map(x1, y1),
            c2: self.map(x2, y2),
            to: self.map(x, y),
        });
    }
    fn close(&mut self) {
        self.commands.push(PathCommand::Close);
    }
}
