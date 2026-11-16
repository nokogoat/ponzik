//! Étirement temporel et rééchantillonnage, hors temps réel.
//!
//! [`wsola`] change la durée sans changer la hauteur (WSOLA : chaque trame de
//! sortie est prise dans l'entrée autour de sa position nominale, au décalage
//! qui prolonge le mieux la trame précédente, puis superposée avec une
//! fenêtre de Hann à 50 %). [`resample`] change durée et hauteur ensemble,
//! comme un disque accéléré. [`pitch_shift`] combine les deux.
//!
//! Ces fonctions allouent : elles servent à l'édition, pas au thread audio.

use std::f32::consts::PI;

/// Durée d'une trame WSOLA, en secondes : assez courte pour garder le mordant
/// d'une batterie, assez longue pour une note tenue.
const FRAME_SECONDS: f32 = 0.032;
/// Recherche du meilleur raccord : ± cette part de trame.
const TOLERANCE: f32 = 0.25;
/// Décimation de la recherche grossière.
const COARSE: usize = 4;

/// Étire un signal stéréo : la sortie dure `factor` fois l'entrée (2,0 : deux
/// fois plus long), à hauteur égale. Les raccords sont choisis sur le mélange
/// mono, les deux canaux suivent le même chemin.
pub fn wsola_stereo(left: &[f32], right: &[f32], factor: f64, sample_rate: f32) -> (Vec<f32>, Vec<f32>) {
    let len = left.len().min(right.len());
    let out_len = (len as f64 * factor).round().max(0.0) as usize;
    if len == 0 || out_len == 0 {
        return (Vec::new(), Vec::new());
    }
    if (factor - 1.0).abs() < 1e-9 {
        return (left[..len].to_vec(), right[..len].to_vec());
    }
    let n = (((sample_rate.max(8_000.0) * FRAME_SECONDS) as usize) / 2 * 2).max(64);
    let hs = n / 2;
    let delta = (n as f32 * TOLERANCE) as usize;
    let mono: Vec<f32> = left[..len].iter().zip(&right[..len]).map(|(l, r)| 0.5 * (l + r)).collect();
    let window: Vec<f32> = (0..n).map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / n as f32).cos()).collect();
    let at = |x: &[f32], i: isize| if i >= 0 && (i as usize) < x.len() { x[i as usize] } else { 0.0 };

    let mut out_l = vec![0.0f32; out_len + n];
    let mut out_r = vec![0.0f32; out_len + n];
    let mut norm = vec![0.0f32; out_len + n];
    // Début de la trame d'entrée précédente ; la première commence à 0.
    let mut prev: isize = -(hs as isize);
    let frames = out_len.div_ceil(hs) + 1;
    for k in 0..frames {
        // Trame de sortie k : centrée sur k·hs, commence à k·hs − hs.
        let out_start = k as isize * hs as isize - hs as isize;
        let nominal = (out_start as f64 / factor).round() as isize;
        let natural = prev + hs as isize;
        let start = if k == 0 { nominal } else { best_offset(&mono, natural, nominal, delta, n, &at) };
        for (i, &w) in window.iter().enumerate() {
            let o = out_start + i as isize;
            if o < 0 || o as usize >= out_l.len() {
                continue;
            }
            let src = start + i as isize;
            out_l[o as usize] += w * at(left, src);
            out_r[o as usize] += w * at(right, src);
            norm[o as usize] += w;
        }
        prev = start;
    }
    for ((l, r), w) in out_l.iter_mut().zip(out_r.iter_mut()).zip(&norm) {
        if *w > 1e-3 {
            *l /= w;
            *r /= w;
        }
    }
    out_l.truncate(out_len);
    out_r.truncate(out_len);
    (out_l, out_r)
}

/// Début de trame dans `[nominal − delta, nominal + delta]` qui ressemble le
/// plus à `natural` (prolongement naturel de la trame précédente) :
/// recherche grossière décimée, puis fine autour du meilleur point.
fn best_offset(x: &[f32], natural: isize, nominal: isize, delta: usize, n: usize, at: &dyn Fn(&[f32], isize) -> f32) -> isize {
    let d = delta as isize;
    let corr = |cand: isize, step: usize| -> f32 {
        let (mut xy, mut yy) = (0.0f32, 0.0f32);
        let mut i = 0;
        while i < n {
            let a = at(x, natural + i as isize);
            let b = at(x, cand + i as isize);
            xy += a * b;
            yy += b * b;
            i += step;
        }
        if yy > 1e-9 { xy / yy.sqrt() } else { 0.0 }
    };
    let mut best = nominal;
    let mut best_c = f32::MIN;
    let mut cand = nominal - d;
    while cand <= nominal + d {
        let c = corr(cand, COARSE);
        if c > best_c {
            (best, best_c) = (cand, c);
        }
        cand += COARSE as isize;
    }
    let coarse = best;
    best_c = f32::MIN;
    for cand in (coarse - COARSE as isize)..=(coarse + COARSE as isize) {
        if (cand - nominal).abs() > d {
            continue;
        }
        let c = corr(cand, 1);
        if c > best_c {
            (best, best_c) = (cand, c);
        }
    }
    best
}

/// Rééchantillonne : la sortie dure `factor` fois l'entrée et sonne
/// `1 / factor` fois plus aigu (interpolation cubique d'Hermite).
pub fn resample(x: &[f32], factor: f64) -> Vec<f32> {
    let out_len = (x.len() as f64 * factor).round().max(0.0) as usize;
    if x.is_empty() || out_len == 0 {
        return Vec::new();
    }
    let step = 1.0 / factor;
    let get = |i: isize| x[i.clamp(0, x.len() as isize - 1) as usize];
    (0..out_len)
        .map(|j| {
            let pos = j as f64 * step;
            let i = pos.floor() as isize;
            let t = (pos - i as f64) as f32;
            let (y0, y1, y2, y3) = (get(i - 1), get(i), get(i + 1), get(i + 2));
            let c1 = 0.5 * (y2 - y0);
            let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
            let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
            ((c3 * t + c2) * t + c1) * t + y1
        })
        .collect()
}

/// Transpose de `semitones` demi-tons sans changer la durée.
pub fn pitch_shift(left: &[f32], right: &[f32], semitones: f32, sample_rate: f32) -> (Vec<f32>, Vec<f32>) {
    let len = left.len().min(right.len());
    if semitones.abs() < 1e-4 || len == 0 {
        return (left[..len].to_vec(), right[..len].to_vec());
    }
    let ratio = (semitones as f64 / 12.0).exp2();
    let (l, r) = wsola_stereo(left, right, ratio, sample_rate);
    let fix = |v: Vec<f32>| {
        let mut v = resample(&v, len as f64 / v.len().max(1) as f64);
        v.resize(len, 0.0);
        v
    };
    (fix(l), fix(r))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48_000.0;

    fn sine(freq: f32, len: usize) -> Vec<f32> {
        (0..len).map(|i| 0.5 * (i as f32 * freq * 2.0 * PI / SR).sin()).collect()
    }

    /// Fréquence par comptage des passages à zéro montants.
    fn freq(x: &[f32]) -> f32 {
        let ups = x.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        ups as f32 * SR / x.len() as f32
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    #[test]
    fn stretch_keeps_pitch_and_level() {
        let x = sine(440.0, 48_000);
        for factor in [0.5, 0.8, 1.25, 2.0] {
            let (l, r) = wsola_stereo(&x, &x, factor, SR);
            assert_eq!(l.len(), (48_000.0 * factor).round() as usize);
            assert_eq!(l, r);
            // Hors des bords, la hauteur et le niveau ne bougent pas.
            let mid = &l[4_000..l.len() - 4_000];
            assert!((freq(mid) / 440.0 - 1.0).abs() < 0.01, "×{factor} : {} Hz", freq(mid));
            for block in mid.chunks(960) {
                let level = rms(block) / rms(&x);
                assert!((0.85..1.1).contains(&level), "×{factor} : niveau {level}");
            }
        }
    }

    #[test]
    fn unity_is_identity() {
        let x = sine(220.0, 10_000);
        let (l, _) = wsola_stereo(&x, &x, 1.0, SR);
        assert_eq!(l, x);
    }

    #[test]
    fn stretch_keeps_the_hits() {
        // Quatre clics : on les retrouve aux positions étirées.
        let mut x = vec![0.0f32; 48_000];
        for k in 0..4 {
            for i in 0..400 {
                x[k * 12_000 + i] = (1.0 - i as f32 / 400.0) * if i % 2 == 0 { 0.8 } else { -0.8 };
            }
        }
        let (l, _) = wsola_stereo(&x, &x, 1.5, SR);
        let a = crate::onset::OnsetAnalysis::new(&l, SR);
        let hits = a.pick(&l, 0.5, 64);
        assert_eq!(hits.len(), 4, "{hits:?}");
        for (k, h) in hits.iter().enumerate() {
            let want = (k * 18_000) as isize;
            assert!((h.pos as isize - want).abs() < 900, "{} au lieu de {want}", h.pos);
        }
    }

    #[test]
    fn resample_changes_length_and_pitch() {
        let x = sine(440.0, 48_000);
        let y = resample(&x, 0.5);
        assert_eq!(y.len(), 24_000);
        assert!((freq(&y) / 880.0 - 1.0).abs() < 0.01, "{}", freq(&y));
        let z = resample(&x, 1.0);
        for (a, b) in x.iter().zip(&z) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn pitch_shift_keeps_length() {
        let x = sine(440.0, 48_000);
        let (l, _) = pitch_shift(&x, &x, 12.0, SR);
        assert_eq!(l.len(), x.len());
        let mid = &l[4_000..44_000];
        assert!((freq(mid) / 880.0 - 1.0).abs() < 0.015, "{}", freq(mid));
    }

    #[test]
    fn empty_input() {
        assert!(wsola_stereo(&[], &[], 2.0, SR).0.is_empty());
        assert!(resample(&[], 2.0).is_empty());
    }
}
