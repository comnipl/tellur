//! [`DecoratedBox`]: background / border decoration behind a child.

use crate::geometry::{Constraints, Rect, Transform, Vec2};
use crate::vector::{
    Fill, Group, Node, Paint, Path, PathCommand, Stroke, VectorComponent, VectorGraphic,
};
use crate::Keyable;

/// Paints a background fill and/or stroke behind a child, sized to the
/// child's layout size. Combine with [`Padding`](super::Padding) for the
/// typical CSS-style "padded box with a background".
#[crate::component(vector)]
#[derive(Keyable)]
pub struct DecoratedBox {
    #[builder(into)]
    pub child: Box<dyn VectorComponent>,
    #[builder(into)]
    pub background: Option<Paint>,
    #[builder(into)]
    pub border: Option<Stroke>,
}

impl VectorComponent for DecoratedBox {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        self.child.layout(constraints)
    }

    fn render(&self, size: Vec2) -> VectorGraphic {
        let inner = self.child.render(size);
        let mut children: Vec<Node> = Vec::new();
        if self.background.is_some() || self.border.is_some() {
            children.push(Node::Path(Path {
                commands: vec![
                    PathCommand::MoveTo(Vec2(0.0, 0.0)),
                    PathCommand::LineTo(Vec2(size.0, 0.0)),
                    PathCommand::LineTo(Vec2(size.0, size.1)),
                    PathCommand::LineTo(Vec2(0.0, size.1)),
                    PathCommand::Close,
                ],
                fill: self.background.clone().map(|paint| Fill { paint }),
                stroke: self.border.clone(),
                transform: Transform::IDENTITY,
            }));
        }
        children.push(inner.root);
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

pub(super) mod raster {
    use crate::color::Color;
    use crate::geometry::{Constraints, Rect, Vec2};
    use crate::layer::composite_children;
    use crate::raster::{PixelFormat, RasterComponent, RasterImage, Resolution};
    use crate::render_context::RenderContext;
    use crate::Keyable;

    /// Raster decoration. Only solid-color backgrounds are supported for
    /// now; stroking on raster is left to the vector path. For richer
    /// decoration, decorate on the vector side and rasterize after.
    #[crate::component(raster)]
    #[derive(Keyable)]
    pub struct DecoratedBox {
        #[builder(into)]
        pub child: Box<dyn RasterComponent>,
        #[builder(into)]
        pub background: Option<Color>,
    }

    impl RasterComponent for DecoratedBox {
        fn layout(&self, constraints: Constraints) -> Vec2 {
            self.child.layout(constraints)
        }

        // paint_bounds intentionally falls back to the default
        // `Rect { origin: 0, size }`, so a `DecoratedBox` acts as a
        // clip rectangle for children whose paint bounds spill outward
        // (e.g. drop shadows on outer children).

        fn render(
            &self,
            size: Vec2,
            target: Resolution,
            ctx: &mut dyn RenderContext,
        ) -> RasterImage {
            let paint_rect = Rect {
                origin: Vec2::ZERO,
                size,
            };
            match self.background {
                Some(color) => {
                    let bg = SolidRect { color };
                    let placed: Vec<(Vec2, Vec2, &dyn RasterComponent)> = vec![
                        (Vec2::ZERO, size, &bg as &dyn RasterComponent),
                        (Vec2::ZERO, size, self.child.as_ref()),
                    ];
                    composite_children(paint_rect, target, &placed, ctx)
                }
                None => composite_children(
                    paint_rect,
                    target,
                    &[(Vec2::ZERO, size, self.child.as_ref())],
                    ctx,
                ),
            }
        }
    }

    /// Internal helper: a solid-color rectangle that fills any layout
    /// size the parent assigns, rasterized by buffer-filling.
    #[derive(PartialEq, Hash)]
    struct SolidRect {
        color: Color,
    }

    impl RasterComponent for SolidRect {
        fn layout(&self, constraints: Constraints) -> Vec2 {
            constraints.constrain(constraints.max)
        }

        fn render(
            &self,
            _size: Vec2,
            target: Resolution,
            ctx: &mut dyn RenderContext,
        ) -> RasterImage {
            if ctx.prefers_gpu() {
                if let Some(gpu) = ctx.gpu_backend() {
                    if let Some(image) = gpu.solid_fill(target, self.color) {
                        return image;
                    }
                }
            }
            let pixels = (target.width as usize) * (target.height as usize);
            let mut buf = Vec::with_capacity(pixels * 4);
            let r = (self.color.r * 255.0).round().clamp(0.0, 255.0) as u8;
            let g = (self.color.g * 255.0).round().clamp(0.0, 255.0) as u8;
            let b = (self.color.b * 255.0).round().clamp(0.0, 255.0) as u8;
            let a = (self.color.a * 255.0).round().clamp(0.0, 255.0) as u8;
            for _ in 0..pixels {
                buf.push(r);
                buf.push(g);
                buf.push(b);
                buf.push(a);
            }
            RasterImage::cpu(target.width, target.height, PixelFormat::Rgba8, buf)
        }
    }
}
