use bytes::Bytes;
use tellur_core::color::Color;
use tellur_core::component::Component;
use tellur_core::geometry::{Rect, Transform};
use tellur_core::raster::{PixelFormat, RasterComponent, RasterImage, Resolution};
use tellur_core::vector::{Node, Paint, Path, PathCommand, VectorComponent, VectorGraphic};

/// A `RasterComponent` that rasterizes a `VectorComponent` at the resolution
/// requested by the caller of `render`.
pub struct Rasterize<V: VectorComponent> {
    pub vector: V,
}

impl<V: VectorComponent> Component for Rasterize<V> {}

impl<V: VectorComponent> RasterComponent for Rasterize<V> {
    fn render(&self, target: Resolution) -> RasterImage {
        let graphic = self.vector.render();
        rasterize(&graphic, target.width, target.height)
    }
}

fn rasterize(graphic: &VectorGraphic, width: u32, height: u32) -> RasterImage {
    let mut pixmap = tiny_skia::Pixmap::new(width, height)
        .expect("pixmap dimensions must be non-zero");

    let view_box_xform = view_box_transform(&graphic.view_box, width, height);
    render_node(&mut pixmap, &graphic.root, view_box_xform);

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
    let sx = width as f32 / view_box.size.0;
    let sy = height as f32 / view_box.size.1;
    let tx = -view_box.origin.0 * sx;
    let ty = -view_box.origin.1 * sy;
    tiny_skia::Transform::from_row(sx, 0.0, 0.0, sy, tx, ty)
}

fn render_node(pixmap: &mut tiny_skia::Pixmap, node: &Node, parent_xform: tiny_skia::Transform) {
    match node {
        Node::Group(group) => {
            let xform = parent_xform.pre_concat(to_skia_transform(&group.transform));
            if group.opacity >= 1.0 {
                for child in &group.children {
                    render_node(pixmap, child, xform);
                }
            } else if group.opacity > 0.0 {
                // Children are rendered into a separate layer, then composited
                // with the group's opacity. This is required for correct alpha
                // blending of overlapping descendants; multiplying opacity into
                // each child's alpha would double-darken overlap regions.
                let mut layer = tiny_skia::Pixmap::new(pixmap.width(), pixmap.height())
                    .expect("pixmap dimensions must be non-zero");
                for child in &group.children {
                    render_node(&mut layer, child, xform);
                }
                let pp = tiny_skia::PixmapPaint {
                    opacity: group.opacity,
                    ..Default::default()
                };
                pixmap.draw_pixmap(
                    0,
                    0,
                    layer.as_ref(),
                    &pp,
                    tiny_skia::Transform::identity(),
                    None,
                );
            }
            // opacity <= 0.0: skip the group entirely.
        }
        Node::Path(path) => {
            let xform = parent_xform.pre_concat(to_skia_transform(&path.transform));
            render_path(pixmap, path, xform);
        }
    }
}

fn render_path(pixmap: &mut tiny_skia::Pixmap, path: &Path, xform: tiny_skia::Transform) {
    let Some(skia_path) = build_skia_path(&path.commands) else {
        return;
    };

    if let Some(fill) = &path.fill {
        let mut paint = tiny_skia::Paint {
            anti_alias: true,
            ..Default::default()
        };
        apply_paint(&mut paint, &fill.paint);
        pixmap.fill_path(
            &skia_path,
            &paint,
            tiny_skia::FillRule::Winding,
            xform,
            None,
        );
    }

    if let Some(stroke) = &path.stroke {
        let mut paint = tiny_skia::Paint {
            anti_alias: true,
            ..Default::default()
        };
        apply_paint(&mut paint, &stroke.paint);
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
            PathCommand::MoveTo(p) => pb.move_to(p.0, p.1),
            PathCommand::LineTo(p) => pb.line_to(p.0, p.1),
            PathCommand::QuadTo { control, to } => pb.quad_to(control.0, control.1, to.0, to.1),
            PathCommand::CubicTo { c1, c2, to } => {
                pb.cubic_to(c1.0, c1.1, c2.0, c2.1, to.0, to.1)
            }
            PathCommand::Close => pb.close(),
        }
    }
    pb.finish()
}

fn apply_paint(paint: &mut tiny_skia::Paint, source: &Paint) {
    match source {
        Paint::Solid(color) => {
            paint.set_color(to_skia_color(color));
        }
    }
}

fn to_skia_color(color: &Color) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba(
        color.r.clamp(0.0, 1.0),
        color.g.clamp(0.0, 1.0),
        color.b.clamp(0.0, 1.0),
        color.a.clamp(0.0, 1.0),
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
