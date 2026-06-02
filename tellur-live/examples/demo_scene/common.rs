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

use std::f32::consts::{PI, TAU};

use tellur_core::builder::VectorBuilderPlacement;
use tellur_core::color::Color;
use tellur_core::component;
use tellur_core::fragment::Fragment;
use tellur_core::geometry::{Anchor, Constraints, Rect as Aabb, Transform, Vec2};
use tellur_core::phase::Phase;
use tellur_core::placement::VectorPlacement;
use tellur_core::shapes;
use tellur_core::text::{Text, TextSpan, Weight, MONOSPACE};
use tellur_core::time::Time;
use tellur_core::vector::{Group, Node, Paint, Stroke, VectorComponent, VectorGraphic};
use tellur_core::Keyable;

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

// Composable rotation + non-uniform scale + opacity wrapper.
#[derive(Keyable)]
pub struct Fx {
    pub angle: f32,
    pub sx: f32,
    pub sy: f32,
    pub opacity: f32,
    pub child: Box<dyn VectorComponent>,
}

impl VectorComponent for Fx {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        self.child.layout(constraints)
    }

    fn paint_bounds(&self, size: Vec2) -> Aabb {
        Aabb {
            origin: Vec2(-size.0 * 2.0, -size.1 * 2.0),
            size: Vec2(size.0 * 5.0, size.1 * 5.0),
        }
    }

    fn render(&self, size: Vec2) -> VectorGraphic {
        let inner = self.child.render(size);
        let sx = self.sx.max(0.0001);
        let sy = self.sy.max(0.0001);
        let cos = self.angle.cos();
        let sin = self.angle.sin();
        let a = cos * sx;
        let b = sin * sx;
        let c = -sin * sy;
        let d = cos * sy;
        let center = Vec2(size.0 * 0.5, size.1 * 0.5);
        let tx = center.0 - (a * center.0 + c * center.1);
        let ty = center.1 - (b * center.0 + d * center.1);

        VectorGraphic {
            view_box: Aabb {
                origin: Vec2(-size.0 * 2.0, -size.1 * 2.0),
                size: Vec2(size.0 * 5.0, size.1 * 5.0),
            },
            root: Node::Group(Group {
                transform: Transform { a, b, c, d, tx, ty },
                opacity: self.opacity.clamp(0.0, 1.0),
                children: vec![inner.root],
            }),
        }
    }
}

pub fn solid(color: Color) -> Paint {
    Paint::Solid(color)
}

pub fn alpha(color: Color, value: f32) -> Color {
    Color {
        a: value.clamp(0.0, 1.0),
        ..color
    }
}

pub fn lerp(from: f32, to: f32, p: f32) -> f32 {
    from + (to - from) * p
}

// --- easing functions ---

pub fn ease_out_cubic(p: Phase) -> f32 {
    1.0 - (1.0 - p.get()).powi(3)
}

pub fn ease_out_quint(p: Phase) -> f32 {
    1.0 - (1.0 - p.get()).powi(5)
}

pub fn ease_in_out_quint(p: Phase) -> f32 {
    let x = p.get();
    if x < 0.5 {
        16.0 * x.powi(5)
    } else {
        1.0 - (-2.0 * x + 2.0).powi(5) * 0.5
    }
}

pub fn ease_in_out_expo(p: Phase) -> f32 {
    let x = p.get();
    if x <= 0.0 {
        0.0
    } else if x >= 1.0 {
        1.0
    } else if x < 0.5 {
        2.0_f32.powf(20.0 * x - 10.0) * 0.5
    } else {
        (2.0 - 2.0_f32.powf(-20.0 * x + 10.0)) * 0.5
    }
}

pub fn ease_in_back(p: Phase) -> f32 {
    let x = p.get();
    let c1 = 1.70158;
    let c3 = c1 + 1.0;
    c3 * x.powi(3) - c1 * x.powi(2)
}

pub fn ease_out_elastic(p: Phase) -> f32 {
    let x = p.get();
    if x <= 0.0 {
        0.0
    } else if x >= 1.0 {
        1.0
    } else {
        let c4 = (2.0 * PI) / 3.0;
        2.0_f32.powf(-10.0 * x) * ((x * 10.0 - 0.75) * c4).sin() + 1.0
    }
}

pub fn wave<T: Time>(time: T, period: f32, offset: f32) -> f32 {
    ((time.seconds() / period + offset) * TAU).sin()
}

// Time-bracketed envelope: rises with `rise`, holds, falls with `fall`.
pub fn envelope<T: Time, R, F>(
    time: T,
    rise_span: (f32, f32),
    fall_span: (f32, f32),
    rise: R,
    fall: F,
) -> f32
where
    R: Fn(Phase) -> f32,
    F: Fn(Phase) -> f32,
{
    let r = rise(time.phase(rise_span.0, rise_span.1));
    let f = fall(time.phase(fall_span.0, fall_span.1));
    (r * (1.0 - f)).clamp(0.0, 1.0)
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
            .fill(solid(color))
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
        Aabb {
            origin: Vec2::ZERO,
            size,
        }
    }

    fn render(&self, _size: Vec2) -> VectorGraphic {
        let d = self.radius * 2.0;
        let inner = shapes::Circle::builder()
            .radius(self.radius)
            .maybe_fill(self.fill.map(solid))
            .maybe_stroke(self.stroke_color.map(|c| Stroke {
                paint: solid(c),
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
            .fill(solid(color))
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
        Fx {
            angle,
            sx: scale.0,
            sy: scale.1,
            opacity,
            child: Box::new(
                shapes::Rectangle::builder()
                    .size(size)
                    .fill(solid(color))
                    .build(),
            ),
        }
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
        Fx {
            angle,
            sx: scale.0,
            sy: scale.1,
            opacity,
            child: Box::new(
                shapes::Rectangle::builder()
                    .size(size)
                    .stroke(Stroke {
                        paint: solid(color),
                        width,
                    })
                    .build(),
            ),
        }
        .anchored(Anchor::CENTER)
        .snap_to(center),
    )
}
