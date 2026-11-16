//! Rastériseur à accumulation de surface signée (même principe que font-rs) :
//! chaque segment dépose l'aire qu'il couvre, une somme préfixe par ligne donne
//! la couverture exacte de chaque pixel. Anti-aliasing analytique, sans suréchantillonnage.

pub(crate) struct Rasterizer {
    width: usize,
    height: usize,
    /// Largeur interne : deux colonnes de marge à droite.
    stride: usize,
    acc: Vec<f32>,
}

impl Rasterizer {
    pub(crate) fn new() -> Self {
        Self { width: 0, height: 0, stride: 0, acc: Vec::new() }
    }

    /// Prépare une zone vierge de `width` × `height` pixels.
    pub(crate) fn reset(&mut self, width: usize, height: usize) {
        self.width = width;
        self.height = height;
        self.stride = width + 2;
        let n = self.stride * height + 2;
        self.acc.clear();
        self.acc.resize(n, 0.0);
    }

    pub(crate) fn line(&mut self, (x0, y0): (f32, f32), (x1, y1): (f32, f32)) {
        if (y0 - y1).abs() <= f32::EPSILON || self.height == 0 {
            return;
        }
        let w = self.width as f32;
        let (dir, (x0, y0), (x1, y1)) =
            if y0 < y1 { (1.0, (x0, y0), (x1, y1)) } else { (-1.0, (x1, y1), (x0, y0)) };
        let dxdy = (x1 - x0) / (y1 - y0);
        let mut x = x0;
        if y0 < 0.0 {
            x -= y0 * dxdy;
        }
        let y_start = y0.max(0.0) as usize;
        let y_end = (self.height as f32).min(y1.ceil()).max(0.0) as usize;
        for y in y_start..y_end {
            let row = y * self.stride;
            let dy = ((y + 1) as f32).min(y1) - (y as f32).max(y0);
            let x_after = x + dxdy * dy;
            // Écraser x dans [0, w] ligne par ligne ne change pas la couverture visible.
            let (xc, xnext) = (x.clamp(0.0, w), x_after.clamp(0.0, w));
            let d = dy * dir;
            let (xa, xb) = if xc < xnext { (xc, xnext) } else { (xnext, xc) };
            let xa_floor = xa.floor();
            let xai = xa_floor as usize;
            let xb_ceil = xb.ceil();
            let xbi = xb_ceil as usize;
            if xbi <= xai + 1 {
                let xmf = 0.5 * (xc + xnext) - xa_floor;
                self.acc[row + xai] += d - d * xmf;
                self.acc[row + xai + 1] += d * xmf;
            } else {
                let s = (xb - xa).recip();
                let xaf = xa - xa_floor;
                let a0 = 0.5 * s * (1.0 - xaf) * (1.0 - xaf);
                let xbf = xb - xb_ceil + 1.0;
                let am = 0.5 * s * xbf * xbf;
                self.acc[row + xai] += d * a0;
                if xbi == xai + 2 {
                    self.acc[row + xai + 1] += d * (1.0 - a0 - am);
                } else {
                    let a1 = s * (1.5 - xaf);
                    self.acc[row + xai + 1] += d * (a1 - a0);
                    for xi in xai + 2..xbi - 1 {
                        self.acc[row + xi] += d * s;
                    }
                    let a2 = a1 + (xbi - xai - 3) as f32 * s;
                    self.acc[row + xbi - 1] += d * (1.0 - a2 - am);
                }
                self.acc[row + xbi] += d * am;
            }
            x = x_after;
        }
    }

    /// Courbe quadratique, découpée selon sa courbure.
    pub(crate) fn quad(&mut self, p0: (f32, f32), p1: (f32, f32), p2: (f32, f32)) {
        let devx = p0.0 - 2.0 * p1.0 + p2.0;
        let devy = p0.1 - 2.0 * p1.1 + p2.1;
        let devsq = devx * devx + devy * devy;
        if devsq < 0.333 {
            self.line(p0, p2);
            return;
        }
        let n = 1 + (3.0 * devsq).sqrt().sqrt().floor() as usize;
        let mut prev = p0;
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let mt = 1.0 - t;
            let p = (
                mt * mt * p0.0 + 2.0 * mt * t * p1.0 + t * t * p2.0,
                mt * mt * p0.1 + 2.0 * mt * t * p1.1 + t * t * p2.1,
            );
            self.line(prev, p);
            prev = p;
        }
    }

    /// Couverture finale de chaque pixel, ligne par ligne.
    pub(crate) fn for_each_coverage(&mut self, f: impl FnMut(usize, usize, f32)) {
        self.for_each_coverage_in(0, 0, self.width, self.height, f);
    }

    /// Couverture des seuls pixels de [x0, x1) × [y0, y1) ; la somme de chaque
    /// ligne part toujours de la colonne 0, donc les valeurs sont identiques.
    pub(crate) fn for_each_coverage_in(
        &mut self,
        x0: usize,
        y0: usize,
        x1: usize,
        y1: usize,
        mut f: impl FnMut(usize, usize, f32),
    ) {
        for y in y0..y1.min(self.height) {
            let row = &self.acc[y * self.stride..y * self.stride + x1.min(self.width)];
            let mut sum = 0.0f32;
            for (x, &a) in row.iter().enumerate() {
                sum += a;
                if x < x0 {
                    continue;
                }
                let c = sum.abs().min(1.0);
                if c > 1.0 / 512.0 {
                    f(x, y, c);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn total(r: &mut Rasterizer) -> f32 {
        let mut s = 0.0;
        r.for_each_coverage(|_, _, c| s += c);
        s
    }

    #[test]
    fn square_area_is_exact() {
        let mut r = Rasterizer::new();
        r.reset(10, 10);
        let pts = [(2.5, 2.5), (7.5, 2.5), (7.5, 7.5), (2.5, 7.5)];
        for i in 0..4 {
            r.line(pts[i], pts[(i + 1) % 4]);
        }
        assert!((total(&mut r) - 25.0).abs() < 1e-3);
    }

    #[test]
    fn triangle_area_and_clipping() {
        let mut r = Rasterizer::new();
        r.reset(20, 20);
        let pts = [(1.0, 1.0), (19.0, 1.0), (1.0, 19.0)];
        for i in 0..3 {
            r.line(pts[i], pts[(i + 1) % 3]);
        }
        assert!((total(&mut r) - 162.0).abs() < 1e-2);

        // Une forme qui déborde est rognée proprement.
        r.reset(10, 10);
        let pts = [(-5.0, -5.0), (15.0, -5.0), (15.0, 15.0), (-5.0, 15.0)];
        for i in 0..4 {
            r.line(pts[i], pts[(i + 1) % 4]);
        }
        assert!((total(&mut r) - 100.0).abs() < 1e-3);
    }
}
