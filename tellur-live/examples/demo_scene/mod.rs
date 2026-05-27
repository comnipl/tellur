use std::f32::consts::{PI, TAU};
use std::hash::{Hash, Hasher};

use tellur_core::color::Color;
use tellur_core::dyn_compare::hash_f32;
use tellur_core::geometry::{Anchor, Constraints, Rect, Transform, Vec2};
use tellur_core::layer::{Layer, VectorLayer};
use tellur_core::layout::raster::RasterLayoutExt;
use tellur_core::phase::Phase;
use tellur_core::placement::{RasterPlacement, VectorPlacement};
use tellur_core::raster::{RasterComponent, Resolution};
use tellur_core::shapes::{Circle, Rectangle};
use tellur_core::text::{Text, TextSpan, Weight, MONOSPACE};
use tellur_core::time::Time;
use tellur_core::timeline::{timeline, Timeline};
use tellur_core::vector::{Group, Node, Paint, Stroke, VectorComponent, VectorGraphic};
use tellur_renderer::{DropShadow, Rasterizable};

const DURATION: f32 = 7.6;
const SCENE_SIZE: Vec2 = Vec2(1920.0, 1080.0);
const CX: f32 = 960.0;
const CY: f32 = 540.0;

// Restrained palette: a deep ink bg, a warm paper for the scaffolding /
// typography, and two saturated accents (a hot pink and an electric cyan).
// Holding to three foreground tones gives the piece a deliberate,
// design-system feel instead of a confetti palette.
//
// `PartialEq + Hash` so structs holding a `Palette` (like `Hud`) compose
// into a `CachingRenderContext`-friendly key without manual plumbing.
#[derive(Clone, Copy, PartialEq, Hash)]
struct Palette {
    bg: Color,
    paper: Color,
    pink: Color,
    cyan: Color,
}

// Composable rotation + non-uniform scale + opacity wrapper.
struct Fx {
    angle: f32,
    sx: f32,
    sy: f32,
    opacity: f32,
    child: Box<dyn VectorComponent>,
}

impl PartialEq for Fx {
    fn eq(&self, other: &Self) -> bool {
        self.angle.to_bits() == other.angle.to_bits()
            && self.sx.to_bits() == other.sx.to_bits()
            && self.sy.to_bits() == other.sy.to_bits()
            && self.opacity.to_bits() == other.opacity.to_bits()
            && *self.child == *other.child
    }
}

impl Hash for Fx {
    fn hash<H: Hasher>(&self, state: &mut H) {
        hash_f32(self.angle, state);
        hash_f32(self.sx, state);
        hash_f32(self.sy, state);
        hash_f32(self.opacity, state);
        self.child.hash(state);
    }
}

impl VectorComponent for Fx {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        self.child.layout(constraints)
    }

    fn paint_bounds(&self, size: Vec2) -> Rect {
        Rect {
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
            view_box: Rect {
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

fn solid(color: Color) -> Paint {
    Paint::Solid(color)
}

fn alpha(color: Color, value: f32) -> Color {
    Color {
        a: value.clamp(0.0, 1.0),
        ..color
    }
}

fn lerp(from: f32, to: f32, p: f32) -> f32 {
    from + (to - from) * p
}

// --- easing functions ---

fn ease_out_cubic(p: Phase) -> f32 {
    1.0 - (1.0 - p.get()).powi(3)
}

fn ease_out_quint(p: Phase) -> f32 {
    1.0 - (1.0 - p.get()).powi(5)
}

fn ease_in_out_quint(p: Phase) -> f32 {
    let x = p.get();
    if x < 0.5 {
        16.0 * x.powi(5)
    } else {
        1.0 - (-2.0 * x + 2.0).powi(5) * 0.5
    }
}

fn ease_in_out_expo(p: Phase) -> f32 {
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

fn ease_in_back(p: Phase) -> f32 {
    let x = p.get();
    let c1 = 1.70158;
    let c3 = c1 + 1.0;
    c3 * x.powi(3) - c1 * x.powi(2)
}

fn ease_out_elastic(p: Phase) -> f32 {
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

fn wave<T: Time>(time: T, period: f32, offset: f32) -> f32 {
    ((time.seconds() / period + offset) * TAU).sin()
}

// Time-bracketed envelope: rises with `rise`, holds, falls with `fall`.
fn envelope<T: Time, R, F>(
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

// --- drawing primitives ---

fn add_rect(scene: &mut VectorLayer, position: Vec2, size: Vec2, color: Color) {
    if size.0 <= 0.0 || size.1 <= 0.0 || color.a <= 0.0 {
        return;
    }
    scene.add(
        Rectangle {
            size,
            fill: solid(color).into(),
            stroke: None,
        }
        .at(position),
    );
}

// `Circle::layout` clamps its bounding box to the parent's `Constraints`, so
// any circle whose diameter exceeds the scene's shorter side (1080) gets
// axis-squashed into an ellipse. `TrueCircle` overrides `layout` to always
// return the intrinsic `2 * radius` size — the scene's clip handles overflow,
// not the layout. Used by `add_circle` so every circle stays a real circle
// regardless of how big it grows (e.g. the outward pulse in RESOLVE).
struct TrueCircle {
    radius: f32,
    fill: Option<Color>,
    stroke_color: Option<Color>,
    stroke_width: f32,
}

impl PartialEq for TrueCircle {
    fn eq(&self, other: &Self) -> bool {
        self.radius.to_bits() == other.radius.to_bits()
            && self.fill == other.fill
            && self.stroke_color == other.stroke_color
            && self.stroke_width.to_bits() == other.stroke_width.to_bits()
    }
}

impl Hash for TrueCircle {
    fn hash<H: Hasher>(&self, state: &mut H) {
        hash_f32(self.radius, state);
        self.fill.hash(state);
        self.stroke_color.hash(state);
        hash_f32(self.stroke_width, state);
    }
}

impl VectorComponent for TrueCircle {
    fn layout(&self, _constraints: Constraints) -> Vec2 {
        // Intentionally ignore the parent's constraints.
        let d = self.radius * 2.0;
        Vec2(d, d)
    }

    fn paint_bounds(&self, size: Vec2) -> Rect {
        Rect {
            origin: Vec2::ZERO,
            size,
        }
    }

    fn render(&self, _size: Vec2) -> VectorGraphic {
        let d = self.radius * 2.0;
        let inner = Circle {
            radius: self.radius,
            fill: self.fill.map(solid).map(Into::into),
            stroke: self.stroke_color.map(|c| Stroke {
                paint: solid(c),
                width: self.stroke_width,
            }),
        };
        inner.render(Vec2(d, d))
    }
}

fn add_circle(
    scene: &mut VectorLayer,
    center: Vec2,
    radius: f32,
    fill: Option<Color>,
    stroke: Option<(Color, f32)>,
) {
    if radius <= 0.0 {
        return;
    }
    let fill = fill.filter(|c| c.a > 0.0);
    let stroke = stroke.filter(|(c, w)| c.a > 0.0 && *w > 0.0);
    if fill.is_none() && stroke.is_none() {
        return;
    }
    let (stroke_color, stroke_width) = stroke.map_or((None, 0.0), |(c, w)| (Some(c), w));
    scene.add(
        TrueCircle {
            radius,
            fill,
            stroke_color,
            stroke_width,
        }
        .anchored(Anchor::CENTER)
        .snap_to(center),
    );
}

// Anchor-and-position text helper. Uses the system monospace face for that
// "instrument readout" feel. Skips zero-alpha calls so an animated label can
// be added inside a tight loop without branching at every call site.
fn add_label(
    scene: &mut VectorLayer,
    position: Vec2,
    anchor: Anchor,
    text: &str,
    size: f32,
    color: Color,
    weight: Weight,
) {
    if color.a <= 0.0 || text.is_empty() {
        return;
    }
    scene.add(
        Text {
            font: MONOSPACE.clone(),
            size,
            weight,
            fill: solid(color),
            spans: vec![TextSpan::plain(text)],
        }
        .anchored(anchor)
        .snap_to(position),
    );
}

fn add_fx_rect(
    scene: &mut VectorLayer,
    center: Vec2,
    size: Vec2,
    angle: f32,
    color: Color,
    opacity: f32,
    scale: Vec2,
) {
    if size.0 <= 0.0 || size.1 <= 0.0 || opacity <= 0.0 || color.a <= 0.0 {
        return;
    }
    if scale.0 <= 0.0 || scale.1 <= 0.0 {
        return;
    }
    scene.add(
        Fx {
            angle,
            sx: scale.0,
            sy: scale.1,
            opacity,
            child: Box::new(Rectangle {
                size,
                fill: solid(color).into(),
                stroke: None,
            }),
        }
        .anchored(Anchor::CENTER)
        .snap_to(center),
    );
}

fn add_fx_outline_rect(
    scene: &mut VectorLayer,
    center: Vec2,
    size: Vec2,
    angle: f32,
    color: Color,
    opacity: f32,
    scale: Vec2,
    width: f32,
) {
    if size.0 <= 0.0 || size.1 <= 0.0 || opacity <= 0.0 || color.a <= 0.0 || width <= 0.0 {
        return;
    }
    if scale.0 <= 0.0 || scale.1 <= 0.0 {
        return;
    }
    scene.add(
        Fx {
            angle,
            sx: scale.0,
            sy: scale.1,
            opacity,
            child: Box::new(Rectangle {
                size,
                fill: None,
                stroke: Some(Stroke {
                    paint: solid(color),
                    width,
                }),
            }),
        }
        .anchored(Anchor::CENTER)
        .snap_to(center),
    );
}

// --- scenes ---

fn draw_backdrop<T: Time>(scene: &mut VectorLayer, time: T, p: Palette) {
    // bg color is painted by the outer `.background(...)`. This layer carries
    // only the faintest ambient texture so the foreground reads as the
    // intended subject.
    //
    // All animation here is time-windowed `reveal` only — no continuous wave
    // — so once the reveals saturate (~0.6s) the layer becomes byte-for-byte
    // identical every frame and `CachingRenderContext` returns the cached
    // raster instead of re-rendering it.
    for i in 0..18 {
        let y = 64.0 + i as f32 * 56.0;
        let reveal =
            ease_in_out_expo(time.phase(0.05 + i as f32 * 0.008, 0.45 + i as f32 * 0.008));
        add_rect(
            scene,
            Vec2(lerp(-1920.0, 0.0, reveal), y),
            Vec2(1920.0, 1.0),
            alpha(p.paper, 0.022 * reveal),
        );
    }

    // Two extremely dim "field boundary" rings — just inside the HUD frame
    // and just outside it. They suggest "this scene happens inside a
    // measured field" and pull the eye toward center without actually
    // darkening the corners.
    let ring_reveal = ease_in_out_expo(time.phase(0.6, 1.1));
    if ring_reveal > 0.0 {
        for (r, a_mult) in [(720.0_f32, 1.0_f32), (860.0_f32, 0.55_f32)] {
            add_circle(
                scene,
                Vec2(CX, CY),
                r * ring_reveal,
                None,
                Some((alpha(p.paper, 0.05 * a_mult), 1.0)),
            );
        }
    }

    // 12 micro tick marks along the outer "field boundary" at 30° spacing —
    // subliminal "graduated horizon" detail.
    for i in 0..12 {
        let a = i as f32 / 12.0 * TAU - PI * 0.5;
        let reveal = ease_in_out_expo(time.phase(0.75 + i as f32 * 0.012, 1.2 + i as f32 * 0.012));
        if reveal <= 0.0 {
            continue;
        }
        let major = i % 3 == 0;
        let r_base = 720.0;
        let length = if major { 16.0 } else { 8.0 };
        let mid_r = r_base + length * 0.5;
        let mid = Vec2(CX + a.cos() * mid_r, CY + a.sin() * mid_r);
        add_fx_rect(
            scene,
            mid,
            Vec2(if major { 2.0 } else { 1.4 }, length * reveal),
            a + PI * 0.5,
            alpha(p.paper, if major { 0.16 } else { 0.1 }),
            1.0,
            Vec2(1.0, 1.0),
        );
    }
}

// HUD intro/outro time windows. All bracket/tick/label staggers fit inside
// `INTRO`; everything saturates by `INTRO_END` and the component becomes
// byte-identical between frames.
const HUD_INTRO_START: f32 = 0.15;
const HUD_INTRO_END: f32 = 1.24;
const HUD_OUTRO_START: f32 = 7.1;
const HUD_OUTRO_END: f32 = 7.55;
const HUD_INTRO_WIDTH: f32 = HUD_INTRO_END - HUD_INTRO_START;
const HUD_OUTRO_WIDTH: f32 = HUD_OUTRO_END - HUD_OUTRO_START;

fn section_marker(section: u8, p: Palette) -> (&'static str, Color) {
    match section {
        0 => ("01 / OVERTURE", p.pink),
        1 => ("02 / FIELD", p.cyan),
        2 => ("03 / SCAN", p.pink),
        _ => ("04 / RESOLVE", p.cyan),
    }
}

fn section_index_at(t: f32) -> u8 {
    if t < 1.85 {
        0
    } else if t < 3.4 {
        1
    } else if t < 5.0 {
        2
    } else {
        3
    }
}

// Phase-local helper: returns the sub-phase reached at virtual time
// `virtual_t` (measured from the intro/outro start) for an event spanning
// `[start, end]` in that same virtual frame.
fn local_phase(virtual_t: f32, start: f32, end: f32) -> Phase {
    Phase::saturating((virtual_t - start) / (end - start))
}

// Persistent UI scaffolding — the "instrument panel" framing that keeps the
// piece reading as a deliberate system rather than free-floating shapes.
//
// Implemented as a `VectorComponent` whose state is reduced to three small,
// stable values: a `Palette` and two `Phase`s plus a `section` discriminator.
// Once the intro phase saturates to 1.0 (after ~1.24s), and as long as
// `section` hasn't ticked over, the struct hashes and compares equal across
// frames — so wrapping `Hud` in `.rasterize()` lets `CachingRenderContext`
// reuse the rasterized image for the full steady-state span instead of
// re-rendering every frame.
#[derive(Clone, Copy)]
struct Hud {
    palette: Palette,
    intro: Phase,
    outro: Phase,
    section: u8,
}

impl PartialEq for Hud {
    fn eq(&self, other: &Self) -> bool {
        self.section == other.section
            && self.intro == other.intro
            && self.outro == other.outro
            && self.palette == other.palette
    }
}

impl Hash for Hud {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.section.hash(state);
        hash_f32(self.intro.get(), state);
        hash_f32(self.outro.get(), state);
        self.palette.hash(state);
    }
}

impl VectorComponent for Hud {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        constraints.constrain(SCENE_SIZE)
    }

    // paint_bounds defaults to (0, 0)..size — the HUD doesn't paint outside
    // its own scene rect, so the default is exactly right.

    fn render(&self, size: Vec2) -> VectorGraphic {
        let mut layer = VectorLayer::new(size);
        self.draw_into(&mut layer);
        layer.render(size)
    }
}

impl Hud {
    fn draw_into(&self, scene: &mut VectorLayer) {
        // `intro_t` / `outro_t` are virtual elapsed seconds inside the intro
        // / outro window. Each sub-event below is expressed relative to that
        // virtual frame — so when `intro = 1.0` (saturated), every
        // sub-phase is also fully saturated, and the resulting `VectorLayer`
        // is structurally identical to last frame's.
        let intro_t = self.intro.get() * HUD_INTRO_WIDTH;
        let outro_t = self.outro.get() * HUD_OUTRO_WIDTH;

        let alpha_in = ease_in_out_expo(local_phase(intro_t, 0.0, 0.4));
        let alpha_out = ease_in_out_expo(local_phase(outro_t, 0.0, HUD_OUTRO_WIDTH));
        let life = (alpha_in * (1.0 - alpha_out)).clamp(0.0, 1.0);
        if life <= 0.0 {
            return;
        }

        let p = self.palette;
        let stroke_w = 3.0_f32;
        let inset = 96.0_f32;
        let bracket_len = 92.0_f32;

        let corners = [
            (inset, inset, 1.0_f32, 1.0_f32),
            (SCENE_SIZE.0 - inset, inset, -1.0, 1.0),
            (inset, SCENE_SIZE.1 - inset, 1.0, -1.0),
            (SCENE_SIZE.0 - inset, SCENE_SIZE.1 - inset, -1.0, -1.0),
        ];
        for (i, &(ax, ay, dx, dy)) in corners.iter().enumerate() {
            let stagger = i as f32 * 0.05;
            let pop = ease_out_cubic(local_phase(intro_t, 0.1 + stagger, 0.6 + stagger));
            let len = bracket_len * pop;
            let color = alpha(p.paper, 0.55 * life);
            let hx = if dx > 0.0 { ax } else { ax - len };
            let vy = if dy > 0.0 { ay } else { ay - len };
            add_rect(scene, Vec2(hx, ay - stroke_w * 0.5), Vec2(len, stroke_w), color);
            add_rect(scene, Vec2(ax - stroke_w * 0.5, vy), Vec2(stroke_w, len), color);
        }

        let label_in = ease_in_out_expo(local_phase(intro_t, 0.4, 0.8));
        let label_alpha = 0.95 * life * label_in;

        add_label(
            scene,
            Vec2(inset, inset - 22.0),
            Anchor::BOTTOM_LEFT,
            "TELLUR",
            20.0,
            alpha(p.paper, label_alpha),
            Weight::BOLD,
        );
        add_label(
            scene,
            Vec2(inset + 96.0, inset - 22.0),
            Anchor::BOTTOM_LEFT,
            "kinetic-motion · 7.6s",
            13.0,
            alpha(p.paper, 0.5 * life * label_in),
            Weight::NORMAL,
        );

        let (idx_text, idx_color) = section_marker(self.section, p);
        let marker_x = SCENE_SIZE.0 - inset;
        add_label(
            scene,
            Vec2(marker_x, inset - 22.0),
            Anchor::BOTTOM_RIGHT,
            idx_text,
            14.0,
            alpha(p.paper, 0.75 * life * label_in),
            Weight::NORMAL,
        );
        add_circle(
            scene,
            Vec2(marker_x - 128.0, inset - 28.0),
            4.5 * label_in,
            Some(alpha(idx_color, life * label_in)),
            None,
        );

        // Static "OBS" badge sits below the section marker — reads as a
        // "live observation" tag without animating per frame (so it stays
        // inside the cached HUD raster).
        add_label(
            scene,
            Vec2(marker_x, inset + 4.0),
            Anchor::TOP_RIGHT,
            "OBS · TELLUR-04",
            11.0,
            alpha(p.paper, 0.4 * life * label_in),
            Weight::NORMAL,
        );

        // Bottom edge tick ruler — every 4th tick is taller.
        let tick_count = 17;
        let tick_y_top = SCENE_SIZE.1 - inset + 28.0;
        let bar_left = inset + 24.0;
        let bar_right = SCENE_SIZE.0 - inset - 24.0;
        for i in 0..tick_count {
            let stagger = i as f32 * 0.018;
            let pop = ease_out_cubic(local_phase(intro_t, 0.3 + stagger, 0.8 + stagger));
            if pop <= 0.0 {
                continue;
            }
            let frac = i as f32 / (tick_count - 1) as f32;
            let x = lerp(bar_left, bar_right, frac);
            let major = i % 4 == 0;
            let height = if major { 18.0 } else { 8.0 };
            let color = alpha(p.paper, if major { 0.55 } else { 0.35 } * life);
            add_rect(scene, Vec2(x - 1.0, tick_y_top), Vec2(2.0, height * pop), color);
        }

        // Left + right edge tick rulers — completes the four-sided instrument
        // frame so the scaffold reads as a full HUD rather than a desk lamp.
        let v_tick_count = 11;
        let v_bar_top = inset + 60.0;
        let v_bar_bottom = SCENE_SIZE.1 - inset - 60.0;
        for i in 0..v_tick_count {
            let stagger = i as f32 * 0.02;
            let pop = ease_out_cubic(local_phase(intro_t, 0.55 + stagger, 1.0 + stagger));
            if pop <= 0.0 {
                continue;
            }
            let frac = i as f32 / (v_tick_count - 1) as f32;
            let y = lerp(v_bar_top, v_bar_bottom, frac);
            let major = i % 5 == 0;
            let width = if major { 16.0 } else { 7.0 };
            let color = alpha(p.paper, if major { 0.5 } else { 0.3 } * life);
            // Left side ticks point inward.
            add_rect(scene, Vec2(inset - 28.0, y - 1.0), Vec2(width * pop, 2.0), color);
            // Right side ticks point inward.
            add_rect(
                scene,
                Vec2(SCENE_SIZE.0 - inset + 28.0 - width * pop, y - 1.0),
                Vec2(width * pop, 2.0),
                color,
            );
        }

        add_label(
            scene,
            Vec2(inset, SCENE_SIZE.1 - inset + 20.0),
            Anchor::TOP_LEFT,
            "RUNTIME 7600MS · 60FPS",
            12.0,
            alpha(p.paper, 0.45 * life * label_in),
            Weight::NORMAL,
        );
        add_label(
            scene,
            Vec2(SCENE_SIZE.0 - inset, SCENE_SIZE.1 - inset + 20.0),
            Anchor::TOP_RIGHT,
            "1920 × 1080 · RGBA",
            12.0,
            alpha(p.paper, 0.45 * life * label_in),
            Weight::NORMAL,
        );
    }
}

fn draw_overture<T: Time>(scene: &mut VectorLayer, time: T, p: Palette) {
    if time.during(0.0, 2.2).is_none() {
        return;
    }

    // Two clamp bars — one cyan above, one pink below — that frame the
    // central hero. Snappy ease_in_out_expo in, ease_in_back out (lifts off
    // the center toward their respective edges). End-cap mini bracket ticks
    // turn each bar into a "measurement span" instead of an anonymous slab.
    let bars: [(f32, f32, Color); 2] = [(-200.0, -1.0, p.cyan), (200.0, 1.0, p.pink)];
    let bar_w = 1200.0_f32;
    let bar_h = 24.0_f32;
    for (i, &(dy, side, color)) in bars.iter().enumerate() {
        let stagger = i as f32 * 0.06;
        let enter = ease_in_out_expo(time.phase(0.32 + stagger, 0.92 + stagger));
        let leave = ease_in_back(time.phase(1.55 + stagger * 0.5, 2.05 + stagger * 0.5));
        let bar_x = lerp(side * 2400.0 + CX, CX, enter);
        let exit_dy = leave * if side > 0.0 { 320.0 } else { -320.0 };
        let alpha_factor = (enter * (1.0 - leave)).clamp(0.0, 1.0) * 0.92;
        let y_center = CY + dy + exit_dy;

        add_fx_rect(
            scene,
            Vec2(bar_x, y_center),
            Vec2(bar_w, bar_h),
            0.0,
            color,
            alpha_factor,
            Vec2(1.0, 1.0),
        );

        // End-cap mini brackets at each bar end — small perpendicular
        // ticks that turn the bar into a clear measurement span. Same color
        // as the bar; staggered tiny so they "arrive" with the bar.
        let cap_pop = ease_out_cubic(time.phase(0.65 + stagger, 1.05 + stagger))
            * (1.0 - ease_in_back(time.phase(1.55 + stagger * 0.5, 1.95 + stagger * 0.5)));
        if cap_pop > 0.0 {
            let cap_h = 28.0;
            let cap_w = 3.0;
            for &cap_side in &[-1.0_f32, 1.0] {
                let cap_x = bar_x + cap_side * (bar_w * 0.5 + 1.0);
                add_rect(
                    scene,
                    Vec2(cap_x - cap_w * 0.5, y_center - cap_h * 0.5),
                    Vec2(cap_w, cap_h * cap_pop),
                    alpha(color, alpha_factor),
                );
            }
        }
    }

    // Hero square: controlled ease_out_cubic pop (no rubbery elastic),
    // a slow drift-spin, and an ease_in_back dismissal.
    let hero_in = ease_out_cubic(time.phase(0.55, 1.2));
    let hero_out = ease_in_back(time.phase(1.7, 2.15));
    let hero_life = (hero_in * (1.0 - hero_out)).clamp(0.0, 1.0);
    let spin = time.seconds() * 0.35 + PI * 0.25;
    let scale = (0.55 + hero_in * 0.45) * (1.0 - hero_out);

    let s_clamped = scale.max(0.001);

    add_fx_rect(
        scene,
        Vec2(CX, CY),
        Vec2(280.0, 280.0),
        spin,
        p.paper,
        hero_life,
        Vec2(s_clamped, s_clamped),
    );
    add_fx_outline_rect(
        scene,
        Vec2(CX, CY),
        Vec2(420.0, 420.0),
        -spin * 0.4,
        alpha(p.pink, hero_life * 0.82),
        1.0,
        Vec2(s_clamped, s_clamped),
        3.0,
    );

    // Registration markings inside the hero square — a reticle (cross arms +
    // a center ring + gap dashes) plus four small corner dots, all painted
    // in the bg color so they read as "cut into the paper". They rotate
    // with the square via the same `spin`.
    let mark_color = alpha(p.bg, hero_life * 0.55);
    let cross_arm = 38.0 * s_clamped;

    // Crosshair arms — note the gap in the middle (drawn as 4 short
    // segments) so the reticle reads as a scope mark, not a solid plus.
    let arm_inner_gap = 8.0 * s_clamped;
    let arm_outer = cross_arm;
    let arm_segment = arm_outer - arm_inner_gap;
    let arm_mid = (arm_inner_gap + arm_outer) * 0.5;
    let cs2 = spin.cos();
    let sn2 = spin.sin();
    // Four offset directions: +x, -x, +y, -y in local space.
    for &(dx, dy, vertical) in &[
        (1.0_f32, 0.0_f32, false),
        (-1.0, 0.0, false),
        (0.0, 1.0, true),
        (0.0, -1.0, true),
    ] {
        let lx = dx * arm_mid;
        let ly = dy * arm_mid;
        let pos = Vec2(CX + lx * cs2 - ly * sn2, CY + lx * sn2 + ly * cs2);
        let (w, h) = if vertical { (2.0, arm_segment) } else { (arm_segment, 2.0) };
        add_fx_rect(scene, pos, Vec2(w, h), spin, mark_color, 1.0, Vec2(1.0, 1.0));
    }

    // Small open ring at the center of the reticle.
    add_circle(
        scene,
        Vec2(CX, CY),
        6.0 * s_clamped,
        None,
        Some((mark_color, 1.5)),
    );
    // A tiny solid dot at the very center for the bullseye.
    add_circle(
        scene,
        Vec2(CX, CY),
        1.5 * s_clamped,
        Some(mark_color),
        None,
    );
    // Four corner dots inside the paper square, offset 110 from center then
    // rotated by `spin` to follow the square's orientation.
    let corner_off = 110.0 * s_clamped;
    let cs = spin.cos();
    let sn = spin.sin();
    for &(dx, dy) in &[(-1.0_f32, -1.0_f32), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
        let lx = dx * corner_off;
        let ly = dy * corner_off;
        let pos = Vec2(CX + lx * cs - ly * sn, CY + lx * sn + ly * cs);
        add_circle(scene, pos, 3.5 * hero_life, Some(mark_color), None);
    }

    // Four registration dots locked onto the outline corners, alternating
    // pink/cyan so the framing reads as intentional design pairs. Each is
    // linked back to the center by a dim hairline ray, which turns the
    // outside dots from "floating decorations" into "axis terminators" —
    // a small move that pushes the composition toward "instrument layout".
    let ray_in = ease_out_cubic(time.phase(0.85, 1.4)) * (1.0 - hero_out);
    for s in 0..4 {
        let a = s as f32 * PI * 0.5 + spin * 0.5 + PI * 0.25;
        let r = 230.0;
        let pos = Vec2(CX + a.cos() * r, CY + a.sin() * r);

        if ray_in > 0.0 {
            // Hairline from outside the paper-square corner (~200) to just
            // inside the outside dot. The outline frame's diagonal corner
            // sits at ~297, so the ray crosses through it visually.
            let inner_r = 200.0;
            let outer_r = r - 10.0;
            let length = (outer_r - inner_r) * ray_in;
            let mid_r = inner_r + length * 0.5;
            let mid = Vec2(CX + a.cos() * mid_r, CY + a.sin() * mid_r);
            add_fx_rect(
                scene,
                mid,
                Vec2(1.5, length),
                a + PI * 0.5,
                alpha(p.paper, hero_life * 0.45),
                1.0,
                Vec2(1.0, 1.0),
            );
        }

        add_circle(
            scene,
            pos,
            6.0 * hero_life,
            Some(alpha(if s % 2 == 0 { p.pink } else { p.cyan }, hero_life)),
            None,
        );

        // Small index tag next to each outside dot. The tag follows the
        // dot's rotated position so it always reads on the outside.
        let label_in = ease_out_cubic(time.phase(1.0 + s as f32 * 0.04, 1.4 + s as f32 * 0.04));
        let label_alpha = hero_life * label_in
            * (1.0 - ease_in_back(time.phase(1.7, 2.05)))
            * 0.6;
        if label_alpha > 0.0 {
            let label_r = r + 18.0;
            let lpos = Vec2(CX + a.cos() * label_r, CY + a.sin() * label_r);
            let tag_text = format!("0{}", s + 1);
            // Anchor toward the outward direction.
            let anchor = if a.cos().abs() > a.sin().abs() {
                if a.cos() > 0.0 { Anchor::CENTER_LEFT } else { Anchor::CENTER_RIGHT }
            } else if a.sin() > 0.0 {
                Anchor::TOP_CENTER
            } else {
                Anchor::BOTTOM_CENTER
            };
            add_label(
                scene,
                lpos,
                anchor,
                &tag_text,
                10.0,
                alpha(p.paper, label_alpha.clamp(0.0, 1.0)),
                Weight::NORMAL,
            );
        }
    }

    // Length tag beneath the central composition — small data-design touch
    // that gives the OVERTURE a "measurement readout" character.
    let tag_in = ease_in_out_expo(time.phase(0.95, 1.35)) * (1.0 - ease_in_out_expo(time.phase(1.55, 2.0)));
    if tag_in > 0.0 {
        let y = CY + 280.0;
        // End ticks of the measurement line.
        let tick_h = 8.0;
        let half_span = 90.0;
        add_rect(
            scene,
            Vec2(CX - half_span - 1.0, y - tick_h * 0.5),
            Vec2(2.0, tick_h),
            alpha(p.paper, hero_life * tag_in * 0.65),
        );
        add_rect(
            scene,
            Vec2(CX + half_span - 1.0, y - tick_h * 0.5),
            Vec2(2.0, tick_h),
            alpha(p.paper, hero_life * tag_in * 0.65),
        );
        // The measurement bar itself.
        add_rect(
            scene,
            Vec2(CX - half_span * tag_in, y - 1.0),
            Vec2(half_span * 2.0 * tag_in, 2.0),
            alpha(p.paper, hero_life * tag_in * 0.55),
        );
        add_label(
            scene,
            Vec2(CX, y + 18.0),
            Anchor::TOP_CENTER,
            "L = 280 PX",
            12.0,
            alpha(p.paper, hero_life * tag_in * 0.75),
            Weight::NORMAL,
        );
    }

    // Pink horizontal scan stripe sweeping vertically as the scene exits —
    // a transition wipe that hands the frame off to FIELD.
    let sweep = ease_in_out_expo(time.phase(1.45, 2.0));
    if sweep > 0.0 && sweep < 1.0 {
        let y = lerp(-80.0, SCENE_SIZE.1 + 80.0, sweep);
        let visibility = 4.0 * sweep * (1.0 - sweep);
        add_rect(
            scene,
            Vec2(0.0, y - 3.0),
            Vec2(SCENE_SIZE.0, 6.0),
            alpha(p.pink, visibility * 0.88),
        );
    }
}

fn draw_field<T: Time>(scene: &mut VectorLayer, time: T, p: Palette) {
    if time.during(1.7, 3.6).is_none() {
        return;
    }

    let life = envelope(
        time,
        (1.9, 2.25),
        (3.2, 3.55),
        ease_in_out_expo,
        ease_in_out_expo,
    );

    // A tight, deliberate 5×3 grid — fewer dots, more breathing room, more
    // alignment. Columns alternate pink / cyan so the palette reads as
    // intentional pairs rather than a confetti.
    let rows = 3_i32;
    let cols = 5_i32;
    let spacing_x = 220.0;
    let spacing_y = 200.0;

    for r in 0..rows {
        for c in 0..cols {
            let dx = (c as f32 - (cols as f32 - 1.0) * 0.5) * spacing_x;
            let dy = (r as f32 - (rows as f32 - 1.0) * 0.5) * spacing_y;
            let stagger = c as f32 * 0.05 + r as f32 * 0.025;
            let pop = ease_out_cubic(time.phase(1.95 + stagger, 2.45 + stagger));
            let collapse = ease_in_back(time.phase(3.05 + stagger * 0.4, 3.5 + stagger * 0.4));
            let s = (pop * (1.0 - collapse)).clamp(0.0, 1.0);

            let breathe = 1.0 + wave(time, 1.6, stagger) * 0.15;
            let cx = CX + dx;
            let cy = CY + dy * (1.0 - collapse * 0.45);

            let color = if c % 2 == 0 { p.pink } else { p.cyan };
            add_circle(
                scene,
                Vec2(cx, cy),
                14.0 * breathe * s,
                Some(alpha(color, life * 0.92)),
                None,
            );

            // The four corner dots get an accent outline ring — that small
            // hierarchy cue costs nothing and pulls the eye to the frame.
            if (r == 0 || r == rows - 1) && (c == 0 || c == cols - 1) {
                let ring_in = ease_out_cubic(time.phase(2.4 + stagger, 2.9 + stagger));
                add_circle(
                    scene,
                    Vec2(cx, cy),
                    26.0 * ring_in * s,
                    None,
                    Some((alpha(color, life * 0.55), 2.0)),
                );
            }
        }
    }

    // Row labels on the left side of the grid — tiny "R00/R01/R02" marks
    // that make the grid feel like a numbered coordinate space rather than
    // just dots. Fade in with the grid itself.
    for r in 0..rows {
        let dy = (r as f32 - (rows as f32 - 1.0) * 0.5) * spacing_y;
        let label_in = ease_out_cubic(time.phase(2.1 + r as f32 * 0.04, 2.55 + r as f32 * 0.04));
        let label_out = ease_in_back(time.phase(3.1, 3.5));
        let row_alpha = (label_in * (1.0 - label_out)).clamp(0.0, 1.0) * life * 0.55;
        if row_alpha > 0.0 {
            add_label(
                scene,
                Vec2(CX - 720.0, CY + dy),
                Anchor::CENTER_RIGHT,
                &format!("R{:02}", r),
                12.0,
                alpha(p.paper, row_alpha),
                Weight::NORMAL,
            );
        }
    }

    // Column labels along the top of the grid — symmetric with the rows so
    // the grid reads as a proper coordinate field.
    for c in 0..cols {
        let dx = (c as f32 - (cols as f32 - 1.0) * 0.5) * spacing_x;
        let label_in = ease_out_cubic(time.phase(2.05 + c as f32 * 0.04, 2.5 + c as f32 * 0.04));
        let label_out = ease_in_back(time.phase(3.1, 3.5));
        let col_alpha = (label_in * (1.0 - label_out)).clamp(0.0, 1.0) * life * 0.55;
        if col_alpha > 0.0 {
            add_label(
                scene,
                Vec2(CX + dx, CY - (rows as f32 - 1.0) * 0.5 * spacing_y - 38.0),
                Anchor::BOTTOM_CENTER,
                &format!("C{:02}", c),
                12.0,
                alpha(p.paper, col_alpha),
                Weight::NORMAL,
            );
        }
    }

    // Vertical scan line sweeping left-to-right through the grid. Bright
    // head + dimmer trailing wash, with a small "SCAN" data tag at the top
    // and a running-position readout at the bottom.
    let sweep = ease_in_out_expo(time.phase(2.35, 3.0));
    if sweep > 0.0 && sweep < 1.0 {
        let x = lerp(CX - 580.0, CX + 580.0, sweep);
        let visibility = 4.0 * sweep * (1.0 - sweep);
        let height = (rows as f32 + 0.3) * spacing_y * 0.5 * 2.0;
        let top_y = CY - height * 0.5;
        let bottom_y = top_y + height;

        // Trailing wash — a soft pink band behind the leading line that
        // gives the sweep a feeling of "leaving a trace".
        let trail_w = 120.0;
        add_rect(
            scene,
            Vec2(x - trail_w, top_y),
            Vec2(trail_w, height),
            alpha(p.pink, visibility * life * 0.14),
        );
        // Inner brighter trail.
        let inner_trail_w = 32.0;
        add_rect(
            scene,
            Vec2(x - inner_trail_w, top_y),
            Vec2(inner_trail_w, height),
            alpha(p.pink, visibility * life * 0.22),
        );

        // The crisp leading line.
        add_rect(
            scene,
            Vec2(x - 2.0, top_y),
            Vec2(4.0, height),
            alpha(p.pink, visibility * life * 0.95),
        );

        // Bright head dot at the top of the sweep — like a phosphor pixel.
        add_circle(
            scene,
            Vec2(x, top_y),
            7.0,
            Some(alpha(p.paper, visibility * life)),
            Some((alpha(p.pink, visibility * life), 2.0)),
        );

        // Mirror head dot at the bottom for symmetry.
        add_circle(
            scene,
            Vec2(x, bottom_y),
            7.0,
            Some(alpha(p.paper, visibility * life)),
            Some((alpha(p.pink, visibility * life), 2.0)),
        );

        // Top tag.
        add_label(
            scene,
            Vec2(x + 16.0, top_y - 8.0),
            Anchor::BOTTOM_LEFT,
            "SCAN →",
            14.0,
            alpha(p.pink, visibility * life),
            Weight::BOLD,
        );

        // Bottom percentage readout — "treats this as data" cue.
        let pct = (sweep * 100.0) as i32;
        let pct_text = format!("{:03}%", pct);
        add_label(
            scene,
            Vec2(x + 16.0, bottom_y + 8.0),
            Anchor::TOP_LEFT,
            &pct_text,
            13.0,
            alpha(p.paper, visibility * life * 0.95),
            Weight::BOLD,
        );
    }
}

fn draw_scan<T: Time>(scene: &mut VectorLayer, time: T, p: Palette) {
    if time.during(3.4, 5.5).is_none() {
        return;
    }

    let life = envelope(
        time,
        (3.4, 3.8),
        (5.05, 5.45),
        ease_in_out_expo,
        ease_in_out_expo,
    );

    // Cyan horizontal scan stripe sweeping vertically — the matching
    // transition wipe between FIELD and SCAN. Echoes the pink stripe at
    // the OVERTURE→FIELD handoff so the structure rhymes.
    let intro_sweep = ease_in_out_expo(time.phase(3.35, 3.85));
    if intro_sweep > 0.0 && intro_sweep < 1.0 {
        let y = lerp(-80.0, SCENE_SIZE.1 + 80.0, intro_sweep);
        let visibility = 4.0 * intro_sweep * (1.0 - intro_sweep);
        add_rect(
            scene,
            Vec2(0.0, y - 3.0),
            Vec2(SCENE_SIZE.0, 6.0),
            alpha(p.cyan, visibility * 0.88),
        );
    }

    // Center reticle: a small crosshair + cyan dot. Reads as "this is the
    // origin", not "this is a hero blob".
    let reticle = ease_out_cubic(time.phase(3.5, 3.95));
    let cross_arm = 64.0 * reticle;
    add_rect(
        scene,
        Vec2(CX - cross_arm, CY - 1.0),
        Vec2(cross_arm * 2.0, 2.0),
        alpha(p.paper, life * 0.7),
    );
    add_rect(
        scene,
        Vec2(CX - 1.0, CY - cross_arm),
        Vec2(2.0, cross_arm * 2.0),
        alpha(p.paper, life * 0.7),
    );
    add_circle(
        scene,
        Vec2(CX, CY),
        9.0 * reticle,
        Some(alpha(p.cyan, life)),
        None,
    );

    // Single hero ring — this is the scene's one elastic moment.
    let ring_pop = ease_out_elastic(time.phase(3.6, 4.4)).max(0.0);
    let ring_r = lerp(80.0, 300.0, ring_pop);
    add_circle(
        scene,
        Vec2(CX, CY),
        ring_r,
        None,
        Some((alpha(p.cyan, life * 0.75), 3.5)),
    );

    // Inner secondary reticle — a thinner cyan ring at ~120 + 4 cardinal
    // mini-ticks. Layers visual depth between the crosshair and the hero ring.
    let inner_ring_in = ease_out_cubic(time.phase(3.7, 4.15));
    if inner_ring_in > 0.0 {
        let inner_r2 = 116.0;
        add_circle(
            scene,
            Vec2(CX, CY),
            inner_r2 * inner_ring_in,
            None,
            Some((alpha(p.cyan, life * 0.35), 1.5)),
        );
        for i in 0..4 {
            let a = i as f32 * PI * 0.5 - PI * 0.5;
            let mid_r = inner_r2 - 6.0;
            let mid = Vec2(CX + a.cos() * mid_r, CY + a.sin() * mid_r);
            add_fx_rect(
                scene,
                mid,
                Vec2(2.0, 10.0 * inner_ring_in),
                a + PI * 0.5,
                alpha(p.paper, life * 0.45),
                1.0,
                Vec2(1.0, 1.0),
            );
        }
    }

    // 12 tick marks at the ring (every 30°). Every 3rd is taller (a
    // graduated dial look). Quartz-watch fidelity.
    const ANGLE_LABELS: [&str; 12] = [
        "000", "030", "060", "090", "120", "150", "180", "210", "240", "270", "300", "330",
    ];
    for i in 0..12 {
        let a = i as f32 / 12.0 * TAU - PI * 0.5;
        let stagger = i as f32 * 0.025;
        let tk = ease_out_cubic(time.phase(3.85 + stagger, 4.3 + stagger));
        if tk <= 0.0 {
            continue;
        }
        let major = i % 3 == 0;
        let inner_off = if major { 22.0 } else { 12.0 };
        let outer_off = if major { 22.0 } else { 12.0 };
        let inner_r = ring_r - inner_off;
        let outer_r = ring_r + outer_off;
        let mid_r = (inner_r + outer_r) * 0.5;
        let length = outer_r - inner_r;
        let mid = Vec2(CX + a.cos() * mid_r, CY + a.sin() * mid_r);
        add_fx_rect(
            scene,
            mid,
            Vec2(if major { 3.0 } else { 2.0 }, length * tk),
            a + PI * 0.5,
            alpha(p.paper, life * if major { 0.85 } else { 0.55 }),
            1.0,
            Vec2(1.0, 1.0),
        );

        // Angle label outside major ticks — that gradicule-numeral detail
        // pushes the SCAN scene from "geometric" to "instrument readout".
        // We skip i=3 (090°) — dedicated `R = 300 PX` readout — and
        // i=9 (270°) — dedicated `θ = NNN°` readout sits there.
        if major && i != 3 && i != 9 {
            let label_in = ease_out_cubic(time.phase(4.1 + i as f32 * 0.012, 4.5 + i as f32 * 0.012));
            let label_alpha =
                life * label_in * (1.0 - ease_in_out_expo(time.phase(5.0, 5.35)));
            if label_alpha > 0.0 {
                let label_r = outer_r + 32.0;
                add_label(
                    scene,
                    Vec2(CX + a.cos() * label_r, CY + a.sin() * label_r),
                    Anchor::CENTER,
                    ANGLE_LABELS[i],
                    11.0,
                    alpha(p.paper, label_alpha * 0.7),
                    Weight::NORMAL,
                );
            }
        }
    }

    // 8 satellites at the cardinal / intercardinal points on the ring,
    // each tagged with a tiny zero-padded index — "01..08" — that gives
    // the SCAN a "nodes are individually identified" sense.
    let sat_label_in = ease_in_out_expo(time.phase(4.35, 4.75))
        * (1.0 - ease_in_out_expo(time.phase(4.9, 5.2)));
    for i in 0..8 {
        let a = i as f32 / 8.0 * TAU - PI * 0.5;
        let stagger = i as f32 * 0.04;
        let sp = ease_out_cubic(time.phase(4.05 + stagger, 4.55 + stagger));
        let pos = Vec2(CX + a.cos() * ring_r, CY + a.sin() * ring_r);
        let color = if i % 2 == 0 { p.pink } else { p.cyan };
        add_circle(scene, pos, 12.0 * sp, Some(alpha(color, life)), None);

        // Tag position: offset slightly outward from each satellite.
        // i=2 (right cardinal) and i=6 (left cardinal) directions are
        // already occupied by the `R = 300 PX` / `θ = NNN°` readouts, so
        // we skip those two indices to avoid label collisions.
        let skip_label = i == 2 || i == 6;
        if sat_label_in > 0.0 && !skip_label {
            let label_r = ring_r + 22.0;
            let lpos = Vec2(CX + a.cos() * label_r, CY + a.sin() * label_r);
            // Choose anchor based on quadrant so labels read toward the
            // empty side (away from center).
            let anchor = if a.cos().abs() > a.sin().abs() {
                if a.cos() > 0.0 { Anchor::CENTER_LEFT } else { Anchor::CENTER_RIGHT }
            } else if a.sin() > 0.0 {
                Anchor::TOP_CENTER
            } else {
                Anchor::BOTTOM_CENTER
            };
            // Slight push along the chosen axis to avoid the satellite.
            let push = 6.0;
            let lpos = Vec2(lpos.0 + a.cos() * push, lpos.1 + a.sin() * push);
            let label_text = format!("0{}", i + 1);
            add_label(
                scene,
                lpos,
                anchor,
                &label_text,
                10.0,
                alpha(p.paper, life * sat_label_in * 0.55),
                Weight::NORMAL,
            );
        }
    }

    // Radar-style angular sweep: a fan of N narrowing rectangles whose
    // leading edge sweeps clockwise once, fading along its trail.
    let sweep_in = ease_in_out_expo(time.phase(3.9, 4.2));
    let sweep_out = ease_in_out_expo(time.phase(5.05, 5.4));
    let sweep_life = (sweep_in * (1.0 - sweep_out)).clamp(0.0, 1.0);
    if sweep_life > 0.0 {
        // Slower rotation (~2.4s per full revolution) with a wider, longer
        // trail so the sweep reads as deliberate observation rather than a
        // quick flyby.
        let base_angle = (time.seconds() - 3.95).max(0.0) * (TAU / 2.4) - PI * 0.5;
        let trail_count = 32_i32;
        let trail_span = PI * 0.75;
        for j in 0..trail_count {
            let frac = j as f32 / (trail_count - 1) as f32;
            let a = base_angle - frac * trail_span;
            let fade = (1.0 - frac).powi(2);
            let r = ring_r * 0.95;
            let mid = Vec2(CX + a.cos() * r * 0.5, CY + a.sin() * r * 0.5);
            add_fx_rect(
                scene,
                mid,
                Vec2(4.0, r),
                a + PI * 0.5,
                alpha(p.pink, life * sweep_life * fade * 0.6),
                1.0,
                Vec2(1.0, 1.0),
            );
        }
    }

    // Single numeric annotation, attached to the ring at 0°. Small detail,
    // big payoff for "instrumentation" feel.
    let annot = ease_in_out_expo(time.phase(4.15, 4.5)) * (1.0 - ease_in_out_expo(time.phase(4.85, 5.2)));
    if annot > 0.0 {
        // Mini connector tick from the ring to the label.
        add_rect(
            scene,
            Vec2(CX + ring_r, CY - 1.0),
            Vec2(28.0, 2.0),
            alpha(p.paper, life * annot * 0.7),
        );
        add_label(
            scene,
            Vec2(CX + ring_r + 36.0, CY - 6.0),
            Anchor::CENTER_LEFT,
            "R = 300 PX",
            13.0,
            alpha(p.paper, life * annot * 0.85),
            Weight::NORMAL,
        );
        // Sub-label one line down (smaller, dimmer) — extra-instrument feel.
        add_label(
            scene,
            Vec2(CX + ring_r + 36.0, CY + 10.0),
            Anchor::CENTER_LEFT,
            "NODES = 08",
            11.0,
            alpha(p.paper, life * annot * 0.55),
            Weight::NORMAL,
        );
    }

    // "θ" readout in the 270° direction, where there was room left after
    // skipping the angle numeral there. Tracks the radar sweep's current
    // angle so the SCAN scene has a live data feel.
    let theta_in = ease_in_out_expo(time.phase(4.2, 4.55))
        * (1.0 - ease_in_out_expo(time.phase(4.95, 5.25)));
    if theta_in > 0.0 {
        let sweep_in = ease_in_out_expo(time.phase(3.9, 4.2));
        let sweep_active = sweep_in > 0.05;
        if sweep_active {
            let base_angle = (time.seconds() - 3.95).max(0.0) * (TAU / 2.4);
            // Convert to degrees and wrap to [0, 360).
            let deg = (base_angle.to_degrees().rem_euclid(360.0)) as i32;
            let theta_text = format!("θ = {:03}°", deg);
            add_rect(
                scene,
                Vec2(CX - ring_r - 28.0, CY - 1.0),
                Vec2(28.0, 2.0),
                alpha(p.paper, life * theta_in * 0.7),
            );
            add_label(
                scene,
                Vec2(CX - ring_r - 36.0, CY - 6.0),
                Anchor::CENTER_RIGHT,
                &theta_text,
                13.0,
                alpha(p.paper, life * theta_in * 0.85),
                Weight::NORMAL,
            );
        }
    }

    // SCAN's exit burst — 24 short radial spokes shoot outward from the
    // hero ring just as the scene flashes white into RESOLVE. Acts as a
    // physical "ignition" punctuation: the ring snaps, sparks fly, the
    // flash takes over.
    let burst_kick = ease_out_quint(time.phase(4.88, 5.02));
    let burst_fade = ease_in_out_expo(time.phase(5.05, 5.25));
    let burst_life = (burst_kick * (1.0 - burst_fade)).clamp(0.0, 1.0);
    if burst_life > 0.0 {
        for i in 0..24 {
            let a = i as f32 / 24.0 * TAU;
            let inner_r = ring_r + 6.0;
            let length = 40.0 + burst_kick * 110.0;
            let mid_r = inner_r + length * 0.5;
            let mid = Vec2(CX + a.cos() * mid_r, CY + a.sin() * mid_r);
            let color = if i % 3 == 0 { p.paper } else { p.cyan };
            add_fx_rect(
                scene,
                mid,
                Vec2(2.5, length),
                a + PI * 0.5,
                alpha(color, life * burst_life * 0.9),
                1.0,
                Vec2(1.0, 1.0),
            );
        }
    }
}

fn draw_resolve<T: Time>(scene: &mut VectorLayer, time: T, p: Palette) {
    if time.during(4.9, DURATION).is_none() {
        return;
    }

    let life = ease_in_out_expo(time.phase(5.05, 5.5));

    // Aperture-close finale. SCAN's R=300 cyan ring + 8 satellites are
    // inherited and pulled toward the center, leaving a single dot that
    // emits an outward ripple — the gesture that "settles" the whole piece.

    // Phase 1: contracting hero ring (same R=300 that SCAN ended on).
    let contract = ease_in_out_expo(time.phase(5.2, 6.3));
    let ring_r = lerp(300.0, 22.0, contract);
    let ring_alpha = (1.0 - contract * 0.55) * life;
    if ring_r > 4.0 {
        add_circle(
            scene,
            Vec2(CX, CY),
            ring_r,
            None,
            Some((alpha(p.cyan, ring_alpha * 0.9), 3.5)),
        );
    }

    // Phase 2: 8 satellites slide along their angular rays toward center.
    // They settle into two interleaved layers — cardinals further out, the
    // intercardinals closer in — so the final cluster reads as a deliberate
    // double-ring composition instead of a flat ring of dots.
    let sats_in = ease_in_out_expo(time.phase(5.25, 6.35));
    // Slow continuous rotation that only becomes perceptible after the
    // satellites have settled. Starts at 0 (no rotation during collapse) and
    // ramps in after 6.4s. The cluster keeps "feeling alive" until fade out.
    let cluster_spin = ease_out_cubic(time.phase(6.35, 7.0)) * (time.seconds() - 6.35).max(0.0) * 0.18;
    for i in 0..8 {
        let cardinal = i % 2 == 0;
        let base_a = i as f32 / 8.0 * TAU - PI * 0.5;
        let final_r = if cardinal { 46.0 } else { 22.0 };
        let target_r = lerp(300.0, final_r, sats_in);
        let a = base_a + cluster_spin;
        let pos = Vec2(CX + a.cos() * target_r, CY + a.sin() * target_r);
        let color = if cardinal { p.pink } else { p.cyan };
        let size = lerp(12.0, if cardinal { 6.0 } else { 4.5 }, sats_in);
        let sat_alpha = (1.0 - sats_in * 0.4) * life;
        add_circle(scene, pos, size, Some(alpha(color, sat_alpha)), None);
    }

    // Phase 3: "memory" rings — ghosts of where the contracting ring used
    // to be. Three after-images spawn as the ring sweeps inward, each
    // fading slowly. Subtle texture for the contraction.
    let memory_rings: [(f32, f32); 3] = [(5.55, 230.0), (5.85, 160.0), (6.12, 95.0)];
    for &(start, r) in memory_rings.iter() {
        let ghost_in = ease_out_cubic(time.phase(start, start + 0.08));
        let ghost_out = ease_in_out_expo(time.phase(start + 0.25, start + 1.3));
        let ghost = (ghost_in * (1.0 - ghost_out)).clamp(0.0, 1.0);
        if ghost > 0.0 {
            add_circle(
                scene,
                Vec2(CX, CY),
                r,
                None,
                Some((alpha(p.cyan, life * ghost * 0.32), 1.8)),
            );
        }
    }

    // Phase 4: the singularity flash — a brief paper bloom at the moment
    // everything collapses to the center, plus a burst of short radial
    // shards emitted right at the impact so the moment hits with real
    // energy instead of just "a circle gets bigger".
    let sing = ease_out_quint(time.phase(6.2, 6.32))
        * (1.0 - ease_in_out_expo(time.phase(6.32, 6.62)));
    if sing > 0.0 {
        add_circle(
            scene,
            Vec2(CX, CY),
            46.0 + sing * 20.0,
            Some(alpha(p.paper, sing * life * 0.95)),
            None,
        );
    }

    // 16 short radial shards — emit-then-fade exactly at the singularity.
    // Length peaks then collapses inward as they vanish, giving the impact
    // a real "sparks flying outward" sensation.
    let shard_kick = ease_out_quint(time.phase(6.22, 6.36));
    let shard_fade = ease_in_out_expo(time.phase(6.36, 6.6));
    let shard_life = (shard_kick * (1.0 - shard_fade)).clamp(0.0, 1.0);
    if shard_life > 0.0 {
        for i in 0..16 {
            let a = i as f32 / 16.0 * TAU;
            let inner_r = 32.0 + shard_kick * 30.0;
            let length = 26.0 + shard_kick * 50.0;
            let mid_r = inner_r + length * 0.5;
            let mid = Vec2(CX + a.cos() * mid_r, CY + a.sin() * mid_r);
            let color = if i % 2 == 0 { p.pink } else { p.paper };
            add_fx_rect(
                scene,
                mid,
                Vec2(2.0, length),
                a + PI * 0.5,
                alpha(color, life * shard_life * 0.9),
                1.0,
                Vec2(1.0, 1.0),
            );
        }
    }

    // Phase 5: the outward ripple. The scene's one elastic moment — the
    // ring kicks out from center with an elastic-eased launch, then a
    // quint-eased growth carries it past the edge. Stroke alpha decays
    // along the expansion so it reads as a fading wavefront.
    let pulse_kick = ease_out_elastic(time.phase(6.22, 6.55)).max(0.0);
    let pulse_grow = ease_out_quint(time.phase(6.3, 7.15));
    let pulse_fade = 1.0 - ease_in_out_expo(time.phase(6.7, 7.25));
    let pulse_r = pulse_kick * 600.0 * (0.05 + pulse_grow * 0.95);
    if pulse_r > 12.0 && pulse_fade > 0.0 {
        add_circle(
            scene,
            Vec2(CX, CY),
            pulse_r,
            None,
            Some((alpha(p.cyan, life * pulse_fade * 0.9), 3.5)),
        );
    }

    // Secondary, smaller, delayed ripple in pink for rhythm.
    let pulse2_grow = ease_out_quint(time.phase(6.6, 7.3));
    let pulse2_fade = 1.0 - ease_in_out_expo(time.phase(6.95, 7.4));
    let pulse2_r = pulse2_grow * 420.0;
    if pulse2_r > 12.0 && pulse2_fade > 0.0 {
        add_circle(
            scene,
            Vec2(CX, CY),
            pulse2_r,
            None,
            Some((alpha(p.pink, life * pulse2_fade * 0.55), 2.0)),
        );
    }

    // Phase 6: the surviving central composition — a deliberate layered
    // mark, not just a lone dot. Hub + pink hairline collar + 4 axis rays +
    // a dim outer field ring with hash marks. All keyed to a slow
    // post-settle rotation shared with the satellites.

    let comp_in = ease_out_cubic(time.phase(6.28, 6.7));
    if comp_in > 0.0 {
        let breath = 1.0 + wave(time, 1.4, 0.0) * 0.06;

        // (a) the hub: a filled cyan core.
        add_circle(
            scene,
            Vec2(CX, CY),
            7.5 * comp_in * breath,
            Some(alpha(p.cyan, life)),
            None,
        );

        // (b) pink hairline collar one step out from the hub.
        add_circle(
            scene,
            Vec2(CX, CY),
            14.0 * comp_in,
            None,
            Some((alpha(p.pink, life * 0.9), 1.6)),
        );

        // (c) four axis rays at the cardinals, rotating with the cluster.
        // They start just outside the collar and end just inside the outer
        // field ring — visually connecting the inner core to the outer field.
        let ray_in = ease_out_cubic(time.phase(6.5, 6.95));
        if ray_in > 0.0 {
            for k in 0..4 {
                let a = k as f32 * PI * 0.5 + cluster_spin;
                let inner = 19.0;
                let outer = 38.0 + ray_in * 22.0;
                let mid_r = (inner + outer) * 0.5;
                let length = outer - inner;
                let mid = Vec2(CX + a.cos() * mid_r, CY + a.sin() * mid_r);
                add_fx_rect(
                    scene,
                    mid,
                    Vec2(1.5, length),
                    a + PI * 0.5,
                    alpha(p.paper, life * 0.55),
                    1.0,
                    Vec2(1.0, 1.0),
                );
            }
        }

        // (d) outer "field" ring with 12 micro-hash-marks — echoes the SCAN
        // graduated dial at miniature scale. Reads as "still observing".
        let field_in = ease_out_cubic(time.phase(6.65, 7.1));
        if field_in > 0.0 {
            let field_r = 78.0 + (1.0 - breath) * 4.0;
            add_circle(
                scene,
                Vec2(CX, CY),
                field_r * field_in,
                None,
                Some((alpha(p.paper, life * 0.25), 1.0)),
            );
            for k in 0..12 {
                let a = k as f32 / 12.0 * TAU - PI * 0.5 + cluster_spin * 0.5;
                let inner_r = field_r - 4.0;
                let outer_r = field_r + 4.0;
                let mid_r = (inner_r + outer_r) * 0.5;
                let mid = Vec2(CX + a.cos() * mid_r, CY + a.sin() * mid_r);
                let major = k % 3 == 0;
                add_fx_rect(
                    scene,
                    mid,
                    Vec2(if major { 2.0 } else { 1.2 }, 8.0 * field_in),
                    a + PI * 0.5,
                    alpha(p.paper, life * if major { 0.6 } else { 0.35 }),
                    1.0,
                    Vec2(1.0, 1.0),
                );
            }

            // (e) a single small "scan dot" orbiting the field ring — like
            // a slow second-hand. Period ~3.6s. Adds a quiet pulse of life
            // to the otherwise-static central mark.
            let orbit_in = ease_out_cubic(time.phase(6.8, 7.2));
            if orbit_in > 0.0 {
                let orbit_period = 3.6;
                let orbit_a =
                    (time.seconds() - 6.8).max(0.0) * (TAU / orbit_period) - PI * 0.5;
                let orbit_pos =
                    Vec2(CX + orbit_a.cos() * field_r, CY + orbit_a.sin() * field_r);
                add_circle(
                    scene,
                    orbit_pos,
                    3.0 * orbit_in,
                    Some(alpha(p.cyan, life * orbit_in)),
                    None,
                );
                // Tiny leading whisker — a 1px hairline behind the dot for
                // motion sense.
                let trail_count = 4_i32;
                let trail_span = PI * 0.04;
                for j in 1..=trail_count {
                    let frac = j as f32 / trail_count as f32;
                    let a_t = orbit_a - frac * trail_span;
                    let pos = Vec2(CX + a_t.cos() * field_r, CY + a_t.sin() * field_r);
                    let fade = (1.0 - frac).powi(2);
                    add_circle(
                        scene,
                        pos,
                        1.5 * orbit_in,
                        Some(alpha(p.cyan, life * orbit_in * fade * 0.5)),
                        None,
                    );
                }
            }
        }
    }

    // Four alignment ticks at the cardinals just outside the field ring,
    // making the central mark a fully four-way-symmetric axis indicator.
    let tick_in = ease_out_cubic(time.phase(6.85, 7.15));
    if tick_in > 0.0 {
        let tick_alpha = alpha(p.paper, life * 0.5);
        let arm_len = 22.0 * tick_in;
        let offset = 96.0;
        // Down + up vertical ticks.
        add_rect(
            scene,
            Vec2(CX - 1.0, CY + offset),
            Vec2(2.0, arm_len),
            tick_alpha,
        );
        add_rect(
            scene,
            Vec2(CX - 1.0, CY - offset - arm_len),
            Vec2(2.0, arm_len),
            tick_alpha,
        );
        // Right + left horizontal ticks.
        add_rect(
            scene,
            Vec2(CX + offset, CY - 1.0),
            Vec2(arm_len, 2.0),
            tick_alpha,
        );
        add_rect(
            scene,
            Vec2(CX - offset - arm_len, CY - 1.0),
            Vec2(arm_len, 2.0),
            tick_alpha,
        );
    }
}

fn draw_overlay<T: Time>(scene: &mut VectorLayer, time: T, p: Palette) {
    // Pre-OVERTURE "boot screen" — a big monospace timecode + tiny init
    // subtitle briefly appears at center then fades, before the HUD has
    // finished assembling. Reads as a system startup flash.
    let boot_in = ease_in_out_expo(time.phase(0.05, 0.18));
    let boot_out = ease_in_out_expo(time.phase(0.32, 0.55));
    let boot_life = (boot_in * (1.0 - boot_out)).clamp(0.0, 1.0);
    if boot_life > 0.0 {
        add_label(
            scene,
            Vec2(CX, CY - 18.0),
            Anchor::BOTTOM_CENTER,
            "TELLUR",
            42.0,
            alpha(p.paper, boot_life * 0.95),
            Weight::BOLD,
        );
        add_label(
            scene,
            Vec2(CX, CY + 4.0),
            Anchor::TOP_CENTER,
            "00:00:00.000 · INIT",
            13.0,
            alpha(p.paper, boot_life * 0.7),
            Weight::NORMAL,
        );
        // A tiny pink underline dash to the right of "INIT".
        add_rect(
            scene,
            Vec2(CX + 88.0, CY + 18.0),
            Vec2(20.0 * boot_in, 2.0),
            alpha(p.pink, boot_life),
        );
    }

    // Crisp white flash at the SCAN → RESOLVE transition. Lives in the
    // unshadowed overlay so it doesn't smear into a grey haze through the
    // foreground shadow pass.
    let flash = ease_out_quint(time.phase(4.9, 5.05))
        * (1.0 - ease_in_out_expo(time.phase(5.05, 5.35)));
    if flash > 0.0 {
        add_rect(scene, Vec2::ZERO, SCENE_SIZE, alpha(p.paper, flash * 0.22));
    }

    // Exit fade — gentle quint ease into the bg color.
    let fade = ease_in_out_quint(time.phase(7.25, DURATION));
    if fade > 0.0 {
        add_rect(scene, Vec2::ZERO, SCENE_SIZE, alpha(p.bg, fade));
    }
}

pub fn build_timeline() -> impl Timeline + Send {
    timeline(DURATION, move |t, target: Resolution, ctx| {
        let palette = Palette {
            bg: Color::rgb_u8(12, 11, 24),
            paper: Color::rgb_u8(247, 240, 224),
            pink: Color::rgb_u8(255, 79, 138),
            cyan: Color::rgb_u8(73, 222, 226),
        };

        // backdrop is shadow-free and time-stable after its reveal animation
        // saturates (~0.6s), so it caches as its own raster child.
        let mut backdrop = VectorLayer::new(SCENE_SIZE);
        draw_backdrop(&mut backdrop, t, palette);

        // HUD is a dedicated component whose Hash/Eq is driven by a tiny set
        // of inputs (palette + two `Phase`s + section index). After the intro
        // saturates and between section-marker switches, the struct compares
        // equal across frames, so `Rasterize<Hud>` lookup in
        // `CachingRenderContext` hits the cache and returns the previously
        // rendered raster instead of re-shaping all the text + re-drawing
        // every bracket / tick.
        let hud = Hud {
            palette,
            intro: t.phase(HUD_INTRO_START, HUD_INTRO_END),
            outro: t.phase(HUD_OUTRO_START, HUD_OUTRO_END),
            section: section_index_at(t.seconds()),
        };

        let mut foreground = VectorLayer::new(SCENE_SIZE);
        draw_overture(&mut foreground, t, palette);
        draw_field(&mut foreground, t, palette);
        draw_scan(&mut foreground, t, palette);
        draw_resolve(&mut foreground, t, palette);

        let mut overlay = VectorLayer::new(SCENE_SIZE);
        draw_overlay(&mut overlay, t, palette);

        // Two stacked shadows on the foreground: a soft paper-tinted halo
        // (no offset → ambient backlight) directly behind the shapes, then
        // a deeper dark drop shadow further back. The outer DropShadow
        // paints first, then the inner, then the child — so the shape
        // itself stays crisp on top.
        let shadowed_foreground = DropShadow {
            offset: Vec2(0.0, 22.0),
            blur: 26.0,
            color: Color::rgba_u8(0, 0, 0, 170),
            child: Box::new(DropShadow {
                offset: Vec2(0.0, 0.0),
                blur: 18.0,
                color: alpha(palette.paper, 0.26),
                child: Box::new(foreground.rasterize()),
            }),
        };

        let mut stage = Layer::new(SCENE_SIZE);
        stage.add(backdrop.rasterize().at(Vec2::ZERO));
        stage.add(shadowed_foreground.at(Vec2::ZERO));
        stage.add(hud.rasterize().at(Vec2::ZERO));
        stage.add(overlay.rasterize().at(Vec2::ZERO));

        // `.background(palette.bg)` paints the bg fill and — critically —
        // pins the paint_bounds to `(0, 0)..SCENE_SIZE`, clipping the
        // shadow's outward spill so the final frame isn't squished. See
        // the DecoratedBox docstring in tellur_core::layout::raster.
        stage
            .background(palette.bg)
            .render(SCENE_SIZE, target, ctx)
    })
}

pub const TITLE: &str = "Kinetic Motion";

// Consumed by `demo_timeline_mp4` but not by the plugin entry; tell the
// per-binary dead-code lint to allow it.
#[allow(dead_code)]
pub const SCENE_RESOLUTION: (u32, u32) = (1920, 1080);
