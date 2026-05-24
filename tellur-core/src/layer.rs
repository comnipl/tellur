//! Layer types for composing components into a scene.
//!
//! Both layer types share the same coordinate model: each layer has a
//! logical `size` defining its coordinate space (top-left at `(0, 0)`),
//! and children are placed at logical positions within it.
//!
//! Children are stored as [`Placed<dyn _>`](Placed) — the position lives on
//! a wrapper around the component rather than as a field of the component
//! itself, keeping the component types focused on intrinsic shape. Use the
//! placement extension traits in [`crate::placement`]
//! ([`VectorPlacement`](crate::placement::VectorPlacement) /
//! [`RasterPlacement`](crate::placement::RasterPlacement)) to construct
//! placed children with `at`, `anchor(...).snap_to(...)`, etc.
//!
//! `VectorLayer` composes `VectorComponent` children into a single
//! `VectorGraphic`. Each child is placed by wrapping it in a translating
//! `Group` so the composed result remains pure vector data.
//!
//! `Layer` composes `RasterComponent` children by rendering each one at a
//! pixel sub-resolution that matches its logical size and source-over
//! compositing it onto the output at the corresponding pixel offset.
//! Vector content has to be rasterized before being added — see the
//! `Rasterizable::rasterize` extension in `tellur-renderer`.

use bytes::Bytes;

use crate::geometry::{Transform, Vec2};
use crate::placement::Placed;
use crate::raster::{PixelFormat, RasterComponent, RasterImage, Resolution};
use crate::vector::{Group, Node, VectorComponent, VectorGraphic};

pub struct VectorLayer {
    pub size: Vec2,
    pub children: Vec<Placed<dyn VectorComponent>>,
}

impl VectorLayer {
    pub fn new(size: Vec2) -> Self {
        Self {
            size,
            children: Vec::new(),
        }
    }

    pub fn add(&mut self, child: Placed<dyn VectorComponent>) -> &mut Self {
        self.children.push(child);
        self
    }
}

impl VectorComponent for VectorLayer {
    fn view_box(&self) -> Vec2 {
        self.size
    }

    fn render(&self) -> VectorGraphic {
        let children = self
            .children
            .iter()
            .map(|placed| {
                let child = placed.child.render();
                Node::Group(Group {
                    transform: Transform::translate(placed.position),
                    opacity: 1.0,
                    children: vec![child.root],
                })
            })
            .collect();
        VectorGraphic {
            view_box: self.size,
            root: Node::Group(Group {
                transform: Transform::IDENTITY,
                opacity: 1.0,
                children,
            }),
        }
    }
}

pub struct Layer {
    pub size: Vec2,
    pub children: Vec<Placed<dyn RasterComponent>>,
}

impl Layer {
    pub fn new(size: Vec2) -> Self {
        Self {
            size,
            children: Vec::new(),
        }
    }

    pub fn add(&mut self, child: Placed<dyn RasterComponent>) -> &mut Self {
        self.children.push(child);
        self
    }
}

impl RasterComponent for Layer {
    fn view_box(&self) -> Vec2 {
        self.size
    }

    fn render(&self, target: Resolution) -> RasterImage {
        let placed: Vec<(Vec2, &dyn RasterComponent)> = self
            .children
            .iter()
            .map(|p| (p.position, p.child.as_ref()))
            .collect();
        composite_children(self.size, target, &placed)
    }
}

/// Rasterizes a set of placed raster components into a `container_size`
/// logical coordinate space and returns the composited image at `target`
/// pixel resolution.
///
/// Shared between `Layer::render` and the raster layout containers, which
/// all need the same "place children at logical offsets, then source-over
/// composite" pipeline.
pub(crate) fn composite_children(
    container_size: Vec2,
    target: Resolution,
    placed: &[(Vec2, &dyn RasterComponent)],
) -> RasterImage {
    let pixel_count = (target.width as usize) * (target.height as usize);
    let mut accum = vec![0u8; pixel_count * 4];

    // Pixels per logical unit on each axis. SVG's `preserveAspectRatio="none"`
    // — independent scaling on each axis.
    let scale_x = target.width as f32 / container_size.0;
    let scale_y = target.height as f32 / container_size.1;

    for (position, child) in placed {
        let child_size = child.view_box();
        let child_px_w = (child_size.0 * scale_x).round().max(1.0) as u32;
        let child_px_h = (child_size.1 * scale_y).round().max(1.0) as u32;
        let offset_x = (position.0 * scale_x).round() as i32;
        let offset_y = (position.1 * scale_y).round() as i32;

        let image = child.render(Resolution::new(child_px_w, child_px_h));
        composite_at(&mut accum, target, &image, offset_x, offset_y);
    }

    RasterImage {
        width: target.width,
        height: target.height,
        format: PixelFormat::Rgba8,
        pixels: Bytes::from(accum),
    }
}

// Source-over compositing of `src` onto `dst` at pixel offset
// `(offset_x, offset_y)`. Both buffers hold 8-bit straight-alpha RGBA.
// Pixels of `src` that fall outside `dst_size` are clipped away.
fn composite_at(
    dst: &mut [u8],
    dst_size: Resolution,
    src: &RasterImage,
    offset_x: i32,
    offset_y: i32,
) {
    assert_eq!(
        src.format,
        PixelFormat::Rgba8,
        "Layer only supports Rgba8 children for now"
    );
    let src_pixels = src.pixels.as_ref();
    let dst_w = dst_size.width as i32;
    let dst_h = dst_size.height as i32;
    let src_w = src.width as i32;
    let src_h = src.height as i32;

    // Iterate only over the overlapping rectangle to skip clipped rows/cols.
    let x_start = offset_x.max(0);
    let y_start = offset_y.max(0);
    let x_end = (offset_x + src_w).min(dst_w);
    let y_end = (offset_y + src_h).min(dst_h);

    for dy in y_start..y_end {
        for dx in x_start..x_end {
            let sx = dx - offset_x;
            let sy = dy - offset_y;
            let src_idx = ((sy * src_w + sx) * 4) as usize;
            let dst_idx = ((dy * dst_w + dx) * 4) as usize;

            let sr = src_pixels[src_idx] as f32 / 255.0;
            let sg = src_pixels[src_idx + 1] as f32 / 255.0;
            let sb = src_pixels[src_idx + 2] as f32 / 255.0;
            let sa = src_pixels[src_idx + 3] as f32 / 255.0;
            let dr = dst[dst_idx] as f32 / 255.0;
            let dg = dst[dst_idx + 1] as f32 / 255.0;
            let db = dst[dst_idx + 2] as f32 / 255.0;
            let da = dst[dst_idx + 3] as f32 / 255.0;

            let inv_sa = 1.0 - sa;
            let out_a = sa + da * inv_sa;
            let (out_r, out_g, out_b) = if out_a > 0.0 {
                (
                    (sr * sa + dr * da * inv_sa) / out_a,
                    (sg * sa + dg * da * inv_sa) / out_a,
                    (sb * sa + db * da * inv_sa) / out_a,
                )
            } else {
                (0.0, 0.0, 0.0)
            };

            dst[dst_idx] = (out_r * 255.0).round().clamp(0.0, 255.0) as u8;
            dst[dst_idx + 1] = (out_g * 255.0).round().clamp(0.0, 255.0) as u8;
            dst[dst_idx + 2] = (out_b * 255.0).round().clamp(0.0, 255.0) as u8;
            dst[dst_idx + 3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
}
