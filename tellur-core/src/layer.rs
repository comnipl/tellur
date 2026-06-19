//! Layer types for composing components into a scene.
//!
//! Both layer types share the same coordinate model: each layer has a
//! fixed logical `size` defining its coordinate space (top-left at
//! `(0, 0)`), and children are placed at logical positions within it via
//! [`Positioned`](crate::placement::Positioned). For a grouping that
//! auto-fits the children's bounding box instead of fixing a size, use
//! [`Fragment`](crate::fragment::Fragment).
//!
//! Layers participate in the constraint-based layout protocol:
//! `layout(constraints)` returns `size` (clamped to the constraints), and
//! `render(size)` lays out each child with constraints loose to `size`,
//! then composes them at their stored positions.
//!
//! `VectorLayer` composes `VectorComponent` children into a single
//! `VectorGraphic`. Each child is wrapped in a translating `Group` so
//! the composed result remains pure vector data.
//!
//! `Layer` composes `RasterComponent` children by rendering each one at
//! a pixel sub-resolution matching its logical paint bounds and
//! source-over compositing it onto the output at the corresponding pixel
//! offset.

use std::sync::{LazyLock, Mutex};

use lru::LruCache;

use crate::composite::composite_at;
use crate::geometry::{Constraints, Rect, Transform, Vec2};
use crate::raster::{PixelFormat, RasterComponent, RasterImage, RasterStorageId, Resolution};
use crate::render_context::{CompositeInput, RenderContext};
use crate::vector::{Group, Node, VectorComponent, VectorGraphic};

const COMPOSITE_CHILDREN_CACHE_ENTRIES: usize = 32;

static COMPOSITE_CHILDREN_CACHE: LazyLock<
    Mutex<LruCache<CompositeChildrenCacheKey, CompositeChildrenCacheEntry>>,
> = LazyLock::new(|| Mutex::new(LruCache::unbounded()));

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct CompositeChildrenCacheInput {
    image: RasterStorageId,
    offset_x: i32,
    offset_y: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CompositeChildrenCacheKey {
    target: Resolution,
    inputs: Vec<CompositeChildrenCacheInput>,
}

#[derive(Clone)]
struct CompositeChildrenCacheEntry {
    _inputs: Vec<RasterImage>,
    output: RasterImage,
}

#[cfg(test)]
fn clear_composite_children_cache_for_tests() {
    if let Ok(mut cache) = COMPOSITE_CHILDREN_CACHE.lock() {
        cache.clear();
    }
}

fn composite_children_cache_key(
    rendered: &[(RasterImage, i32, i32)],
    target: Resolution,
) -> CompositeChildrenCacheKey {
    CompositeChildrenCacheKey {
        target,
        inputs: rendered
            .iter()
            .map(|(image, offset_x, offset_y)| CompositeChildrenCacheInput {
                image: image.storage_id(),
                offset_x: *offset_x,
                offset_y: *offset_y,
            })
            .collect(),
    }
}

fn cached_composite_children(key: &CompositeChildrenCacheKey) -> Option<RasterImage> {
    COMPOSITE_CHILDREN_CACHE
        .lock()
        .ok()
        .and_then(|mut cache| cache.get(key).map(|entry| entry.output.clone()))
}

fn cache_composite_children(
    key: CompositeChildrenCacheKey,
    inputs: Vec<RasterImage>,
    output: RasterImage,
) {
    if let Ok(mut cache) = COMPOSITE_CHILDREN_CACHE.lock() {
        cache.put(
            key,
            CompositeChildrenCacheEntry {
                _inputs: inputs,
                output,
            },
        );
        while cache.len() > COMPOSITE_CHILDREN_CACHE_ENTRIES {
            cache.pop_lru();
        }
    }
}

#[crate::component(vector)]
#[derive(PartialEq, Hash)]
pub struct VectorLayer {
    // `#[builder(field)]` members must precede the setter members.
    #[children(each = child)]
    pub children: Vec<Box<dyn VectorComponent>>,
    /// The fixed logical extent of the layer's coordinate space. For an
    /// extent that auto-fits the children, use
    /// [`Fragment`](crate::fragment::Fragment) instead.
    pub size: Vec2,
}

impl VectorLayer {
    /// Fixed-size layer of the given extent.
    pub fn new(size: Vec2) -> Self {
        Self {
            size,
            children: Vec::new(),
        }
    }

    pub fn add(&mut self, child: impl Into<Box<dyn VectorComponent>>) -> &mut Self {
        self.children.push(child.into());
        self
    }
}

impl VectorComponent for VectorLayer {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        constraints.constrain(self.size)
    }

    fn render(&self, size: Vec2) -> VectorGraphic {
        render_vector_children(
            &self.children,
            Rect {
                origin: Vec2::ZERO,
                size,
            },
            Constraints::loose(size),
        )
    }
}

/// Bounding rect of all children's paint bounds, each laid out under
/// `Constraints::UNBOUNDED`. Returns a zero rect when there are no
/// children. Each child carries its own offset (via
/// [`Positioned`](crate::placement::Positioned)), so its `paint_bounds`
/// is already expressed in the parent's coordinate space.
pub(crate) fn vector_children_bounds(children: &[Box<dyn VectorComponent>]) -> Rect {
    let mut iter = children.iter().map(|child| {
        let child_size = child.layout(Constraints::UNBOUNDED);
        child.paint_bounds(child_size)
    });
    let Some(first) = iter.next() else {
        return Rect {
            origin: Vec2::ZERO,
            size: Vec2::ZERO,
        };
    };
    iter.fold(first, union_rect)
}

/// Overlays `children` into one graphic: each child's root node is placed
/// directly under a transparent identity group (the child supplies its own
/// translation when it is a `Positioned`). Shared by [`VectorLayer`] and
/// [`Fragment`](crate::fragment::Fragment).
pub(crate) fn render_vector_children(
    children: &[Box<dyn VectorComponent>],
    view_box: Rect,
    child_constraints: Constraints,
) -> VectorGraphic {
    let nodes: Vec<Node> = children
        .iter()
        .filter_map(|child| {
            let child_size = child.layout(child_constraints);
            let node = child.render(child_size).root;
            (!node.is_empty()).then_some(node)
        })
        .collect();
    VectorGraphic {
        view_box,
        root: Node::Group(Group {
            transform: Transform::IDENTITY,
            opacity: 1.0,
            children: nodes,
        }),
    }
}

#[crate::component(raster)]
#[derive(PartialEq, Hash)]
pub struct Layer {
    // `#[builder(field)]` members must precede the setter members.
    #[children(each = child)]
    pub children: Vec<Box<dyn RasterComponent>>,
    /// The fixed logical extent of the layer's coordinate space. For an
    /// extent that auto-fits the children, use
    /// [`Fragment`](crate::fragment::raster::Fragment) instead.
    pub size: Vec2,
}

impl Layer {
    /// Fixed-size layer of the given extent.
    pub fn new(size: Vec2) -> Self {
        Self {
            size,
            children: Vec::new(),
        }
    }

    pub fn add(&mut self, child: impl Into<Box<dyn RasterComponent>>) -> &mut Self {
        self.children.push(child.into());
        self
    }
}

impl RasterComponent for Layer {
    fn layout(&self, constraints: Constraints) -> Vec2 {
        constraints.constrain(self.size)
    }

    fn paint_bounds(&self, size: Vec2) -> Rect {
        // Start from the `(0,0)..size` rect and grow it to include any
        // children that overflow the box.
        let child_constraints = Constraints::loose(size);
        let mut bounds = Rect {
            origin: Vec2::ZERO,
            size,
        };
        for child in &self.children {
            let child_size = child.layout(child_constraints);
            bounds = union_rect(bounds, child.paint_bounds(child_size));
        }
        bounds
    }

    fn render(&self, size: Vec2, target: Resolution, ctx: &mut dyn RenderContext) -> RasterImage {
        let paint_rect = self.paint_bounds(size);
        let child_constraints = Constraints::loose(size);
        let placed: Vec<(Vec2, Vec2, &dyn RasterComponent)> = self
            .children
            .iter()
            .map(|child| {
                let child_size = child.layout(child_constraints);
                (Vec2::ZERO, child_size, child.as_ref())
            })
            .collect();
        composite_children(paint_rect, target, &placed, ctx)
    }
}

/// Raster counterpart of [`vector_children_bounds`]: the bounding rect of all
/// children's paint bounds, each laid out under `Constraints::UNBOUNDED`.
pub(crate) fn raster_children_bounds(children: &[Box<dyn RasterComponent>]) -> Rect {
    let mut iter = children.iter().map(|child| {
        let child_size = child.layout(Constraints::UNBOUNDED);
        child.paint_bounds(child_size)
    });
    let Some(first) = iter.next() else {
        return Rect {
            origin: Vec2::ZERO,
            size: Vec2::ZERO,
        };
    };
    iter.fold(first, union_rect)
}

/// Rasterizes a set of placed-and-sized raster components into the
/// `paint_rect` logical region and returns the composited image at
/// `target` pixel resolution.
///
/// `paint_rect` is the parent's own paint bounds expressed in the
/// parent's logical coordinate space (its origin may be negative).
/// `target` pixels span exactly that rectangle, so 1 target pixel
/// equals `paint_rect.size / target` logical units on each axis.
///
/// Each entry's tuple is `(position, child_size, child)`:
/// - `position` is the child's layout origin in the parent's logical
///   coordinate space (i.e. relative to `paint_rect.origin = (0,0)` in
///   the layout sense, not relative to `paint_rect.origin`).
/// - `child_size` is the size returned by the child's `layout`.
/// - The child's `paint_bounds(child_size)` decides the actual pixel
///   region (the rectangle may have a negative origin or be larger than
///   `child_size` for effects like drop shadows); the child renders
///   into a buffer matching that paint-bounds size and the parent
///   composites it at `position + child_paint_bounds.origin -
///   paint_rect.origin` (i.e. shifted into the buffer's local space).
///
/// Any spill beyond the buffer is clipped at the buffer's edge — that
/// is how containers like `DecoratedBox` (whose own paint_bounds equals
/// its layout box) act as natural clip rectangles.
pub(crate) fn composite_children(
    paint_rect: Rect,
    target: Resolution,
    placed: &[(Vec2, Vec2, &dyn RasterComponent)],
    ctx: &mut dyn RenderContext,
) -> RasterImage {
    let scale_x = target.width as f32 / paint_rect.size.0;
    let scale_y = target.height as f32 / paint_rect.size.1;
    let gpu_available = ctx.prefers_gpu() && ctx.gpu_backend().is_some();

    let mut rendered = Vec::with_capacity(placed.len());
    for (position, child_size, child) in placed {
        let bounds = child.paint_bounds(*child_size);
        let child_px_w = (bounds.size.0 * scale_x).round().max(1.0) as u32;
        let child_px_h = (bounds.size.1 * scale_y).round().max(1.0) as u32;
        let paint_x = position.0 + bounds.origin.0 - paint_rect.origin.0;
        let paint_y = position.1 + bounds.origin.1 - paint_rect.origin.1;
        let offset_x = (paint_x * scale_x).round() as i32;
        let offset_y = (paint_y * scale_y).round() as i32;
        let image = ctx.render(*child, *child_size, Resolution::new(child_px_w, child_px_h));
        rendered.push((image, offset_x, offset_y));
    }

    let cache_key = composite_children_cache_key(&rendered, target);
    if let Some(image) = cached_composite_children(&cache_key) {
        return image;
    }
    let cache_inputs = rendered
        .iter()
        .map(|(image, _, _)| image.clone())
        .collect::<Vec<_>>();

    if gpu_available {
        let inputs: Vec<CompositeInput<'_>> = rendered
            .iter()
            .map(|(image, offset_x, offset_y)| CompositeInput {
                image,
                offset_x: *offset_x,
                offset_y: *offset_y,
            })
            .collect();
        if let Some(gpu) = ctx.gpu_backend() {
            if let Some(image) = gpu.composite(target, &inputs) {
                cache_composite_children(cache_key, cache_inputs, image.clone());
                return image;
            }
        }
    }

    let mut accum = vec![0u8; (target.width as usize) * (target.height as usize) * 4];
    for (image, offset_x, offset_y) in rendered {
        let image = ctx.readback(image);
        composite_at(&mut accum, target, &image, offset_x, offset_y);
    }

    let image = RasterImage::cpu(target.width, target.height, PixelFormat::Rgba8, accum);
    cache_composite_children(cache_key, cache_inputs, image.clone());
    image
}

/// Smallest axis-aligned rectangle containing both `a` and `b`.
pub(crate) fn union_rect(a: Rect, b: Rect) -> Rect {
    let a_end = Vec2(a.origin.0 + a.size.0, a.origin.1 + a.size.1);
    let b_end = Vec2(b.origin.0 + b.size.0, b.origin.1 + b.size.1);
    let origin = Vec2(a.origin.0.min(b.origin.0), a.origin.1.min(b.origin.1));
    let end = Vec2(a_end.0.max(b_end.0), a_end.1.max(b_end.1));
    Rect {
        origin,
        size: Vec2(end.0 - origin.0, end.1 - origin.1),
    }
}

/// Translates a rect by `delta`, leaving its size unchanged.
pub(crate) fn translate_rect(r: Rect, delta: Vec2) -> Rect {
    Rect {
        origin: Vec2(r.origin.0 + delta.0, r.origin.1 + delta.1),
        size: r.size,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Color;
    use crate::placement::VectorPlacement;
    use crate::raster::CpuRasterImage;
    use crate::render_context::{DropShadowInput, GpuPreference, GpuRasterBackend, OutlineInput};
    use crate::shapes::Rectangle;
    use crate::vector::{Node, Paint};

    fn rect(w: f32, h: f32) -> Rectangle {
        Rectangle {
            size: Vec2(w, h),
            fill: Paint::Solid(Color::rgb_u8(0, 0, 0)).into(),
            stroke: None,
        }
    }

    #[test]
    fn vector_layer_fixed_size_unchanged() {
        let layer = VectorLayer {
            size: Vec2(500.0, 300.0),
            children: vec![rect(80.0, 40.0).place_at(Vec2(10.0, 20.0)).into()],
        };
        assert_eq!(layer.layout(Constraints::UNBOUNDED), Vec2(500.0, 300.0));
    }

    #[test]
    fn vector_layer_prunes_empty_child_nodes() {
        let invisible = Rectangle {
            size: Vec2(10.0, 10.0),
            fill: Paint::Solid(Color::rgba_u8(255, 0, 0, 0)).into(),
            stroke: None,
        };
        let layer = VectorLayer {
            size: Vec2(100.0, 100.0),
            children: vec![invisible.into(), rect(10.0, 10.0).into()],
        };

        let graphic = layer.render(Vec2(100.0, 100.0));
        let Node::Group(root) = graphic.root else {
            panic!("vector layer should render a root group");
        };
        assert_eq!(root.children.len(), 1);
        assert!(matches!(root.children[0], Node::Path(_)));
    }

    #[derive(PartialEq, Eq, Hash)]
    struct DummyRaster;

    impl RasterComponent for DummyRaster {
        fn layout(&self, constraints: Constraints) -> Vec2 {
            constraints.constrain(Vec2(1.0, 1.0))
        }

        fn render(
            &self,
            _size: Vec2,
            _target: Resolution,
            _ctx: &mut dyn RenderContext,
        ) -> RasterImage {
            panic!("test context intercepts renders")
        }
    }

    #[derive(Default)]
    struct FakeGpu {
        composites: usize,
    }

    impl GpuRasterBackend for FakeGpu {
        fn composite(
            &mut self,
            target: Resolution,
            _inputs: &[CompositeInput<'_>],
        ) -> Option<RasterImage> {
            self.composites += 1;
            Some(RasterImage::cpu(
                target.width,
                target.height,
                PixelFormat::Rgba8,
                vec![7u8; (target.width as usize) * (target.height as usize) * 4],
            ))
        }

        fn drop_shadow(&mut self, _input: DropShadowInput<'_>) -> Option<RasterImage> {
            None
        }

        fn outline(&mut self, _input: OutlineInput<'_>) -> Option<RasterImage> {
            None
        }

        fn rasterize(
            &mut self,
            _graphic: &VectorGraphic,
            _target: Resolution,
        ) -> Option<RasterImage> {
            None
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

        fn readback(&mut self, _image: RasterImage) -> Option<CpuRasterImage> {
            None
        }
    }

    struct FakeContext {
        image: RasterImage,
        gpu: FakeGpu,
        renders: usize,
        readbacks: usize,
    }

    impl FakeContext {
        fn new() -> Self {
            Self {
                image: RasterImage::cpu(1, 1, PixelFormat::Rgba8, vec![1, 2, 3, 255]),
                gpu: FakeGpu::default(),
                renders: 0,
                readbacks: 0,
            }
        }
    }

    impl RenderContext for FakeContext {
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }

        fn gpu_preference(&self) -> GpuPreference {
            GpuPreference::PreferGpu
        }

        fn gpu_backend(&mut self) -> Option<&mut dyn GpuRasterBackend> {
            Some(&mut self.gpu)
        }

        fn render(
            &mut self,
            _component: &dyn RasterComponent,
            _size: Vec2,
            _target: Resolution,
        ) -> RasterImage {
            self.renders += 1;
            self.image.clone()
        }

        fn readback(&mut self, image: RasterImage) -> CpuRasterImage {
            self.readbacks += 1;
            image
                .into_cpu()
                .expect("fake context only returns CPU images")
        }
    }

    #[test]
    fn composite_children_reuses_cached_batch_for_same_input_storage() {
        clear_composite_children_cache_for_tests();
        let child = DummyRaster;
        let placed = [(Vec2::ZERO, Vec2(1.0, 1.0), &child as &dyn RasterComponent)];
        let paint_rect = Rect {
            origin: Vec2::ZERO,
            size: Vec2(1.0, 1.0),
        };
        let mut ctx = FakeContext::new();

        let first = composite_children(paint_rect, Resolution::new(1, 1), &placed, &mut ctx);
        let second = composite_children(paint_rect, Resolution::new(1, 1), &placed, &mut ctx);

        assert_eq!(first.into_cpu().unwrap().pixels.as_ref(), &[7, 7, 7, 7]);
        assert_eq!(second.into_cpu().unwrap().pixels.as_ref(), &[7, 7, 7, 7]);
        assert_eq!(ctx.renders, 2);
        assert_eq!(ctx.gpu.composites, 1);
        assert_eq!(ctx.readbacks, 0);
        clear_composite_children_cache_for_tests();
    }
}
