//! Transformée de Fourier rapide radix-2 (tailles puissances de deux, jusqu'à 16384) et fenêtres d'analyse.
//!
//! Les tables (facteurs de rotation, permutation) sont calculées à la construction : `forward` et
//! `inverse` n'allouent rien et peuvent tourner dans le thread audio.

use std::f64::consts::TAU;

/// Plus grande taille acceptée.
pub const MAX_SIZE: usize = 16384;

/// FFT complexe en place, décimation temporelle itérative.
#[derive(Clone, Debug)]
pub struct Fft {
    n: usize,
    /// `e^(-2iπ k / n)` pour `k < n / 2`, en parties réelle et imaginaire.
    cos: Vec<f32>,
    sin: Vec<f32>,
    /// Paires `(i, j)` à échanger (`i < j`) pour la permutation bit-inversée.
    swaps: Vec<(u32, u32)>,
}

impl Fft {
    /// `n` doit être une puissance de deux entre 1 et [`MAX_SIZE`].
    pub fn new(n: usize) -> Self {
        assert!(n.is_power_of_two() && n <= MAX_SIZE, "taille de FFT invalide : {n}");
        let (cos, sin) = (0..n / 2)
            .map(|k| {
                let (s, c) = (-TAU * k as f64 / n as f64).sin_cos();
                (c as f32, s as f32)
            })
            .unzip();
        let bits = n.trailing_zeros();
        let swaps = (0..n)
            .filter_map(|i| {
                let j = if bits == 0 { 0 } else { i.reverse_bits() >> (usize::BITS - bits) };
                (i < j).then_some((i as u32, j as u32))
            })
            .collect();
        Self { n, cos, sin, swaps }
    }

    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// Transformée directe non normalisée : `X[k] = Σ x[j] e^(-2iπ jk / n)`.
    pub fn forward(&self, re: &mut [f32], im: &mut [f32]) {
        assert!(re.len() == self.n && im.len() == self.n, "tampons de taille différente de la FFT");
        for &(i, j) in &self.swaps {
            re.swap(i as usize, j as usize);
            im.swap(i as usize, j as usize);
        }
        let mut half = 1;
        while half < self.n {
            let stride = self.n / (2 * half);
            for start in (0..self.n).step_by(2 * half) {
                for k in 0..half {
                    let (wr, wi) = (self.cos[k * stride], self.sin[k * stride]);
                    let (a, b) = (start + k, start + k + half);
                    let tr = re[b] * wr - im[b] * wi;
                    let ti = re[b] * wi + im[b] * wr;
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
            }
            half *= 2;
        }
    }

    /// Transformée inverse normalisée (divisée par `n`) : `inverse(forward(x)) = x`.
    pub fn inverse(&self, re: &mut [f32], im: &mut [f32]) {
        im.iter_mut().for_each(|v| *v = -*v);
        self.forward(re, im);
        let scale = 1.0 / self.n as f32;
        re.iter_mut().for_each(|v| *v *= scale);
        im.iter_mut().for_each(|v| *v *= -scale);
    }
}

/// FFT d'un signal réel de `n` échantillons, calculée par une FFT complexe de `n / 2` points.
///
/// Sortie : les `n / 2 + 1` premiers coefficients (les autres sont leurs conjugués).
#[derive(Clone, Debug)]
pub struct RealFft {
    n: usize,
    half: Fft,
    /// `e^(-2iπ k / n)` pour `k ≤ n / 2`.
    cos: Vec<f32>,
    sin: Vec<f32>,
    re: Vec<f32>,
    im: Vec<f32>,
}

impl RealFft {
    /// `n` doit être une puissance de deux entre 2 et [`MAX_SIZE`].
    pub fn new(n: usize) -> Self {
        assert!(n >= 2 && n.is_power_of_two() && n <= MAX_SIZE, "taille de FFT réelle invalide : {n}");
        let (cos, sin) = (0..=n / 2)
            .map(|k| {
                let (s, c) = (-TAU * k as f64 / n as f64).sin_cos();
                (c as f32, s as f32)
            })
            .unzip();
        Self { n, half: Fft::new(n / 2), cos, sin, re: vec![0.0; n / 2], im: vec![0.0; n / 2] }
    }

    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// Nombre de coefficients produits : `n / 2 + 1`.
    pub fn bins(&self) -> usize {
        self.n / 2 + 1
    }

    /// `input` : `n` échantillons ; `out_re`, `out_im` : `n / 2 + 1` coefficients non normalisés.
    pub fn forward(&mut self, input: &[f32], out_re: &mut [f32], out_im: &mut [f32]) {
        let h = self.n / 2;
        assert!(input.len() == self.n, "entrée de taille différente de la FFT");
        assert!(out_re.len() == h + 1 && out_im.len() == h + 1, "sortie de taille différente de n / 2 + 1");
        // Échantillons pairs en partie réelle, impairs en partie imaginaire.
        for (k, pair) in input.chunks_exact(2).enumerate() {
            self.re[k] = pair[0];
            self.im[k] = pair[1];
        }
        self.half.forward(&mut self.re, &mut self.im);
        // Séparation : Z[k] = E[k] + i O[k], puis X[k] = E[k] + W^k O[k].
        for k in 0..=h {
            let (zr, zi) = (self.re[k % h], self.im[k % h]);
            let (cr, ci) = (self.re[(h - k) % h], -self.im[(h - k) % h]);
            let (er, ei) = (0.5 * (zr + cr), 0.5 * (zi + ci));
            // O = (Z - conj(Z[h-k])) / 2i
            let (or, oi) = (0.5 * (zi - ci), -0.5 * (zr - cr));
            let (wr, wi) = (self.cos[k], self.sin[k]);
            out_re[k] = er + or * wr - oi * wi;
            out_im[k] = ei + or * wi + oi * wr;
        }
    }
}

/// Fenêtres d'analyse, en version périodique (adaptée à la FFT).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Window {
    Rectangular,
    Hann,
    /// Blackman-Harris à 4 termes : lobes secondaires à −92 dB.
    #[default]
    BlackmanHarris,
}

impl Window {
    /// Remplit `out` avec la fenêtre de longueur `out.len()`.
    pub fn fill(self, out: &mut [f32]) {
        let n = out.len() as f64;
        for (i, w) in out.iter_mut().enumerate() {
            let x = TAU * i as f64 / n;
            *w = match self {
                Self::Rectangular => 1.0,
                Self::Hann => 0.5 - 0.5 * x.cos(),
                Self::BlackmanHarris => {
                    0.35875 - 0.48829 * x.cos() + 0.14128 * (2.0 * x).cos() - 0.01168 * (3.0 * x).cos()
                }
            } as f32;
        }
    }
}

/// Fenêtre de Hann périodique.
pub fn hann(out: &mut [f32]) {
    Window::Hann.fill(out);
}

/// Fenêtre de Blackman-Harris à 4 termes, périodique.
pub fn blackman_harris(out: &mut [f32]) {
    Window::BlackmanHarris.fill(out);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signal(n: usize, seed: u32) -> Vec<f32> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (s >> 8) as f32 / (1u32 << 23) as f32 - 1.0
            })
            .collect()
    }

    fn naive_dft(re: &[f32], im: &[f32]) -> (Vec<f64>, Vec<f64>) {
        let n = re.len();
        (0..n)
            .map(|k| {
                let (mut sr, mut si) = (0.0, 0.0);
                for j in 0..n {
                    let (s, c) = (-TAU * ((j * k) % n) as f64 / n as f64).sin_cos();
                    sr += re[j] as f64 * c - im[j] as f64 * s;
                    si += re[j] as f64 * s + im[j] as f64 * c;
                }
                (sr, si)
            })
            .unzip()
    }

    fn assert_close(a: &[f32], b: &[f64], tol: f64) {
        for (k, (x, y)) in a.iter().zip(b).enumerate() {
            assert!((*x as f64 - y).abs() < tol, "coefficient {k} : {x} au lieu de {y}");
        }
    }

    #[test]
    fn complex_matches_naive_dft() {
        for n in [1, 2, 4, 8, 16, 64, 256, 1024] {
            let (mut re, mut im) = (signal(n, 1), signal(n, 2));
            let (er, ei) = naive_dft(&re, &im);
            Fft::new(n).forward(&mut re, &mut im);
            let tol = 1e-4 * n as f64;
            assert_close(&re, &er, tol);
            assert_close(&im, &ei, tol);
        }
    }

    #[test]
    fn real_matches_naive_dft() {
        for n in [2, 4, 8, 32, 512, 2048] {
            let x = signal(n, 7);
            let (er, ei) = naive_dft(&x, &vec![0.0; n]);
            let mut fft = RealFft::new(n);
            let (mut re, mut im) = (vec![0.0; n / 2 + 1], vec![0.0; n / 2 + 1]);
            fft.forward(&x, &mut re, &mut im);
            let tol = 1e-4 * n as f64;
            assert_close(&re, &er[..=n / 2], tol);
            assert_close(&im, &ei[..=n / 2], tol);
        }
    }

    #[test]
    fn inverse_roundtrip_at_max_size() {
        let x = signal(MAX_SIZE, 3);
        let (mut re, mut im) = (x.clone(), vec![0.0; MAX_SIZE]);
        let fft = Fft::new(MAX_SIZE);
        fft.forward(&mut re, &mut im);
        fft.inverse(&mut re, &mut im);
        for (a, b) in re.iter().zip(&x) {
            assert!((a - b).abs() < 1e-4);
        }
        assert!(im.iter().all(|v| v.abs() < 1e-4));
    }

    #[test]
    fn sine_lands_in_its_bin() {
        let n = 1024;
        let x: Vec<f32> = (0..n).map(|i| (TAU * 37.0 * i as f64 / n as f64).cos() as f32).collect();
        let mut fft = RealFft::new(n);
        let (mut re, mut im) = (vec![0.0; n / 2 + 1], vec![0.0; n / 2 + 1]);
        fft.forward(&x, &mut re, &mut im);
        for k in 0..=n / 2 {
            let mag = re[k].hypot(im[k]);
            let want = if k == 37 { n as f32 / 2.0 } else { 0.0 };
            assert!((mag - want).abs() < 1e-2, "coefficient {k} : {mag}");
        }
    }

    #[test]
    fn windows_shape() {
        let mut w = vec![0.0; 1024];
        hann(&mut w);
        assert!(w[0].abs() < 1e-7 && (w[512] - 1.0).abs() < 1e-7);
        assert!((w.iter().sum::<f32>() / 1024.0 - 0.5).abs() < 1e-5);
        assert!((1..512).all(|i| (w[i] - w[1024 - i]).abs() < 1e-6));
        blackman_harris(&mut w);
        assert!(w[0].abs() < 1e-4 && (w[512] - 1.0).abs() < 1e-5);
        assert!((w.iter().sum::<f32>() / 1024.0 - 0.35875).abs() < 1e-5);
        Window::Rectangular.fill(&mut w);
        assert!(w.iter().all(|&v| v == 1.0));
    }
}
