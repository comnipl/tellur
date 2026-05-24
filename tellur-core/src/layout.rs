//! Layout containers for composing components.
//!
//! These containers describe CSS-Box / Flexbox-style arrangements without
//! introducing a constraint-based layout pass: every container reports its
//! `view_box` as a pure function of its children's intrinsic sizes (plus
//! any explicit sizing it owns), matching the existing
//! [`VectorComponent`](crate::vector::VectorComponent) /
//! [`RasterComponent`](crate::raster::RasterComponent) model.
//!
//! - [`Padding`] adds an outer border of empty space around a child.
//! - [`Align`] places a child inside a fixed-size box at a chosen anchor.
//! - [`Stack`] arranges children along an axis with spacing and alignment.
//! - [`DecoratedBox`] paints a background fill (and optionally a border for
//!   the vector variant) underneath its child.
//! - [`SizedBox`] is an empty placeholder of a given size, useful as a
//!   spacer or to reserve a region.
//!
//! Vector containers live at the module root and operate on
//! `Box<dyn VectorComponent>`. Their raster counterparts share the same
//! names under [`raster`] and operate on `Box<dyn RasterComponent>`.

use crate::color::Color;
use crate::geometry::{Anchor, EdgeInsets, Transform, Vec2};
use crate::vector::{
    Fill, Group, Node, Paint, Path, PathCommand, Stroke, VectorComponent, VectorGraphic,
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

/// Main-axis distribution of children in a [`Stack`]. The `Space*` variants
/// override `Stack::spacing` and derive the gap from the leftover space on
/// the main axis; `Start` / `Center` / `End` keep `Stack::spacing` as the
/// inter-child gap.
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
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CrossAlign {
    Start,
    Center,
    End,
}

// ─── shared stack measurement ────────────────────────────────────────────

pub(crate) struct StackLayout {
    pub own_size: Vec2,
    /// Top-left position of each child in the stack's coordinate space,
    /// same length as the input `child_sizes`.
    pub placements: Vec<Vec2>,
}

/// Pure layout math shared by the vector and raster [`Stack`] variants.
///
/// `explicit_size` is the stack's outer size when the caller set it
/// (`Stack::size`); otherwise the layout collapses to the intrinsic size
/// `sum(children) + spacing*(n-1)` on the main axis and `max(children)` on
/// the cross axis, and `main_align` / `cross_align` become no-ops because
/// there is no free space to distribute.
pub(crate) fn compute_stack_layout(
    axis: Axis,
    explicit_size: Option<Vec2>,
    spacing: f32,
    main_align: MainAlign,
    cross_align: CrossAlign,
    child_sizes: &[Vec2],
) -> StackLayout {
    let n = child_sizes.len();
    let (mains, crosses): (Vec<f32>, Vec<f32>) = match axis {
        Axis::Horizontal => child_sizes.iter().map(|s| (s.0, s.1)).unzip(),
        Axis::Vertical => child_sizes.iter().map(|s| (s.1, s.0)).unzip(),
    };
    let total_main_children: f32 = mains.iter().sum();
    let max_cross_children: f32 = crosses.iter().cloned().fold(0.0_f32, f32::max);

    let gap_count = n.saturating_sub(1) as f32;
    let intrinsic_main = total_main_children + spacing * gap_count;
    let intrinsic_cross = max_cross_children;

    let (own_main, own_cross) = match explicit_size {
        Some(s) => match axis {
            Axis::Horizontal => (s.0, s.1),
            Axis::Vertical => (s.1, s.0),
        },
        None => (intrinsic_main, intrinsic_cross),
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
            CrossAlign::Start => 0.0,
            CrossAlign::Center => (own_cross - cross_size) * 0.5,
            CrossAlign::End => own_cross - cross_size,
        };
        let pos = match axis {
            Axis::Horizontal => Vec2(cursor, cross_pos),
            Axis::Vertical => Vec2(cross_pos, cursor),
        };
        placements.push(pos);
        cursor += main_size + gap;
    }

    let own_size = match axis {
        Axis::Horizontal => Vec2(own_main, own_cross),
        Axis::Vertical => Vec2(own_cross, own_main),
    };

    StackLayout {
        own_size,
        placements,
    }
}

// ─── vector containers ───────────────────────────────────────────────────

/// Wraps a child with empty space on each side.
pub struct Padding {
    pub insets: EdgeInsets,
    pub child: Box<dyn VectorComponent>,
}

impl VectorComponent for Padding {
    fn view_box(&self) -> Vec2 {
        let c = self.child.view_box();
        Vec2(c.0 + self.insets.horizontal(), c.1 + self.insets.vertical())
    }

    fn render(&self) -> VectorGraphic {
        let inner = self.child.render();
        let view_box = Vec2(
            inner.view_box.0 + self.insets.horizontal(),
            inner.view_box.1 + self.insets.vertical(),
        );
        VectorGraphic {
            view_box,
            root: Node::Group(Group {
                transform: Transform::translate(self.insets.top_left()),
                opacity: 1.0,
                children: vec![inner.root],
            }),
        }
    }
}

/// Places a child inside a fixed-size box, snapping the child's `anchor`
/// onto the same anchor point of the parent box. For example,
/// `anchor: Anchor::CENTER` produces center-in-box; `Anchor::BOTTOM_RIGHT`
/// pins to the bottom-right corner.
pub struct Align {
    pub size: Vec2,
    pub anchor: Anchor,
    pub child: Box<dyn VectorComponent>,
}

impl VectorComponent for Align {
    fn view_box(&self) -> Vec2 {
        self.size
    }

    fn render(&self) -> VectorGraphic {
        let inner = self.child.render();
        let pos = inner
            .view_box
            .anchored(self.anchor)
            .snap_to(self.anchor.point(self.size));
        VectorGraphic {
            view_box: self.size,
            root: Node::Group(Group {
                transform: Transform::translate(pos),
                opacity: 1.0,
                children: vec![inner.root],
            }),
        }
    }
}

/// Arranges children along [`axis`](Self::axis) with [`spacing`](Self::spacing)
/// between them.
///
/// When [`size`](Self::size) is `None`, the stack reports its intrinsic
/// extent and `main_align` / `cross_align` only have an observable effect
/// for the cross axis (no free space on the main axis). Set `size` to
/// `Some(...)` to expand into a fixed box and let `main_align` distribute
/// the leftover main-axis space.
pub struct Stack {
    pub axis: Axis,
    pub size: Option<Vec2>,
    pub spacing: f32,
    pub main_align: MainAlign,
    pub cross_align: CrossAlign,
    pub children: Vec<Box<dyn VectorComponent>>,
}

impl VectorComponent for Stack {
    fn view_box(&self) -> Vec2 {
        let sizes: Vec<Vec2> = self.children.iter().map(|c| c.view_box()).collect();
        compute_stack_layout(
            self.axis,
            self.size,
            self.spacing,
            self.main_align,
            self.cross_align,
            &sizes,
        )
        .own_size
    }

    fn render(&self) -> VectorGraphic {
        let sizes: Vec<Vec2> = self.children.iter().map(|c| c.view_box()).collect();
        let layout = compute_stack_layout(
            self.axis,
            self.size,
            self.spacing,
            self.main_align,
            self.cross_align,
            &sizes,
        );
        let nodes: Vec<Node> = self
            .children
            .iter()
            .zip(layout.placements.iter())
            .map(|(child, pos)| {
                let inner = child.render();
                Node::Group(Group {
                    transform: Transform::translate(*pos),
                    opacity: 1.0,
                    children: vec![inner.root],
                })
            })
            .collect();
        VectorGraphic {
            view_box: layout.own_size,
            root: Node::Group(Group {
                transform: Transform::IDENTITY,
                opacity: 1.0,
                children: nodes,
            }),
        }
    }
}

/// Paints a background fill and/or stroke behind a child, sized to the
/// child's own view_box. Combine with [`Padding`] for the typical
/// CSS-style "padded box with a background".
pub struct DecoratedBox {
    pub child: Box<dyn VectorComponent>,
    pub background: Option<Paint>,
    pub border: Option<Stroke>,
}

impl VectorComponent for DecoratedBox {
    fn view_box(&self) -> Vec2 {
        self.child.view_box()
    }

    fn render(&self) -> VectorGraphic {
        let inner = self.child.render();
        let size = inner.view_box;
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
            view_box: size,
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
    fn view_box(&self) -> Vec2 {
        self.size
    }

    fn render(&self) -> VectorGraphic {
        VectorGraphic {
            view_box: self.size,
            root: Node::Group(Group {
                transform: Transform::IDENTITY,
                opacity: 1.0,
                children: vec![],
            }),
        }
    }
}

/// Fluent extension adding `.padding(...)`, `.background(...)`,
/// `.border(...)`, `.align(...)` to every [`VectorComponent`].
pub trait VectorLayoutExt: VectorComponent + Sized + 'static {
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

    fn align(self, size: Vec2, anchor: Anchor) -> Align {
        Align {
            size,
            anchor,
            child: Box::new(self),
        }
    }
}

impl<T: VectorComponent + 'static> VectorLayoutExt for T {}

// ─── raster containers ───────────────────────────────────────────────────

pub mod raster {
    //! Raster equivalents of the vector layout containers. Same shape and
    //! semantics; operate on `Box<dyn RasterComponent>`.

    use bytes::Bytes;

    use super::{compute_stack_layout, Axis, Color, CrossAlign, EdgeInsets, MainAlign, Vec2};
    use crate::geometry::Anchor;
    use crate::layer::composite_children;
    use crate::raster::{PixelFormat, RasterComponent, RasterImage, Resolution};

    pub struct Padding {
        pub insets: EdgeInsets,
        pub child: Box<dyn RasterComponent>,
    }

    impl RasterComponent for Padding {
        fn view_box(&self) -> Vec2 {
            let c = self.child.view_box();
            Vec2(c.0 + self.insets.horizontal(), c.1 + self.insets.vertical())
        }

        fn render(&self, target: Resolution) -> RasterImage {
            let outer = self.view_box();
            composite_children(
                outer,
                target,
                &[(self.insets.top_left(), self.child.as_ref())],
            )
        }
    }

    pub struct Align {
        pub size: Vec2,
        pub anchor: Anchor,
        pub child: Box<dyn RasterComponent>,
    }

    impl RasterComponent for Align {
        fn view_box(&self) -> Vec2 {
            self.size
        }

        fn render(&self, target: Resolution) -> RasterImage {
            let child_size = self.child.view_box();
            let pos = child_size
                .anchored(self.anchor)
                .snap_to(self.anchor.point(self.size));
            composite_children(self.size, target, &[(pos, self.child.as_ref())])
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
        fn view_box(&self) -> Vec2 {
            let sizes: Vec<Vec2> = self.children.iter().map(|c| c.view_box()).collect();
            compute_stack_layout(
                self.axis,
                self.size,
                self.spacing,
                self.main_align,
                self.cross_align,
                &sizes,
            )
            .own_size
        }

        fn render(&self, target: Resolution) -> RasterImage {
            let sizes: Vec<Vec2> = self.children.iter().map(|c| c.view_box()).collect();
            let layout = compute_stack_layout(
                self.axis,
                self.size,
                self.spacing,
                self.main_align,
                self.cross_align,
                &sizes,
            );
            let placed: Vec<(Vec2, &dyn RasterComponent)> = self
                .children
                .iter()
                .zip(layout.placements.iter())
                .map(|(c, p)| (*p, c.as_ref()))
                .collect();
            composite_children(layout.own_size, target, &placed)
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
        fn view_box(&self) -> Vec2 {
            self.child.view_box()
        }

        fn render(&self, target: Resolution) -> RasterImage {
            let size = self.view_box();
            match self.background {
                Some(color) => {
                    let bg = SolidRect { size, color };
                    let placed: Vec<(Vec2, &dyn RasterComponent)> =
                        vec![(Vec2::ZERO, &bg), (Vec2::ZERO, self.child.as_ref())];
                    composite_children(size, target, &placed)
                }
                None => composite_children(size, target, &[(Vec2::ZERO, self.child.as_ref())]),
            }
        }
    }

    pub struct SizedBox {
        pub size: Vec2,
    }

    impl RasterComponent for SizedBox {
        fn view_box(&self) -> Vec2 {
            self.size
        }

        fn render(&self, target: Resolution) -> RasterImage {
            let bytes = (target.width as usize) * (target.height as usize) * 4;
            RasterImage {
                width: target.width,
                height: target.height,
                format: PixelFormat::Rgba8,
                pixels: Bytes::from(vec![0u8; bytes]),
            }
        }
    }

    /// Internal helper: a solid-color rectangle of the given logical size,
    /// rasterized by directly filling the pixel buffer.
    struct SolidRect {
        size: Vec2,
        color: Color,
    }

    impl RasterComponent for SolidRect {
        fn view_box(&self) -> Vec2 {
            self.size
        }

        fn render(&self, target: Resolution) -> RasterImage {
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
    pub trait RasterLayoutExt: RasterComponent + Sized + 'static {
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

        fn align(self, size: Vec2, anchor: Anchor) -> Align {
            Align {
                size,
                anchor,
                child: Box::new(self),
            }
        }
    }

    impl<T: RasterComponent + 'static> RasterLayoutExt for T {}
}
