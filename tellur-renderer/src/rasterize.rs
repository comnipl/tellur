use std::hash::Hash;

use tellur_core::color::Color;
use tellur_core::geometry::{Constraints, Rect, Transform, Vec2};
use tellur_core::raster::{PixelFormat, RasterComponent, RasterImage, RasterResidency, Resolution};
use tellur_core::render_context::RenderContext;
use tellur_core::vector::{
    DashPattern, Node, Paint, Path, PathCommand, VectorComponent, VectorGraphic,
};

/// A `RasterComponent` that rasterizes a `VectorComponent`. The layout
/// protocol forwards to the wrapped vector: layout / paint_bounds /
/// render(size) all delegate, and `render(size, target, residency, ctx)`
/// rasterizes the vector's `render(size)` output into a `target`-sized image.
/// Before backend dispatch it normalizes the graphic's `view_box` to the
/// vector's `paint_bounds(size)`, defensively enforcing the vector contract.
/// Non-positive paint bounds produce a transparent target without dispatching
/// an invalid transform to a raster backend.
#[derive(PartialEq, Hash)]
pub struct Rasterize<V: VectorComponent> {
    pub vector: V,
}

impl<V: VectorComponent + PartialEq + Hash + 'static> RasterComponent for Rasterize<V> {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        self.vector.layout(constraints)
    }

    fn paint_bounds(&self, size: Vec2) -> Rect {
        self.vector.paint_bounds(size)
    }

    fn render(
        &self,
        size: Vec2,
        target: Resolution,
        residency: RasterResidency,
        ctx: &mut dyn RenderContext,
    ) -> RasterImage {
        let mut graphic = self.vector.render(size);
        // A component's paint bounds are authoritative for the raster target.
        // Enforce the VectorComponent contract here so a stale view box cannot
        // distort or clip either the GPU path or the CPU fallback below.
        graphic.view_box = self.vector.paint_bounds(size);
        if graphic.view_box.size.0 <= 0.0 || graphic.view_box.size.1 <= 0.0 {
            let pixels = vec![0; target.width as usize * target.height as usize * 4];
            let image = RasterImage::cpu(target.width, target.height, PixelFormat::Rgba8, pixels);
            return ctx.ensure_residency(image, residency);
        }
        if ctx.prefers_gpu() {
            if let Some(gpu) = ctx.gpu_backend() {
                if let Some(image) = gpu.rasterize(&graphic, target) {
                    return ctx.ensure_residency(image, residency);
                }
            }
        }
        let image = rasterize(&graphic, target.width, target.height);
        ctx.ensure_residency(image, residency)
    }
}

/// Extension trait that lets any `VectorComponent` be turned into a
/// `RasterComponent` via `.rasterize()`.
pub trait Rasterizable: VectorComponent + Sized {
    fn rasterize(self) -> Rasterize<Self> {
        Rasterize { vector: self }
    }
}

impl<T: VectorComponent> Rasterizable for T {}

/// Lets a rasterized vector flow into a parent's
/// `child(impl Into<Box<dyn RasterComponent>>)` slot.
impl<V: VectorComponent + PartialEq + Hash + 'static> From<Rasterize<V>>
    for Box<dyn RasterComponent>
{
    fn from(r: Rasterize<V>) -> Self {
        Box::new(r)
    }
}

/// Builder-side `.rasterize()`: a complete vector-component *builder*
/// rasterizes without an explicit `.build()`, mirroring [`Rasterizable`].
pub trait RasterizableBuilder: tellur_core::builder::VectorBuilder {
    fn rasterize(self) -> Rasterize<Self::Output> {
        Rasterize {
            vector: self.build_component(),
        }
    }
}

impl<B: tellur_core::builder::VectorBuilder> RasterizableBuilder for B {}

fn rasterize(graphic: &VectorGraphic, width: u32, height: u32) -> RasterImage {
    let mut pixmap =
        tiny_skia::Pixmap::new(width, height).expect("pixmap dimensions must be non-zero");

    let view_box_xform = view_box_transform(graphic.view_box, width, height);
    render_node(&mut pixmap, &graphic.root, view_box_xform);

    // tiny-skia outputs premultiplied alpha for efficient compositing, but
    // `RasterImage` is defined as straight alpha (matching PNG, web, and most
    // image libraries). Demultiply here so the public type stays consistent.
    let mut straight = Vec::with_capacity(pixmap.data().len());
    for p in pixmap.pixels() {
        let c = p.demultiply();
        straight.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }

    RasterImage::cpu(width, height, PixelFormat::Rgba8, straight)
}

/// Transform that maps the graphic's local coordinate space
/// `view_box.origin..view_box.origin + view_box.size` into pixel space
/// `(0, 0)..(width, height)`. Equivalent to SVG's
/// `preserveAspectRatio="none"` (each axis is scaled independently). The
/// `view_box.origin` offset shifts the graphic so the top-left of
/// `view_box` lands on pixel `(0, 0)`, which is required for effects
/// like drop shadows whose paint bounds extend into negative
/// coordinates.
fn view_box_transform(view_box: Rect, width: u32, height: u32) -> tiny_skia::Transform {
    debug_assert!(
        view_box.size.0 > 0.0 && view_box.size.1 > 0.0,
        "view box dimensions must be positive"
    );
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
        Node::SingleGroup(group) => {
            let xform = parent_xform.pre_concat(to_skia_transform(&group.transform));
            if group.opacity >= 1.0 {
                render_node(pixmap, &group.child, xform);
            } else if group.opacity > 0.0 {
                let mut layer = tiny_skia::Pixmap::new(pixmap.width(), pixmap.height())
                    .expect("pixmap dimensions must be non-zero");
                render_node(&mut layer, &group.child, xform);
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
        }
        Node::ClipGroup(group) => {
            render_clipped_node(pixmap, group, parent_xform);
        }
        Node::Path(path) => {
            let xform = parent_xform.pre_concat(to_skia_transform(&path.transform));
            render_path(pixmap, path, xform);
        }
    }
}

fn render_clipped_node(
    pixmap: &mut tiny_skia::Pixmap,
    group: &tellur_core::vector::ClipGroup,
    parent_xform: tiny_skia::Transform,
) {
    let Some(clip_path) = build_skia_path(&group.commands) else {
        return;
    };
    let clip_xform = parent_xform.pre_concat(to_skia_transform(&group.transform));
    let Some(mut mask) = tiny_skia::Mask::new(pixmap.width(), pixmap.height()) else {
        return;
    };
    mask.fill_path(&clip_path, tiny_skia::FillRule::Winding, true, clip_xform);

    let mut layer = tiny_skia::Pixmap::new(pixmap.width(), pixmap.height())
        .expect("pixmap dimensions must be non-zero");
    render_node(&mut layer, &group.child, parent_xform);
    pixmap.draw_pixmap(
        0,
        0,
        layer.as_ref(),
        &tiny_skia::PixmapPaint::default(),
        tiny_skia::Transform::identity(),
        Some(&mask),
    );
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
            dash: to_skia_dash(stroke.dash.as_ref()),
            ..Default::default()
        };
        pixmap.stroke_path(&skia_path, &paint, &skia_stroke, xform, None);
    }
}

/// Converts a [`DashPattern`] to tiny-skia's stroke-time dashing. `None` both
/// when there is no pattern and when the pattern cannot draw any dashes (see
/// [`DashPattern::normalized_lengths`]) — either way the stroke draws solid.
fn to_skia_dash(dash: Option<&DashPattern>) -> Option<tiny_skia::StrokeDash> {
    let dash = dash?;
    let lengths = dash.normalized_lengths()?;
    tiny_skia::StrokeDash::new(lengths, dash.offset)
}

fn build_skia_path(commands: &[PathCommand]) -> Option<tiny_skia::Path> {
    let mut pb = tiny_skia::PathBuilder::new();
    for cmd in commands {
        match cmd {
            PathCommand::MoveTo(p) => pb.move_to(p.0, p.1),
            PathCommand::LineTo(p) => pb.line_to(p.0, p.1),
            PathCommand::QuadTo { control, to } => pb.quad_to(control.0, control.1, to.0, to.1),
            PathCommand::CubicTo { c1, c2, to } => pb.cubic_to(c1.0, c1.1, c2.0, c2.1, to.0, to.1),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::any::Any;
    use std::sync::Arc;

    use tellur_core::fragment::Fragment;
    use tellur_core::geometry::Rect;
    use tellur_core::raster::{CpuRasterImage, GpuSurface};
    use tellur_core::render_context::{
        CompositeInput, DropShadowInput, GpuPreference, GpuRasterBackend, OutlineInput, PassThrough,
    };
    use tellur_core::shapes::Rectangle;

    const TEST_GPU_BACKEND: &str = "tellur-rasterize-test";

    #[derive(Default)]
    struct TestGpu {
        rasterizes: usize,
        last_view_box: Option<Rect>,
    }

    impl TestGpu {
        fn image(target: Resolution) -> RasterImage {
            let image = CpuRasterImage::new(
                target.width,
                target.height,
                PixelFormat::Rgba8,
                vec![0; target.width as usize * target.height as usize * 4],
            );
            RasterImage::Gpu(GpuSurface::new(
                target.width,
                target.height,
                PixelFormat::Rgba8,
                TEST_GPU_BACKEND,
                Arc::new(image),
            ))
        }
    }

    impl GpuRasterBackend for TestGpu {
        fn upload(&mut self, image: &CpuRasterImage) -> Option<RasterImage> {
            Some(RasterImage::Gpu(GpuSurface::new(
                image.width,
                image.height,
                image.format,
                TEST_GPU_BACKEND,
                Arc::new(image.clone()),
            )))
        }

        fn composite(
            &mut self,
            _target: Resolution,
            _inputs: &[CompositeInput<'_>],
        ) -> Option<RasterImage> {
            None
        }

        fn drop_shadow(&mut self, _input: DropShadowInput<'_>) -> Option<RasterImage> {
            None
        }

        fn outline(&mut self, _input: OutlineInput<'_>) -> Option<RasterImage> {
            None
        }

        fn rasterize(
            &mut self,
            graphic: &VectorGraphic,
            target: Resolution,
        ) -> Option<RasterImage> {
            self.rasterizes += 1;
            self.last_view_box = Some(graphic.view_box);
            Some(Self::image(target))
        }

        fn solid_fill(&mut self, _target: Resolution, _color: Color) -> Option<RasterImage> {
            None
        }

        fn temporal_average(
            &mut self,
            _target: Resolution,
            _frames: &[&RasterImage],
            _total: u32,
        ) -> Option<RasterImage> {
            None
        }

        fn readback(&mut self, image: RasterImage) -> Option<CpuRasterImage> {
            match image {
                RasterImage::Cpu(image) => Some(image),
                RasterImage::Gpu(surface) if surface.backend() == TEST_GPU_BACKEND => {
                    Arc::downcast::<CpuRasterImage>(surface.handle_arc())
                        .ok()
                        .map(|image| (*image).clone())
                }
                RasterImage::Gpu(_) => None,
            }
        }
    }

    struct TestContext {
        gpu: TestGpu,
        preference: GpuPreference,
    }

    impl Default for TestContext {
        fn default() -> Self {
            Self {
                gpu: TestGpu::default(),
                preference: GpuPreference::PreferGpu,
            }
        }
    }

    impl RenderContext for TestContext {
        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }

        fn gpu_preference(&self) -> GpuPreference {
            self.preference
        }

        fn gpu_backend(&mut self) -> Option<&mut dyn GpuRasterBackend> {
            Some(&mut self.gpu)
        }

        fn render(
            &mut self,
            component: &dyn RasterComponent,
            size: Vec2,
            target: Resolution,
            residency: RasterResidency,
        ) -> RasterImage {
            let image = component.render(size, target, residency, self);
            self.ensure_residency(image, residency)
        }
    }

    fn alpha_at(image: &RasterImage, x: u32, y: u32, width: u32) -> u8 {
        let cpu = image
            .as_cpu()
            .expect("rasterize() always returns a CPU image");
        cpu.pixels[((y * width + x) * 4 + 3) as usize]
    }

    #[derive(PartialEq, Eq, Hash)]
    struct StaleViewBoxVector;

    impl VectorComponent for StaleViewBoxVector {
        fn layout(&self, _constraints: Constraints) -> Vec2 {
            Vec2(10.0, 10.0)
        }

        fn paint_bounds(&self, _size: Vec2) -> Rect {
            Rect {
                origin: Vec2(-2.0, -3.0),
                size: Vec2(15.0, 17.0),
            }
        }

        fn render(&self, size: Vec2) -> VectorGraphic {
            VectorGraphic {
                // Deliberately violate the component contract. Rasterize must
                // repair this before selecting the GPU or CPU path.
                view_box: Rect {
                    origin: Vec2::ZERO,
                    size,
                },
                // This square lies outside the stale 0..10 view box but inside
                // paint_bounds. The CPU regression test can therefore observe
                // normalization in actual pixels.
                root: Node::Path(Path {
                    commands: vec![
                        PathCommand::MoveTo(Vec2(10.0, 10.0)),
                        PathCommand::LineTo(Vec2(12.0, 10.0)),
                        PathCommand::LineTo(Vec2(12.0, 12.0)),
                        PathCommand::LineTo(Vec2(10.0, 12.0)),
                        PathCommand::Close,
                    ],
                    fill: Some(Paint::Solid(Color::rgb_u8(0, 0, 0)).into()),
                    stroke: None,
                    transform: Transform::IDENTITY,
                }),
            }
        }
    }

    #[test]
    fn to_skia_dash_is_none_without_a_pattern() {
        assert!(to_skia_dash(None).is_none());
    }

    #[test]
    fn cpu_result_can_use_gpu_execution() {
        let component = Rasterize {
            vector: Rectangle {
                size: Vec2(1.0, 1.0),
                fill: None,
                stroke: None,
            },
        };
        let mut ctx = TestContext::default();

        let image = component.render(
            Vec2(1.0, 1.0),
            Resolution::new(1, 1),
            RasterResidency::Cpu,
            &mut ctx,
        );

        assert!(image.as_cpu().is_some());
        assert_eq!(ctx.gpu.rasterizes, 1);
    }

    #[test]
    fn rasterize_enforces_paint_bounds_before_gpu_dispatch() {
        let component = Rasterize {
            vector: StaleViewBoxVector,
        };
        let mut ctx = TestContext::default();
        let size = Vec2(10.0, 10.0);

        component.render(
            size,
            Resolution::new(15, 17),
            RasterResidency::Gpu,
            &mut ctx,
        );

        assert_eq!(
            ctx.gpu.last_view_box,
            Some(component.vector.paint_bounds(size))
        );
    }

    #[test]
    fn rasterize_enforces_paint_bounds_in_cpu_output() {
        let component = Rasterize {
            vector: StaleViewBoxVector,
        };
        let mut ctx = PassThrough;
        let size = Vec2(10.0, 10.0);
        let target = Resolution::new(15, 17);

        let stale = rasterize(&component.vector.render(size), target.width, target.height);
        assert_eq!(
            alpha_at(&stale, 13, 14, target.width),
            0,
            "the component's stale view box clips this geometry"
        );

        let image = component.render(size, target, RasterResidency::Cpu, &mut ctx);

        assert!(
            alpha_at(&image, 13, 14, target.width) > 0,
            "geometry outside the stale view box remains visible"
        );
    }

    #[test]
    fn non_positive_paint_bounds_return_a_transparent_target() {
        let component = Rasterize {
            vector: Fragment::empty(),
        };
        let mut ctx = TestContext::default();
        ctx.preference = GpuPreference::Disabled;
        let size = component.layout(Constraints::tight(Vec2(10.0, 10.0)));

        let image = component.render(size, Resolution::new(3, 2), RasterResidency::Gpu, &mut ctx);

        assert_eq!(size, Vec2(10.0, 10.0));
        assert_eq!(ctx.gpu.rasterizes, 0);
        assert_eq!(image.residency(), RasterResidency::Gpu);
        let cpu = ctx.readback(image);
        assert_eq!((cpu.width, cpu.height), (3, 2));
        assert!(cpu.pixels.iter().all(|&byte| byte == 0));
    }

    #[test]
    fn to_skia_dash_is_none_for_a_degenerate_pattern() {
        let dash = DashPattern::new(vec![0.0, 0.0], 0.0);
        assert!(to_skia_dash(Some(&dash)).is_none());
    }

    #[test]
    fn to_skia_dash_builds_from_a_valid_pattern() {
        let dash = DashPattern::new(vec![4.0, 4.0], 0.0);
        assert!(to_skia_dash(Some(&dash)).is_some());
    }

    #[test]
    fn dashed_stroke_leaves_gaps_along_the_line() {
        // 4-on/4-off dashes along a horizontal line spanning the full
        // 20x10 view box (1 logical unit == 1 pixel, so sampled x positions
        // land squarely inside a dash or a gap).
        let stroke = tellur_core::vector::Stroke {
            paint: Paint::Solid(Color::rgba_u8(0, 0, 0, 255)),
            width: 2.0,
            dash: Some(DashPattern::new(vec![4.0, 4.0], 0.0)),
        };
        let graphic = VectorGraphic {
            view_box: Rect {
                origin: Vec2::ZERO,
                size: Vec2(20.0, 10.0),
            },
            root: Node::Path(Path {
                commands: vec![
                    PathCommand::MoveTo(Vec2(0.0, 5.0)),
                    PathCommand::LineTo(Vec2(20.0, 5.0)),
                ],
                fill: None,
                stroke: Some(stroke),
                transform: Transform::IDENTITY,
            }),
        };

        let image = rasterize(&graphic, 20, 10);
        assert!(
            alpha_at(&image, 2, 5, 20) > 0,
            "x=2 sits inside the first dash (0..4)"
        );
        assert_eq!(
            alpha_at(&image, 6, 5, 20),
            0,
            "x=6 sits inside the first gap (4..8)"
        );
        assert!(
            alpha_at(&image, 10, 5, 20) > 0,
            "x=10 sits inside the second dash (8..12)"
        );
    }

    #[test]
    fn undashed_stroke_paints_the_whole_line() {
        let stroke = tellur_core::vector::Stroke {
            paint: Paint::Solid(Color::rgba_u8(0, 0, 0, 255)),
            width: 2.0,
            dash: None,
        };
        let graphic = VectorGraphic {
            view_box: Rect {
                origin: Vec2::ZERO,
                size: Vec2(20.0, 10.0),
            },
            root: Node::Path(Path {
                commands: vec![
                    PathCommand::MoveTo(Vec2(0.0, 5.0)),
                    PathCommand::LineTo(Vec2(20.0, 5.0)),
                ],
                fill: None,
                stroke: Some(stroke),
                transform: Transform::IDENTITY,
            }),
        };

        let image = rasterize(&graphic, 20, 10);
        assert!(alpha_at(&image, 6, 5, 20) > 0, "a solid stroke has no gaps");
    }
}
