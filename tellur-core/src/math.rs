//! Inline LaTeX math as a [`Span`], backed by [RaTeX](https://github.com/erweixin/RaTeX).
//!
//! [`MathSpan`] typesets a LaTeX math fragment and emits it as the same
//! kind of baseline-relative vector paths a [`TextSpan`](crate::text::TextSpan)
//! produces, so a formula drops into a line of [`Text`](crate::text::Text)
//! beside ordinary words:
//!
//! ```ignore
//! Text::builder()
//!     .font(SERIF.clone())
//!     .size(48.0)
//!     .span("This is the line ")
//!     .span(MathSpan::builder().source(r"y = \frac{2}{3} x^2"))
//!     .span(".")
//! ```
//!
//! RaTeX lays the formula out into a flat display list of glyphs and rules
//! in **em** units with the baseline at `display_list.height`. We outline
//! each glyph from the embedded KaTeX fonts (via `ab_glyph`), turn rules
//! into rectangles, and re-origin everything so the formula's baseline is
//! at `y = 0` — matching the [`Span`] contract. The font-independent
//! geometry is memoized; the (possibly animating) fill is re-applied per
//! call, exactly as text shaping does.
//!
//! Module is available only with the `latex` feature.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::{Arc, LazyLock, Mutex};

use ab_glyph::{Font as _, FontRef, OutlineCurve};
use lru::LruCache;
use ratex_font::FontId;
use ratex_font_loader::outline_cache;
use ratex_layout::LayoutOptions;
use ratex_types::display_item::DisplayItem;
use ratex_types::MathStyle;

use crate::geometry::Vec2;
use crate::span::{ShapedSpan, Span, SpanContext};
use crate::text::TextSpan;
use crate::vector::{Paint, PathCommand};
use crate::Keyable;

/// A run of inline LaTeX math within a line of [`Text`](crate::text::Text).
///
/// `source` is the LaTeX body (math mode — no surrounding `$`), e.g.
/// `r"\frac{2}{3} x^2"`. `size` and `fill` inherit from the enclosing
/// `Text` when left unset. Pass it to the builder like any other span:
/// `Text::builder().span(MathSpan::builder().source(r"e^{i\pi}+1=0"))`.
#[derive(Clone, Keyable, bon::Builder)]
#[builder(derive(Into))]
pub struct MathSpan {
    /// The LaTeX math body, without `$` delimiters.
    #[builder(into)]
    pub source: String,
    /// Em size in logical pixels; inherits the base `Text` size if `None`.
    pub size: Option<f32>,
    /// Fill for the whole formula; inherits the base `Text` fill if `None`.
    #[builder(into)]
    pub fill: Option<Paint>,
}

impl MathSpan {
    /// A math span carrying only its source; size and fill inherit from
    /// the enclosing [`Text`](crate::text::Text).
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            size: None,
            fill: None,
        }
    }
}

impl Span for MathSpan {
    fn shape(&self, ctx: &SpanContext<'_>) -> ShapedSpan {
        let size = self.size.unwrap_or(ctx.size);
        let fill = self.fill.clone().unwrap_or_else(|| ctx.fill.clone());

        match math_geometry(&self.source, size) {
            Some(geo) => ShapedSpan {
                width: geo.width,
                ascent: geo.ascent,
                descent: geo.descent,
                paths: geo
                    .paths
                    .iter()
                    .map(|commands| (commands.clone(), fill.clone()))
                    .collect(),
            },
            // Invalid LaTeX (or a layout/font failure) falls back to
            // showing the raw source as plain text, so a typo is visible
            // rather than silently dropped.
            None => TextSpan::builder()
                .text(self.source.clone())
                .size(size)
                .fill(fill)
                .build()
                .shape(ctx),
        }
    }
}

impl From<MathSpan> for Box<dyn Span> {
    fn from(span: MathSpan) -> Self {
        Box::new(span)
    }
}

// Lets `Text::builder().span(MathSpan::builder()…)` accept a *complete*
// builder with no explicit `.build()`, matching `TextSpan`.
impl<S: math_span_builder::IsComplete> From<MathSpanBuilder<S>> for Box<dyn Span> {
    fn from(builder: MathSpanBuilder<S>) -> Self {
        Box::new(builder.build())
    }
}

/// Font-independent geometry of one typeset formula, in baseline-relative
/// coordinates (baseline at `y = 0`, y increasing downward). Paint is not
/// stored; the caller pairs each path with the current fill.
struct MathGeometry {
    /// One filled path per glyph / rule / delimiter.
    paths: Vec<Vec<PathCommand>>,
    width: f32,
    ascent: f32,
    descent: f32,
}

#[derive(PartialEq, Eq, Hash)]
struct MathKey {
    source: String,
    size_bits: u32,
}

const MATH_CACHE_CAPACITY: usize = 256;

// Geometry is fully determined by `(source, size)` and re-typesetting a
// formula every frame is expensive, so memoize it. `None` (a parse or
// layout failure) is cached too, so bad input is not reparsed each frame.
static MATH_CACHE: LazyLock<Mutex<LruCache<MathKey, Option<Arc<MathGeometry>>>>> =
    LazyLock::new(|| {
        Mutex::new(LruCache::new(
            NonZeroUsize::new(MATH_CACHE_CAPACITY).expect("cache capacity is non-zero"),
        ))
    });

fn math_geometry(source: &str, size: f32) -> Option<Arc<MathGeometry>> {
    let key = MathKey {
        source: source.to_owned(),
        size_bits: size.to_bits(),
    };
    if let Ok(mut cache) = MATH_CACHE.lock() {
        if let Some(hit) = cache.get(&key) {
            return hit.clone();
        }
    }
    let geo = compute_geometry(source, size).map(Arc::new);
    if let Ok(mut cache) = MATH_CACHE.lock() {
        cache.put(key, geo.clone());
    }
    geo
}

/// Parses, lays out, and outlines `source` into baseline-relative paths.
/// Returns `None` if the source does not parse.
fn compute_geometry(source: &str, size: f32) -> Option<MathGeometry> {
    let nodes = ratex_parser::parse(source).ok()?;
    let options = LayoutOptions {
        // Inline math uses text style (e.g. inline-size fractions), not
        // the larger display style.
        style: MathStyle::Text,
        ..Default::default()
    };
    let layout_box = ratex_layout::layout(&nodes, &options);
    let display_list = ratex_layout::to_display_list(&layout_box);

    let em = size;
    // RaTeX places the baseline at `height` (em) from the box top; shift
    // every item up by it so our output is baseline-relative.
    let baseline = display_list.height as f32;

    // Embedded KaTeX fonts needed by this formula. `font_dir` is ignored
    // because the `embed-fonts` feature supplies the bytes.
    let fonts = ratex_font_loader::load_fonts_for_items("", &display_list.items).ok()?;
    let mut font_refs: HashMap<FontId, FontRef<'_>> = HashMap::new();
    for (id, bytes) in fonts.iter() {
        if let Ok(font) = FontRef::try_from_slice(bytes) {
            font_refs.insert(*id, font);
        }
    }

    let mut paths: Vec<Vec<PathCommand>> = Vec::new();
    for item in &display_list.items {
        match item {
            DisplayItem::GlyphPath {
                x,
                y,
                scale,
                font,
                char_code,
                ..
            } => {
                let font_id = FontId::parse(font).unwrap_or(FontId::MainRegular);
                let Some(font_ref) = font_refs.get(&font_id) else {
                    continue;
                };
                let ch = ratex_font::katex_ttf_glyph_char(font_id, *char_code);
                let glyph_id = font_ref.glyph_id(ch);
                if glyph_id.0 == 0 {
                    continue;
                }
                let Some(curves) =
                    outline_cache::get_or_compute_outline(font_id, font_ref, glyph_id)
                else {
                    continue;
                };
                if curves.is_empty() {
                    continue;
                }
                let units_per_em = font_ref.units_per_em().unwrap_or(1000.0);
                let glyph_scale = (*scale as f32 * em) / units_per_em;
                let origin_x = *x as f32 * em;
                let origin_y = (*y as f32 - baseline) * em;
                let glyph = outline_to_path(&curves, origin_x, origin_y, glyph_scale);
                if !glyph.is_empty() {
                    paths.push(glyph);
                }
            }
            DisplayItem::Line {
                x,
                y,
                width,
                thickness,
                ..
            } => {
                // A rule (fraction bar, overline) centered on `y`.
                let t = (*thickness as f32 * em).max(0.5);
                let x0 = *x as f32 * em;
                let y0 = (*y as f32 - baseline) * em - t / 2.0;
                paths.push(rect_path(x0, y0, *width as f32 * em, t));
            }
            DisplayItem::Rect {
                x,
                y,
                width,
                height,
                ..
            } => {
                let x0 = *x as f32 * em;
                let y0 = (*y as f32 - baseline) * em;
                paths.push(rect_path(x0, y0, *width as f32 * em, *height as f32 * em));
            }
            DisplayItem::Path {
                x,
                y,
                commands,
                fill,
                ..
            } => {
                // v1 draws filled decorations (radicals, large delimiters,
                // stretchy arrows); the rarer stroked paths are skipped.
                if !*fill {
                    continue;
                }
                let origin_x = *x as f32 * em;
                let origin_y = (*y as f32 - baseline) * em;
                paths.push(ratex_path_to_path(commands, origin_x, origin_y, em));
            }
        }
    }

    Some(MathGeometry {
        paths,
        width: display_list.width as f32 * em,
        ascent: baseline * em,
        descent: display_list.depth as f32 * em,
    })
}

/// Converts `ab_glyph` outline curves (font units, y-up) into tellur path
/// commands placed at `(origin_x, origin_y)` and scaled by `scale`, with
/// y flipped into our y-down space. Contours are reconstructed by starting
/// a new sub-path whenever a curve does not continue from the previous
/// end, mirroring RaTeX's own renderers.
fn outline_to_path(
    curves: &[OutlineCurve],
    origin_x: f32,
    origin_y: f32,
    scale: f32,
) -> Vec<PathCommand> {
    let map = |p: ab_glyph::Point| Vec2(origin_x + p.x * scale, origin_y - p.y * scale);
    let mut commands = Vec::new();
    let mut last_end: Option<Vec2> = None;

    for curve in curves {
        let (start, end) = match curve {
            OutlineCurve::Line(p0, p1) => (map(*p0), map(*p1)),
            OutlineCurve::Quad(p0, _, p2) => (map(*p0), map(*p2)),
            OutlineCurve::Cubic(p0, _, _, p3) => (map(*p0), map(*p3)),
        };
        let need_move = match last_end {
            None => true,
            Some(le) => (le.0 - start.0).abs() > 0.01 || (le.1 - start.1).abs() > 0.01,
        };
        if need_move {
            if last_end.is_some() {
                commands.push(PathCommand::Close);
            }
            commands.push(PathCommand::MoveTo(start));
        }
        match curve {
            OutlineCurve::Line(_, p1) => commands.push(PathCommand::LineTo(map(*p1))),
            OutlineCurve::Quad(_, p1, p2) => commands.push(PathCommand::QuadTo {
                control: map(*p1),
                to: map(*p2),
            }),
            OutlineCurve::Cubic(_, p1, p2, p3) => commands.push(PathCommand::CubicTo {
                c1: map(*p1),
                c2: map(*p2),
                to: map(*p3),
            }),
        }
        last_end = Some(end);
    }

    if last_end.is_some() {
        commands.push(PathCommand::Close);
    }
    commands
}

/// An axis-aligned filled rectangle as a closed path.
fn rect_path(x: f32, y: f32, width: f32, height: f32) -> Vec<PathCommand> {
    vec![
        PathCommand::MoveTo(Vec2(x, y)),
        PathCommand::LineTo(Vec2(x + width, y)),
        PathCommand::LineTo(Vec2(x + width, y + height)),
        PathCommand::LineTo(Vec2(x, y + height)),
        PathCommand::Close,
    ]
}

/// Converts a RaTeX display-list path (em units, already y-down) into
/// tellur path commands at `(origin_x, origin_y)`, scaled by `em`.
fn ratex_path_to_path(
    commands: &[ratex_types::path_command::PathCommand],
    origin_x: f32,
    origin_y: f32,
    em: f32,
) -> Vec<PathCommand> {
    use ratex_types::path_command::PathCommand as R;
    let point = |x: f64, y: f64| Vec2(origin_x + x as f32 * em, origin_y + y as f32 * em);
    commands
        .iter()
        .map(|cmd| match cmd {
            R::MoveTo { x, y } => PathCommand::MoveTo(point(*x, *y)),
            R::LineTo { x, y } => PathCommand::LineTo(point(*x, *y)),
            R::QuadTo { x1, y1, x, y } => PathCommand::QuadTo {
                control: point(*x1, *y1),
                to: point(*x, *y),
            },
            R::CubicTo {
                x1,
                y1,
                x2,
                y2,
                x,
                y,
            } => PathCommand::CubicTo {
                c1: point(*x1, *y1),
                c2: point(*x2, *y2),
                to: point(*x, *y),
            },
            R::Close => PathCommand::Close,
        })
        .collect()
}
