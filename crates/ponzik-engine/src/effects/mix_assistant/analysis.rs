//! Écoute de l'assistant de mixage : spectre à long terme par 1/6 d'octave,
//! dynamique par blocs de 50 ms, sifflantes, image stéréo.
//!
//! Tout est préalloué dans `set_sample_rate` : le thread audio accumule, puis
//! résume l'écoute (tri de quelques centaines de valeurs, en place) et publie le
//! résultat dans [`MixShared`], lu sans verrou par l'interface.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use ponzik_dsp::biquad::{Biquad, BiquadCoefs};
use ponzik_dsp::fft::{RealFft, hann};

/// Bandes de 1/6 d'octave de 20 Hz à 20,5 kHz.
pub const N_BANDS: usize = 60;
const BAND_LO: f32 = 20.0;
const BANDS_PER_OCTAVE: f32 = 6.0;

/// Durée d'écoute utile (blocs non silencieux) avant de résumer.
pub const LISTEN_SECONDS: f32 = 10.0;

const FFT_N: usize = 8_192;
const HOP: usize = FFT_N / 2;
const BLOCK_SECONDS: f32 = 0.05;
/// Assez de blocs pour 16 s d'écoute.
const MAX_BLOCKS: usize = 320;
/// Sous ce niveau (dBFS), un bloc ou une trame est du silence et ne compte pas.
const GATE_DB: f32 = -60.0;
/// Plancher des niveaux publiés.
pub const FLOOR_DB: f32 = -120.0;
/// Bande des sifflantes mesurée pendant l'écoute.
const SIB_BAND: (f32, f32) = (5_000.0, 9_000.0);

/// Fréquence centrale de la bande `i`.
pub fn band_center(i: usize) -> f32 {
    BAND_LO * 2f32.powf((i as f32 + 0.5) / BANDS_PER_OCTAVE)
}

/// Bords de la bande `i`.
pub fn band_edges(i: usize) -> (f32, f32) {
    let f = |k: f32| BAND_LO * 2f32.powf(k / BANDS_PER_OCTAVE);
    (f(i as f32), f(i as f32 + 1.0))
}

fn to_db(power: f64) -> f32 {
    if power <= 1e-12 { FLOOR_DB } else { (10.0 * power.log10()).max(FLOOR_DB as f64) as f32 }
}

/// Résultat d'une écoute. Les bandes sont en puissance par bande (dB) : un bruit
/// rose y est plat.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Analysis {
    pub bands: [f32; N_BANDS],
    /// Niveau efficace moyen des passages non silencieux (dBFS).
    pub rms_db: f32,
    /// Crête d'échantillon (dBFS).
    pub peak_db: f32,
    /// Facteur de crête moyen des blocs de 50 ms (dB) : élevé pour les sons percussifs.
    pub crest_db: f32,
    /// Écart entre blocs forts et faibles (90e − 10e centile du niveau efficace, dB).
    pub range_db: f32,
    /// Crêtes des blocs aux 10e, 25e, 50e, 75e et 90e centiles (dBFS).
    pub peaks_db: [f32; 5],
    /// Sifflantes : 90e centile de (niveau 5–9 kHz − niveau total) par bloc (dB).
    pub sibilance_db: f32,
    /// 90e centile du niveau 5–9 kHz des blocs (dBFS).
    pub sib_level_db: f32,
    /// Écart des sifflantes : 90e − 50e centile de (5–9 kHz − total). Grand quand
    /// des « s » ressortent par moments, nul sur un bruit régulier.
    pub sib_spread_db: f32,
    /// Énergie du côté par rapport au centre (dB) ; très bas pour un son mono.
    pub side_db: f32,
    /// Corrélation gauche/droite, de −1 à 1.
    pub correlation: f32,
    /// Durée écoutée (s).
    pub seconds: f32,
}

const SCALARS: usize = 15;
const ANALYSIS_LEN: usize = N_BANDS + SCALARS;

impl Analysis {
    fn to_array(self) -> [f32; ANALYSIS_LEN] {
        let mut a = [0.0; ANALYSIS_LEN];
        a[..N_BANDS].copy_from_slice(&self.bands);
        let s = [
            self.rms_db,
            self.peak_db,
            self.crest_db,
            self.range_db,
            self.peaks_db[0],
            self.peaks_db[1],
            self.peaks_db[2],
            self.peaks_db[3],
            self.peaks_db[4],
            self.sibilance_db,
            self.sib_level_db,
            self.side_db,
            self.correlation,
            self.seconds,
            self.sib_spread_db,
        ];
        a[N_BANDS..].copy_from_slice(&s);
        a
    }

    fn from_array(a: &[f32; ANALYSIS_LEN]) -> Self {
        let s = &a[N_BANDS..];
        Self {
            bands: std::array::from_fn(|i| a[i]),
            rms_db: s[0],
            peak_db: s[1],
            crest_db: s[2],
            range_db: s[3],
            peaks_db: [s[4], s[5], s[6], s[7], s[8]],
            sibilance_db: s[9],
            sib_level_db: s[10],
            side_db: s[11],
            correlation: s[12],
            seconds: s[13],
            sib_spread_db: s[14],
        }
    }
}

fn store(a: &AtomicU32, v: f32) {
    a.store(v.to_bits(), Ordering::Relaxed);
}

fn load(a: &AtomicU32) -> f32 {
    f32::from_bits(a.load(Ordering::Relaxed))
}

/// État partagé entre le thread audio (écriture) et l'interface (lecture), sans verrou.
pub struct MixShared {
    /// Incrémenté à chaque écoute terminée.
    generation: AtomicU32,
    progress: AtomicU32,
    listening: AtomicBool,
    /// Spectre récent de l'entrée (dB par bande), pour l'affichage.
    live: [AtomicU32; N_BANDS],
    result: [AtomicU32; ANALYSIS_LEN],
    comp_gr: AtomicU32,
    deess_gr: AtomicU32,
}

impl Default for MixShared {
    fn default() -> Self {
        let floor = || AtomicU32::new(FLOOR_DB.to_bits());
        Self {
            generation: AtomicU32::new(0),
            progress: AtomicU32::new(0),
            listening: AtomicBool::new(false),
            live: std::array::from_fn(|_| floor()),
            result: std::array::from_fn(|_| AtomicU32::new(0)),
            comp_gr: AtomicU32::new(0),
            deess_gr: AtomicU32::new(0),
        }
    }
}

impl MixShared {
    /// Nombre d'écoutes terminées depuis la création de l'effet.
    pub fn generation(&self) -> u32 {
        self.generation.load(Ordering::Acquire)
    }

    /// Avancement de l'écoute en cours, de 0 à 1.
    pub fn progress(&self) -> f32 {
        load(&self.progress)
    }

    pub fn listening(&self) -> bool {
        self.listening.load(Ordering::Relaxed)
    }

    /// Spectre récent de l'entrée, en dB par bande.
    pub fn live(&self) -> [f32; N_BANDS] {
        std::array::from_fn(|i| load(&self.live[i]))
    }

    /// Dernière écoute terminée.
    pub fn analysis(&self) -> Option<Analysis> {
        if self.generation() == 0 {
            return None;
        }
        let a: [f32; ANALYSIS_LEN] = std::array::from_fn(|i| load(&self.result[i]));
        Some(Analysis::from_array(&a))
    }

    /// Réduction de gain du compresseur sur le dernier bloc (dB, positive).
    pub fn compressor_gr(&self) -> f32 {
        load(&self.comp_gr)
    }

    /// Réduction des sifflantes sur le dernier bloc (dB, positive).
    pub fn deesser_gr(&self) -> f32 {
        load(&self.deess_gr)
    }

    pub(super) fn set_meters(&self, comp: f32, deess: f32) {
        store(&self.comp_gr, comp);
        store(&self.deess_gr, deess);
    }
}

/// Plage de bins d'une bande et facteur ramenant leur somme à la largeur de la bande.
#[derive(Clone, Copy, Default)]
struct BandBins {
    start: usize,
    end: usize,
    scale: f32,
}

/// Analyseur temps réel de l'entrée de l'assistant.
pub(super) struct Analyzer {
    pub shared: Arc<MixShared>,
    pub listen_seconds: f32,
    fft: RealFft,
    window: Vec<f32>,
    /// `2 / (N Σw²)` : puissance moyenne du signal contenue dans les bins.
    norm: f32,
    ring: Vec<f32>,
    pos: usize,
    since_hop: usize,
    frame: Vec<f32>,
    re: Vec<f32>,
    im: Vec<f32>,
    bins: [BandBins; N_BANDS],
    live: [f32; N_BANDS],
    sum: [f64; N_BANDS],
    frames: u32,
    // Blocs de 50 ms.
    block_len: usize,
    block_n: usize,
    block_sq: f64,
    block_hf_sq: f64,
    block_peak: f32,
    block_mid: f64,
    block_side: f64,
    block_lr: [f64; 3],
    hf: [Biquad; 2],
    rms: Vec<f32>,
    peaks: Vec<f32>,
    hf_rms: Vec<f32>,
    blocks: usize,
    scratch: Vec<f32>,
    total_sq: f64,
    total_n: u64,
    peak: f32,
    mid_sq: f64,
    side_sq: f64,
    lr: [f64; 3],
    listening: bool,
}

impl Analyzer {
    pub fn new(sample_rate: f32) -> Self {
        let mut a = Self {
            shared: Arc::new(MixShared::default()),
            listen_seconds: LISTEN_SECONDS,
            fft: RealFft::new(FFT_N),
            window: vec![0.0; FFT_N],
            norm: 0.0,
            ring: vec![0.0; FFT_N],
            pos: 0,
            since_hop: 0,
            frame: vec![0.0; FFT_N],
            re: vec![0.0; FFT_N / 2 + 1],
            im: vec![0.0; FFT_N / 2 + 1],
            bins: [BandBins::default(); N_BANDS],
            live: [0.0; N_BANDS],
            sum: [0.0; N_BANDS],
            frames: 0,
            block_len: 1,
            block_n: 0,
            block_sq: 0.0,
            block_hf_sq: 0.0,
            block_peak: 0.0,
            block_mid: 0.0,
            block_side: 0.0,
            block_lr: [0.0; 3],
            hf: Default::default(),
            rms: vec![0.0; MAX_BLOCKS],
            peaks: vec![0.0; MAX_BLOCKS],
            hf_rms: vec![0.0; MAX_BLOCKS],
            blocks: 0,
            scratch: vec![0.0; MAX_BLOCKS],
            total_sq: 0.0,
            total_n: 0,
            peak: 0.0,
            mid_sq: 0.0,
            side_sq: 0.0,
            lr: [0.0; 3],
            listening: false,
        };
        hann(&mut a.window);
        let energy: f32 = a.window.iter().map(|w| w * w).sum();
        a.norm = 2.0 / (FFT_N as f32 * energy);
        a.set_sample_rate(sample_rate);
        a
    }

    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        let bin_hz = sample_rate / FFT_N as f32;
        let last = FFT_N / 2;
        for (i, b) in self.bins.iter_mut().enumerate() {
            let (lo, hi) = band_edges(i);
            if lo >= sample_rate * 0.5 {
                *b = BandBins::default();
                continue;
            }
            let hi = hi.min(sample_rate * 0.5);
            let mut start = ((lo / bin_hz).ceil() as usize).max(1);
            let mut end = ((hi / bin_hz).ceil() as usize).min(last + 1);
            if start >= end {
                // Bande plus étroite qu'un bin : le bin le plus proche, à l'échelle.
                start = ((band_center(i) / bin_hz).round() as usize).clamp(1, last);
                end = start + 1;
            }
            b.start = start;
            b.end = end;
            b.scale = (hi - lo) / (bin_hz * (end - start) as f32);
        }
        self.block_len = ((BLOCK_SECONDS * sample_rate) as usize).max(16);
        let (lo, hi) = (SIB_BAND.0.min(sample_rate * 0.4), SIB_BAND.1.min(sample_rate * 0.45));
        self.hf[0].coefs = BiquadCoefs::highpass(lo, std::f32::consts::FRAC_1_SQRT_2, sample_rate);
        self.hf[1].coefs = BiquadCoefs::lowpass(hi, std::f32::consts::FRAC_1_SQRT_2, sample_rate);
        self.reset();
    }

    pub fn reset(&mut self) {
        self.ring.fill(0.0);
        self.pos = 0;
        self.since_hop = 0;
        self.live = [0.0; N_BANDS];
        self.hf.iter_mut().for_each(Biquad::reset);
        self.clear_block();
    }

    fn clear_block(&mut self) {
        self.block_n = 0;
        self.block_sq = 0.0;
        self.block_hf_sq = 0.0;
        self.block_peak = 0.0;
        self.block_mid = 0.0;
        self.block_side = 0.0;
        self.block_lr = [0.0; 3];
    }

    /// Commence une nouvelle écoute.
    pub fn start(&mut self) {
        self.sum = [0.0; N_BANDS];
        self.frames = 0;
        self.blocks = 0;
        self.total_sq = 0.0;
        self.total_n = 0;
        self.peak = 0.0;
        self.mid_sq = 0.0;
        self.side_sq = 0.0;
        self.lr = [0.0; 3];
        self.clear_block();
        self.listening = true;
        self.shared.listening.store(true, Ordering::Relaxed);
        store(&self.shared.progress, 0.0);
    }

    /// Abandonne l'écoute en cours (le dernier résultat reste publié).
    pub fn stop(&mut self) {
        self.listening = false;
        self.shared.listening.store(false, Ordering::Relaxed);
    }

    pub fn process(&mut self, left: &[f32], right: &[f32]) {
        for (&l, &r) in left.iter().zip(right) {
            let mid = 0.5 * (l + r);
            self.ring[self.pos] = mid;
            self.pos = (self.pos + 1) % FFT_N;
            self.since_hop += 1;
            if self.since_hop >= HOP {
                self.since_hop = 0;
                self.spectrum_frame();
            }
            if !self.listening {
                continue;
            }
            let side = 0.5 * (l - r);
            let [h0, h1] = &mut self.hf;
            let hf = h1.process(h0.process(mid));
            self.block_sq += 0.5 * (l as f64 * l as f64 + r as f64 * r as f64);
            self.block_hf_sq += hf as f64 * hf as f64;
            self.block_peak = self.block_peak.max(l.abs()).max(r.abs());
            self.block_mid += mid as f64 * mid as f64;
            self.block_side += side as f64 * side as f64;
            self.block_lr[0] += l as f64 * r as f64;
            self.block_lr[1] += l as f64 * l as f64;
            self.block_lr[2] += r as f64 * r as f64;
            self.block_n += 1;
            if self.block_n >= self.block_len {
                self.end_block();
            }
        }
    }

    fn spectrum_frame(&mut self) {
        // Trame : du plus ancien au plus récent, fenêtrée.
        for k in 0..FFT_N {
            self.frame[k] = self.ring[(self.pos + k) % FFT_N] * self.window[k];
        }
        self.fft.forward(&self.frame, &mut self.re, &mut self.im);
        let mut power = [0.0f32; N_BANDS];
        for (p, b) in power.iter_mut().zip(&self.bins) {
            let sq: f32 = (b.start..b.end).map(|k| self.re[k] * self.re[k] + self.im[k] * self.im[k]).sum();
            *p = sq * b.scale * self.norm;
        }
        for (i, &p) in power.iter().enumerate() {
            self.live[i] = 0.6 * self.live[i] + 0.4 * p;
            store(&self.shared.live[i], to_db(self.live[i] as f64));
        }
        // Les trames silencieuses ne pèsent pas dans la moyenne.
        let total: f32 = power.iter().sum();
        if self.listening && to_db(total as f64) >= GATE_DB {
            for (s, &p) in self.sum.iter_mut().zip(&power) {
                *s += p as f64;
            }
            self.frames += 1;
        }
    }

    fn end_block(&mut self) {
        let n = self.block_n as f64;
        let ms = self.block_sq / n;
        let rms_db = to_db(ms);
        if rms_db > GATE_DB && self.blocks < MAX_BLOCKS {
            let b = self.blocks;
            self.rms[b] = rms_db;
            self.peaks[b] = to_db((self.block_peak as f64).powi(2));
            self.hf_rms[b] = to_db(self.block_hf_sq / n);
            self.blocks += 1;
            self.total_sq += self.block_sq;
            self.total_n += self.block_n as u64;
            self.peak = self.peak.max(self.block_peak);
            self.mid_sq += self.block_mid;
            self.side_sq += self.block_side;
            for k in 0..3 {
                self.lr[k] += self.block_lr[k];
            }
            let heard = self.blocks as f32 * BLOCK_SECONDS;
            store(&self.shared.progress, (heard / self.listen_seconds).min(1.0));
            if heard >= self.listen_seconds || self.blocks == MAX_BLOCKS {
                self.finish();
            }
        }
        self.clear_block();
    }

    /// `q`-ième centile (0..1) des `n` premières valeurs de `src`, trié dans `scratch`.
    fn percentile(scratch: &mut [f32], src: &[f32], n: usize, q: f32) -> f32 {
        if n == 0 {
            return FLOOR_DB;
        }
        let s = &mut scratch[..n];
        s.copy_from_slice(&src[..n]);
        s.sort_unstable_by(f32::total_cmp);
        s[((n - 1) as f32 * q).round() as usize]
    }

    fn finish(&mut self) {
        let n = self.blocks;
        let frames = self.frames.max(1) as f64;
        let bands = std::array::from_fn(|i| if self.frames == 0 { FLOOR_DB } else { to_db(self.sum[i] / frames) });
        let rms_db = to_db(self.total_sq / self.total_n.max(1) as f64);
        let crest = (0..n).map(|b| self.peaks[b] - self.rms[b]).sum::<f32>() / n.max(1) as f32;
        let sc = &mut self.scratch;
        let range = Self::percentile(sc, &self.rms, n, 0.9) - Self::percentile(sc, &self.rms, n, 0.1);
        let peaks_db = [0.1, 0.25, 0.5, 0.75, 0.9].map(|q| Self::percentile(sc, &self.peaks, n, q));
        let sib_level = Self::percentile(sc, &self.hf_rms, n, 0.9);
        // Rapport aigus / total par bloc, rangé dans `scratch` après coup.
        for (d, (hf, rms)) in sc.iter_mut().zip(self.hf_rms.iter().zip(&self.rms)).take(n) {
            *d = hf - rms;
        }
        sc[..n].sort_unstable_by(f32::total_cmp);
        let at = |q: f32| if n == 0 { FLOOR_DB } else { sc[((n - 1) as f32 * q).round() as usize] };
        let (sibilance, sib_median) = (at(0.9), at(0.5));
        let [lr, ll, rr] = self.lr;
        let correlation = if ll * rr > 1e-20 { (lr / (ll * rr).sqrt()) as f32 } else { 1.0 };
        let side_db = if self.mid_sq > 1e-20 { to_db(self.side_sq / self.mid_sq) } else { FLOOR_DB };
        let analysis = Analysis {
            bands,
            rms_db,
            peak_db: to_db((self.peak as f64).powi(2)),
            crest_db: crest,
            range_db: range,
            peaks_db,
            sibilance_db: sibilance,
            sib_level_db: sib_level,
            sib_spread_db: sibilance - sib_median,
            side_db,
            correlation,
            seconds: n as f32 * BLOCK_SECONDS,
        };
        for (slot, v) in self.shared.result.iter().zip(analysis.to_array()) {
            store(slot, v);
        }
        store(&self.shared.progress, 1.0);
        self.shared.generation.fetch_add(1, Ordering::Release);
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::testing::*;

    #[test]
    fn bands_cover_the_audio_range() {
        assert!((band_center(0) - 21.2).abs() < 0.2);
        assert!(band_edges(N_BANDS - 1).1 > 20_000.0);
        let a = Analyzer::new(SR);
        for b in &a.bins {
            assert!(b.end > b.start && b.scale > 0.0);
        }
    }

    #[test]
    fn full_scale_sine_reads_minus_3_db_in_its_band() {
        let mut a = Analyzer::new(SR);
        let x = sine(1_000.0, 1.0, FFT_N * 8);
        a.process(&x, &x);
        let live = a.shared.live();
        let i = (0..N_BANDS).max_by(|&i, &j| live[i].total_cmp(&live[j])).unwrap();
        let (lo, hi) = band_edges(i);
        assert!(lo <= 1_000.0 && hi >= 1_000.0, "{lo} {hi}");
        // Toute la puissance (A² / 2) dans sa bande, une fois le lissage établi.
        assert!((live[i] + 3.0).abs() < 0.3, "{}", live[i]);
    }

    #[test]
    fn white_noise_rises_3_db_per_octave_and_listening_ends() {
        let mut a = Analyzer::new(SR);
        a.listen_seconds = 2.0;
        a.start();
        let x = noise(0.3, 48_000 * 3, 5);
        a.process(&x, &x);
        assert_eq!(a.shared.generation(), 1);
        assert!(!a.shared.listening());
        let r = a.shared.analysis().unwrap();
        let (b1, b2) = (36, 48);
        let slope = (r.bands[b2] - r.bands[b1]) / 2.0;
        assert!((slope - 3.0).abs() < 0.8, "{slope}");
        assert!(r.side_db < -100.0 && r.correlation > 0.99);
        assert!((r.seconds - 2.0).abs() < 0.06);
        assert!(r.range_db < 2.0, "{}", r.range_db);
    }
}
