use tellur_core::geometry::{Constraints, Rect, Vec2};
use tellur_core::vector::{Node, VectorComponent, VectorGraphic};

#[tellur_core::component(vector)]
#[derive(Clone, tellur_core::Keyable)]
struct StructLayers {
    #[children(each = under)]
    unders: Vec<u32>,
    #[children(each = over)]
    overs: Vec<u32>,
    base: u32,
}

impl VectorComponent for StructLayers {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        constraints.constrain(Vec2(self.base as f32, 1.0))
    }

    fn render(&self, size: Vec2) -> VectorGraphic {
        VectorGraphic {
            view_box: Rect {
                origin: Vec2::ZERO,
                size,
            },
            root: Node::empty(),
        }
    }
}

#[tellur_core::component(vector)]
fn FunctionLayers(
    #[children(each = under)] unders: Vec<u32>,
    #[children(each = over)] overs: Vec<u32>,
    base: u32,
) -> impl VectorComponent {
    StructLayers {
        unders,
        base,
        overs,
    }
}

#[test]
fn struct_form_generates_independent_setter_families() {
    let layers = StructLayers::builder()
        .under(1_u32)
        .unders([2_u32, 3])
        .maybe_under(Some(4_u32))
        .maybe_under(None::<u32>)
        .maybe_unders(Some([5_u32, 6]))
        .maybe_unders(None::<[u32; 0]>)
        .base(10)
        .over(7_u32)
        .overs([8_u32, 9])
        .maybe_over(Some(10_u32))
        .maybe_over(None::<u32>)
        .maybe_overs(Some([11_u32, 12]))
        .maybe_overs(None::<[u32; 0]>)
        .build();

    assert_eq!(layers.unders, [1, 2, 3, 4, 5, 6]);
    assert_eq!(layers.overs, [7, 8, 9, 10, 11, 12]);
}

#[test]
fn function_form_generates_independent_setter_families() {
    let layers = FunctionLayers::builder()
        .under(1_u32)
        .maybe_under(Some(2_u32))
        .unders([3_u32, 4])
        .base(10)
        .over(5_u32)
        .maybe_over(Some(6_u32))
        .overs([7_u32, 8])
        .build();

    assert_eq!(layers.unders, [1, 2, 3, 4]);
    assert_eq!(layers.overs, [5, 6, 7, 8]);
}
