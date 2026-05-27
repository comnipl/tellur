//! Source-over compositing of 8-bit straight-alpha RGBA rasters.
//!
//! Every layer in the raster pipeline ultimately funnels into the same
//! pixel-blend kernel: render a child to its own `RasterImage`, then
//! source-over composite it onto the parent buffer at some pixel
//! offset. The fast path of an exporter is dominated by this loop —
//! the inner kernel runs (`overlap_w * overlap_h`) times per composited
//! child, and a 1080p frame can easily push that into the tens of
//! millions of pixels per frame. Keeping the implementation in one
//! place makes it the single thing to tune.
//!
//! The kernel itself is straight-alpha source-over carried out entirely
//! in `u32` fixed-point — no `f32` conversion, no `round`/`clamp`. The
//! two scalar `out_c` divisions are by the same divisor (`out_a_x255`)
//! and only run on partially-transparent pixels; fully-transparent
//! source pixels skip the write entirely and fully-opaque ones go
//! through a 4-byte copy.

use crate::raster::{CpuRasterImage, PixelFormat, Resolution};

/// Source-over composites `src` onto `dst` at pixel offset
/// `(offset_x, offset_y)`. Both buffers hold 8-bit straight-alpha RGBA
/// laid out as `[r, g, b, a, r, g, b, a, …]` in row-major order. Pixels
/// of `src` that fall outside `dst_size` are clipped away.
///
/// Panics if `src.format` is not [`PixelFormat::Rgba8`] — the only
/// pixel layout the raster pipeline currently supports.
pub fn composite_at(
    dst: &mut [u8],
    dst_size: Resolution,
    src: &CpuRasterImage,
    offset_x: i32,
    offset_y: i32,
) {
    assert_eq!(
        src.format,
        PixelFormat::Rgba8,
        "composite_at only supports Rgba8 sources",
    );

    let dst_w = dst_size.width as i32;
    let dst_h = dst_size.height as i32;
    let src_w = src.width as i32;
    let src_h = src.height as i32;

    // Iterate only over the overlapping rectangle to skip clipped rows/cols.
    let x_start = offset_x.max(0);
    let y_start = offset_y.max(0);
    let x_end = (offset_x + src_w).min(dst_w);
    let y_end = (offset_y + src_h).min(dst_h);

    if x_end <= x_start || y_end <= y_start {
        return;
    }

    let span_w = (x_end - x_start) as usize;
    let rows = (y_end - y_start) as usize;
    let stride_dst = dst_w as usize * 4;
    let stride_src = src_w as usize * 4;

    // Constant offsets for the top-left corner of the overlap region.
    let dst_base = (y_start as usize) * stride_dst + (x_start as usize) * 4;
    let src_base =
        ((y_start - offset_y) as usize) * stride_src + ((x_start - offset_x) as usize) * 4;

    let src_pixels = src.pixels.as_ref();

    for row in 0..rows {
        let dst_row = &mut dst[dst_base + row * stride_dst..][..span_w * 4];
        let src_row = &src_pixels[src_base + row * stride_src..][..span_w * 4];
        blend_row(dst_row, src_row);
    }
}

/// Source-over blends `span_w` consecutive RGBA pixels of `src` onto
/// `dst`. Both slices must be exactly `4 * span_w` bytes long; the
/// caller guarantees that via slice indexing in [`composite_at`].
#[inline]
fn blend_row(dst: &mut [u8], src: &[u8]) {
    debug_assert_eq!(dst.len(), src.len());
    debug_assert_eq!(dst.len() % 4, 0);

    // Process pixels with chunked slices so the bounds check fires
    // once per pixel block instead of once per byte read.
    let dst_chunks = dst.chunks_exact_mut(4);
    let src_chunks = src.chunks_exact(4);

    for (d, s) in dst_chunks.zip(src_chunks) {
        let sa = s[3] as u32;
        if sa == 0 {
            // Fully transparent source: `dst` unchanged.
            continue;
        }
        if sa == 255 {
            // Fully opaque source: direct copy.
            d.copy_from_slice(s);
            continue;
        }

        // Partial coverage — straight-alpha Porter-Duff source-over:
        //     out_a   = sa + da * (1 - sa)
        //     out_rgb = (sr * sa + dr * da * (1 - sa)) / out_a
        // Carried out in `u32` fixed-point with 255 as the unit, then
        // rounded to nearest u8. Maximum intermediate value is
        // 255 * 255 * 255 ≈ 1.7 × 10^7, well within `u32`.
        let inv_sa = 255 - sa;
        let sr = s[0] as u32;
        let sg = s[1] as u32;
        let sb = s[2] as u32;
        let dr = d[0] as u32;
        let dg = d[1] as u32;
        let db = d[2] as u32;
        let da = d[3] as u32;

        let out_a_x255 = sa * 255 + da * inv_sa;
        let half = out_a_x255 / 2;

        let out_r = (sr * sa * 255 + dr * da * inv_sa + half) / out_a_x255;
        let out_g = (sg * sa * 255 + dg * da * inv_sa + half) / out_a_x255;
        let out_b = (sb * sa * 255 + db * da * inv_sa + half) / out_a_x255;
        let out_a = (out_a_x255 + 127) / 255;

        d[0] = out_r as u8;
        d[1] = out_g as u8;
        d[2] = out_b as u8;
        d[3] = out_a as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(width: u32, height: u32, pixels: Vec<u8>) -> CpuRasterImage {
        assert_eq!(pixels.len(), (width * height * 4) as usize);
        CpuRasterImage::new(width, height, PixelFormat::Rgba8, pixels)
    }

    /// Straight-alpha Porter-Duff source-over carried out in `f64`, used
    /// as the oracle for the integer kernel. Mirrors the per-pixel math
    /// the old `f32` implementation performed, but in higher precision
    /// so any rounding mismatch is genuinely the kernel's fault.
    fn blend_pixel_oracle(d: [u8; 4], s: [u8; 4]) -> [u8; 4] {
        let to_f = |v: u8| v as f64 / 255.0;
        let from_f = |v: f64| (v * 255.0).round().clamp(0.0, 255.0) as u8;

        let (sr, sg, sb, sa) = (to_f(s[0]), to_f(s[1]), to_f(s[2]), to_f(s[3]));
        let (dr, dg, db, da) = (to_f(d[0]), to_f(d[1]), to_f(d[2]), to_f(d[3]));

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
        [from_f(out_r), from_f(out_g), from_f(out_b), from_f(out_a)]
    }

    #[test]
    fn transparent_source_leaves_dst_unchanged() {
        let mut dst = vec![10, 20, 30, 200, 1, 2, 3, 4];
        let src = image(2, 1, vec![255, 255, 255, 0, 255, 255, 255, 0]);
        let expected = dst.clone();
        composite_at(&mut dst, Resolution::new(2, 1), &src, 0, 0);
        assert_eq!(dst, expected);
    }

    #[test]
    fn opaque_source_replaces_dst() {
        let mut dst = vec![10, 20, 30, 200, 1, 2, 3, 4];
        let src = image(2, 1, vec![100, 150, 200, 255, 1, 2, 3, 255]);
        composite_at(&mut dst, Resolution::new(2, 1), &src, 0, 0);
        assert_eq!(dst, vec![100, 150, 200, 255, 1, 2, 3, 255]);
    }

    #[test]
    fn partial_alpha_matches_f64_oracle_within_one_lsb() {
        // Sweep a representative grid of (s, d) RGBA combinations and
        // confirm the integer kernel never disagrees with a `f64`
        // implementation by more than 1 LSB on any channel.
        for sa in [16u8, 64, 128, 200, 240, 254] {
            for da in [0u8, 32, 128, 200, 255] {
                for sr in [0u8, 64, 200, 255] {
                    for dr in [0u8, 64, 200, 255] {
                        let s = [sr, 128, 64, sa];
                        let d = [dr, 200, 32, da];
                        let mut dst = d.to_vec();
                        let src = image(1, 1, s.to_vec());
                        composite_at(&mut dst, Resolution::new(1, 1), &src, 0, 0);

                        let expected = blend_pixel_oracle(d, s);
                        for ch in 0..4 {
                            let diff = (dst[ch] as i32 - expected[ch] as i32).abs();
                            assert!(
                                diff <= 1,
                                "channel {ch} mismatch >1 LSB: got {} expected {} (s={:?} d={:?})",
                                dst[ch],
                                expected[ch],
                                s,
                                d,
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn clips_source_falling_outside_dst() {
        // `src` is larger than `dst` and offset so only the bottom-right
        // 1×1 pixel overlaps.
        let mut dst = vec![0u8; 4];
        let src = image(
            2,
            2,
            vec![10, 20, 30, 255, 1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255],
        );
        composite_at(&mut dst, Resolution::new(1, 1), &src, -1, -1);
        // The pixel of `src` landing on `dst[0,0]` is `src[1,1] = (7,8,9,255)`.
        assert_eq!(dst, vec![7, 8, 9, 255]);
    }

    #[test]
    fn fully_clipped_source_is_a_noop() {
        let mut dst = vec![42u8; 16];
        let src = image(2, 2, vec![255u8; 16]);
        let expected = dst.clone();
        composite_at(&mut dst, Resolution::new(2, 2), &src, 5, 5);
        assert_eq!(dst, expected);
    }
}
