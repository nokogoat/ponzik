//! Analyseur de spectre : FFT fenêtrée avec recouvrement, balistique (attaque instantanée, retombée en dB/s),
//! maintien des crêtes et lecture en bandes espacées logarithmiquement pour l'affichage.
//!
//! Tous les tampons sont alloués à la construction ; `push`, `bins` et les réglages n'allouent rien.

use crate::fft::{MAX_SIZE, RealFft, Window};

/// Niveau plancher (dB) : silence, et valeur de départ des courbes.
pub const FLOOR_DB: f32 = -160.0;

/// Fréquence où la pente d'affichage vaut 0 dB.
pub const TILT_PIVOT_HZ: f32 = 1000.0;

/// Agrégation des coefficients FFT tombant dans une même bande d'affichage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BandMode {
    /// Maximum en dB : une raie garde son niveau quelle que soit la largeur de la bande.
    #[default]
    Peak,
    /// Moyenne de puissance : courbe plus calme sur le bruit.
    Average,
}

/// Analyseur de spectre alimenté par blocs d'échantillons.
#[derive(Clone, Debug)]
pub struct SpectrumAnalyzer {
    sample_rate: f32,
    fft: RealFft,
    window: Vec<f32>,
    /// `2 / Σ fenêtre` : une sinusoïde d'amplitude 1 lit 0 dB.
    norm: f32,
    ring: Vec<f32>,
    pos: usize,
    filled: usize,
    pending: usize,
    hop: usize,
    frame: Vec<f32>,
    re: Vec<f32>,
    im: Vec<f32>,
    tilt_db_per_oct: f32,
    tilt: Vec<f32>,
    current: Vec<f32>,
    smoothed: Vec<f32>,
    peak: Vec<f32>,
    release_db_per_s: f32,
    peak_release_db_per_s: f32,
    mode: BandMode,
    frames: u64,
}

impl SpectrumAnalyzer {
    /// `size` : puissance de deux entre 64 et 16384 (2048, 4096 ou 8192 en pratique).
    /// `overlap` : recouvrement entre trames, borné à [0, 0,875] (50 à 75 % conseillé).
    pub fn new(sample_rate: f32, size: usize, overlap: f32) -> Self {
        assert!(size.is_power_of_two() && (64..=MAX_SIZE).contains(&size), "taille d'analyse invalide : {size}");
        let bins = size / 2 + 1;
        let hop = ((size as f32 * (1.0 - overlap.clamp(0.0, 0.875))).round() as usize).max(1);
        let mut analyzer = Self {
            sample_rate,
            fft: RealFft::new(size),
            window: vec![0.0; size],
            norm: 0.0,
            ring: vec![0.0; size],
            pos: 0,
            filled: 0,
            pending: 0,
            hop,
            frame: vec![0.0; size],
            re: vec![0.0; bins],
            im: vec![0.0; bins],
            tilt_db_per_oct: 0.0,
            tilt: vec![0.0; bins],
            current: vec![FLOOR_DB; bins],
            smoothed: vec![FLOOR_DB; bins],
            peak: vec![FLOOR_DB; bins],
            release_db_per_s: 30.0,
            peak_release_db_per_s: 0.0,
            mode: BandMode::Peak,
            frames: 0,
        };
        analyzer.set_window(Window::BlackmanHarris);
        analyzer
    }

    pub fn size(&self) -> usize {
        self.ring.len()
    }

    /// Échantillons entre deux trames.
    pub fn hop(&self) -> usize {
        self.hop
    }

    /// Écart en Hz entre deux coefficients FFT.
    pub fn bin_hz(&self) -> f32 {
        self.sample_rate / self.size() as f32
    }

    /// Nombre de trames analysées depuis la construction ou le dernier `reset`.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Dernière trame brute, en dB par coefficient FFT (pente comprise).
    pub fn current(&self) -> &[f32] {
        &self.current
    }

    /// Courbe lissée, en dB par coefficient FFT.
    pub fn smoothed(&self) -> &[f32] {
        &self.smoothed
    }

    /// Crêtes maintenues, en dB par coefficient FFT.
    pub fn peaks(&self) -> &[f32] {
        &self.peak
    }

    pub fn set_window(&mut self, window: Window) {
        window.fill(&mut self.window);
        self.norm = 2.0 / self.window.iter().sum::<f32>().max(1e-12);
    }

    /// Pente d'affichage en dB par octave, nulle à 1 kHz (3 rend le bruit rose plat, 4,5 est l'usage en mixage).
    pub fn set_tilt(&mut self, db_per_oct: f32) {
        self.tilt_db_per_oct = db_per_oct;
        let bin_hz = self.bin_hz();
        for (k, t) in self.tilt.iter_mut().enumerate() {
            let hz = k.max(1) as f32 * bin_hz;
            *t = db_per_oct * (hz / TILT_PIVOT_HZ).log2();
        }
    }

    pub fn tilt(&self) -> f32 {
        self.tilt_db_per_oct
    }

    /// Retombée de la courbe lissée en dB/s (`f32::INFINITY` : suit chaque trame).
    pub fn set_release(&mut self, db_per_s: f32) {
        self.release_db_per_s = db_per_s.max(0.0);
    }

    /// Retombée des crêtes en dB/s (0 : maintien infini jusqu'à `reset_peaks`).
    pub fn set_peak_release(&mut self, db_per_s: f32) {
        self.peak_release_db_per_s = db_per_s.max(0.0);
    }

    pub fn set_band_mode(&mut self, mode: BandMode) {
        self.mode = mode;
    }

    pub fn reset_peaks(&mut self) {
        self.peak.fill(FLOOR_DB);
    }

    /// Vide l'historique et les courbes.
    pub fn reset(&mut self) {
        self.ring.fill(0.0);
        (self.pos, self.filled, self.pending, self.frames) = (0, 0, 0, 0);
        self.current.fill(FLOOR_DB);
        self.smoothed.fill(FLOOR_DB);
        self.peak.fill(FLOOR_DB);
    }

    /// Ajoute des échantillons mono ; renvoie le nombre de trames analysées.
    pub fn push(&mut self, samples: &[f32]) -> usize {
        samples.iter().map(|&x| usize::from(self.push_sample(x))).sum()
    }

    /// Ajoute un bloc stéréo, sommé en mono (`(g + d) / 2`) ; renvoie le nombre de trames analysées.
    pub fn push_stereo(&mut self, left: &[f32], right: &[f32]) -> usize {
        left.iter().zip(right).map(|(&l, &r)| usize::from(self.push_sample(0.5 * (l + r)))).sum()
    }

    #[inline]
    fn push_sample(&mut self, x: f32) -> bool {
        let size = self.ring.len();
        self.ring[self.pos] = x;
        self.pos = (self.pos + 1) % size;
        self.filled = (self.filled + 1).min(size);
        self.pending += 1;
        if self.filled == size && self.pending >= self.hop {
            self.pending = 0;
            self.analyze();
            true
        } else {
            false
        }
    }

    fn analyze(&mut self) {
        // Le plus ancien échantillon est à `pos`.
        let (newer, older) = self.ring.split_at(self.pos);
        for (i, (&x, &w)) in older.iter().chain(newer).zip(&self.window).enumerate() {
            self.frame[i] = x * w;
        }
        self.fft.forward(&self.frame, &mut self.re, &mut self.im);
        let dt = self.hop as f32 / self.sample_rate;
        let release = self.release_db_per_s * dt;
        let peak_release = self.peak_release_db_per_s * dt;
        let floor_power = 10f32.powf(FLOOR_DB / 10.0);
        for k in 0..self.current.len() {
            let power = (self.re[k] * self.re[k] + self.im[k] * self.im[k]) * self.norm * self.norm;
            let db = 10.0 * power.max(floor_power).log10() + self.tilt[k];
            self.current[k] = db;
            self.smoothed[k] = db.max(self.smoothed[k] - release);
            self.peak[k] = db.max(self.peak[k] - peak_release);
        }
        self.frames += 1;
    }

    /// Courbe lissée en `n_bins` bandes logarithmiques de `f_min` à `f_max` Hz, écrite dans `out[..n_bins]`.
    pub fn bins(&self, n_bins: usize, f_min: f32, f_max: f32, out: &mut [f32]) {
        self.bands(&self.smoothed, n_bins, f_min, f_max, out);
    }

    /// Crêtes maintenues, mêmes bandes que [`Self::bins`].
    pub fn peak_bins(&self, n_bins: usize, f_min: f32, f_max: f32, out: &mut [f32]) {
        self.bands(&self.peak, n_bins, f_min, f_max, out);
    }

    /// Fréquence centrale (moyenne géométrique) de la bande `i` sur `n_bins`.
    pub fn band_center(i: usize, n_bins: usize, f_min: f32, f_max: f32) -> f32 {
        f_min * (f_max / f_min).powf((i as f32 + 0.5) / n_bins as f32)
    }

    fn bands(&self, src: &[f32], n_bins: usize, f_min: f32, f_max: f32, out: &mut [f32]) {
        assert!(out.len() >= n_bins, "tampon de sortie trop court");
        let f_min = f_min.max(1e-3);
        let f_max = f_max.max(f_min * 1.0001);
        let ratio = (f_max / f_min).powf(1.0 / n_bins.max(1) as f32);
        let bin_hz = self.bin_hz();
        let last = src.len() - 1;
        for (i, o) in out[..n_bins].iter_mut().enumerate() {
            let lo = f_min * ratio.powi(i as i32);
            let hi = lo * ratio;
            let k0 = ((lo / bin_hz).ceil() as usize).min(last + 1);
            let k1 = ((hi / bin_hz).ceil() as usize).min(last + 1);
            *o = if k0 < k1 {
                let band = &src[k0..k1];
                match self.mode {
                    BandMode::Peak => band.iter().copied().fold(FLOOR_DB, f32::max),
                    BandMode::Average => {
                        let mean = band.iter().map(|&db| 10f32.powf(db / 10.0)).sum::<f32>() / band.len() as f32;
                        (10.0 * mean.log10()).max(FLOOR_DB)
                    }
                }
            } else {
                // Bande plus étroite qu'un coefficient : interpolation au centre.
                let p = (lo * hi).sqrt() / bin_hz;
                let k = p.floor() as usize;
                if k >= last {
                    src[last]
                } else {
                    let t = p - k as f32;
                    src[k] + (src[k + 1] - src[k]) * t
                }
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::noise::WhiteNoise;
    use std::f32::consts::TAU;

    fn sine(hz: f32, amp: f32, sr: f32, n: usize) -> Vec<f32> {
        (0..n).map(|i| amp * (TAU * hz * i as f32 / sr).sin()).collect()
    }

    fn argmax(v: &[f32]) -> usize {
        (0..v.len()).max_by(|&a, &b| v[a].total_cmp(&v[b])).unwrap()
    }

    #[test]
    fn sine_peaks_at_its_band() {
        let (sr, n_bins, f_min, f_max) = (48_000.0, 240, 20.0, 20_000.0);
        for hz in [100.0, 440.0, 1000.0, 5000.0, 12_000.0] {
            let mut a = SpectrumAnalyzer::new(sr, 4096, 0.75);
            assert!(a.push(&sine(hz, 0.5, sr, 16_384)) > 0);
            let mut out = vec![0.0; n_bins];
            a.bins(n_bins, f_min, f_max, &mut out);
            let i = argmax(&out);
            let center = SpectrumAnalyzer::band_center(i, n_bins, f_min, f_max);
            let width = center * ((f_max / f_min).powf(1.0 / n_bins as f32) - 1.0);
            assert!((center - hz).abs() <= a.bin_hz().max(width), "{hz} Hz lu à {center} Hz");
            assert!((out[i] + 6.02).abs() < 1.0, "{hz} Hz : {} dB", out[i]);
            // Loin de la raie, le niveau tombe au plancher des lobes secondaires.
            let far = if hz > 1000.0 { 20 } else { n_bins - 20 };
            assert!(out[far] < -90.0, "{hz} Hz : fuite à {} dB", out[far]);
        }
    }

    #[test]
    fn stereo_is_summed_and_sizes_work() {
        for size in [2048, 4096, 8192] {
            let mut a = SpectrumAnalyzer::new(48_000.0, size, 0.5);
            assert_eq!(a.hop(), size / 2);
            let l = sine(1000.0, 1.0, 48_000.0, 3 * size);
            let r = vec![0.0; l.len()];
            assert_eq!(a.push_stereo(&l, &r), 5);
            let k = (1000.0 / a.bin_hz()).round() as usize;
            let peak = a.smoothed()[k - 2..=k + 2].iter().copied().fold(FLOOR_DB, f32::max);
            assert!((peak + 6.02).abs() < 1.0, "{size} : {peak} dB");
        }
    }

    /// Moyenne de puissance, sur de nombreuses trames, des bandes d'affichage.
    fn average_bands(
        a: &mut SpectrumAnalyzer,
        mut next: impl FnMut() -> f32,
        n_bins: usize,
        lo: f32,
        hi: f32,
    ) -> Vec<f32> {
        a.set_release(f32::INFINITY);
        a.set_band_mode(BandMode::Average);
        let mut block = vec![0.0; a.hop()];
        let mut out = vec![0.0; n_bins];
        let mut sum = vec![0.0; n_bins];
        let mut frames = 0;
        while frames < 400 {
            block.iter_mut().for_each(|x| *x = next());
            if a.push(&block) > 0 {
                a.bins(n_bins, lo, hi, &mut out);
                sum.iter_mut().zip(&out).for_each(|(s, &db)| *s += 10f32.powf(db / 10.0));
                frames += 1;
            }
        }
        sum.iter().map(|&p| 10.0 * (p / frames as f32).log10()).collect()
    }

    fn assert_flat(bands: &[f32], tol: f32) {
        let mean = bands.iter().sum::<f32>() / bands.len() as f32;
        for (i, &b) in bands.iter().enumerate() {
            assert!((b - mean).abs() < tol, "bande {i} : {b} dB pour une moyenne de {mean} dB");
        }
    }

    #[test]
    fn white_noise_is_flat() {
        let mut a = SpectrumAnalyzer::new(48_000.0, 4096, 0.75);
        let mut noise = WhiteNoise::new(1);
        assert_flat(&average_bands(&mut a, || noise.next_sample(), 24, 60.0, 16_000.0), 1.5);
    }

    #[test]
    fn pink_noise_is_flat_with_3db_tilt() {
        let mut a = SpectrumAnalyzer::new(44_100.0, 4096, 0.75);
        a.set_tilt(3.0);
        let mut noise = WhiteNoise::new(2);
        // Filtre rose de Paul Kellet (±0,05 dB au-dessus de 9 Hz à 44,1 kHz).
        let mut b = [0.0f32; 7];
        let pink = move || {
            let w = noise.next_sample();
            b[0] = 0.99886 * b[0] + w * 0.055_517_9;
            b[1] = 0.99332 * b[1] + w * 0.075_075_9;
            b[2] = 0.96900 * b[2] + w * 0.153_852;
            b[3] = 0.86650 * b[3] + w * 0.310_485_6;
            b[4] = 0.55000 * b[4] + w * 0.532_952_2;
            b[5] = -0.7616 * b[5] - w * 0.016_898;
            let out = b.iter().sum::<f32>() + w * 0.5362;
            b[6] = w * 0.115_926;
            out * 0.1
        };
        assert_flat(&average_bands(&mut a, pink, 24, 60.0, 16_000.0), 1.5);
    }

    #[test]
    fn tilt_slopes_white_noise() {
        let mut a = SpectrumAnalyzer::new(48_000.0, 4096, 0.75);
        a.set_tilt(4.5);
        let mut noise = WhiteNoise::new(3);
        // Deux bandes de même largeur relative, à 1 kHz et 8 kHz : trois octaves d'écart.
        let bands = average_bands(&mut a, || noise.next_sample(), 9, 1000.0 / 1.1, 8000.0 * 1.1);
        let rise = bands[8] - bands[0];
        assert!((rise - 13.5).abs() < 1.5, "pente lue : {rise} dB sur 3 octaves");
    }

    #[test]
    fn release_and_peak_hold() {
        let sr = 48_000.0;
        let mut a = SpectrumAnalyzer::new(sr, 2048, 0.5);
        a.set_release(60.0);
        a.push(&sine(1000.0, 0.5, sr, 8192));
        let k = (1000.0 / a.bin_hz()).round() as usize;
        let before = a.smoothed()[k];
        a.push(&vec![0.0; 24_000]);
        let drop = before - a.smoothed()[k];
        assert!((drop - 30.0).abs() < 3.0, "retombée de {drop} dB en 0,5 s");
        assert!((a.peaks()[k] - before).abs() < 1e-3, "la crête doit être maintenue");
        a.reset_peaks();
        assert_eq!(a.peaks()[k], FLOOR_DB);
        a.reset();
        assert_eq!((a.frames(), a.smoothed()[k]), (0, FLOOR_DB));
    }
}
