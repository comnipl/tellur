use bytes::Bytes;
use tellur_core::color::Color;
use tellur_core::component::Component;
use tellur_core::geometry::{Rect, Transform};
use tellur_core::raster::{PixelFormat, RasterComponent, RasterImage};
use tellur_core::vector::{
    Node, Paint, Path, PathCommand, VectorComponent, VectorGraphic,
};

/// A `RasterComponent` that takes a `VectorComponent` and produces a raster image
/// at the given resolution.
pub struct Rasterize<V: VectorComponent> {
    pub vector: V,
    pub width: u32,
    pub height: u32,
}

impl<V: VectorComponent> Component for Rasterize<V> {}

impl<V: VectorComponent> RasterComponent for Rasterize<V> {
    fn render(&self) -> RasterImage {
        let graphic = self.vector.render();
        rasterize(&graphic, self.width, self.height)
    }
}

fn rasterize(graphic: &VectorGraphic, width: u32, height: u32) -> RasterImage {
    let mut pixmap = tiny_skia::Pixmap::new(width, height)
        .expect("pixmap dimensions must be non-zero");

    let view_box_xform = view_box_transform(&graphic.view_box, width, height);
    render_node(&mut pixmap, &graphic.root, view_box_xform, 1.0);

    // tiny-skia outputs premultiplied alpha for efficient compositing, but
    // `RasterImage` is defined as straight alpha (matching PNG, web, and most
    // image libraries). Demultiply here so the public type stays consistent.
    let mut straight = Vec::with_capacity(pixmap.data().len());
    for p in pixmap.pixels() {
        let c = p.demultiply();
        straight.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }

    RasterImage {
        width,
        height,
        format: PixelFormat::Rgba8,
        pixels: Bytes::from(straight),
    }
}

/// Transform that maps the range of `view_box` into pixel space `(0, 0)..(width, height)`.
/// Equivalent to SVG's `preserveAspectRatio="none"` (each axis is scaled independently).
fn view_box_transform(view_box: &Rect, width: u32, height: u32) -> tiny_skia::Transform {
    let sx = width as f32 / view_box.size.x;
    let sy = height as f32 / view_box.size.y;
    let tx = -view_box.origin.x * sx;
    let ty = -view_box.origin.y * sy;
    tiny_skia::Transform::from_row(sx, 0.0, 0.0, sy, tx, ty)
}

fn render_node(
    pixmap: &mut tiny_skia::Pixmap,
    node: &Node,
    parent_xform: tiny_skia::Transform,
    parent_opacity: f32,
) {
    match node {
        Node::Group(group) => {
            let xform = parent_xform.pre_concat(to_skia_transform(&group.transform));
            let opacity = parent_opacity * group.opacity;
            for child in &group.children {
                render_node(pixmap, child, xform, opacity);
            }
        }
        Node::Path(path) => {
            let xform = parent_xform.pre_concat(to_skia_transform(&path.transform));
            render_path(pixmap, path, xform, parent_opacity);
        }
    }
}

fn render_path(
    pixmap: &mut tiny_skia::Pixmap,
    path: &Path,
    xform: tiny_skia::Transform,
    opacity: f32,
) {
    let Some(skia_path) = build_skia_path(&path.commands) else {
        return;
    };

    if let Some(fill) = &path.fill {
        let mut paint = tiny_skia::Paint::default();
        paint.anti_alias = true;
        apply_paint(&mut paint, &fill.paint, fill.opacity * opacity);
        pixmap.fill_path(
            &skia_path,
            &paint,
            tiny_skia::FillRule::Winding,
            xform,
            None,
        );
    }

    if let Some(stroke) = &path.stroke {
        let mut paint = tiny_skia::Paint::default();
        paint.anti_alias = true;
        apply_paint(&mut paint, &stroke.paint, opacity);
        let skia_stroke = tiny_skia::Stroke {
            width: stroke.width,
            ..Default::default()
        };
        pixmap.stroke_path(&skia_path, &paint, &skia_stroke, xform, None);
    }
}

fn build_skia_path(commands: &[PathCommand]) -> Option<tiny_skia::Path> {
    let mut pb = tiny_skia::PathBuilder::new();
    for cmd in commands {
        match cmd {
            PathCommand::MoveTo(p) => pb.move_to(p.x, p.y),
            PathCommand::LineTo(p) => pb.line_to(p.x, p.y),
            PathCommand::QuadTo { control, to } => pb.quad_to(control.x, control.y, to.x, to.y),
            PathCommand::CubicTo { c1, c2, to } => {
                pb.cubic_to(c1.x, c1.y, c2.x, c2.y, to.x, to.y)
            }
            PathCommand::Close => pb.close(),
        }
    }
    pb.finish()
}

fn apply_paint(paint: &mut tiny_skia::Paint, source: &Paint, opacity: f32) {
    match source {
        Paint::Solid(color) => {
            paint.set_color(to_skia_color(color, opacity));
        }
    }
}

fn to_skia_color(color: &Color, opacity: f32) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba(
        color.r.clamp(0.0, 1.0),
        color.g.clamp(0.0, 1.0),
        color.b.clamp(0.0, 1.0),
        (color.a * opacity).clamp(0.0, 1.0),
    )
    .expect("clamped components are within [0, 1]")
}

fn to_skia_transform(t: &Transform) -> tiny_skia::Transform {
    // Our Transform:                 tiny_skia's from_row(sx, ky, kx, sy, tx, ty):
    //   | a c tx |                      | sx kx tx |
    //   | b d ty |                      | ky sy ty |
    tiny_skia::Transform::from_row(t.a, t.b, t.c, t.d, t.tx, t.ty)
}

// The compile-time dyn-safety guarantee for `Rasterize` is covered by the
// `const _: Option<&dyn RasterComponent> = None;` assertion in `RasterComponent`.
