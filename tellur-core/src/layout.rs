//! Layout containers for composing components.
//!
//! All containers participate in the constraint-based layout protocol
//! defined by [`VectorComponent`](crate::vector::VectorComponent) /
//! [`RasterComponent`](crate::raster::RasterComponent): they accept a
//! [`Constraints`] block from the parent, decide their layout size, and
//! then render at that size.
//!
//! - [`Padding`] adds an outer border of empty space around a child.
//! - [`Sized`] picks the outer width / height per axis with `SizeMode`
//!   (Fill / Hug / Fixed) and renders the child top-left.
//! - [`Place`] fills the parent and snaps the child by an anchor pair.
//! - [`Frame`] combines `Sized` + `Place` in one container.
//! - [`Stack`] arranges children along an axis with spacing and
//!   alignment; the `CrossAlign::Stretch` mode propagates a tight
//!   cross-axis constraint so children can fill the stack's cross
//!   extent.
//! - [`DecoratedBox`] paints a background fill (and optionally a border
//!   on the vector variant) behind the child.
//! - [`SizedBox`] is an empty placeholder of a given size.
//!
//! Vector containers live at the module root and operate on
//! `Box<dyn VectorComponent>`. Their raster counterparts share the same
//! names under [`raster`] and operate on `Box<dyn RasterComponent>`.

use crate::color::Color;
pub use crate::geometry::Axis;
use crate::geometry::{Anchor, Constraints, EdgeInsets, Rect, Transform, Vec2};
use crate::vector::{
    Fill, Group, Node, Paint, Path, PathCommand, Stroke, VectorComponent, VectorGraphic,
};

/// Main-axis distribution of children in a [`Stack`]. The `Space*` variants
/// override `Stack::spacing` and derive the gap from the leftover space
/// on the main axis; `Start` / `Center` / `End` keep `Stack::spacing` as
/// the inter-child gap.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MainAlign {
    Start,
    Center,
    End,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

/// Cross-axis alignment of each child inside the stack's cross extent.
/// `Stretch` propagates a tight cross-axis constraint to the child so
/// it can fill the stack's full cross extent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CrossAlign {
    Start,
    Center,
    End,
    Stretch,
}

/// How a sizing-container picks its size on one axis, given the parent's
/// constraints and the child's intrinsic size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SizeMode {
    /// Take the parent's max constraint on this axis (collapse to `0.0`
    /// if the max is unbounded). Equivalent to CSS `width: 100%` or
    /// SwiftUI's `.frame(maxWidth: .infinity)`.
    Fill,
    /// Hug the child's intrinsic size on this axis. The child is
    /// queried for its own preferred size and the result is used.
    Hug,
    /// Use exactly the given number of logical units on this axis.
    Fixed(f32),
}

pub(crate) fn resolve_size_mode<F: FnOnce(Constraints) -> Vec2>(
    width: SizeMode,
    height: SizeMode,
    constraints: Constraints,
    child_layout: F,
) -> Vec2 {
    let needs_hug = matches!(width, SizeMode::Hug) || matches!(height, SizeMode::Hug);
    let hug = needs_hug.then(|| child_layout(constraints));
    let w = match width {
        SizeMode::Fill => finite_axis(constraints.max.0),
        SizeMode::Hug => hug.unwrap().0,
        SizeMode::Fixed(v) => v,
    };
    let h = match height {
        SizeMode::Fill => finite_axis(constraints.max.1),
        SizeMode::Hug => hug.unwrap().1,
        SizeMode::Fixed(v) => v,
    };
    constraints.constrain(Vec2(w, h))
}

pub(crate) fn finite_axis(v: f32) -> f32 {
    if v.is_finite() {
        v
    } else {
        0.0
    }
}

// ─── vector containers ───────────────────────────────────────────────────

/// Wraps a child with empty space on each side.
pub struct Padding {
    pub insets: EdgeInsets,
    pub child: Box<dyn VectorComponent>,
}

impl Padding {
    fn inset_size(&self) -> Vec2 {
        Vec2(self.insets.horizontal(), self.insets.vertical())
    }
}

impl VectorComponent for Padding {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        let inset = self.inset_size();
        let child_size = self.child.layout(constraints.shrink(inset));
        Vec2(child_size.0 + inset.0, child_size.1 + inset.1)
    }

    fn render(&self, size: Vec2) -> VectorGraphic {
        let inset = self.inset_size();
        let inner_size = Vec2((size.0 - inset.0).max(0.0), (size.1 - inset.1).max(0.0));
        let inner = self.child.render(inner_size);
        VectorGraphic {
            view_box: Rect {
                origin: Vec2::ZERO,
                size,
            },
            root: Node::Group(Group {
                transform: Transform::translate(self.insets.top_left()),
                opacity: 1.0,
                children: vec![inner.root],
            }),
        }
    }
}

/// Sizes the outer box independently on each axis (`Fill` / `Hug` /
/// `Fixed`) and places the child at the outer box's top-left.
///
/// Use [`Place`] alone if you need an anchor placement at the parent's
/// max size, or [`Frame`] when you want sizing and anchored placement
/// in one container.
pub struct Sized {
    pub width: SizeMode,
    pub height: SizeMode,
    pub child: Box<dyn VectorComponent>,
}

impl VectorComponent for Sized {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        resolve_size_mode(self.width, self.height, constraints, |c| {
            self.child.layout(c)
        })
    }

    fn render(&self, size: Vec2) -> VectorGraphic {
        let child_size = self.child.layout(Constraints::loose(size));
        let inner = self.child.render(child_size);
        VectorGraphic {
            view_box: Rect {
                origin: Vec2::ZERO,
                size,
            },
            root: inner.root,
        }
    }
}

/// Fills the parent's max constraint and places the child by snapping
/// the child's `child_anchor` onto the `at` anchor of the outer box.
///
/// For an anchor placement that doesn't claim the whole available
/// region, wrap a [`Sized`] inside `Place`, or use [`Frame`] which
/// combines both in one container.
pub struct Place {
    pub child_anchor: Anchor,
    pub at: Anchor,
    pub child: Box<dyn VectorComponent>,
}

impl VectorComponent for Place {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        let max = Vec2(
            finite_axis(constraints.max.0),
            finite_axis(constraints.max.1),
        );
        constraints.constrain(max)
    }

    fn render(&self, size: Vec2) -> VectorGraphic {
        let child_size = self.child.layout(Constraints::loose(size));
        let pos = child_size
            .anchored(self.child_anchor)
            .snap_to(self.at.point(size));
        let inner = self.child.render(child_size);
        VectorGraphic {
            view_box: Rect {
                origin: Vec2::ZERO,
                size,
            },
            root: Node::Group(Group {
                transform: Transform::translate(pos),
                opacity: 1.0,
                children: vec![inner.root],
            }),
        }
    }
}

/// Shorthand for `Sized` + `Place`: declares the outer size on each
/// axis with a `SizeMode` and anchors the child inside that box. Pass
/// `Anchor::TOP_LEFT` for both anchors to get pure top-left placement.
pub struct Frame {
    pub width: SizeMode,
    pub height: SizeMode,
    pub child_anchor: Anchor,
    pub at: Anchor,
    pub child: Box<dyn VectorComponent>,
}

impl VectorComponent for Frame {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        resolve_size_mode(self.width, self.height, constraints, |c| {
            self.child.layout(c)
        })
    }

    fn render(&self, size: Vec2) -> VectorGraphic {
        let child_size = self.child.layout(Constraints::loose(size));
        let pos = child_size
            .anchored(self.child_anchor)
            .snap_to(self.at.point(size));
        let inner = self.child.render(child_size);
        VectorGraphic {
            view_box: Rect {
                origin: Vec2::ZERO,
                size,
            },
            root: Node::Group(Group {
                transform: Transform::translate(pos),
                opacity: 1.0,
                children: vec![inner.root],
            }),
        }
    }
}

/// Arranges children along [`axis`](Self::axis) with
/// [`spacing`](Self::spacing) between them.
///
/// `size` lets the caller pin the stack's own outer size. When `None`,
/// the stack expands to the parent's max constraint on the main axis
/// (or collapses to the intrinsic sum if the constraint is unbounded),
/// and follows the cross-align rule on the cross axis.
pub struct Stack {
    pub axis: Axis,
    pub size: Option<Vec2>,
    pub spacing: f32,
    pub main_align: MainAlign,
    pub cross_align: CrossAlign,
    pub children: Vec<Box<dyn VectorComponent>>,
}

pub(crate) struct StackPass {
    pub own_size: Vec2,
    /// `(position, size)` for each child in the input order.
    pub children: Vec<(Vec2, Vec2)>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn compute_stack_pass(
    axis: Axis,
    explicit_size: Option<Vec2>,
    spacing: f32,
    main_align: MainAlign,
    cross_align: CrossAlign,
    parent_constraints: Constraints,
    child_count: usize,
    mut layout_child: impl FnMut(usize, Constraints) -> Vec2,
) -> StackPass {
    let horizontal = matches!(axis, Axis::Horizontal);
    let (parent_main_max, parent_cross_max) = if horizontal {
        (parent_constraints.max.0, parent_constraints.max.1)
    } else {
        (parent_constraints.max.1, parent_constraints.max.0)
    };
    let explicit_main = explicit_size.map(|s| if horizontal { s.0 } else { s.1 });
    let explicit_cross = explicit_size.map(|s| if horizontal { s.1 } else { s.0 });

    // Decide the cross extent the children should target. `Stretch`
    // tightens children's cross axis to it; other modes leave the cross
    // axis loose and use the child's natural cross size for placement.
    let stretch_cross = match cross_align {
        CrossAlign::Stretch => Some(
            explicit_cross
                .or_else(|| parent_cross_max.is_finite().then_some(parent_cross_max))
                .unwrap_or(0.0),
        ),
        _ => None,
    };

    let base_child_constraints = Constraints::loose(parent_constraints.max);
    let child_constraints = match stretch_cross {
        Some(t) => base_child_constraints.tighten_cross(axis, t),
        None => base_child_constraints,
    };

    let n = child_count;
    let child_sizes: Vec<Vec2> = (0..n).map(|i| layout_child(i, child_constraints)).collect();
    let (mains, crosses): (Vec<f32>, Vec<f32>) = if horizontal {
        child_sizes.iter().map(|s| (s.0, s.1)).unzip()
    } else {
        child_sizes.iter().map(|s| (s.1, s.0)).unzip()
    };

    let total_main_children: f32 = mains.iter().sum();
    let max_cross_children: f32 = crosses.iter().cloned().fold(0.0_f32, f32::max);
    let gap_count = n.saturating_sub(1) as f32;
    let intrinsic_main = total_main_children + spacing * gap_count;

    let own_main = explicit_main.unwrap_or_else(|| {
        if parent_main_max.is_finite() {
            parent_main_max
        } else {
            intrinsic_main
        }
    });
    let own_cross = match cross_align {
        CrossAlign::Stretch => stretch_cross.unwrap_or(max_cross_children),
        _ => explicit_cross.unwrap_or(max_cross_children),
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

    StackPass {
        own_size,
        children: placements,
    }
}

impl VectorComponent for Stack {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        compute_stack_pass(
            self.axis,
            self.size,
            self.spacing,
            self.main_align,
            self.cross_align,
            constraints,
            self.children.len(),
            |i, c| self.children[i].layout(c),
        )
        .own_size
    }

    fn render(&self, size: Vec2) -> VectorGraphic {
        let pass = compute_stack_pass(
            self.axis,
            self.size,
            self.spacing,
            self.main_align,
            self.cross_align,
            Constraints::tight(size),
            self.children.len(),
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

/// Paints a background fill and/or stroke behind a child, sized to the
/// child's layout size. Combine with [`Padding`] for the typical
/// CSS-style "padded box with a background".
pub struct DecoratedBox {
    pub child: Box<dyn VectorComponent>,
    pub background: Option<Paint>,
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

/// An empty box of the given size. Useful as a spacer between stack
/// children or to reserve a region without any visible content.
pub struct SizedBox {
    pub size: Vec2,
}

impl VectorComponent for SizedBox {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        constraints.constrain(self.size)
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
                children: vec![],
            }),
        }
    }
}

/// Fluent extension adding `.padding(...)`, `.background(...)`,
/// `.border(...)` to every [`VectorComponent`]. For `Sized` / `Place` /
/// `Frame` use the struct literal form directly.
pub trait VectorLayoutExt: VectorComponent + ::core::marker::Sized + 'static {
    fn padding(self, insets: EdgeInsets) -> Padding {
        Padding {
            insets,
            child: Box::new(self),
        }
    }

    fn background(self, paint: Paint) -> DecoratedBox {
        DecoratedBox {
            child: Box::new(self),
            background: Some(paint),
            border: None,
        }
    }

    fn border(self, stroke: Stroke) -> DecoratedBox {
        DecoratedBox {
            child: Box::new(self),
            background: None,
            border: Some(stroke),
        }
    }
}

impl<T: VectorComponent + 'static> VectorLayoutExt for T {}

// ─── raster containers ───────────────────────────────────────────────────

pub mod raster {
    //! Raster equivalents of the vector layout containers. Same shape
    //! and semantics; operate on `Box<dyn RasterComponent>`.

    use bytes::Bytes;

    use super::{
        compute_stack_pass, resolve_size_mode, Axis, Color, CrossAlign, EdgeInsets, MainAlign,
        SizeMode, Vec2,
    };
    use crate::geometry::{Anchor, Constraints, Rect};
    use crate::layer::{composite_children, translate_rect, union_rect};
    use crate::raster::{PixelFormat, RasterComponent, RasterImage, Resolution};

    pub struct Padding {
        pub insets: EdgeInsets,
        pub child: Box<dyn RasterComponent>,
    }

    impl Padding {
        fn inset_size(&self) -> Vec2 {
            Vec2(self.insets.horizontal(), self.insets.vertical())
        }
    }

    impl RasterComponent for Padding {
        fn layout(&self, constraints: Constraints) -> Vec2 {
            let inset = self.inset_size();
            let child_size = self.child.layout(constraints.shrink(inset));
            Vec2(child_size.0 + inset.0, child_size.1 + inset.1)
        }

        fn paint_bounds(&self, size: Vec2) -> Rect {
            let inset = self.inset_size();
            let inner_size = Vec2((size.0 - inset.0).max(0.0), (size.1 - inset.1).max(0.0));
            let child_paint = self.child.paint_bounds(inner_size);
            union_rect(
                Rect {
                    origin: Vec2::ZERO,
                    size,
                },
                translate_rect(child_paint, self.insets.top_left()),
            )
        }

        fn render(&self, size: Vec2, target: Resolution) -> RasterImage {
            let inset = self.inset_size();
            let inner_size = Vec2((size.0 - inset.0).max(0.0), (size.1 - inset.1).max(0.0));
            let paint_rect = self.paint_bounds(size);
            composite_children(
                paint_rect,
                target,
                &[(self.insets.top_left(), inner_size, self.child.as_ref())],
            )
        }
    }

    /// Sizes the outer box on each axis (`Fill` / `Hug` / `Fixed`) and
    /// places the child at the outer box's top-left.
    pub struct Sized {
        pub width: SizeMode,
        pub height: SizeMode,
        pub child: Box<dyn RasterComponent>,
    }

    impl RasterComponent for Sized {
        fn layout(&self, constraints: Constraints) -> Vec2 {
            resolve_size_mode(self.width, self.height, constraints, |c| {
                self.child.layout(c)
            })
        }

        fn paint_bounds(&self, size: Vec2) -> Rect {
            let child_size = self.child.layout(Constraints::loose(size));
            let child_paint = self.child.paint_bounds(child_size);
            union_rect(
                Rect {
                    origin: Vec2::ZERO,
                    size,
                },
                child_paint,
            )
        }

        fn render(&self, size: Vec2, target: Resolution) -> RasterImage {
            let child_size = self.child.layout(Constraints::loose(size));
            let paint_rect = self.paint_bounds(size);
            composite_children(
                paint_rect,
                target,
                &[(Vec2::ZERO, child_size, self.child.as_ref())],
            )
        }
    }

    /// Fills the parent's max constraint and places the child via
    /// anchor snapping.
    pub struct Place {
        pub child_anchor: Anchor,
        pub at: Anchor,
        pub child: Box<dyn RasterComponent>,
    }

    impl RasterComponent for Place {
        fn layout(&self, constraints: Constraints) -> Vec2 {
            let max = Vec2(
                super::finite_axis(constraints.max.0),
                super::finite_axis(constraints.max.1),
            );
            constraints.constrain(max)
        }

        fn paint_bounds(&self, size: Vec2) -> Rect {
            let child_size = self.child.layout(Constraints::loose(size));
            let pos = child_size
                .anchored(self.child_anchor)
                .snap_to(self.at.point(size));
            let child_paint = self.child.paint_bounds(child_size);
            union_rect(
                Rect {
                    origin: Vec2::ZERO,
                    size,
                },
                translate_rect(child_paint, pos),
            )
        }

        fn render(&self, size: Vec2, target: Resolution) -> RasterImage {
            let child_size = self.child.layout(Constraints::loose(size));
            let pos = child_size
                .anchored(self.child_anchor)
                .snap_to(self.at.point(size));
            let paint_rect = self.paint_bounds(size);
            composite_children(paint_rect, target, &[(pos, child_size, self.child.as_ref())])
        }
    }

    /// Shorthand for `Sized` + `Place` combined.
    pub struct Frame {
        pub width: SizeMode,
        pub height: SizeMode,
        pub child_anchor: Anchor,
        pub at: Anchor,
        pub child: Box<dyn RasterComponent>,
    }

    impl RasterComponent for Frame {
        fn layout(&self, constraints: Constraints) -> Vec2 {
            resolve_size_mode(self.width, self.height, constraints, |c| {
                self.child.layout(c)
            })
        }

        fn paint_bounds(&self, size: Vec2) -> Rect {
            let child_size = self.child.layout(Constraints::loose(size));
            let pos = child_size
                .anchored(self.child_anchor)
                .snap_to(self.at.point(size));
            let child_paint = self.child.paint_bounds(child_size);
            union_rect(
                Rect {
                    origin: Vec2::ZERO,
                    size,
                },
                translate_rect(child_paint, pos),
            )
        }

        fn render(&self, size: Vec2, target: Resolution) -> RasterImage {
            let child_size = self.child.layout(Constraints::loose(size));
            let pos = child_size
                .anchored(self.child_anchor)
                .snap_to(self.at.point(size));
            let paint_rect = self.paint_bounds(size);
            composite_children(paint_rect, target, &[(pos, child_size, self.child.as_ref())])
        }
    }

    pub struct Stack {
        pub axis: Axis,
        pub size: Option<Vec2>,
        pub spacing: f32,
        pub main_align: MainAlign,
        pub cross_align: CrossAlign,
        pub children: Vec<Box<dyn RasterComponent>>,
    }

    impl RasterComponent for Stack {
        fn layout(&self, constraints: Constraints) -> Vec2 {
            compute_stack_pass(
                self.axis,
                self.size,
                self.spacing,
                self.main_align,
                self.cross_align,
                constraints,
                self.children.len(),
                |i, c| self.children[i].layout(c),
            )
            .own_size
        }

        fn paint_bounds(&self, size: Vec2) -> Rect {
            let pass = compute_stack_pass(
                self.axis,
                self.size,
                self.spacing,
                self.main_align,
                self.cross_align,
                Constraints::tight(size),
                self.children.len(),
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

        fn render(&self, size: Vec2, target: Resolution) -> RasterImage {
            let pass = compute_stack_pass(
                self.axis,
                self.size,
                self.spacing,
                self.main_align,
                self.cross_align,
                Constraints::tight(size),
                self.children.len(),
                |i, c| self.children[i].layout(c),
            );
            let placed: Vec<(Vec2, Vec2, &dyn RasterComponent)> = self
                .children
                .iter()
                .zip(pass.children.iter())
                .map(|(child, &(pos, child_size))| (pos, child_size, child.as_ref()))
                .collect();
            let paint_rect = self.paint_bounds(size);
            composite_children(paint_rect, target, &placed)
        }
    }

    /// Raster decoration. Only solid-color backgrounds are supported for
    /// now; stroking on raster is left to the vector path. For richer
    /// decoration, decorate on the vector side and rasterize after.
    pub struct DecoratedBox {
        pub child: Box<dyn RasterComponent>,
        pub background: Option<Color>,
    }

    impl RasterComponent for DecoratedBox {
        fn layout(&self, constraints: Constraints) -> Vec2 {
            self.child.layout(constraints)
        }

        // paint_bounds intentionally falls back to the default
        // `Rect { origin: 0, size }`, so a `.background()` acts as a
        // clip rectangle for children whose paint bounds spill outward
        // (e.g. drop shadows on outer children).

        fn render(&self, size: Vec2, target: Resolution) -> RasterImage {
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
                    composite_children(paint_rect, target, &placed)
                }
                None => composite_children(
                    paint_rect,
                    target,
                    &[(Vec2::ZERO, size, self.child.as_ref())],
                ),
            }
        }
    }

    pub struct SizedBox {
        pub size: Vec2,
    }

    impl RasterComponent for SizedBox {
        fn layout(&self, constraints: Constraints) -> Vec2 {
            constraints.constrain(self.size)
        }

        fn render(&self, _size: Vec2, target: Resolution) -> RasterImage {
            let bytes = (target.width as usize) * (target.height as usize) * 4;
            RasterImage {
                width: target.width,
                height: target.height,
                format: PixelFormat::Rgba8,
                pixels: Bytes::from(vec![0u8; bytes]),
            }
        }
    }

    /// Internal helper: a solid-color rectangle that fills any layout
    /// size the parent assigns, rasterized by buffer-filling.
    struct SolidRect {
        color: Color,
    }

    impl RasterComponent for SolidRect {
        fn layout(&self, constraints: Constraints) -> Vec2 {
            constraints.constrain(constraints.max)
        }

        fn render(&self, _size: Vec2, target: Resolution) -> RasterImage {
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
            RasterImage {
                width: target.width,
                height: target.height,
                format: PixelFormat::Rgba8,
                pixels: Bytes::from(buf),
            }
        }
    }

    /// Fluent extension mirroring [`super::VectorLayoutExt`] for raster.
    pub trait RasterLayoutExt: RasterComponent + ::core::marker::Sized + 'static {
        fn padding(self, insets: EdgeInsets) -> Padding {
            Padding {
                insets,
                child: Box::new(self),
            }
        }

        fn background(self, color: Color) -> DecoratedBox {
            DecoratedBox {
                child: Box::new(self),
                background: Some(color),
            }
        }
    }

    impl<T: RasterComponent + 'static> RasterLayoutExt for T {}
}
