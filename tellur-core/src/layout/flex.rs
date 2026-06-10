//! [`Flex`]: flexbox-style arrangement of children along one axis.

use crate::geometry::{Axis, Constraints, Rect, Transform, Vec2};
use crate::vector::{Group, Node, VectorComponent, VectorGraphic};
use crate::Keyable;

/// Main-axis distribution of children in a [`Flex`]. The `Space*` variants
/// override `Flex::spacing` and derive the gap from the leftover space
/// on the main axis; `Start` / `Center` / `End` keep `Flex::spacing` as
/// the inter-child gap.
///
/// [`Flexible`] children consume the leftover space *before* alignment
/// distributes it, so with any positive grow weight present the `Space*`
/// variants (and `Center` / `End` offsets) degenerate to `Start` — the same
/// interplay as CSS `flex-grow` vs `justify-content`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MainAlign {
    Start,
    Center,
    End,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

/// Cross-axis alignment of each child inside the flex's cross extent.
/// `Stretch` propagates a tight cross-axis constraint to the child so
/// it can fill the flex's full cross extent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CrossAlign {
    Start,
    Center,
    End,
    Stretch,
}

/// Arranges children along [`axis`](Self::axis) with
/// [`spacing`](Self::spacing) between them, flexbox-style.
///
/// On the main axis the flex expands to the parent's max constraint (or
/// collapses to the intrinsic sum if the constraint is unbounded); pin it
/// with an outer [`Frame`](super::Frame) to size it explicitly. Children
/// wrapped in [`Flexible`] split the leftover main-axis space by their grow
/// weights.
#[crate::component(vector)]
#[derive(Keyable)]
pub struct Flex {
    // `#[builder(field)]` members (the streamed children) must precede the
    // setter members, per bon's member-ordering rule.
    #[children(each = child)]
    pub children: Vec<Box<dyn VectorComponent>>,
    pub axis: Axis,
    #[builder(default)]
    pub spacing: f32,
    #[builder(default = MainAlign::Start)]
    pub main_align: MainAlign,
    #[builder(default = CrossAlign::Start)]
    pub cross_align: CrossAlign,
}

impl Flex {
    fn child_grows(&self) -> Vec<f32> {
        self.children
            .iter()
            .map(|child| {
                // Deref through the Box explicitly: the blanket `DynEq` also
                // matches `Box<dyn VectorComponent>` itself, and resolving
                // `as_any` on the box would wrap the box, not the component.
                child
                    .as_ref()
                    .as_any()
                    .downcast_ref::<Flexible>()
                    .map_or(0.0, |flexible| flexible.grow.max(0.0))
            })
            .collect()
    }
}

pub(super) struct FlexPass {
    pub own_size: Vec2,
    /// `(position, size)` for each child in the input order.
    pub children: Vec<(Vec2, Vec2)>,
}

pub(super) fn compute_flex_pass(
    axis: Axis,
    spacing: f32,
    main_align: MainAlign,
    cross_align: CrossAlign,
    parent_constraints: Constraints,
    grows: &[f32],
    mut layout_child: impl FnMut(usize, Constraints) -> Vec2,
) -> FlexPass {
    let horizontal = matches!(axis, Axis::Horizontal);
    let (parent_main_max, parent_cross_max) = if horizontal {
        (parent_constraints.max.0, parent_constraints.max.1)
    } else {
        (parent_constraints.max.1, parent_constraints.max.0)
    };

    // Decide the cross extent the children should target. `Stretch`
    // tightens children's cross axis to it; other modes leave the cross
    // axis loose and use the child's natural cross size for placement.
    let stretch_cross = match cross_align {
        CrossAlign::Stretch => Some(if parent_cross_max.is_finite() {
            parent_cross_max
        } else {
            0.0
        }),
        _ => None,
    };

    let base_child_constraints = Constraints::loose(parent_constraints.max);
    let child_constraints = match stretch_cross {
        Some(t) => base_child_constraints.tighten_cross(axis, t),
        None => base_child_constraints,
    };

    let n = grows.len();
    let gap_count = n.saturating_sub(1) as f32;
    let total_grow: f32 = grows.iter().copied().filter(|g| *g > 0.0).sum();
    // Grow weights only bite when there is a bounded main extent whose
    // leftover can be shared; under an unbounded main axis every child
    // just takes its intrinsic size (as in CSS, where `flex-grow` is
    // inert without a definite container size).
    let flex_active = parent_main_max.is_finite() && total_grow > 0.0;

    let mut child_sizes: Vec<Vec2> = vec![Vec2::ZERO; n];
    let mut inflexible_main = 0.0_f32;
    for (i, &grow) in grows.iter().enumerate() {
        if flex_active && grow > 0.0 {
            continue;
        }
        let size = layout_child(i, child_constraints);
        inflexible_main += if horizontal { size.0 } else { size.1 };
        child_sizes[i] = size;
    }

    if flex_active {
        // Start/Center/End reserve the authored spacing between children;
        // the Space* modes derive their gaps from leftover space instead,
        // so no spacing is reserved ahead of the grow distribution there.
        let reserved_spacing = match main_align {
            MainAlign::Start | MainAlign::Center | MainAlign::End => spacing * gap_count,
            _ => 0.0,
        };
        let free = (parent_main_max - inflexible_main - reserved_spacing).max(0.0);
        for (i, &grow) in grows.iter().enumerate() {
            if grow <= 0.0 {
                continue;
            }
            let share = free * grow / total_grow;
            child_sizes[i] = layout_child(i, child_constraints.tighten_main(axis, share));
        }
    }

    let (mains, crosses): (Vec<f32>, Vec<f32>) = if horizontal {
        child_sizes.iter().map(|s| (s.0, s.1)).unzip()
    } else {
        child_sizes.iter().map(|s| (s.1, s.0)).unzip()
    };

    let total_main_children: f32 = mains.iter().sum();
    let max_cross_children: f32 = crosses.iter().cloned().fold(0.0_f32, f32::max);
    let intrinsic_main = total_main_children + spacing * gap_count;

    let own_main = if parent_main_max.is_finite() {
        parent_main_max
    } else {
        intrinsic_main
    };
    let own_cross = match cross_align {
        CrossAlign::Stretch => stretch_cross.unwrap_or(max_cross_children),
        _ => max_cross_children,
    };

    let (start_offset, gap) = if n == 0 {
        (0.0, 0.0)
    } else {
        match main_align {
            MainAlign::Start => (0.0, spacing),
            MainAlign::Center => {
                let used = total_main_children + spacing * gap_count;
                ((own_main - used) * 0.5, spacing)
            }
            MainAlign::End => {
                let used = total_main_children + spacing * gap_count;
                (own_main - used, spacing)
            }
            MainAlign::SpaceBetween => {
                let free = (own_main - total_main_children).max(0.0);
                if n >= 2 {
                    (0.0, free / (n - 1) as f32)
                } else {
                    ((own_main - total_main_children) * 0.5, 0.0)
                }
            }
            MainAlign::SpaceAround => {
                let free = (own_main - total_main_children).max(0.0);
                let g = free / n as f32;
                (g * 0.5, g)
            }
            MainAlign::SpaceEvenly => {
                let free = (own_main - total_main_children).max(0.0);
                let g = free / (n + 1) as f32;
                (g, g)
            }
        }
    };

    let mut placements = Vec::with_capacity(n);
    let mut cursor = start_offset;
    for (i, &main_size) in mains.iter().enumerate() {
        let cross_size = crosses[i];
        let cross_pos = match cross_align {
            CrossAlign::Start | CrossAlign::Stretch => 0.0,
            CrossAlign::Center => (own_cross - cross_size) * 0.5,
            CrossAlign::End => own_cross - cross_size,
        };
        let pos = if horizontal {
            Vec2(cursor, cross_pos)
        } else {
            Vec2(cross_pos, cursor)
        };
        placements.push((pos, child_sizes[i]));
        cursor += main_size + gap;
    }

    let own_size = if horizontal {
        Vec2(own_main, own_cross)
    } else {
        Vec2(own_cross, own_main)
    };

    FlexPass {
        own_size: parent_constraints.constrain(own_size),
        children: placements,
    }
}

impl VectorComponent for Flex {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        compute_flex_pass(
            self.axis,
            self.spacing,
            self.main_align,
            self.cross_align,
            constraints,
            &self.child_grows(),
            |i, c| self.children[i].layout(c),
        )
        .own_size
    }

    fn render(&self, size: Vec2) -> VectorGraphic {
        let pass = compute_flex_pass(
            self.axis,
            self.spacing,
            self.main_align,
            self.cross_align,
            Constraints::tight(size),
            &self.child_grows(),
            |i, c| self.children[i].layout(c),
        );
        let nodes: Vec<Node> = self
            .children
            .iter()
            .zip(pass.children.iter())
            .map(|(child, &(pos, child_size))| {
                let inner = child.render(child_size);
                Node::Group(Group {
                    transform: Transform::translate(pos),
                    opacity: 1.0,
                    children: vec![inner.root],
                })
            })
            .collect();
        VectorGraphic {
            view_box: Rect {
                origin: Vec2::ZERO,
                size: pass.own_size,
            },
            root: Node::Group(Group {
                transform: Transform::IDENTITY,
                opacity: 1.0,
                children: nodes,
            }),
        }
    }
}

/// Marks a direct child of [`Flex`] as flexible, the spatial analogue of
/// CSS `flex-grow`.
///
/// After the inflexible siblings take their intrinsic main-axis sizes, the
/// leftover main-axis space is divided between the flexible children in
/// proportion to their `grow` weights, and each is laid out with a tight
/// main-axis constraint at its share.
///
/// `Flex` only recognizes a `Flexible` that is its *direct* child; anywhere
/// else the wrapper is transparent and `grow` has no effect. Construct one
/// with [`VectorFlex::grow`] (`circle.grow(1.0)`), its builder mirror, or
/// [`Flexible::spacer`] for empty space.
#[derive(Keyable)]
pub struct Flexible {
    pub grow: f32,
    pub child: Box<dyn VectorComponent>,
}

impl Flexible {
    pub fn new(grow: f32, child: impl Into<Box<dyn VectorComponent>>) -> Self {
        Self {
            grow,
            child: child.into(),
        }
    }

    /// Empty flexible space: absorbs `grow` shares of the leftover
    /// main-axis space, like an auto margin / `Spacer` in other systems.
    pub fn spacer(grow: f32) -> Self {
        Self {
            grow,
            child: Box::new(super::SizedBox { size: Vec2::ZERO }),
        }
    }
}

impl VectorComponent for Flexible {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        self.child.layout(constraints)
    }

    fn paint_bounds(&self, size: Vec2) -> Rect {
        self.child.paint_bounds(size)
    }

    fn render(&self, size: Vec2) -> VectorGraphic {
        self.child.render(size)
    }
}

impl From<Flexible> for Box<dyn VectorComponent> {
    fn from(flexible: Flexible) -> Self {
        Box::new(flexible)
    }
}

/// Extension trait adding the [`Flexible`] wrapper to every vector
/// component: `circle.grow(1.0)` marks the component to take one share of
/// a parent [`Flex`]'s leftover main-axis space.
pub trait VectorFlex: VectorComponent + Sized + 'static {
    fn grow(self, grow: f32) -> Flexible {
        Flexible {
            grow,
            child: Box::new(self),
        }
    }
}

impl<T: VectorComponent + 'static> VectorFlex for T {}

pub(super) mod raster {
    use super::{compute_flex_pass, CrossAlign, MainAlign};
    use crate::geometry::{Axis, Constraints, Rect, Vec2};
    use crate::layer::{composite_children, translate_rect, union_rect};
    use crate::raster::{RasterComponent, RasterImage, Resolution};
    use crate::render_context::{CachePolicy, RenderContext};
    use crate::Keyable;

    /// Raster mirror of the vector [`Flex`](super::Flex).
    #[crate::component(raster)]
    #[derive(Keyable)]
    pub struct Flex {
        // `#[builder(field)]` members must precede the setter members.
        #[children(each = child)]
        pub children: Vec<Box<dyn RasterComponent>>,
        pub axis: Axis,
        #[builder(default)]
        pub spacing: f32,
        #[builder(default = MainAlign::Start)]
        pub main_align: MainAlign,
        #[builder(default = CrossAlign::Start)]
        pub cross_align: CrossAlign,
    }

    impl Flex {
        fn child_grows(&self) -> Vec<f32> {
            self.children
                .iter()
                .map(|child| {
                    // Deref through the Box explicitly; see the vector
                    // `child_grows` for why.
                    child
                        .as_ref()
                        .as_any()
                        .downcast_ref::<Flexible>()
                        .map_or(0.0, |flexible| flexible.grow.max(0.0))
                })
                .collect()
        }
    }

    impl RasterComponent for Flex {
        fn layout(&self, constraints: Constraints) -> Vec2 {
            compute_flex_pass(
                self.axis,
                self.spacing,
                self.main_align,
                self.cross_align,
                constraints,
                &self.child_grows(),
                |i, c| self.children[i].layout(c),
            )
            .own_size
        }

        fn paint_bounds(&self, size: Vec2) -> Rect {
            let pass = compute_flex_pass(
                self.axis,
                self.spacing,
                self.main_align,
                self.cross_align,
                Constraints::tight(size),
                &self.child_grows(),
                |i, c| self.children[i].layout(c),
            );
            let mut bounds = Rect {
                origin: Vec2::ZERO,
                size,
            };
            for (child, &(pos, child_size)) in self.children.iter().zip(pass.children.iter()) {
                let child_paint = child.paint_bounds(child_size);
                bounds = union_rect(bounds, translate_rect(child_paint, pos));
            }
            bounds
        }

        fn render(
            &self,
            size: Vec2,
            target: Resolution,
            ctx: &mut dyn RenderContext,
        ) -> RasterImage {
            let pass = compute_flex_pass(
                self.axis,
                self.spacing,
                self.main_align,
                self.cross_align,
                Constraints::tight(size),
                &self.child_grows(),
                |i, c| self.children[i].layout(c),
            );
            let placed: Vec<(Vec2, Vec2, &dyn RasterComponent)> = self
                .children
                .iter()
                .zip(pass.children.iter())
                .map(|(child, &(pos, child_size))| (pos, child_size, child.as_ref()))
                .collect();
            let paint_rect = self.paint_bounds(size);
            composite_children(paint_rect, target, &placed, ctx)
        }
    }

    /// Raster mirror of the vector [`Flexible`](super::Flexible). Same
    /// semantics: a grow weight a parent [`Flex`] reads off its direct
    /// children.
    #[derive(Keyable)]
    pub struct Flexible {
        pub grow: f32,
        pub child: Box<dyn RasterComponent>,
    }

    impl Flexible {
        pub fn new(grow: f32, child: impl Into<Box<dyn RasterComponent>>) -> Self {
            Self {
                grow,
                child: child.into(),
            }
        }

        /// Empty flexible space, mirroring [`super::Flexible::spacer`].
        pub fn spacer(grow: f32) -> Self {
            Self {
                grow,
                child: Box::new(crate::layout::raster::SizedBox { size: Vec2::ZERO }),
            }
        }
    }

    impl RasterComponent for Flexible {
        fn layout(&self, constraints: Constraints) -> Vec2 {
            self.child.layout(constraints)
        }

        fn paint_bounds(&self, size: Vec2) -> Rect {
            self.child.paint_bounds(size)
        }

        fn cache_policy(&self) -> CachePolicy {
            // A `Flexible` produces no image of its own — it delegates
            // straight to its child. Stay transparent so the child owns
            // the cache slot, as `Positioned` does.
            CachePolicy::Transparent
        }

        fn render(
            &self,
            size: Vec2,
            target: Resolution,
            ctx: &mut dyn RenderContext,
        ) -> RasterImage {
            ctx.render(self.child.as_ref(), size, target)
        }
    }

    impl From<Flexible> for Box<dyn RasterComponent> {
        fn from(flexible: Flexible) -> Self {
            Box::new(flexible)
        }
    }

    /// Raster mirror of [`VectorFlex`](super::VectorFlex).
    pub trait RasterFlex: RasterComponent + Sized + 'static {
        fn grow(self, grow: f32) -> Flexible {
            Flexible {
                grow,
                child: Box::new(self),
            }
        }
    }

    impl<T: RasterComponent + 'static> RasterFlex for T {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shapes::Rectangle;

    fn rect(w: f32, h: f32) -> Rectangle {
        Rectangle {
            size: Vec2(w, h),
            fill: None,
            stroke: None,
        }
    }

    fn flex_row(children: Vec<Box<dyn VectorComponent>>) -> Flex {
        Flex {
            children,
            axis: Axis::Horizontal,
            spacing: 0.0,
            main_align: MainAlign::Start,
            cross_align: CrossAlign::Start,
        }
    }

    fn child_positions(graphic: &VectorGraphic) -> Vec<Vec2> {
        let Node::Group(root) = &graphic.root else {
            panic!("flex should render a root group");
        };
        root.children
            .iter()
            .map(|node| {
                let Node::Group(group) = node else {
                    panic!("each flex child should be wrapped in a group");
                };
                Vec2(group.transform.tx, group.transform.ty)
            })
            .collect()
    }

    #[test]
    fn flex_grow_splits_leftover_space() {
        let flex = flex_row(vec![
            rect(20.0, 10.0).boxed(),
            Flexible::new(1.0, rect(0.0, 10.0)).into(),
            Flexible::new(3.0, rect(0.0, 10.0)).into(),
        ]);
        let size = flex.layout(Constraints::loose(Vec2(100.0, 50.0)));
        assert_eq!(size, Vec2(100.0, 10.0));

        let graphic = flex.render(size);
        // 100 - 20 = 80 leftover, split 1:3 → 20 and 60.
        assert_eq!(
            child_positions(&graphic),
            vec![Vec2(0.0, 0.0), Vec2(20.0, 0.0), Vec2(40.0, 0.0)]
        );
    }

    #[test]
    fn flex_spacer_pushes_following_children_to_the_end() {
        let flex = flex_row(vec![
            rect(20.0, 10.0).boxed(),
            Flexible::spacer(1.0).into(),
            rect(30.0, 10.0).boxed(),
        ]);
        let graphic = flex.render(flex.layout(Constraints::loose(Vec2(100.0, 50.0))));
        assert_eq!(
            child_positions(&graphic),
            vec![Vec2(0.0, 0.0), Vec2(20.0, 0.0), Vec2(70.0, 0.0)]
        );
    }

    #[test]
    fn flex_grow_reserves_spacing_for_start_alignment() {
        let flex = Flex {
            children: vec![
                rect(20.0, 10.0).boxed(),
                Flexible::spacer(1.0).into(),
                rect(30.0, 10.0).boxed(),
            ],
            axis: Axis::Horizontal,
            spacing: 10.0,
            main_align: MainAlign::Start,
            cross_align: CrossAlign::Start,
        };
        let graphic = flex.render(flex.layout(Constraints::loose(Vec2(100.0, 50.0))));
        // 100 - 20 - 30 - 2 gaps × 10 = 30 for the spacer.
        assert_eq!(
            child_positions(&graphic),
            vec![Vec2(0.0, 0.0), Vec2(30.0, 0.0), Vec2(70.0, 0.0)]
        );
    }

    #[test]
    fn flex_grow_is_inert_under_unbounded_main_axis() {
        let flex = flex_row(vec![
            rect(20.0, 10.0).boxed(),
            Flexible::new(1.0, rect(40.0, 10.0)).into(),
        ]);
        // No bounded main extent to share: children take intrinsic sizes.
        assert_eq!(flex.layout(Constraints::UNBOUNDED), Vec2(60.0, 10.0));
    }

    #[test]
    fn flex_without_grow_keeps_main_align_distribution() {
        let flex = Flex {
            children: vec![rect(20.0, 10.0).boxed(), rect(20.0, 10.0).boxed()],
            axis: Axis::Horizontal,
            spacing: 0.0,
            main_align: MainAlign::SpaceBetween,
            cross_align: CrossAlign::Start,
        };
        let graphic = flex.render(flex.layout(Constraints::loose(Vec2(100.0, 50.0))));
        assert_eq!(
            child_positions(&graphic),
            vec![Vec2(0.0, 0.0), Vec2(80.0, 0.0)]
        );
    }

    #[test]
    fn flex_grow_via_extension_method() {
        use super::VectorFlex;
        let flex = flex_row(vec![
            rect(20.0, 10.0).boxed(),
            rect(0.0, 10.0).grow(1.0).into(),
        ]);
        let graphic = flex.render(flex.layout(Constraints::loose(Vec2(100.0, 50.0))));
        assert_eq!(
            child_positions(&graphic),
            vec![Vec2(0.0, 0.0), Vec2(20.0, 0.0)]
        );
    }
}
