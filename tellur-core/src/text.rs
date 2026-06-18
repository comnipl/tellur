//! Text rendering as a vector component.
//!
//! [`Text`] shapes styled text with rustybuzz, lays the resulting glyph
//! runs out left-to-right, and emits one filled
//! [`Path`](crate::vector::Path) per glyph through the existing
//! [`VectorGraphic`] pipeline. OpenType `palt` is enabled during
//! shaping, so Japanese proportional alternate widths are honored when
//! the font provides them. The base `Text { font, size, fill }` provides
//! defaults that each `TextSpan` may override on a per-field basis: a
//! `Some(_)` value on a span replaces the base; `None` inherits it.
//! Coloring a substring red is therefore just "insert a `TextSpan` whose
//! `fill: Some(Paint::Solid(red))` between plain spans". A span can also
//! apply independent X/Y scale to its glyph outlines for slightly wide or
//! tall lettering while still participating in the same line layout.
//! Adjacent `TextSpan`s with the same resolved shaping style are shaped
//! as one run, so splitting a string for styling does not change
//! kerning, ligatures, or other cross-boundary glyph positioning.
//!
//! Single-line only for now: `\n` is not interpreted; multi-line layout
//! and line breaking are deferred to a follow-up.

use std::hash::{Hash, Hasher};
use std::num::NonZeroUsize;
use std::path::Path as FsPath;
use std::sync::{Arc, LazyLock, Mutex};

use lru::LruCache;
use rustybuzz::{ttf_parser, UnicodeBuffer};
use thiserror::Error;

use crate::geometry::{Constraints, Rect, Transform, Vec2};
use crate::placement::{Positioned, VectorPlacement};
use crate::span::{ShapedSpan, Span, SpanContext};
use crate::vector::{
    Fill, Group, Node, Paint, Path as VPath, PathCommand, VectorComponent, VectorGraphic,
};
use crate::Keyable;

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

/// Number of distinct shaped runs cached per font. Bounds memory for
/// long-running sessions whose text varies continuously (e.g. a live
/// numeric readout) while comfortably covering any realistic set of
/// static labels.
const SHAPE_CACHE_CAPACITY: usize = 512;

fn default_shaping_features() -> [rustybuzz::Feature; 1] {
    [rustybuzz::Feature::new(
        ttf_parser::Tag::from_bytes(b"palt"),
        1,
        ..,
    )]
}

/// Cache key for a shaped run: everything that determines the produced
/// glyph geometry except the font itself (the cache is per-font). The
/// fill/paint is deliberately excluded — it is cheap to re-apply and
/// often animates frame to frame, so keying on it would defeat the cache.
#[derive(PartialEq, Eq, Hash)]
struct ShapeKey {
    weight: u16,
    size_bits: u32,
    baseline_bits: u32,
    text: String,
}

/// One glyph slot inside a shaped run. Whitespace glyphs have no outline but
/// still carry their cluster and advance so span boundaries can be recovered.
struct ShapedGlyph {
    /// UTF-8 byte offset in the shaped input that produced this glyph.
    cluster: usize,
    /// Pen position immediately after this glyph's advance.
    advance_end: f32,
    /// The visible outline, empty for whitespace / empty-outline glyphs.
    commands: Vec<PathCommand>,
}

/// Memoized geometry of one shaped run, in run-local coordinates (the first
/// glyph origin sits near `x = 0` and the baseline is baked in). Paint is not
/// stored; the caller pairs each glyph with the current fill.
struct ShapedGlyphs {
    /// One entry per shaped glyph, including whitespace.
    glyphs: Vec<ShapedGlyph>,
    /// Total advance of the run.
    width: f32,
}

/// An owned font, cheaply shareable via `Arc<Font>` across components.
///
/// The byte buffer is reference-counted. Shaping a run — building a
/// `rustybuzz::Face`, shaping with rustybuzz, and outlining each glyph —
/// is comparatively expensive and fully determined by
/// `(weight, size, baseline, text)`, so the shaped-run result is memoized
/// in `shape_cache`: a label whose content is stable across frames shapes
/// once and is reused thereafter, even while its fill color animates (the
/// cache stores geometry only; the caller re-attaches the paint). The
/// cache lives on the font so its lifetime is bound to the backing bytes
/// and shared exactly along the `Arc<Font>` graph.
pub struct Font {
    data: Arc<Vec<u8>>,
    face_index: u32,
    shape_cache: Mutex<LruCache<ShapeKey, Arc<ShapedGlyphs>>>,
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
        Ok(Self {
            data,
            face_index,
            shape_cache: Mutex::new(LruCache::new(
                NonZeroUsize::new(SHAPE_CACHE_CAPACITY).expect("cache capacity is non-zero"),
            )),
        })
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

    /// This font's `(ascent, descent)` at `size`, both as non-negative
    /// distances from the baseline (ascent above, descent below). Used by
    /// a [`TextSpan`] to report how far it paints around the line baseline
    /// so the enclosing [`Text`] can size the line box to fit every span.
    fn vertical_metrics(&self, size: f32) -> (f32, f32) {
        let face = self.face();
        let upem = face.units_per_em() as f32;
        let scale = size / upem;
        let ascent = face.ascender() as f32 * scale;
        // `descender` is conventionally negative (below baseline); flip it
        // to a non-negative depth.
        let descent = -(face.descender() as f32) * scale;
        (ascent.max(0.0), descent.max(0.0))
    }

    /// Returns the memoized run-local geometry for `text` at the given
    /// style, computing and caching it on a miss. Keyed on
    /// `(weight, size, baseline, text)`; the result carries no paint, so
    /// callers re-attach the (possibly per-frame) fill themselves.
    fn shaped_glyphs(
        &self,
        weight: Weight,
        size: f32,
        baseline_y: f32,
        text: &str,
    ) -> Arc<ShapedGlyphs> {
        let key = ShapeKey {
            weight: weight.0,
            size_bits: size.to_bits(),
            baseline_bits: baseline_y.to_bits(),
            text: text.to_owned(),
        };
        if let Ok(mut cache) = self.shape_cache.lock() {
            if let Some(hit) = cache.get(&key) {
                return Arc::clone(hit);
            }
        }
        let shaped = Arc::new(self.shape_uncached(weight, size, baseline_y, text));
        if let Ok(mut cache) = self.shape_cache.lock() {
            cache.put(key, Arc::clone(&shaped));
        }
        shaped
    }

    /// The uncached path behind [`Font::shaped_glyphs`]: builds a fresh
    /// `rustybuzz::Face`, shapes `text`, and outlines every glyph into
    /// run-local path commands.
    fn shape_uncached(
        &self,
        weight: Weight,
        size: f32,
        baseline_y: f32,
        text: &str,
    ) -> ShapedGlyphs {
        let mut face = self.face();
        // Apply the OpenType `wght` axis. No effect on fonts without a
        // `wght` axis; the call returns `None` and we just keep going.
        face.set_variations(&[rustybuzz::Variation {
            tag: ttf_parser::Tag::from_bytes(b"wght"),
            value: weight.0 as f32,
        }]);
        let upem = face.units_per_em() as f32;
        let scale = size / upem;

        let mut buffer = UnicodeBuffer::new();
        buffer.push_str(text);
        let features = default_shaping_features();
        let glyph_buffer = rustybuzz::shape(&face, &features, buffer);

        let mut glyphs = Vec::new();
        let mut pen_x: f32 = 0.0;
        for (info, pos) in glyph_buffer
            .glyph_infos()
            .iter()
            .zip(glyph_buffer.glyph_positions().iter())
        {
            let glyph_id = ttf_parser::GlyphId(info.glyph_id as u16);
            let x_off = pos.x_offset as f32 * scale;
            let y_off = pos.y_offset as f32 * scale;
            // Span starts at x = 0 in its own space.
            let glyph_x = pen_x + x_off;
            // Font Y points up; flipping by subtracting `y_off` from the
            // Y-down baseline puts the glyph in our space.
            let glyph_y = baseline_y - y_off;

            let mut builder = OutlinePathBuilder {
                commands: Vec::new(),
                scale,
                origin_x: glyph_x,
                origin_y: glyph_y,
            };
            face.outline_glyph(glyph_id, &mut builder);

            pen_x += pos.x_advance as f32 * scale;
            // y_advance is typically 0 for horizontal text.
            glyphs.push(ShapedGlyph {
                cluster: info.cluster as usize,
                advance_end: pen_x,
                commands: builder.commands,
            });
        }

        ShapedGlyphs {
            glyphs,
            width: pen_x,
        }
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

// Pointer-identity equality is reflexive, so `Font` is soundly `Eq`. The
// explicit marker lets types that contain a `Font` (e.g. `Text`, `TextSpan`)
// rest their own `Eq` on a compiler-checked guarantee rather than an
// assumption.
impl Eq for Font {}

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
#[derive(Default, Clone, Keyable, bon::Builder)]
#[builder(derive(Into))]
pub struct TextSpan {
    #[builder(into)]
    pub text: String,
    #[builder(into)]
    pub fill: Option<Paint>,
    pub font: Option<Arc<Font>>,
    pub size: Option<f32>,
    pub weight: Option<Weight>,
    /// Horizontal multiplier for the span's glyph outlines and advance.
    /// `None` inherits the normal 1:1 text shape.
    pub scale_x: Option<f32>,
    /// Vertical multiplier for the span's glyph outlines and line metrics,
    /// applied around the baseline. `None` inherits the normal 1:1 text shape.
    pub scale_y: Option<f32>,
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

// Lets `Text::builder().span("…")` accept a bare string as a plain span.
impl From<&str> for TextSpan {
    fn from(text: &str) -> Self {
        Self::plain(text)
    }
}

impl From<String> for TextSpan {
    fn from(text: String) -> Self {
        Self::plain(text)
    }
}

impl Span for TextSpan {
    fn shape(&self, ctx: &SpanContext<'_>) -> ShapedSpan {
        let font = self.font.as_ref().unwrap_or(ctx.font);
        let size = self.size.unwrap_or(ctx.size);
        let weight = self.weight.unwrap_or(ctx.weight);
        let fill = self.fill.clone().unwrap_or_else(|| ctx.fill.clone());
        let scale_x = self.scale_x.unwrap_or(1.0);
        let scale_y = self.scale_y.unwrap_or(1.0);

        if self.text.is_empty() {
            return ShapedSpan {
                width: 0.0,
                ascent: 0.0,
                descent: 0.0,
                paths: Vec::new(),
            };
        }

        // Shape with the baseline at `y = 0`, so the geometry is
        // baseline-relative (ink above the baseline lands at negative y).
        // The shaping is memoized on the font; only the (possibly
        // animating) fill is re-attached per call.
        let shaped = font.shaped_glyphs(weight, size, 0.0, &self.text);
        let paths = shaped
            .glyphs
            .iter()
            .filter(|glyph| !glyph.commands.is_empty())
            .map(|glyph| {
                let commands = if scale_x == 1.0 && scale_y == 1.0 {
                    glyph.commands.clone()
                } else {
                    glyph
                        .commands
                        .iter()
                        .copied()
                        .map(|c| scale_command(c, Vec2(scale_x, scale_y)))
                        .collect()
                };
                (commands, fill.clone())
            })
            .collect();
        let (ascent, descent) = font.vertical_metrics(size);
        ShapedSpan {
            width: shaped.width * scale_x,
            ascent: ascent * scale_y,
            descent: descent * scale_y,
            paths,
        }
    }
}

impl From<TextSpan> for Box<dyn Span> {
    fn from(span: TextSpan) -> Self {
        Box::new(span)
    }
}

impl From<&str> for Box<dyn Span> {
    fn from(text: &str) -> Self {
        Box::new(TextSpan::plain(text))
    }
}

impl From<String> for Box<dyn Span> {
    fn from(text: String) -> Self {
        Box::new(TextSpan::plain(text))
    }
}

// Lets `Text::builder().span(TextSpan::builder()…)` accept a *complete*
// builder with no explicit `.build()`, mirroring the buildless children
// the `#[component]` macro emits for boxed components.
impl<S: text_span_builder::IsComplete> From<TextSpanBuilder<S>> for Box<dyn Span> {
    fn from(builder: TextSpanBuilder<S>) -> Self {
        Box::new(builder.build())
    }
}

/// A single line of styled text.
///
/// `font`, `size`, `weight`, and `fill` are the defaults used by every
/// `TextSpan` that does not override them; `spans` carries the actual
/// content and any per-region styling.
#[crate::component(vector)]
#[derive(Clone, Keyable)]
pub struct Text {
    #[children(each = span)]
    pub spans: Vec<Box<dyn Span>>,
    pub font: Arc<Font>,
    pub size: f32,
    #[builder(default)]
    pub weight: Weight,
    #[builder(into)]
    pub fill: Paint,
}

#[derive(Clone)]
struct ResolvedTextRunStyle {
    font: Arc<Font>,
    size: f32,
    weight: Weight,
    scale_x: f32,
    scale_y: f32,
}

impl ResolvedTextRunStyle {
    fn from_span(span: &TextSpan, ctx: &SpanContext<'_>) -> Self {
        Self {
            font: span.font.clone().unwrap_or_else(|| ctx.font.clone()),
            size: span.size.unwrap_or(ctx.size),
            weight: span.weight.unwrap_or(ctx.weight),
            scale_x: span.scale_x.unwrap_or(1.0),
            scale_y: span.scale_y.unwrap_or(1.0),
        }
    }

    fn matches(&self, other: &Self) -> bool {
        self.font == other.font
            && self.size.to_bits() == other.size.to_bits()
            && self.weight == other.weight
            && self.scale_x.to_bits() == other.scale_x.to_bits()
            && self.scale_y.to_bits() == other.scale_y.to_bits()
    }
}

struct TextRunPart {
    start: usize,
    end: usize,
    fill: Paint,
}

fn boundary_advance(shaped: &ShapedGlyphs, byte_offset: usize) -> f32 {
    shaped
        .glyphs
        .iter()
        .take_while(|glyph| glyph.cluster < byte_offset)
        .last()
        .map_or(0.0, |glyph| glyph.advance_end)
}

fn shape_text_run(
    style: &ResolvedTextRunStyle,
    text: &str,
    parts: &[TextRunPart],
) -> Vec<ShapedSpan> {
    let shaped = style
        .font
        .shaped_glyphs(style.weight, style.size, 0.0, text);
    let (ascent, descent) = style.font.vertical_metrics(style.size);
    let ascent = ascent * style.scale_y;
    let descent = descent * style.scale_y;
    let scale = Vec2(style.scale_x, style.scale_y);

    parts
        .iter()
        .map(|part| {
            if part.start == part.end {
                return ShapedSpan {
                    width: 0.0,
                    ascent: 0.0,
                    descent: 0.0,
                    paths: Vec::new(),
                };
            }

            let start_x = boundary_advance(&shaped, part.start) * style.scale_x;
            let end_x = boundary_advance(&shaped, part.end) * style.scale_x;
            let paths = shaped
                .glyphs
                .iter()
                .filter(|glyph| {
                    !glyph.commands.is_empty()
                        && part.start <= glyph.cluster
                        && glyph.cluster < part.end
                })
                .map(|glyph| {
                    let commands = if style.scale_x == 1.0 && style.scale_y == 1.0 {
                        glyph.commands.clone()
                    } else {
                        glyph
                            .commands
                            .iter()
                            .copied()
                            .map(|c| scale_command(c, scale))
                            .collect()
                    };
                    let commands = commands
                        .into_iter()
                        .map(|c| translate_command(c, Vec2(-start_x, 0.0)))
                        .collect();
                    (commands, part.fill.clone())
                })
                .collect();

            ShapedSpan {
                width: end_x - start_x,
                ascent,
                descent,
                paths,
            }
        })
        .collect()
}

impl Text {
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

    /// Shapes every input span independently and lays them out
    /// left-to-right. Returns each span's start-x paired with its
    /// [`ShapedSpan`] (paths still baseline-relative), the line baseline
    /// `y`, and the line's intrinsic `(width, height)`.
    ///
    /// This is used by [`Text::into_spans`], where the exact one-output-
    /// per-input-span contract is more important than cross-boundary
    /// kerning.
    ///
    /// The line box grows to fit the tallest span: its baseline sits at
    /// `max(base ascent, span ascents)` and it extends down to
    /// `max(base depth, span descents)`, so a span that overrides
    /// `font`/`size` — or a [`MathSpan`](crate::math::MathSpan) taller
    /// than the surrounding text — is enclosed rather than clipped.
    fn shape_line_preserving_spans(&self) -> (Vec<(f32, ShapedSpan)>, f32, Vec2) {
        let ctx = SpanContext {
            font: &self.font,
            size: self.size,
            weight: self.weight,
            fill: &self.fill,
        };
        let (base_ascent, base_line_height) = self.line_metrics();
        let base_below = (base_line_height - base_ascent).max(0.0);

        let mut placed: Vec<(f32, ShapedSpan)> = Vec::with_capacity(self.spans.len());
        let mut pen_x: f32 = 0.0;
        let mut line_ascent = base_ascent;
        let mut line_below = base_below;

        for span in &self.spans {
            let shaped = span.shape(&ctx);
            line_ascent = line_ascent.max(shaped.ascent);
            line_below = line_below.max(shaped.descent);
            let start_x = pen_x;
            pen_x += shaped.width;
            placed.push((start_x, shaped));
        }

        let size = Vec2(pen_x, line_ascent + line_below);
        (placed, line_ascent, size)
    }

    /// Shapes the line for normal rendering. Adjacent built-in spans with
    /// matching shaping style are coalesced first, so splitting a
    /// `TextSpan` or compatible `MathSpan` does not introduce artificial
    /// advance at the boundary.
    fn shape_line(&self) -> (Vec<(f32, ShapedSpan)>, f32, Vec2) {
        let ctx = SpanContext {
            font: &self.font,
            size: self.size,
            weight: self.weight,
            fill: &self.fill,
        };
        let (base_ascent, base_line_height) = self.line_metrics();
        let base_below = (base_line_height - base_ascent).max(0.0);

        let mut placed: Vec<(f32, ShapedSpan)> = Vec::with_capacity(self.spans.len());
        let mut pen_x: f32 = 0.0;
        let mut line_ascent = base_ascent;
        let mut line_below = base_below;
        let mut i = 0;

        while i < self.spans.len() {
            if let Some(text_span) = self.spans[i].as_ref().as_any().downcast_ref::<TextSpan>() {
                let style = ResolvedTextRunStyle::from_span(text_span, &ctx);
                let mut text = String::new();
                let mut parts = Vec::new();
                let mut j = i;

                while j < self.spans.len() {
                    let Some(next_span) =
                        self.spans[j].as_ref().as_any().downcast_ref::<TextSpan>()
                    else {
                        break;
                    };
                    let next_style = ResolvedTextRunStyle::from_span(next_span, &ctx);
                    if !style.matches(&next_style) {
                        break;
                    }

                    let start = text.len();
                    text.push_str(&next_span.text);
                    let end = text.len();
                    parts.push(TextRunPart {
                        start,
                        end,
                        fill: next_span.fill.clone().unwrap_or_else(|| ctx.fill.clone()),
                    });
                    j += 1;
                }

                for shaped in shape_text_run(&style, &text, &parts) {
                    line_ascent = line_ascent.max(shaped.ascent);
                    line_below = line_below.max(shaped.descent);
                    let start_x = pen_x;
                    pen_x += shaped.width;
                    placed.push((start_x, shaped));
                }
                i = j;
                continue;
            }

            #[cfg(feature = "latex")]
            if let Some(math_span) = self.spans[i]
                .as_ref()
                .as_any()
                .downcast_ref::<crate::math::MathSpan>()
            {
                let size = math_span.size.unwrap_or(ctx.size);
                let fill = math_span.fill.clone().unwrap_or_else(|| ctx.fill.clone());
                let mut source = String::new();
                let mut j = i;

                while j < self.spans.len() {
                    let Some(next_span) = self.spans[j]
                        .as_ref()
                        .as_any()
                        .downcast_ref::<crate::math::MathSpan>()
                    else {
                        break;
                    };
                    let next_size = next_span.size.unwrap_or(ctx.size);
                    let next_fill = next_span.fill.clone().unwrap_or_else(|| ctx.fill.clone());
                    if size.to_bits() != next_size.to_bits() || fill != next_fill {
                        break;
                    }
                    source.push_str(&next_span.source);
                    j += 1;
                }

                let shaped = crate::math::MathSpan {
                    source,
                    size: Some(size),
                    fill: Some(fill),
                }
                .shape(&ctx);
                line_ascent = line_ascent.max(shaped.ascent);
                line_below = line_below.max(shaped.descent);
                let start_x = pen_x;
                pen_x += shaped.width;
                placed.push((start_x, shaped));
                i = j;
                continue;
            }

            let shaped = self.spans[i].shape(&ctx);
            line_ascent = line_ascent.max(shaped.ascent);
            line_below = line_below.max(shaped.descent);
            let start_x = pen_x;
            pen_x += shaped.width;
            placed.push((start_x, shaped));
            i += 1;
        }

        let size = Vec2(pen_x, line_ascent + line_below);
        (placed, line_ascent, size)
    }

    /// Shapes the line and returns `(glyph paths, intrinsic size)`, with
    /// paths in the line's global coordinates (all runs concatenated
    /// left-to-right and dropped onto the baseline).
    fn shape_and_layout(&self) -> (Vec<(Vec<PathCommand>, Paint)>, Vec2) {
        let (placed, baseline_y, size) = self.shape_line();

        let mut all_paths: Vec<(Vec<PathCommand>, Paint)> = Vec::new();
        for (start_x, shaped) in placed {
            let delta = Vec2(start_x, baseline_y);
            for (commands, fill) in shaped.paths {
                let shifted: Vec<PathCommand> = commands
                    .into_iter()
                    .map(|c| translate_command(c, delta))
                    .collect();
                all_paths.push((shifted, fill));
            }
        }

        (all_paths, size)
    }

    /// Decompose the text into per-span graphics, each placed at the
    /// position where its first glyph would land in the line. The
    /// returned `Vec` has exactly one entry per input span (in the same
    /// order), and entries for empty spans are zero-width placeholders so
    /// positional indexing matches.
    ///
    /// Because this preserves one output per input span, it intentionally
    /// shapes those spans independently instead of coalescing adjacent
    /// compatible spans.
    ///
    /// Useful for attaching per-span effects (transforms, drop shadows
    /// on the rasterized form, outlines, ...) by composing each
    /// [`Positioned`] span back into a layer:
    ///
    /// ```ignore
    /// let layer = Text::builder()...
    ///     .into_spans()
    ///     .into_iter()
    ///     .fold(VectorLayer::builder().size(...), |layer, span| layer.child(span))
    ///     .build();
    /// ```
    pub fn into_spans(self) -> Vec<Positioned> {
        let (placed, baseline_y, size) = self.shape_line_preserving_spans();
        let line_height = size.1;
        placed
            .into_iter()
            .map(|(start_x, shaped)| {
                // Drop the span's baseline-relative paths onto the line
                // baseline within the span's own box.
                let paths = shaped
                    .paths
                    .into_iter()
                    .map(|(commands, fill)| {
                        let shifted: Vec<PathCommand> = commands
                            .into_iter()
                            .map(|c| translate_command(c, Vec2(0.0, baseline_y)))
                            .collect();
                        (shifted, fill)
                    })
                    .collect();
                TextSpanGraphic {
                    paths,
                    size: Vec2(shaped.width, line_height),
                }
                .place_at(Vec2(start_x, 0.0))
            })
            .collect()
    }
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

/// Scales every coordinate in a path command around the span origin. Text
/// spans are shaped with the baseline at `y = 0`, so this preserves the
/// baseline while making glyphs wider/narrower or taller/shorter.
fn scale_command(cmd: PathCommand, scale: Vec2) -> PathCommand {
    let transform = Transform::scale(scale);
    match cmd {
        PathCommand::MoveTo(p) => PathCommand::MoveTo(transform.transform_point(p)),
        PathCommand::LineTo(p) => PathCommand::LineTo(transform.transform_point(p)),
        PathCommand::QuadTo { control, to } => PathCommand::QuadTo {
            control: transform.transform_point(control),
            to: transform.transform_point(to),
        },
        PathCommand::CubicTo { c1, c2, to } => PathCommand::CubicTo {
            c1: transform.transform_point(c1),
            c2: transform.transform_point(c2),
            to: transform.transform_point(to),
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
            .filter(|(_, fill)| fill.is_visible())
            .map(|(commands, fill)| {
                Node::Path(VPath {
                    commands: commands.clone(),
                    fill: Some(Fill {
                        paint: fill.clone(),
                    }),
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
            .filter(|(_, fill)| fill.is_visible())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_shaping_enables_proportional_alternates() {
        let features = default_shaping_features();

        assert_eq!(features[0].tag, ttf_parser::Tag::from_bytes(b"palt"));
        assert_eq!(features[0].value, 1);
        assert_eq!(features[0].start, 0);
        assert_eq!(features[0].end, u32::MAX);
    }

    #[test]
    fn text_span_scales_glyphs_and_metrics() {
        let font = SANS_SERIF.clone();
        let fill = Paint::Solid(crate::color::Color::rgb_u8(20, 20, 20));
        let ctx = SpanContext {
            font: &font,
            size: 48.0,
            weight: Weight::NORMAL,
            fill: &fill,
        };

        let normal = TextSpan::plain("Scale").shape(&ctx);
        let scaled = TextSpan::builder()
            .text("Scale")
            .scale_x(1.4)
            .scale_y(1.25)
            .build()
            .shape(&ctx);

        assert!(normal.width > 0.0);
        assert_close(scaled.width, normal.width * 1.4);
        assert_close(scaled.ascent, normal.ascent * 1.25);
        assert_close(scaled.descent, normal.descent * 1.25);

        let (normal_x, normal_y) = first_path_point(&normal);
        let (scaled_x, scaled_y) = first_path_point(&scaled);
        assert_close(scaled_x, normal_x * 1.4);
        assert_close(scaled_y, normal_y * 1.25);
    }

    #[test]
    fn adjacent_text_spans_shape_as_one_run() {
        let fill = Paint::Solid(crate::color::Color::rgb_u8(20, 20, 20));
        let whole = Text::builder()
            .font(SERIF.clone())
            .size(72.0)
            .fill(fill.clone())
            .span("AVAV")
            .build();
        let split = Text::builder()
            .font(SERIF.clone())
            .size(72.0)
            .fill(fill)
            .span("A")
            .span("V")
            .span("A")
            .span("V")
            .build();

        let (whole_paths, whole_size) = whole.shape_and_layout();
        let (split_paths, split_size) = split.shape_and_layout();

        assert_eq!(split_size, whole_size);
        assert_eq!(split_paths, whole_paths);
    }

    #[test]
    fn differently_colored_text_spans_keep_combined_run_advance() {
        let black = Paint::Solid(crate::color::Color::rgb_u8(20, 20, 20));
        let red = Paint::Solid(crate::color::Color::rgb_u8(220, 60, 60));
        let whole = Text::builder()
            .font(SERIF.clone())
            .size(72.0)
            .fill(black.clone())
            .span("AVAV")
            .build();
        let split = Text::builder()
            .font(SERIF.clone())
            .size(72.0)
            .fill(black)
            .span("A")
            .span(TextSpan::builder().text("V").fill(red.clone()))
            .span("A")
            .span(TextSpan::builder().text("V").fill(red))
            .build();

        let (_whole_paths, whole_size) = whole.shape_and_layout();
        let (_split_paths, split_size) = split.shape_and_layout();

        assert_eq!(split_size, whole_size);
    }

    #[cfg(feature = "latex")]
    #[test]
    fn adjacent_math_spans_shape_as_one_formula_when_style_matches() {
        let fill = Paint::Solid(crate::color::Color::rgb_u8(20, 20, 20));
        let whole = Text::builder()
            .font(SERIF.clone())
            .size(72.0)
            .fill(fill.clone())
            .span(crate::math::MathSpan::new(r"x^2+y"))
            .build();
        let split = Text::builder()
            .font(SERIF.clone())
            .size(72.0)
            .fill(fill)
            .span(crate::math::MathSpan::new(r"x^2"))
            .span(crate::math::MathSpan::new(r"+y"))
            .build();

        let (_whole_paths, whole_size) = whole.shape_and_layout();
        let (_split_paths, split_size) = split.shape_and_layout();

        assert_eq!(split_size, whole_size);
    }

    fn first_path_point(span: &ShapedSpan) -> (f32, f32) {
        for (commands, _) in &span.paths {
            for command in commands {
                if let PathCommand::MoveTo(point)
                | PathCommand::LineTo(point)
                | PathCommand::QuadTo { to: point, .. }
                | PathCommand::CubicTo { to: point, .. } = command
                {
                    return (point.0, point.1);
                }
            }
        }
        panic!("span should contain at least one path point");
    }

    fn assert_close(actual: f32, expected: f32) {
        let tolerance = 0.001;
        assert!(
            (actual - expected).abs() <= tolerance,
            "expected {actual} to be within {tolerance} of {expected}"
        );
    }
}
