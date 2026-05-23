/// sRGB with straight alpha. Each component is in the range `[0.0, 1.0]`.
#[derive(Debug, Clone, Copy, PartialEq)]
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
        Self { r: r1 + m, g: g1 + m, b: b1 + m, a }
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
        Self { r: r1 + m, g: g1 + m, b: b1 + m, a }
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
