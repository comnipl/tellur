//! Shared building blocks for the demo scene: the palette, the `peak` hat
//! curve, and a prelude of `tellur_core` re-exports so every section can
//! `use super::common::*` and write shapes / text / transforms directly.
//!
//! There are no scene-local leaf components: core shapes and `Text` cull
//! themselves when invisible (zero size / alpha / empty text), so sections
//! compose `Rectangle` / `Circle` / `Text` straight from the core,
//! positioned via `anchored().snap_to()` / `place_at()` and pivoted via
//! `transform_around`.

use tellur_core::color::Color;
use tellur_core::geometry::Vec2;

pub use tellur_core::builder::{VectorBuilderPlacement, VectorBuilderTransform};
pub use tellur_core::easing::{Easing, PhaseEasing};
pub use tellur_core::geometry::Transform;
pub use tellur_core::placement::VectorPlacement;
pub use tellur_core::shapes::{Circle, Rectangle};
pub use tellur_core::text::{Text, TextSpan, MONOSPACE};
pub use tellur_core::vector::{Stroke, VectorTransform};

pub const DURATION: f64 = 7.6;
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
