use crate::Keyable;

/// sRGB with straight alpha. Each component is in the range `[0.0, 1.0]`.
#[derive(Debug, Clone, Copy, Keyable)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    /// Opaque color from 8-bit sRGB components.
    pub const fn rgb_u8(r: u8, g: u8, b: u8) -> Self {
        Self::rgba_u8(r, g, b, 255)
    }

    /// Color from 8-bit sRGB + alpha components.
    pub const fn rgba_u8(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self {
            r: r as f32 / 255.0,
            g: g as f32 / 255.0,
            b: b as f32 / 255.0,
            a: a as f32 / 255.0,
        }
    }

    /// Returns this color with `a` replaced by `alpha`, clamped to `[0, 1]`.
    /// `alpha` accepts anything convertible to `f32` — passing a
    /// [`Phase`](crate::phase::Phase) directly works via
    /// `From<Phase> for f32`, so callers can avoid an explicit `.get()`.
    pub fn with_alpha(self, alpha: impl Into<f32>) -> Self {
        Self {
            a: alpha.into().clamp(0.0, 1.0),
            ..self
        }
    }

    /// Returns this color with its alpha multiplied by `factor`, clamped to
    /// `[0, 1]` before multiplication. See [`Self::with_alpha`] for the
    /// `Into<f32>` rationale.
    pub fn multiply_alpha(self, factor: impl Into<f32>) -> Self {
        Self {
            a: self.a * factor.into().clamp(0.0, 1.0),
            ..self
        }
    }

    /// Opaque color from HSV.
    ///
    /// `h` is the hue in degrees (wraps around 360); `s` and `v` are in `[0, 1]`.
    /// Result is interpreted as sRGB (matches the usual "color picker" intuition,
    /// not linear light).
    pub fn hsv(h: f32, s: f32, v: f32) -> Self {
        Self::hsva(h, s, v, 1.0)
    }

    /// Color from HSV + alpha. See [`Color::hsv`] for the input ranges.
    pub fn hsva(h: f32, s: f32, v: f32, a: f32) -> Self {
        let c = v * s;
        let x = chroma_x(h, c);
        let m = v - c;
        let (r1, g1, b1) = hue_sector(h, c, x);
        Self {
            r: r1 + m,
            g: g1 + m,
            b: b1 + m,
            a,
        }
    }

    /// Opaque color from HSL.
    ///
    /// `h` is the hue in degrees (wraps around 360); `s` and `l` are in `[0, 1]`.
    /// Result is interpreted as sRGB.
    pub fn hsl(h: f32, s: f32, l: f32) -> Self {
        Self::hsla(h, s, l, 1.0)
    }

    /// Color from HSL + alpha. See [`Color::hsl`] for the input ranges.
    pub fn hsla(h: f32, s: f32, l: f32, a: f32) -> Self {
        let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
        let x = chroma_x(h, c);
        let m = l - c / 2.0;
        let (r1, g1, b1) = hue_sector(h, c, x);
        Self {
            r: r1 + m,
            g: g1 + m,
            b: b1 + m,
            a,
        }
    }
}

// Intermediate `X` value shared by the HSV and HSL conversion formulas.
fn chroma_x(h: f32, c: f32) -> f32 {
    let h6 = h.rem_euclid(360.0) / 60.0;
    c * (1.0 - (h6 % 2.0 - 1.0).abs())
}

// Pick the (R', G', B') components for the hue sector before adding the
// achromatic offset `m`. Hue is wrapped to `[0, 360)`.
fn hue_sector(h: f32, c: f32, x: f32) -> (f32, f32, f32) {
    let sector = (h.rem_euclid(360.0) / 60.0) as u32;
    match sector {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_alpha_replaces_and_clamps_alpha() {
        let color = Color::rgba_u8(10, 20, 30, 128);

        assert_eq!(color.with_alpha(0.25).a, 0.25);
        assert_eq!(color.with_alpha(-1.0).a, 0.0);
        assert_eq!(color.with_alpha(2.0).a, 1.0);
    }

    #[test]
    fn multiply_alpha_scales_existing_alpha() {
        let color = Color::rgb_u8(10, 20, 30).with_alpha(0.5);

        assert_eq!(color.multiply_alpha(0.5).a, 0.25);
        assert_eq!(color.multiply_alpha(-1.0).a, 0.0);
        assert_eq!(color.multiply_alpha(2.0).a, 0.5);
    }
}
