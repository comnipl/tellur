use tellur_core::builder::RasterEffect;
use tellur_core::geometry::{Constraints, Rect, Vec2};
use tellur_core::raster::{PixelFormat, RasterComponent, RasterImage, RasterResidency, Resolution};
use tellur_core::render_context::{PassThrough, RenderContext};

#[derive(Clone, PartialEq, Hash)]
struct ProbeRaster;

impl RasterComponent for ProbeRaster {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        constraints.constrain(Vec2(23.0, 11.0))
    }

    fn paint_bounds(&self, size: Vec2) -> Rect {
        Rect {
            origin: Vec2(-3.0, -4.0),
            size: Vec2(size.0 + 6.0, size.1 + 8.0),
        }
    }

    fn render(
        &self,
        size: Vec2,
        target: Resolution,
        _residency: RasterResidency,
        _ctx: &mut dyn RenderContext,
    ) -> RasterImage {
        let pixel = [size.0 as u8, size.1 as u8, target.width as u8, 255];
        let pixels = pixel.repeat((target.width * target.height) as usize);
        RasterImage::cpu(target.width, target.height, PixelFormat::Rgba8, pixels)
    }
}

#[derive(Clone, tellur_core::Keyable)]
struct ForwardingRaster {
    child: Box<dyn RasterComponent>,
}

impl RasterComponent for ForwardingRaster {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        self.child.layout(constraints)
    }

    fn paint_bounds(&self, size: Vec2) -> Rect {
        self.child.paint_bounds(size)
    }

    fn render(
        &self,
        size: Vec2,
        target: Resolution,
        residency: RasterResidency,
        ctx: &mut dyn RenderContext,
    ) -> RasterImage {
        ctx.render(self.child.as_ref(), size, target, residency)
    }
}

#[tellur_core::component(raster)]
fn ForwardingEffect(#[effect] child: Box<dyn RasterComponent>) -> impl RasterComponent {
    ForwardingRaster { child }
}

#[test]
fn function_component_accepts_and_forwards_a_boxed_effect_child() {
    let effect = ProbeRaster.effect(ForwardingEffect::builder());

    let constraints = Constraints::loose(Vec2(100.0, 100.0));
    assert_eq!(effect.layout(constraints), Vec2(23.0, 11.0));

    let size = Vec2(23.0, 11.0);
    assert_eq!(
        effect.paint_bounds(size),
        Rect {
            origin: Vec2(-3.0, -4.0),
            size: Vec2(29.0, 19.0),
        }
    );

    let image = effect
        .render(
            size,
            Resolution::new(2, 1),
            RasterResidency::Cpu,
            &mut PassThrough,
        )
        .into_cpu()
        .expect("probe renders a CPU image");
    assert_eq!(image.width, 2);
    assert_eq!(image.height, 1);
    assert_eq!(image.pixels.as_ref(), &[23, 11, 2, 255, 23, 11, 2, 255]);
}
