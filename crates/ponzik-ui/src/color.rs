/// Couleur sRGB 8 bits, alpha droit (non prémultiplié).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const TRANSPARENT: Color = Color::rgba(0, 0, 0, 0);

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// `0xRRGGBB`, opaque.
    pub const fn hex(rgb: u32) -> Self {
        Self::rgba((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, 255)
    }

    /// Multiplie l'opacité.
    pub fn alpha(self, a: f32) -> Self {
        Self { a: (self.a as f32 * a.clamp(0.0, 1.0)).round() as u8, ..self }
    }

    /// Interpolation linéaire vers `other`.
    pub fn mix(self, other: Color, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        let l = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Self { r: l(self.r, other.r), g: l(self.g, other.g), b: l(self.b, other.b), a: l(self.a, other.a) }
    }

    pub(crate) fn rgb_u32(self) -> u32 {
        (self.r as u32) << 16 | (self.g as u32) << 8 | self.b as u32
    }
}

/// Mélange `c` sur `dst` avec une couverture dans [0, 1].
#[inline]
pub(crate) fn blend(dst: &mut u32, c: Color, coverage: f32) {
    let a = (c.a as f32 * coverage + 0.5) as u32;
    if a == 0 {
        return;
    }
    if a >= 255 {
        *dst = c.rgb_u32();
        return;
    }
    let inv = 255 - a;
    let d = *dst;
    let ch = |shift: u32, s: u8| {
        let dv = (d >> shift) & 0xff;
        ((s as u32 * a + dv * inv + 127) / 255) << shift
    };
    *dst = ch(16, c.r) | ch(8, c.g) | ch(0, c.b);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blending() {
        let mut px = 0x000000;
        blend(&mut px, Color::hex(0xffffff), 1.0);
        assert_eq!(px, 0xffffff);
        let mut px = 0x000000;
        blend(&mut px, Color::hex(0xffffff), 0.5);
        assert_eq!(px, 0x808080);
        let mut px = 0x123456;
        blend(&mut px, Color::hex(0xffffff).alpha(0.0), 1.0);
        assert_eq!(px, 0x123456);
    }

    #[test]
    fn mixing() {
        assert_eq!(Color::hex(0x000000).mix(Color::hex(0xff8000), 0.5), Color::hex(0x804000));
    }
}
