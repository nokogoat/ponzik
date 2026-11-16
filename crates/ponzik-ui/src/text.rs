use std::collections::HashMap;

use crate::canvas::{Canvas, Path};
use crate::color::Color;
use crate::font::{Contour, Font};
use crate::raster::Rasterizer;

/// Positions horizontales sous-pixel mises en cache par glyphe.
const SUBPIXEL_STEPS: f32 = 4.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FontId(usize);

struct Glyph {
    left: i32,
    top: i32,
    w: usize,
    h: usize,
    mask: Vec<u8>,
}

#[derive(Hash, PartialEq, Eq)]
struct GlyphKey {
    font: usize,
    gid: u16,
    /// Taille en quarts de pixel.
    size: u32,
    subpixel: u8,
}

/// Polices chargées et cache des glyphes rastérisés.
pub struct Fonts {
    fonts: Vec<Font>,
    glyphs: HashMap<GlyphKey, Glyph>,
    raster: Rasterizer,
}

impl Default for Fonts {
    fn default() -> Self {
        Self::new()
    }
}

impl Fonts {
    pub fn new() -> Self {
        Self { fonts: Vec::new(), glyphs: HashMap::new(), raster: Rasterizer::new() }
    }

    pub fn add(&mut self, data: &'static [u8]) -> Result<FontId, String> {
        self.fonts.push(Font::parse(data)?);
        Ok(FontId(self.fonts.len() - 1))
    }

    fn scale(&self, f: FontId, size: f32) -> f32 {
        size / self.fonts[f.0].units_per_em
    }

    /// Hauteur des capitales, pour centrer du texte optiquement.
    pub fn cap_height(&self, f: FontId, size: f32) -> f32 {
        self.fonts[f.0].cap_height * self.scale(f, size)
    }

    pub fn ascent(&self, f: FontId, size: f32) -> f32 {
        self.fonts[f.0].ascent * self.scale(f, size)
    }

    pub fn measure(&self, f: FontId, size: f32, text: &str) -> f32 {
        let font = &self.fonts[f.0];
        let s = self.scale(f, size);
        let mut pen = 0.0;
        let mut prev = None;
        for c in text.chars() {
            let gid = font.glyph_index(c);
            if let Some(p) = prev {
                pen += font.kerning(p, gid) * s;
            }
            pen += font.advance(gid) * s;
            prev = Some(gid);
        }
        pen
    }

    /// Dessine `text` sur la ligne de base `baseline` ; renvoie la largeur avancée.
    /// Tout est logique : les glyphes sont rastérisés à la taille physique
    /// (`size` × l'échelle du canvas), jamais agrandis après coup.
    pub fn draw(
        &mut self,
        canvas: &mut Canvas,
        f: FontId,
        size: f32,
        x: f32,
        baseline: f32,
        text: &str,
        color: Color,
    ) -> f32 {
        let k = canvas.scale();
        let size = size * k;
        let s = self.scale(f, size);
        let baseline = (baseline * k).round() as i32;
        let size_key = (size * 4.0).round() as u32;
        let x = x * k;
        let mut pen = x;
        let mut prev = None;
        for c in text.chars() {
            let font = &self.fonts[f.0];
            let gid = font.glyph_index(c);
            if let Some(p) = prev {
                pen += font.kerning(p, gid) * s;
            }
            let advance = font.advance(gid) * s;
            prev = Some(gid);

            let mut ix = pen.floor();
            let mut sub = ((pen - ix) * SUBPIXEL_STEPS).round() as u8;
            if sub as f32 >= SUBPIXEL_STEPS {
                ix += 1.0;
                sub = 0;
            }
            let key = GlyphKey { font: f.0, gid, size: size_key, subpixel: sub };
            if !self.glyphs.contains_key(&key) {
                let outline = self.fonts[f.0].outline(gid);
                let glyph = rasterize(&mut self.raster, &outline, s, sub as f32 / SUBPIXEL_STEPS);
                self.glyphs.insert(GlyphKey { font: f.0, gid, size: size_key, subpixel: sub }, glyph);
            }
            let g = &self.glyphs[&key];
            if g.w > 0 {
                canvas.draw_mask(ix as i32 + g.left, baseline + g.top, g.w, g.h, &g.mask, color);
            }
            pen += advance;
        }
        (pen - x) / k
    }
}

fn rasterize(raster: &mut Rasterizer, contours: &[Contour], scale: f32, dx: f32) -> Glyph {
    let empty = Glyph { left: 0, top: 0, w: 0, h: 0, mask: Vec::new() };
    let pts = contours.iter().flatten();
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for p in pts {
        let (x, y) = (p.x * scale + dx, -p.y * scale);
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    }
    if x0 > x1 {
        return empty;
    }
    let (left, top) = (x0.floor() as i32, y0.floor() as i32);
    let w = (x1.ceil() as i32 - left + 1).max(1) as usize;
    let h = (y1.ceil() as i32 - top + 1).max(1) as usize;

    let mut path = Path::new();
    for contour in contours {
        add_contour(&mut path, contour, |x, y| (x * scale + dx - left as f32, -y * scale - top as f32));
    }
    raster.reset(w, h);
    path.rasterize(raster, 1.0, 0.0, 0.0);
    let mut mask = vec![0u8; w * h];
    // Légère correction gamma : le texte clair sur fond sombre garde son corps.
    raster.for_each_coverage(|x, y, c| mask[y * w + x] = (c.powf(0.85) * 255.0).round() as u8);
    Glyph { left, top, w, h, mask }
}

/// Contour TrueType → chemin, avec les points « sur la courbe » implicites
/// entre deux points de contrôle consécutifs.
fn add_contour(path: &mut Path, pts: &Contour, map: impl Fn(f32, f32) -> (f32, f32)) {
    let n = pts.len();
    if n == 0 {
        return;
    }
    let p = |i: usize| map(pts[i % n].x, pts[i % n].y);
    let mid = |a: (f32, f32), b: (f32, f32)| ((a.0 + b.0) * 0.5, (a.1 + b.1) * 0.5);
    let (start, offset) = match pts.iter().position(|q| q.on) {
        Some(i) => (p(i), i),
        None => (mid(p(0), p(1)), 0),
    };
    path.move_to(start);
    let mut ctrl: Option<(f32, f32)> = None;
    for k in 1..=n {
        let i = (offset + k) % n;
        let q = p(i);
        if pts[i].on {
            match ctrl.take() {
                Some(c) => path.quad_to(c, q),
                None => path.line_to(q),
            }
        } else {
            if let Some(c) = ctrl {
                path.quad_to(c, mid(c, q));
            }
            ctrl = Some(q);
        }
    }
    if let Some(c) = ctrl {
        path.quad_to(c, start);
    }
    path.close();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedded;

    #[test]
    fn draws_and_measures() {
        let mut fonts = Fonts::new();
        let sans = fonts.add(embedded::SANS_REGULAR).unwrap();
        let mut canvas = Canvas::new(200, 40);
        let w = fonts.draw(&mut canvas, sans, 20.0, 4.0, 28.0, "Ponzik", Color::hex(0xffffff));
        assert!((w - fonts.measure(sans, 20.0, "Ponzik")).abs() < 1e-3);
        assert!(w > 50.0 && w < 90.0, "{w}");
        let lit = canvas.pixels().iter().filter(|&&p| p != 0).count();
        assert!(lit > 150, "{lit} pixels allumés");
    }

    #[test]
    fn scaled_text_is_rasterized_at_physical_size() {
        let mut fonts = Fonts::new();
        let sans = fonts.add(embedded::SANS_REGULAR).unwrap();
        let white = Color::hex(0xffffff);
        let mut scaled = Canvas::new(400, 80);
        scaled.set_scale(2.0);
        let w = fonts.draw(&mut scaled, sans, 10.0, 2.0, 14.0, "Ponzik", white);
        // La largeur rendue reste logique, comme la mesure.
        assert!((w - fonts.measure(sans, 10.0, "Ponzik")).abs() < 1e-3, "{w}");
        // Mêmes pixels qu'un texte deux fois plus grand dessiné à l'échelle 1 :
        // les glyphes sont rastérisés en physique, pas agrandis.
        let mut direct = Canvas::new(400, 80);
        fonts.draw(&mut direct, sans, 20.0, 4.0, 28.0, "Ponzik", white);
        assert_eq!(scaled.pixels(), direct.pixels());
    }
}
