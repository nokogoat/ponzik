//! Détection des attaques (transitoires) d'un échantillon, hors temps réel.
//!
//! Flux spectral : à chaque trame, la somme des hausses de magnitude
//! (compressée en logarithme) d'un bin à l'autre. Une attaque est un pic du
//! flux au-dessus d'un seuil adaptatif (médiane locale plus une marge réglée
//! par la sensibilité). La position est ensuite affinée dans le signal : début
//! de la montée d'enveloppe, recalé sur le passage à zéro qui précède.
//!
//! L'analyse alloue : elle sert à l'édition, pas au thread audio.

use crate::fft::{RealFft, hann};

/// Longueur des trames d'analyse.
pub const FRAME: usize = 1024;
/// Pas entre deux trames.
pub const HOP: usize = 256;
/// Écart minimal entre deux attaques, en secondes.
pub const MIN_GAP: f32 = 0.045;
/// Demi-largeur de la médiane du seuil adaptatif, en trames.
const MEDIAN_SPAN: usize = 12;
/// Trames sous ce niveau (dB sous la trame la plus forte) : silence, pas d'attaque.
const GATE_DB: f32 = 45.0;

/// Fonction d'attaque d'un échantillon : flux normalisé par trame.
#[derive(Clone, Debug, Default)]
pub struct OnsetAnalysis {
    pub sample_rate: f32,
    /// Flux spectral par trame (la trame `k` est centrée sur l'image `k * HOP`), dans [0, 1].
    pub flux: Vec<f32>,
    /// Niveau de chaque trame en dB, relatif à la plus forte (≤ 0).
    pub level: Vec<f32>,
    /// Seuil adaptatif de base (médiane locale du flux) par trame.
    median: Vec<f32>,
}

/// Une attaque : position (images) et force (hauteur du pic de flux, 0 à 1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Onset {
    pub pos: usize,
    pub strength: f32,
}

/// Mélange mono de deux canaux.
pub fn mono(left: &[f32], right: &[f32]) -> Vec<f32> {
    left.iter().zip(right).map(|(l, r)| 0.5 * (l + r)).collect()
}

impl OnsetAnalysis {
    /// Analyse un signal mono.
    pub fn new(signal: &[f32], sample_rate: f32) -> Self {
        let frames = signal.len().div_ceil(HOP);
        let mut fft = RealFft::new(FRAME);
        let mut window = vec![0.0; FRAME];
        hann(&mut window);
        let bins = fft.bins();
        let (mut re, mut im) = (vec![0.0; bins], vec![0.0; bins]);
        let bands = bands(bins, sample_rate);
        let mut prev = vec![0.0f32; bands.len()];
        let mut mag = vec![0.0f32; bands.len()];
        let mut buf = vec![0.0f32; FRAME];
        let mut flux = Vec::with_capacity(frames);
        let mut level = Vec::with_capacity(frames);
        for k in 0..frames {
            // Trame centrée sur k * HOP, zéros hors du signal.
            let start = (k * HOP) as isize - (FRAME / 2) as isize;
            let mut energy = 0.0f32;
            for (i, b) in buf.iter_mut().enumerate() {
                let at = start + i as isize;
                let x = if at >= 0 && (at as usize) < signal.len() { signal[at as usize] } else { 0.0 };
                energy += x * x;
                *b = x * window[i];
            }
            fft.forward(&buf, &mut re, &mut im);
            let mut f = 0.0;
            // Près de la fin, la coupure du signal étale le spectre : pas d'attaque à y voir.
            let tail = k * HOP + FRAME / 2 > signal.len() && k > 0;
            for (b, &(lo, hi)) in bands.iter().enumerate() {
                let e: f32 = (lo..hi).map(|j| re[j] * re[j] + im[j] * im[j]).sum();
                mag[b] = (1.0 + 100.0 * (e / (hi - lo) as f32).sqrt()).ln();
                f += (mag[b] - prev[b]).max(0.0);
            }
            std::mem::swap(&mut prev, &mut mag);
            flux.push(if tail { 0.0 } else { f });
            level.push(10.0 * (energy / FRAME as f32 + 1e-12).log10());
        }
        let top = flux.iter().fold(0.0f32, |m, &f| m.max(f));
        if top > 0.0 {
            for f in &mut flux {
                *f /= top;
            }
        }
        let loudest = level.iter().fold(f32::MIN, |m, &l| m.max(l));
        for l in &mut level {
            *l -= loudest;
        }
        let median = (0..flux.len())
            .map(|k| {
                let lo = k.saturating_sub(MEDIAN_SPAN);
                let hi = (k + MEDIAN_SPAN + 1).min(flux.len());
                let mut w: Vec<f32> = flux[lo..hi].to_vec();
                w.sort_unstable_by(f32::total_cmp);
                w[w.len() / 2]
            })
            .collect();
        Self { sample_rate, flux, level, median }
    }

    /// Analyse d'un échantillon stéréo (mélangé en mono).
    pub fn stereo(left: &[f32], right: &[f32], sample_rate: f32) -> Self {
        Self::new(&mono(left, right), sample_rate)
    }

    /// Trames par seconde.
    pub fn rate(&self) -> f32 {
        self.sample_rate / HOP as f32
    }

    /// Seuil de détection à la trame `k` pour une sensibilité dans [0, 1].
    pub fn threshold(&self, k: usize, sensitivity: f32) -> f32 {
        let s = 1.0 - sensitivity.clamp(0.0, 1.0);
        let base = self.median.get(k).copied().unwrap_or(0.0);
        1.4 * base + 0.02 + 0.5 * s * s
    }

    /// Attaques au-dessus du seuil, dans l'ordre ; au plus `max` (les plus
    /// fortes sont gardées). Les positions sont affinées dans `signal`.
    pub fn pick(&self, signal: &[f32], sensitivity: f32, max: usize) -> Vec<Onset> {
        self.pick_with(signal, sensitivity, MIN_GAP, max)
    }

    /// [`Self::pick`] with the shortest gap between two hits, in seconds
    /// (the stronger of two closer hits wins).
    pub fn pick_with(&self, signal: &[f32], sensitivity: f32, min_gap: f32, max: usize) -> Vec<Onset> {
        let n = self.flux.len();
        let gap = ((min_gap.max(0.0) * self.rate()) as usize).max(1);
        let mut found: Vec<(usize, f32)> = Vec::new();
        for k in 0..n {
            let f = self.flux[k];
            if f <= self.threshold(k, sensitivity) || self.level[k] < -GATE_DB {
                continue;
            }
            // Maximum local sur ±3 trames.
            let lo = k.saturating_sub(3);
            let hi = (k + 4).min(n);
            if (lo..hi).any(|j| self.flux[j] > f || (j < k && self.flux[j] == f)) {
                continue;
            }
            match found.last_mut() {
                Some(last) if k - last.0 < gap => {
                    if f > last.1 {
                        *last = (k, f);
                    }
                }
                _ => found.push((k, f)),
            }
        }
        if found.len() > max {
            found.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));
            found.truncate(max);
            found.sort_unstable_by_key(|f| f.0);
        }
        let mut out: Vec<Onset> = Vec::with_capacity(found.len());
        for (k, strength) in found {
            let pos = refine(signal, k * HOP);
            if out.last().is_none_or(|o| pos > o.pos) {
                out.push(Onset { pos, strength });
            }
        }
        out
    }
}

/// Third-octave bands from 30 Hz, as bin ranges (at least one bin each): a
/// kick's two low bins weigh as much as a snare's hundreds of high ones.
fn bands(bins: usize, sample_rate: f32) -> Vec<(usize, usize)> {
    let hz = sample_rate / (2.0 * (bins - 1) as f32);
    let mut out: Vec<(usize, usize)> = Vec::new();
    let mut lo = 1usize;
    let mut f = 30.0f32;
    while lo < bins {
        f *= 2f32.powf(1.0 / 3.0);
        let hi = ((f / hz).round() as usize).clamp(lo + 1, bins);
        out.push((lo, hi));
        lo = hi;
    }
    out
}

/// Début de l'attaque repérée vers l'image `center` : début de la montée de
/// l'enveloppe (blocs de 32 images), recalé sur le passage à zéro précédent.
fn refine(signal: &[f32], center: usize) -> usize {
    const BLOCK: usize = 32;
    let len = signal.len();
    if len == 0 {
        return 0;
    }
    let lo = center.saturating_sub(FRAME / 2 + HOP).min(len - 1);
    let hi = (center + FRAME / 2).min(len);
    let env: Vec<f32> = signal[lo..hi].chunks(BLOCK).map(|b| b.iter().fold(0.0f32, |m, x| m.max(x.abs()))).collect();
    let Some(peak) = (0..env.len()).max_by(|&a, &b| env[a].total_cmp(&env[b])) else { return lo };
    let base = env[..=peak].iter().fold(f32::MAX, |m, &e| m.min(e));
    let level = base + 0.15 * (env[peak] - base);
    let block = (0..peak).rev().find(|&b| env[b] <= level).map_or(0, |b| b + 1);
    let from = lo + block * BLOCK;
    let to = (from + BLOCK).min(len);
    let mut at = (from..to).find(|&i| signal[i].abs() > level).unwrap_or(from);
    // Recul jusqu'au passage à zéro (au plus 64 images) : la coupe ne claque pas.
    let stop = at.saturating_sub(64);
    while at > stop && at > 0 && signal[at - 1].signum() == signal[at].signum() && signal[at - 1] != 0.0 {
        at -= 1;
    }
    at
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::noise::WhiteNoise;

    const SR: f32 = 48_000.0;

    /// Coups de bruit amortis aux positions données.
    fn hits(len: usize, at: &[usize], decay: f32) -> Vec<f32> {
        let mut noise = WhiteNoise::new(3);
        let mut x = vec![0.0f32; len];
        for &h in at {
            for i in 0..(len - h).min(12_000) {
                x[h + i] += noise.next_sample() * 0.8 * (-(i as f32) / decay).exp();
            }
        }
        x
    }

    #[test]
    fn finds_noise_hits_precisely() {
        let at = [2_000, 14_000, 26_500, 33_000, 47_000];
        let x = hits(60_000, &at, 900.0);
        let a = OnsetAnalysis::new(&x, SR);
        let found = a.pick(&x, 0.5, 64);
        assert_eq!(found.len(), at.len(), "{found:?}");
        for (o, &want) in found.iter().zip(&at) {
            assert!(o.pos <= want + 8 && want <= o.pos + 70, "{} au lieu de {want}", o.pos);
        }
    }

    #[test]
    fn sensitivity_adds_quieter_hits() {
        // Un coup fort, puis des coups de plus en plus faibles.
        let mut x = hits(80_000, &[1_000], 900.0);
        let mut noise = WhiteNoise::new(9);
        for (n, &h) in [20_000usize, 40_000, 60_000].iter().enumerate() {
            let amp = 0.25 / (n + 1) as f32;
            for i in 0..8_000 {
                x[h + i] += noise.next_sample() * amp * (-(i as f32) / 900.0).exp();
            }
        }
        let a = OnsetAnalysis::new(&x, SR);
        let counts: Vec<usize> = [0.0, 0.5, 1.0].iter().map(|&s| a.pick(&x, s, 64).len()).collect();
        assert!(counts[0] <= counts[1] && counts[1] <= counts[2], "{counts:?}");
        assert!(counts[0] >= 1 && counts[2] == 4, "{counts:?}");
    }

    /// A two-bar drum loop at 100 BPM: kick, snare, closed hats, a ghost
    /// snare and an open hat, over a held bass note.
    fn drum_loop() -> (Vec<f32>, Vec<usize>) {
        let beat = 60.0 / 100.0 * SR;
        let len = (beat * 8.0) as usize;
        let mut x = vec![0.0f32; len];
        let mut noise = WhiteNoise::new(21);
        let tau = std::f32::consts::TAU;
        // (time in 16ths, kind, level): 0 kick, 1 snare, 2 closed hat, 3 open hat.
        let hits: [(f32, u8, f32); 17] = [
            (0.0, 0, 1.0), (2.0, 2, 0.5), (4.0, 1, 0.9), (6.0, 2, 0.5), (7.0, 0, 0.7), (8.0, 0, 0.9), (10.0, 2, 0.5),
            (12.0, 1, 0.95), (14.0, 3, 0.5), (16.0, 0, 1.0), (18.0, 2, 0.5), (20.0, 1, 0.9), (22.0, 2, 0.45),
            (23.0, 1, 0.35), (24.0, 0, 0.85), (26.0, 0, 0.6), (28.0, 1, 1.0),
        ];
        let mut at = Vec::new();
        for (t, kind, vel) in hits {
            let start = (t * beat / 4.0) as usize;
            at.push(start);
            let (mut phase, mut hp) = (0.0f32, 0.0f32);
            for i in 0..((0.4 * SR) as usize).min(len - start) {
                let tt = i as f32 / SR;
                let v = match kind {
                    0 => {
                        phase += (50.0 + 90.0 * (-tt * 30.0).exp()) / SR;
                        (phase * tau).sin() * (-tt * 8.0).exp()
                    }
                    1 => {
                        phase += 190.0 / SR;
                        0.5 * (phase * tau).sin() * (-tt * 30.0).exp() + 0.6 * noise.next_sample() * (-tt * 16.0).exp()
                    }
                    _ => {
                        let w = noise.next_sample();
                        let y = w - hp;
                        hp = w;
                        y * 0.4 * (-tt * if kind == 2 { 60.0 } else { 8.0 }).exp()
                    }
                };
                x[start + i] += v * vel * 0.7;
            }
        }
        for (i, v) in x.iter_mut().enumerate() {
            *v += 0.1 * (i as f32 * 55.0 * tau / SR).sin();
        }
        (x, at)
    }

    #[test]
    fn drum_loop_hits_within_5_ms_and_nothing_else() {
        let (x, at) = drum_loop();
        let a = OnsetAnalysis::new(&x, SR);
        let found = a.pick(&x, 0.5, 64);
        let tol = (0.005 * SR) as usize;
        let mut worst = 0usize;
        for &want in &at {
            let d = found.iter().map(|o| o.pos.abs_diff(want)).min().unwrap();
            worst = worst.max(d);
            assert!(d <= tol, "hit at {want} found {d} frames away: {found:?}");
        }
        // No false positive: every detection belongs to a hit.
        for o in &found {
            assert!(at.iter().any(|&w| o.pos.abs_diff(w) <= tol), "false hit at {}", o.pos);
        }
        assert_eq!(found.len(), at.len());
        eprintln!("{} hits, worst error {:.2} ms", at.len(), worst as f32 * 1000.0 / SR);
    }

    #[test]
    fn min_gap_merges_close_hits() {
        let x = hits(60_000, &[2_000, 3_500, 30_000], 400.0);
        let a = OnsetAnalysis::new(&x, SR);
        assert_eq!(a.pick_with(&x, 0.5, 0.02, 64).len(), 3);
        assert_eq!(a.pick_with(&x, 0.5, 0.1, 64).len(), 2);
    }

    #[test]
    fn keeps_the_strongest_when_capped() {
        let x = hits(60_000, &[2_000, 14_000, 26_000, 38_000], 900.0);
        let a = OnsetAnalysis::new(&x, SR);
        assert_eq!(a.pick(&x, 0.5, 2).len(), 2);
    }

    #[test]
    fn silence_and_steady_tone_have_no_hits() {
        let silent = vec![0.0f32; 20_000];
        assert!(OnsetAnalysis::new(&silent, SR).pick(&silent, 1.0, 64).is_empty());
        let tone: Vec<f32> = (0..48_000).map(|i| 0.5 * (i as f32 * 440.0 * std::f32::consts::TAU / SR).sin()).collect();
        let found = OnsetAnalysis::new(&tone, SR).pick(&tone, 0.5, 64);
        // Au plus l'attaque du début.
        assert!(found.len() <= 1 && found.iter().all(|o| o.pos < 1_000), "{found:?}");
    }

    #[test]
    fn refine_lands_on_a_zero_crossing() {
        let tone: Vec<f32> = (0..4_000).map(|i| if i < 1_000 { 0.0 } else { (i as f32 * 0.05).sin() * 0.9 }).collect();
        let at = refine(&tone, 1_100);
        assert!((990..=1_010).contains(&at), "{at}");
    }
}
