//! Shared building blocks for the demo scene: the palette, the composable
//! `Fx` / `TrueCircle` vector components, easing/utility functions, and the
//! `Rect` / `Circle` / `Label` / `FxRect` / `FxOutlineRect` leaf components
//! that every section composes from.
//!
//! The leaf primitives are `#[component(vector)]`s: each builds a positioned,
//! styled shape and self-culls to an empty [`Fragment`] when it would be
//! invisible (zero size / alpha), so a caller can drop one into a builder
//! unconditionally. Because the components are PascalCase of the fn name
//! (`rect` → `Rect`, `circle` → `Circle`), the colliding core types are kept
//! path-qualified here: `shapes::Rectangle` / `shapes::Circle` and the
//! `Aabb` alias for `geometry::Rect`.

use tellur_core::color::Color;
use tellur_core::component;
use tellur_core::fragment::Fragment;
use tellur_core::geometry::{Anchor, Vec2};
use tellur_core::shapes;
use tellur_core::text::{Text, TextSpan, Weight, MONOSPACE};

pub use tellur_core::builder::{VectorBuilderPlacement, VectorBuilderTransform};
pub use tellur_core::easing::{Easing, PhaseEasing};
pub use tellur_core::geometry::Transform;
pub use tellur_core::placement::VectorPlacement;
pub use tellur_core::shapes::Rectangle;
pub use tellur_core::vector::{Stroke, VectorTransform};

pub const DURATION: f32 = 7.6;
pub const SCENE_SIZE: Vec2 = Vec2(1920.0, 1080.0);
pub const CX: f32 = 960.0;
pub const CY: f32 = 540.0;

// Restrained palette: a deep ink bg, a warm paper for the scaffolding /
// typography, and two saturated accents (a hot pink and an electric cyan).
// Holding to three foreground tones gives the piece a deliberate,
// design-system feel instead of a confetti palette.
//
// `PartialEq + Hash` so structs holding a `Palette` (like `Hud`) compose
// into a `CachingRenderContext`-friendly key without manual plumbing.
#[derive(Clone, Copy, PartialEq, Hash)]
pub struct Palette {
    pub bg: Color,
    pub paper: Color,
    pub pink: Color,
    pub cyan: Color,
}

// Rise-fall hat envelope `4x(1-x)`: peaks at 1 when value is 0.5, returns to
// 0 at both endpoints. Used by the transition wipes (OVERTURE→FIELD,
// FIELD→SCAN, SCAN→RESOLVE) so the sweep stripe is brightest mid-screen.
// Expects `s ∈ [0, 1]`; callers feed an already-eased sweep factor.
pub fn peak(s: f32) -> f32 {
    4.0 * s * (1.0 - s)
}

// --- leaf components ---

/// A filled and/or stroked circle, centered on `center`. The stroke is
/// flattened into `stroke` (color) + `stroke_width` so every field is
/// hashable. Renders nothing when there is neither a visible fill nor a
/// visible stroke.
#[component(vector)]
pub fn Circle(
    center: Vec2,
    radius: f32,
    #[builder(into)] fill: Option<Color>,
    #[builder(into)] stroke: Option<Color>,
    #[builder(default = 1.0)] stroke_width: f32,
) -> impl VectorComponent {
    if radius <= 0.0 {
        return Fragment::empty();
    }
    let fill = fill.filter(|c| c.a > 0.0);
    let stroke = stroke.filter(|c| c.a > 0.0 && stroke_width > 0.0);
    if fill.is_none() && stroke.is_none() {
        return Fragment::empty();
    }
    Fragment::single(
        shapes::Circle::builder()
            .radius(radius)
            .maybe_fill(fill)
            .maybe_stroke(stroke.map(|c| Stroke {
                paint: c.into(),
                width: stroke_width,
            }))
            .anchored(Anchor::CENTER)
            .snap_to(center),
    )
}

/// Anchor-and-position text using the system monospace face for that
/// "instrument readout" feel. Renders nothing for empty text or zero alpha.
#[component(vector)]
pub fn Label(
    position: Vec2,
    anchor: Anchor,
    #[builder(into)] text: String,
    size: f32,
    color: Color,
    #[builder(default)] weight: Weight,
) -> impl VectorComponent {
    if color.a <= 0.0 || text.is_empty() {
        return Fragment::empty();
    }
    Fragment::single(
        Text::builder()
            .font(MONOSPACE.clone())
            .size(size)
            .weight(weight)
            .fill(color)
            .span(TextSpan::plain(text))
            .anchored(anchor)
            .snap_to(position),
    )
}
