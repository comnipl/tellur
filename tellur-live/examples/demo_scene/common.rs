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

use tellur_core::builder::VectorBuilderPlacement;
use tellur_core::color::Color;
use tellur_core::component;
use tellur_core::fragment::Fragment;
use tellur_core::geometry::{Anchor, Constraints, Rect as Aabb, Transform, Vec2};
use tellur_core::placement::VectorPlacement;
use tellur_core::shapes;
use tellur_core::text::{Text, TextSpan, Weight, MONOSPACE};
use tellur_core::time::{LocalTime, Time};
use tellur_core::vector::{Stroke, VectorComponent, VectorGraphic, VectorTransform};
use tellur_core::Keyable;

pub use tellur_core::easing::PhaseEasing;

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

pub fn lerp(from: f32, to: f32, p: f32) -> f32 {
    from + (to - from) * p
}

// Sine oscillation in ±1 with a fractional cycle `offset` to decorrelate
// siblings. `Time::wave` is the cosine-start 0..1 form, so shift by the
// offset plus a quarter period to recover `sin` and widen to ±1.
pub fn wave<T: Time>(time: T, period: f32, offset: f32) -> f32 {
    LocalTime::new(time.seconds() + (offset + 0.25) * period)
        .wave(period)
        .linear(-1.0, 1.0)
}

// Rise-fall hat envelope `4x(1-x)`: peaks at 1 when value is 0.5, returns to
// 0 at both endpoints. Used by the transition wipes (OVERTURE→FIELD,
// FIELD→SCAN, SCAN→RESOLVE) so the sweep stripe is brightest mid-screen.
// Expects `s ∈ [0, 1]`; callers feed an already-eased sweep factor.
pub fn peak(s: f32) -> f32 {
    4.0 * s * (1.0 - s)
}

fn center_transform(size: Vec2, angle: f32, scale: Vec2) -> Transform {
    let transform = Transform::scale(Vec2(scale.0.max(0.0001), scale.1.max(0.0001)))
        .then(Transform::rotate(angle));
    Transform::around_point(Vec2(size.0 * 0.5, size.1 * 0.5), transform)
}

// --- leaf components ---

/// A solid-filled rectangle placed with its top-left at `position`. Renders
/// nothing (an empty [`Fragment`]) when it would be invisible.
#[component(vector)]
pub fn Rect(position: Vec2, size: Vec2, color: Color) -> impl VectorComponent {
    if size.0 <= 0.0 || size.1 <= 0.0 || color.a <= 0.0 {
        return Fragment::empty();
    }
    Fragment::single(
        shapes::Rectangle::builder()
            .size(size)
            .fill(color)
            .place_at(position),
    )
}

// `shapes::Circle::layout` clamps its bounding box to the parent's
// `Constraints`, so any circle whose diameter exceeds the scene's shorter side
// (1080) gets axis-squashed into an ellipse. `TrueCircle` overrides `layout` to
// always return the intrinsic `2 * radius` size — the scene's clip handles
// overflow, not the layout. Used by `Circle` so every circle stays a real
// circle regardless of how big it grows (e.g. the outward pulse in RESOLVE).
#[derive(Keyable)]
struct TrueCircle {
    radius: f32,
    fill: Option<Color>,
    stroke_color: Option<Color>,
    stroke_width: f32,
}

impl VectorComponent for TrueCircle {
    fn layout(&self, _constraints: Constraints) -> Vec2 {
        // Intentionally ignore the parent's constraints.
        let d = self.radius * 2.0;
        Vec2(d, d)
    }

    fn paint_bounds(&self, size: Vec2) -> Aabb {
        let outset = self
            .stroke_color
            .map(|_| (self.stroke_width * 0.5).max(0.0))
            .unwrap_or(0.0);
        Aabb {
            origin: Vec2(-outset, -outset),
            size: Vec2(size.0 + outset * 2.0, size.1 + outset * 2.0),
        }
    }

    fn render(&self, _size: Vec2) -> VectorGraphic {
        let d = self.radius * 2.0;
        let inner = shapes::Circle::builder()
            .radius(self.radius)
            .maybe_fill(self.fill)
            .maybe_stroke(self.stroke_color.map(|c| Stroke {
                paint: c.into(),
                width: self.stroke_width,
            }))
            .build();
        inner.render(Vec2(d, d))
    }
}

/// A filled and/or stroked circle, centered on `center`, that stays a true
/// circle regardless of the parent's constraints. The stroke is flattened into
/// `stroke` (color) + `stroke_width` so every field is hashable. Renders
/// nothing when there is neither a visible fill nor a visible stroke.
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
        TrueCircle {
            radius,
            fill,
            stroke_color: stroke,
            stroke_width,
        }
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

/// A rotated / scaled / faded solid rectangle (a filled [`Fx`]). Renders
/// nothing when it would be invisible or degenerate.
#[component(vector)]
pub fn FxRect(
    center: Vec2,
    size: Vec2,
    angle: f32,
    color: Color,
    opacity: f32,
    scale: Vec2,
) -> impl VectorComponent {
    if size.0 <= 0.0 || size.1 <= 0.0 || opacity <= 0.0 || color.a <= 0.0 {
        return Fragment::empty();
    }
    if scale.0 <= 0.0 || scale.1 <= 0.0 {
        return Fragment::empty();
    }
    Fragment::single(
        shapes::Rectangle::builder()
            .size(size)
            .fill(color)
            .build()
            .transform(center_transform(size, angle, scale))
            .opacity(opacity)
            .anchored(Anchor::CENTER)
            .snap_to(center),
    )
}

/// A rotated / scaled / faded stroked rectangle (a stroked [`Fx`]). Renders
/// nothing when it would be invisible or degenerate.
#[component(vector)]
#[allow(clippy::too_many_arguments)]
pub fn FxOutlineRect(
    center: Vec2,
    size: Vec2,
    angle: f32,
    color: Color,
    opacity: f32,
    scale: Vec2,
    width: f32,
) -> impl VectorComponent {
    if size.0 <= 0.0 || size.1 <= 0.0 || opacity <= 0.0 || color.a <= 0.0 || width <= 0.0 {
        return Fragment::empty();
    }
    if scale.0 <= 0.0 || scale.1 <= 0.0 {
        return Fragment::empty();
    }
    Fragment::single(
        shapes::Rectangle::builder()
            .size(size)
            .stroke(Stroke {
                paint: color.into(),
                width,
            })
            .build()
            .transform(center_transform(size, angle, scale))
            .opacity(opacity)
            .anchored(Anchor::CENTER)
            .snap_to(center),
    )
}
