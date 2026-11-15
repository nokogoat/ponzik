use std::f32::consts::TAU;

use crate::color::{Color, blend};
use crate::geom::Rect;
use crate::raster::Rasterizer;

type Pt = (f32, f32);

#[derive(Clone, Copy, Debug)]
enum Seg {
    Line(Pt, Pt),
    Quad(Pt, Pt, Pt),
}

/// Chemin vectoriel fermé : lignes et courbes quadratiques.
#[derive(Clone, Debug, Default)]
pub struct Path {
    segs: Vec<Seg>,
    start: Pt,
    cur: Pt,
}

impl Path {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn move_to(&mut self, p: Pt) {
        self.close();
        self.start = p;
        self.cur = p;
    }

    pub fn line_to(&mut self, p: Pt) {
        self.segs.push(Seg::Line(self.cur, p));
        self.cur = p;
    }

    pub fn quad_to(&mut self, c: Pt, p: Pt) {
        self.segs.push(Seg::Quad(self.cur, c, p));
        self.cur = p;
    }

    pub fn close(&mut self) {
        if self.cur != self.start {
            self.segs.push(Seg::Line(self.cur, self.start));
            self.cur = self.start;
        }
    }

    /// Ajoute un polygone fermé, toujours dans le même sens de rotation pour que
    /// les recouvrements s'additionnent au lieu de s'annuler.
    pub fn polygon(&mut self, pts: &[Pt]) {
        let n = pts.len();
        if n < 3 {
            return;
        }
        self.close();
        let area: f32 = (0..n)
            .map(|i| {
                let (a, b) = (pts[i], pts[(i + 1) % n]);
                a.0 * b.1 - b.0 * a.1
            })
            .sum();
        let at = |i: usize| if area >= 0.0 { pts[i] } else { pts[n - 1 - i] };
        for i in 0..n {
            self.segs.push(Seg::Line(at(i), at((i + 1) % n)));
        }
    }

    fn bounds(&self) -> Option<(f32, f32, f32, f32)> {
        let mut b: Option<(f32, f32, f32, f32)> = None;
        let mut add = |p: Pt| {
            b = Some(match b {
                None => (p.0, p.1, p.0, p.1),
                Some((x0, y0, x1, y1)) => (x0.min(p.0), y0.min(p.1), x1.max(p.0), y1.max(p.1)),
            });
        };
        for s in &self.segs {
            match *s {
                Seg::Line(a, c) => {
                    add(a);
                    add(c);
                }
                Seg::Quad(a, c, d) => {
                    add(a);
                    add(c);
                    add(d);
                }
            }
        }
        b
    }

    pub(crate) fn rasterize(&self, r: &mut Rasterizer, dx: f32, dy: f32) {
        let t = |p: Pt| (p.0 + dx, p.1 + dy);
        for s in &self.segs {
            match *s {
                Seg::Line(a, b) => r.line(t(a), t(b)),
                Seg::Quad(a, c, b) => r.quad(t(a), t(c), t(b)),
            }
        }
        // Un contour resté ouvert est refermé.
        if self.cur != self.start {
            r.line(t(self.cur), t(self.start));
        }
    }
}

/// Longueur de [c - 0.5, c + 0.5] ∩ [lo, hi].
#[inline]
fn overlap(c: f32, lo: f32, hi: f32) -> f32 {
    ((c + 0.5).min(hi) - (c - 0.5).max(lo)).clamp(0.0, 1.0)
}

/// Couverture d'un pixel (centre `cx`, `cy`) par un rectangle arrondi.
#[inline]
fn rrect_cov(r: &Rect, radius: f32, cx: f32, cy: f32) -> f32 {
    if radius > 0.5 {
        let qx = if cx < r.x + radius {
            r.x + radius - cx
        } else if cx > r.right() - radius {
            cx - (r.right() - radius)
        } else {
            -1.0
        };
        let qy = if cy < r.y + radius {
            r.y + radius - cy
        } else if cy > r.bottom() - radius {
            cy - (r.bottom() - radius)
        } else {
            -1.0
        };
        if qx > 0.0 && qy > 0.0 {
            let d = (qx * qx + qy * qy).sqrt() - radius;
            return (0.5 - d).clamp(0.0, 1.0);
        }
    }
    overlap(cx, r.x, r.right()) * overlap(cy, r.y, r.bottom())
}

/// Tampon de pixels `0x00RRGGBB` et primitives de dessin anti-aliasées.
pub struct Canvas {
    width: usize,
    height: usize,
    pixels: Vec<u32>,
    /// Zone de découpe en pixels entiers : x0, y0, x1, y1.
    clip: (usize, usize, usize, usize),
    raster: Rasterizer,
}

impl Canvas {
    pub fn new(width: usize, height: usize) -> Self {
        let mut c =
            Self { width: 0, height: 0, pixels: Vec::new(), clip: (0, 0, 0, 0), raster: Rasterizer::new() };
        c.resize(width, height);
        c
    }

    pub fn resize(&mut self, width: usize, height: usize) {
        self.width = width;
        self.height = height;
        self.pixels.resize(width * height, 0);
        self.clip = (0, 0, width, height);
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    /// Restreint le dessin à `r` (ou à tout le canvas avec `None`).
    pub fn set_clip(&mut self, r: Option<Rect>) {
        self.clip = match r {
            None => (0, 0, self.width, self.height),
            Some(r) => {
                let x0 = (r.x.floor().max(0.0) as usize).min(self.width);
                let y0 = (r.y.floor().max(0.0) as usize).min(self.height);
                let x1 = (r.right().ceil().max(0.0) as usize).clamp(x0, self.width);
                let y1 = (r.bottom().ceil().max(0.0) as usize).clamp(y0, self.height);
                (x0, y0, x1, y1)
            }
        };
    }

    pub fn clear(&mut self, c: Color) {
        self.pixels.fill(c.rgb_u32());
    }

    fn bounds(&self, x0: f32, y0: f32, x1: f32, y1: f32) -> Option<(usize, usize, usize, usize)> {
        let (cx0, cy0, cx1, cy1) = self.clip;
        let bx0 = (x0.floor().max(0.0) as usize).max(cx0);
        let by0 = (y0.floor().max(0.0) as usize).max(cy0);
        let bx1 = (x1.ceil().max(0.0) as usize).min(cx1);
        let by1 = (y1.ceil().max(0.0) as usize).min(cy1);
        (bx0 < bx1 && by0 < by1).then_some((bx0, by0, bx1, by1))
    }

    pub fn fill_rect(&mut self, r: Rect, c: Color) {
        self.fill_rrect(r, 0.0, c);
    }

    pub fn fill_rrect(&mut self, r: Rect, radius: f32, c: Color) {
        self.fill_rrect_with(r, radius, |_| c);
    }

    /// Dégradé vertical, réservé aux nuances d'une même teinte.
    pub fn fill_rrect_vgrad(&mut self, r: Rect, radius: f32, top: Color, bottom: Color) {
        self.fill_rrect_with(r, radius, |t| top.mix(bottom, t));
    }

    fn fill_rrect_with(&mut self, r: Rect, radius: f32, color_at: impl Fn(f32) -> Color) {
        if r.w <= 0.0 || r.h <= 0.0 {
            return;
        }
        let radius = radius.clamp(0.0, r.w.min(r.h) * 0.5);
        let Some((x0, y0, x1, y1)) = self.bounds(r.x, r.y, r.right(), r.bottom()) else { return };
        for py in y0..y1 {
            let cy = py as f32 + 0.5;
            let c = color_at(((cy - r.y) / r.h).clamp(0.0, 1.0));
            if c.a == 0 {
                continue;
            }
            let row = &mut self.pixels[py * self.width..(py + 1) * self.width];
            let cov_y = overlap(cy, r.y, r.bottom());
            let corner_row = radius > 0.5 && (cy < r.y + radius || cy > r.bottom() - radius);
            // Zone droite : pixels entièrement dans la largeur et hors des coins ;
            // seuls les bords et les coins demandent un calcul par pixel.
            let (lo, hi) = if corner_row {
                (r.x + radius - 0.5, r.right() - radius + 0.5)
            } else {
                (r.x, r.right())
            };
            let ix0 = (lo.ceil().max(0.0) as usize).clamp(x0, x1);
            let ix1 = (hi.floor().max(0.0) as usize).clamp(ix0, x1);
            for px in (x0..ix0).chain(ix1..x1) {
                blend(&mut row[px], c, rrect_cov(&r, radius, px as f32 + 0.5, cy));
            }
            fill_span(&mut row[ix0..ix1], c, cov_y);
        }
    }

    /// Contour intérieur d'épaisseur `width`.
    pub fn stroke_rrect(&mut self, r: Rect, radius: f32, width: f32, c: Color) {
        if r.w <= 0.0 || r.h <= 0.0 || c.a == 0 {
            return;
        }
        let radius = radius.clamp(0.0, r.w.min(r.h) * 0.5);
        let inner = r.inset(width);
        let inner_radius = (radius - width).max(0.0);
        let Some((x0, y0, x1, y1)) = self.bounds(r.x, r.y, r.right(), r.bottom()) else { return };
        // Zone droite commune au bord extérieur et intérieur : la couverture n'y
        // dépend que de la ligne.
        let lo = (r.x + radius).max(inner.x + inner_radius);
        let hi = (r.right() - radius).min(inner.right() - inner_radius);
        let sx0 = ((lo - 0.5).ceil().max(0.0) as usize).clamp(x0, x1);
        let sx1 = ((hi + 0.5).floor().max(0.0) as usize).clamp(sx0, x1);
        for py in y0..y1 {
            let cy = py as f32 + 0.5;
            let row = &mut self.pixels[py * self.width..(py + 1) * self.width];
            for px in (x0..sx0).chain(sx1..x1) {
                let cx = px as f32 + 0.5;
                let cov = rrect_cov(&r, radius, cx, cy) - rrect_cov(&inner, inner_radius, cx, cy);
                if cov > 0.0 {
                    blend(&mut row[px], c, cov);
                }
            }
            let inner_y = if inner.w > 0.0 { overlap(cy, inner.y, inner.bottom()) } else { 0.0 };
            let cov = overlap(cy, r.y, r.bottom()) - inner_y;
            if cov > 0.0 {
                fill_span(&mut row[sx0..sx1], c, cov);
            }
        }
    }

    pub fn fill_circle(&mut self, cx: f32, cy: f32, radius: f32, c: Color) {
        self.fill_rrect(Rect::new(cx - radius, cy - radius, radius * 2.0, radius * 2.0), radius, c);
    }

    pub fn stroke_circle(&mut self, cx: f32, cy: f32, radius: f32, width: f32, c: Color) {
        self.stroke_rrect(Rect::new(cx - radius, cy - radius, radius * 2.0, radius * 2.0), radius, width, c);
    }

    pub fn fill_path(&mut self, path: &Path, c: Color) {
        let Some((bx0, by0, bx1, by1)) = path.bounds() else { return };
        let Some((x0, y0, x1, y1)) = self.bounds(bx0, by0, bx1, by1) else { return };
        let Canvas { raster, pixels, width, .. } = self;
        raster.reset(x1 - x0, y1 - y0);
        path.rasterize(raster, -(x0 as f32), -(y0 as f32));
        raster.for_each_coverage(|x, y, cov| blend(&mut pixels[(y0 + y) * *width + x0 + x], c, cov));
    }

    /// Ligne brisée d'épaisseur `width`, jointures arrondies.
    pub fn stroke_polyline(&mut self, pts: &[Pt], width: f32, c: Color) {
        let mut path = Path::new();
        let hw = width * 0.5;
        for w in pts.windows(2) {
            let (a, b) = (w[0], w[1]);
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let len = (dx * dx + dy * dy).sqrt();
            if len < 1e-4 {
                continue;
            }
            let (nx, ny) = (-dy / len * hw, dx / len * hw);
            path.polygon(&[(a.0 + nx, a.1 + ny), (b.0 + nx, b.1 + ny), (b.0 - nx, b.1 - ny), (a.0 - nx, a.1 - ny)]);
        }
        if width > 1.5 {
            for &p in pts.iter().skip(1).take(pts.len().saturating_sub(2)) {
                path.polygon(&disc(p, hw, 10));
            }
        }
        self.fill_path(&path, c);
    }

    /// Arc de cercle épais, angles en radians (0 = droite, sens horaire à l'écran), bouts arrondis.
    pub fn stroke_arc(&mut self, cx: f32, cy: f32, radius: f32, a0: f32, a1: f32, width: f32, c: Color) {
        let (a0, a1) = if a0 <= a1 { (a0, a1) } else { (a1, a0) };
        let sweep = a1 - a0;
        if sweep <= 1e-4 {
            return;
        }
        let n = ((sweep * radius / 2.0).ceil() as usize).clamp(4, 256);
        let (ro, ri) = (radius + width * 0.5, radius - width * 0.5);
        let mut pts = Vec::with_capacity(2 * n + 2);
        for i in 0..=n {
            let a = a0 + sweep * i as f32 / n as f32;
            pts.push((cx + a.cos() * ro, cy + a.sin() * ro));
        }
        for i in (0..=n).rev() {
            let a = a0 + sweep * i as f32 / n as f32;
            pts.push((cx + a.cos() * ri, cy + a.sin() * ri));
        }
        let mut path = Path::new();
        path.polygon(&pts);
        for a in [a0, a1] {
            path.polygon(&disc((cx + a.cos() * radius, cy + a.sin() * radius), width * 0.5, 12));
        }
        self.fill_path(&path, c);
    }

    /// Masque d'opacité 8 bits (glyphes), coin haut-gauche en (`x`, `y`).
    pub fn draw_mask(&mut self, x: i32, y: i32, w: usize, h: usize, mask: &[u8], c: Color) {
        let (cx0, cy0, cx1, cy1) = self.clip;
        for my in 0..h {
            let py = y + my as i32;
            if py < cy0 as i32 || py >= cy1 as i32 {
                continue;
            }
            let row = py as usize * self.width;
            for mx in 0..w {
                let px = x + mx as i32;
                if px < cx0 as i32 || px >= cx1 as i32 {
                    continue;
                }
                let m = mask[my * w + mx];
                if m != 0 {
                    blend(&mut self.pixels[row + px as usize], c, m as f32 / 255.0);
                }
            }
        }
    }
}

/// Remplit une portion de ligne avec une couverture constante.
fn fill_span(span: &mut [u32], c: Color, coverage: f32) {
    if coverage >= 1.0 && c.a == 255 {
        span.fill(c.rgb_u32());
    } else if coverage > 0.0 {
        for dst in span {
            blend(dst, c, coverage);
        }
    }
}

fn disc(center: Pt, r: f32, n: usize) -> Vec<Pt> {
    (0..n).map(|i| {
        let a = TAU * i as f32 / n as f32;
        (center.0 + a.cos() * r, center.1 + a.sin() * r)
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coverage_sum(c: &Canvas) -> f32 {
        c.pixels().iter().map(|&p| (p & 0xff) as f32 / 255.0).sum()
    }

    #[test]
    fn rect_area_matches_with_fractional_edges() {
        let mut c = Canvas::new(40, 40);
        c.fill_rect(Rect::new(5.25, 5.5, 10.5, 20.0), Color::hex(0xffffff));
        assert!((coverage_sum(&c) - 210.0).abs() < 1.0);
    }

    #[test]
    fn rounded_rect_loses_corner_area() {
        let mut c = Canvas::new(40, 40);
        c.fill_rrect(Rect::new(0.0, 0.0, 40.0, 40.0), 10.0, Color::hex(0xffffff));
        let expected = 1600.0 - (4.0 - std::f32::consts::PI) * 100.0;
        assert!((coverage_sum(&c) - expected).abs() < 3.0, "{}", coverage_sum(&c));
    }

    #[test]
    fn stroke_is_a_ring() {
        let mut c = Canvas::new(40, 40);
        c.stroke_rrect(Rect::new(0.0, 0.0, 40.0, 40.0), 0.0, 1.0, Color::hex(0xffffff));
        assert!((coverage_sum(&c) - 156.0).abs() < 1.0);
        assert_eq!(c.pixels()[20 * 40 + 20], 0);
    }

    #[test]
    fn path_and_clip() {
        let mut c = Canvas::new(20, 20);
        c.set_clip(Some(Rect::new(0.0, 0.0, 10.0, 20.0)));
        let mut p = Path::new();
        p.polygon(&[(0.0, 0.0), (20.0, 0.0), (20.0, 20.0), (0.0, 20.0)]);
        c.fill_path(&p, Color::hex(0xffffff));
        assert!((coverage_sum(&c) - 200.0).abs() < 1e-2);
    }

    #[test]
    fn overlapping_strokes_do_not_cancel() {
        let mut c = Canvas::new(20, 20);
        c.stroke_polyline(&[(2.0, 10.0), (18.0, 10.0), (2.0, 10.0)], 4.0, Color::hex(0xffffff));
        assert_eq!(c.pixels()[10 * 20 + 10] & 0xff, 0xff);
    }
}
