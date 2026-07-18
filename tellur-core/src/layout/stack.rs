//! [`Stack`]: layers decorations and overlays around one size-defining child.

use crate::geometry::{Constraints, Rect, Transform, Vec2};
use crate::layer::union_rect;
use crate::vector::{Group, Node, VectorComponent, VectorGraphic};
use crate::Keyable;

/// Layers zero or more `under` children behind one size-defining `base`, then
/// zero or more `over` children in front.
///
/// Only `base` participates in the stack's layout: it receives the caller's
/// constraints unchanged, and its result is the stack size. Every underlay and
/// overlay is then laid out with constraints tight to that resolved size. This
/// makes fill-style components and anchor-targeted
/// [`Positioned`](crate::placement::Positioned) resolve against the base box.
/// Placement itself stays outside `Stack`; wrap a slot child in `Positioned`
/// when it should move relative to the base.
///
/// Paint order is `unders` (in insertion order), `base`, then `overs` (in
/// insertion order). Slot children do not affect layout, but any paint that
/// spills beyond the base box is included in [`paint_bounds`](VectorComponent::paint_bounds).
#[crate::component(vector)]
#[derive(Clone, Keyable)]
pub struct Stack {
    // `#[builder(field)]` members must precede setter members.
    #[children(each = under)]
    pub unders: Vec<Box<dyn VectorComponent>>,
    #[children(each = over)]
    pub overs: Vec<Box<dyn VectorComponent>>,
    #[builder(into)]
    pub base: Box<dyn VectorComponent>,
}

impl Stack {
    fn overlay_size(child: &dyn VectorComponent, size: Vec2) -> Vec2 {
        child.layout(Constraints::tight(size))
    }
}

impl VectorComponent for Stack {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        self.base.layout(constraints)
    }

    fn paint_bounds(&self, size: Vec2) -> Rect {
        self.unders
            .iter()
            .chain(&self.overs)
            .fold(self.base.paint_bounds(size), |bounds, child| {
                let child_size = Self::overlay_size(child.as_ref(), size);
                union_rect(bounds, child.paint_bounds(child_size))
            })
    }

    fn render(&self, size: Vec2) -> VectorGraphic {
        let mut children = Vec::with_capacity(self.unders.len() + 1 + self.overs.len());
        children.extend(self.unders.iter().map(|child| {
            let child_size = Self::overlay_size(child.as_ref(), size);
            child.render(child_size).root
        }));
        children.push(self.base.render(size).root);
        children.extend(self.overs.iter().map(|child| {
            let child_size = Self::overlay_size(child.as_ref(), size);
            child.render(child_size).root
        }));

        VectorGraphic {
            view_box: self.paint_bounds(size),
            root: Node::Group(Group {
                transform: Transform::IDENTITY,
                opacity: 1.0,
                children,
            }),
        }
    }
}

/// Raster counterpart of [`Stack`].
pub(super) mod raster {
    use crate::geometry::{Constraints, Rect, Vec2};
    use crate::layer::{composite_children, union_rect};
    use crate::raster::{RasterComponent, RasterImage, RasterResidency, Resolution};
    use crate::render_context::RenderContext;
    use crate::Keyable;

    /// Raster stack with the same base-sized layout and under/base/over paint
    /// order as the vector [`Stack`](super::Stack).
    #[crate::component(raster)]
    #[derive(Clone, Keyable)]
    pub struct Stack {
        // `#[builder(field)]` members must precede setter members.
        #[children(each = under)]
        pub unders: Vec<Box<dyn RasterComponent>>,
        #[children(each = over)]
        pub overs: Vec<Box<dyn RasterComponent>>,
        #[builder(into)]
        pub base: Box<dyn RasterComponent>,
    }

    impl Stack {
        fn overlay_size(child: &dyn RasterComponent, size: Vec2) -> Vec2 {
            child.layout(Constraints::tight(size))
        }
    }

    impl RasterComponent for Stack {
        fn layout(&self, constraints: Constraints) -> Vec2 {
            self.base.layout(constraints)
        }

        fn paint_bounds(&self, size: Vec2) -> Rect {
            self.unders.iter().chain(&self.overs).fold(
                self.base.paint_bounds(size),
                |bounds, child| {
                    let child_size = Self::overlay_size(child.as_ref(), size);
                    union_rect(bounds, child.paint_bounds(child_size))
                },
            )
        }

        fn render(
            &self,
            size: Vec2,
            target: Resolution,
            residency: RasterResidency,
            ctx: &mut dyn RenderContext,
        ) -> RasterImage {
            let paint_rect = self.paint_bounds(size);
            let mut placed: Vec<(Vec2, Vec2, &dyn RasterComponent)> =
                Vec::with_capacity(self.unders.len() + 1 + self.overs.len());
            placed.extend(self.unders.iter().map(|child| {
                (
                    Vec2::ZERO,
                    Self::overlay_size(child.as_ref(), size),
                    child.as_ref(),
                )
            }));
            placed.push((Vec2::ZERO, size, self.base.as_ref()));
            placed.extend(self.overs.iter().map(|child| {
                (
                    Vec2::ZERO,
                    Self::overlay_size(child.as_ref(), size),
                    child.as_ref(),
                )
            }));
            composite_children(paint_rect, target, &placed, residency, ctx)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Color;
    use crate::composite::composite_at;
    use crate::geometry::Anchor;
    use crate::layout::{Frame, SizeMode};
    use crate::placement::{RasterPlacement, VectorPlacement};
    use crate::raster::{
        CpuRasterImage, PixelFormat, RasterComponent, RasterImage, RasterResidency, Resolution,
    };
    use crate::render_context::{PassThrough, RenderContext};
    use crate::shapes::Rectangle;
    use crate::vector::{Fill, Paint};

    fn rect(size: Vec2, color: Color) -> Rectangle {
        Rectangle {
            size,
            fill: Some(Fill {
                paint: Paint::Solid(color),
            }),
            stroke: None,
        }
    }

    #[derive(Clone, PartialEq, Eq, Hash)]
    struct ConstraintProbe;

    impl VectorComponent for ConstraintProbe {
        fn layout(&self, constraints: Constraints) -> Vec2 {
            constraints.constrain(Vec2(constraints.min.0 + 7.0, constraints.max.1 - 11.0))
        }

        fn render(&self, size: Vec2) -> VectorGraphic {
            VectorGraphic {
                view_box: Rect {
                    origin: Vec2::ZERO,
                    size,
                },
                root: Node::Group(Group {
                    transform: Transform::IDENTITY,
                    opacity: 1.0,
                    children: Vec::new(),
                }),
            }
        }
    }

    #[test]
    fn base_receives_constraints_unchanged_and_defines_stack_size() {
        let stack = Stack {
            unders: Vec::new(),
            overs: Vec::new(),
            base: ConstraintProbe.boxed(),
        };
        let constraints = Constraints {
            min: Vec2(10.0, 20.0),
            max: Vec2(110.0, 220.0),
        };

        assert_eq!(stack.layout(constraints), Vec2(17.0, 209.0));
    }

    #[derive(Clone, PartialEq, Eq, Hash)]
    struct TightProbe {
        expected: Vec2,
    }

    impl VectorComponent for TightProbe {
        fn layout(&self, constraints: Constraints) -> Vec2 {
            assert_eq!(constraints, Constraints::tight(self.expected));
            self.expected
        }

        fn render(&self, size: Vec2) -> VectorGraphic {
            VectorGraphic {
                view_box: Rect {
                    origin: Vec2::ZERO,
                    size,
                },
                root: Node::Group(Group {
                    transform: Transform::IDENTITY,
                    opacity: 1.0,
                    children: Vec::new(),
                }),
            }
        }
    }

    #[test]
    fn underlays_and_overlays_receive_base_sized_tight_constraints() {
        let size = Vec2(40.0, 30.0);
        let stack = Stack {
            unders: vec![TightProbe { expected: size }.boxed()],
            overs: vec![TightProbe { expected: size }.boxed()],
            base: rect(size, Color::rgb_u8(0, 0, 0)).boxed(),
        };

        assert_eq!(stack.layout(Constraints::UNBOUNDED), size);
        assert_eq!(
            stack.paint_bounds(size),
            Rect {
                origin: Vec2::ZERO,
                size,
            }
        );
        assert_eq!(stack.render(size).view_box.size, size);
    }

    #[derive(Clone, PartialEq, Eq, Hash)]
    struct LayoutOnlyBase {
        size: Vec2,
    }

    impl VectorComponent for LayoutOnlyBase {
        fn layout(&self, constraints: Constraints) -> Vec2 {
            constraints.constrain(self.size)
        }

        fn paint_bounds(&self, _size: Vec2) -> Rect {
            Rect {
                origin: Vec2::ZERO,
                size: Vec2::ZERO,
            }
        }

        fn render(&self, _size: Vec2) -> VectorGraphic {
            VectorGraphic {
                view_box: Rect {
                    origin: Vec2::ZERO,
                    size: Vec2::ZERO,
                },
                root: Node::empty(),
            }
        }
    }

    #[crate::component(vector)]
    fn AvailableOverlay(#[available] available: Vec2, expected: Vec2) -> impl VectorComponent {
        assert_eq!(available, expected);
        rect(available, Color::rgb_u8(0, 0, 0))
    }

    #[test]
    fn fill_and_available_overlays_resolve_to_the_base_size() {
        let size = Vec2(40.0, 30.0);
        let expected = Rect {
            origin: Vec2::ZERO,
            size,
        };
        let base = || LayoutOnlyBase { size }.boxed();

        let fill = Frame::builder()
            .width(SizeMode::Fill)
            .height(SizeMode::Fill)
            .child(rect(Vec2(1.0, 1.0), Color::rgb_u8(0, 0, 0)));
        let fill_stack = Stack::builder().under(fill).base(base()).build();
        assert_eq!(fill_stack.paint_bounds(size), expected);

        let available = AvailableOverlay::builder().expected(size);
        let available_stack = Stack::builder().base(base()).over(available).build();
        assert_eq!(available_stack.paint_bounds(size), expected);
        assert_eq!(available_stack.render(size).view_box, expected);
    }

    #[test]
    fn anchor_target_places_an_overlay_relative_to_base_edges() {
        let size = Vec2(100.0, 60.0);
        let chip = rect(Vec2(20.0, 10.0), Color::rgb_u8(255, 0, 0))
            .anchored(Anchor::CENTER_LEFT)
            .snap_to(Anchor::TOP_LEFT)
            .offset(Vec2(28.0, 0.0));
        let stack = Stack {
            unders: Vec::new(),
            overs: vec![chip.boxed()],
            base: rect(size, Color::rgb_u8(0, 0, 0)).boxed(),
        };

        let expected_bounds = Rect {
            origin: Vec2(0.0, -5.0),
            size: Vec2(100.0, 65.0),
        };
        assert_eq!(stack.paint_bounds(size), expected_bounds);

        let graphic = stack.render(size);
        assert_eq!(graphic.view_box, expected_bounds);
        let Node::Group(root) = graphic.root else {
            panic!("stack should render a root group");
        };
        let Node::Group(positioned) = &root.children[1] else {
            panic!("overlay should be rendered through Positioned");
        };
        assert_eq!(
            Vec2(positioned.transform.tx, positioned.transform.ty),
            Vec2(28.0, -5.0)
        );
    }

    #[test]
    fn overlay_spill_expands_paint_bounds_and_vector_view_box() {
        let size = Vec2(10.0, 10.0);
        let stack = Stack {
            unders: Vec::new(),
            overs: vec![rect(Vec2(4.0, 4.0), Color::rgb_u8(255, 0, 0))
                .place_at(Vec2(8.0, 9.0))
                .boxed()],
            base: rect(size, Color::rgb_u8(0, 0, 0)).boxed(),
        };
        let expected = Rect {
            origin: Vec2::ZERO,
            size: Vec2(12.0, 13.0),
        };

        assert_eq!(stack.paint_bounds(size), expected);
        assert_eq!(stack.render(size).view_box, expected);
    }

    #[test]
    fn vector_paint_order_is_unders_then_base_then_overs() {
        let size = Vec2(8.0, 6.0);
        let under_a = rect(size, Color::rgb_u8(10, 0, 0));
        let under_b = rect(size, Color::rgb_u8(20, 0, 0));
        let base = rect(size, Color::rgb_u8(30, 0, 0));
        let over_a = rect(size, Color::rgb_u8(40, 0, 0));
        let over_b = rect(size, Color::rgb_u8(50, 0, 0));
        let expected = vec![
            under_a.render(size).root,
            under_b.render(size).root,
            base.render(size).root,
            over_a.render(size).root,
            over_b.render(size).root,
        ];
        let stack = Stack {
            unders: vec![under_a.boxed(), under_b.boxed()],
            overs: vec![over_a.boxed(), over_b.boxed()],
            base: base.boxed(),
        };

        let Node::Group(root) = stack.render(size).root else {
            panic!("stack should render a root group");
        };
        assert_eq!(root.children, expected);
    }

    #[test]
    fn builder_supports_each_and_maybe_slot_setters() {
        let size = Vec2(8.0, 6.0);
        let stack = Stack::builder()
            .under(rect(size, Color::rgb_u8(10, 0, 0)))
            .maybe_under(None::<Rectangle>)
            .base(rect(size, Color::rgb_u8(20, 0, 0)))
            .maybe_over(Some(rect(size, Color::rgb_u8(30, 0, 0))))
            .maybe_over(None::<Rectangle>)
            .build();

        assert_eq!(stack.unders.len(), 1);
        assert_eq!(stack.overs.len(), 1);
    }

    #[derive(Clone, PartialEq, Eq, Hash)]
    struct SolidRaster {
        intrinsic: Vec2,
        rgba: [u8; 4],
    }

    impl RasterComponent for SolidRaster {
        fn layout(&self, constraints: Constraints) -> Vec2 {
            constraints.constrain(self.intrinsic)
        }

        fn render(
            &self,
            _size: Vec2,
            target: Resolution,
            _residency: RasterResidency,
            _ctx: &mut dyn RenderContext,
        ) -> RasterImage {
            RasterImage::cpu(
                target.width,
                target.height,
                PixelFormat::Rgba8,
                self.rgba.repeat((target.width * target.height) as usize),
            )
        }
    }

    fn solid_raster(intrinsic: Vec2, rgba: [u8; 4]) -> SolidRaster {
        SolidRaster { intrinsic, rgba }
    }

    #[derive(Clone, PartialEq, Eq, Hash)]
    struct TightRasterProbe {
        expected: Vec2,
    }

    impl RasterComponent for TightRasterProbe {
        fn layout(&self, constraints: Constraints) -> Vec2 {
            assert_eq!(constraints, Constraints::tight(self.expected));
            self.expected
        }

        fn render(
            &self,
            _size: Vec2,
            target: Resolution,
            _residency: RasterResidency,
            _ctx: &mut dyn RenderContext,
        ) -> RasterImage {
            RasterImage::cpu(
                target.width,
                target.height,
                PixelFormat::Rgba8,
                vec![0; (target.width * target.height * 4) as usize],
            )
        }
    }

    #[test]
    fn raster_underlays_and_overlays_receive_base_sized_tight_constraints() {
        let size = Vec2(4.0, 3.0);
        let stack = raster::Stack {
            unders: vec![TightRasterProbe { expected: size }.boxed()],
            overs: vec![TightRasterProbe { expected: size }.boxed()],
            base: solid_raster(size, [0, 0, 255, 255]).boxed(),
        };

        assert_eq!(stack.layout(Constraints::UNBOUNDED), size);
        assert_eq!(
            stack.paint_bounds(size),
            Rect {
                origin: Vec2::ZERO,
                size,
            }
        );
        let _ = stack.render(
            size,
            Resolution::new(4, 3),
            RasterResidency::Cpu,
            &mut PassThrough,
        );
    }

    #[test]
    fn raster_anchor_overlay_uses_base_box_and_spill_is_not_clipped() {
        let size = Vec2(4.0, 4.0);
        let stack = raster::Stack {
            unders: Vec::new(),
            overs: vec![solid_raster(Vec2(2.0, 2.0), [255, 0, 0, 255])
                .anchored(Anchor::TOP_LEFT)
                .snap_to(Anchor::BOTTOM_RIGHT)
                .boxed()],
            base: solid_raster(size, [0, 0, 255, 255]).boxed(),
        };
        let expected = Rect {
            origin: Vec2::ZERO,
            size: Vec2(6.0, 6.0),
        };
        assert_eq!(stack.paint_bounds(size), expected);

        let image = stack
            .render(
                size,
                Resolution::new(6, 6),
                RasterResidency::Cpu,
                &mut PassThrough,
            )
            .into_cpu()
            .expect("stack renders on CPU");
        let bottom_right = &image.pixels[(5 * (6 * 4) + 5 * 4)..][..4];
        assert_eq!(bottom_right, [255, 0, 0, 255]);
    }

    fn composite_pixel(layers: &[[u8; 4]]) -> [u8; 4] {
        let mut pixel = [0; 4];
        for rgba in layers {
            let layer = CpuRasterImage::new(1, 1, PixelFormat::Rgba8, rgba.to_vec());
            composite_at(&mut pixel, Resolution::new(1, 1), &layer, 0, 0);
        }
        pixel
    }

    #[test]
    fn raster_paint_order_is_unders_then_base_then_overs() {
        let size = Vec2(1.0, 1.0);
        let layers = [
            [255, 0, 0, 96],
            [0, 255, 0, 96],
            [0, 0, 255, 96],
            [255, 255, 0, 96],
            [255, 0, 255, 96],
        ];
        let stack = raster::Stack {
            unders: vec![
                solid_raster(size, layers[0]).boxed(),
                solid_raster(size, layers[1]).boxed(),
            ],
            overs: vec![
                solid_raster(size, layers[3]).boxed(),
                solid_raster(size, layers[4]).boxed(),
            ],
            base: solid_raster(size, layers[2]).boxed(),
        };

        let image = stack
            .render(
                size,
                Resolution::new(1, 1),
                RasterResidency::Cpu,
                &mut PassThrough,
            )
            .into_cpu()
            .expect("stack renders on CPU");
        assert_eq!(&image.pixels[..4], composite_pixel(&layers));
    }
}
