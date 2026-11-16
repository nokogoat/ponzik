//! Estimation du tempo d'un échantillon, hors temps réel.
//!
//! Autocorrélation de la fonction d'attaque ([`crate::onset`]) sur les
//! périodes de 60 à 200 BPM, renforcée par les multiples de la période (un
//! temps fort revient aussi à deux et quatre temps) et pondérée vers 120 BPM
//! pour départager les octaves. Si l'échantillon dure un nombre entier de
//! mesures à un tempo voisin, ce tempo exact l'emporte : une boucle bien
//! coupée donne son tempo au centième.

use crate::onset::OnsetAnalysis;

const MIN_BPM: f32 = 60.0;
const MAX_BPM: f32 = 200.0;

/// Tempo estimé (BPM) d'une fonction d'attaque ; `None` sans rythme net.
pub fn estimate_bpm(analysis: &OnsetAnalysis, duration: f32) -> Option<f32> {
    let flux = &analysis.flux;
    let rate = analysis.rate();
    if flux.len() < 16 || rate <= 0.0 {
        return None;
    }
    let mean = flux.iter().sum::<f32>() / flux.len() as f32;
    let x: Vec<f32> = flux.iter().map(|f| f - mean).collect();
    let energy: f32 = x.iter().map(|v| v * v).sum();
    if energy <= 1e-9 {
        return None;
    }
    let ac = |lag: usize| -> f32 {
        if lag >= x.len() {
            return 0.0;
        }
        let s: f32 = x[..x.len() - lag].iter().zip(&x[lag..]).map(|(a, b)| a * b).sum();
        // Normalisé par le nombre de termes : les grands retards ne sont pas pénalisés.
        s / (x.len() - lag) as f32 * x.len() as f32 / energy
    };
    let lag_of = |bpm: f32| 60.0 * rate / bpm;
    let lo = lag_of(MAX_BPM).floor().max(1.0) as usize;
    let hi = (lag_of(MIN_BPM).ceil() as usize).min(x.len() / 2);
    if hi <= lo {
        return None;
    }
    let raw: Vec<f32> = (0..=hi * 4).map(ac).collect();
    let at = |lag: f32| -> f32 {
        // Interpolation linéaire de l'autocorrélation.
        let i = lag.floor() as usize;
        let f = lag - i as f32;
        match (raw.get(i), raw.get(i + 1)) {
            (Some(a), Some(b)) => a + (b - a) * f,
            _ => 0.0,
        }
    };
    let score = |lag: f32| -> f32 {
        let comb = at(lag) + 0.5 * at(2.0 * lag) + 0.25 * at(4.0 * lag) + 0.2 * at(lag * 0.5);
        let bpm = 60.0 * rate / lag;
        let octave = (bpm / 120.0).log2();
        comb * (-0.5 * (octave / 0.9).powi(2)).exp()
    };
    let (mut best, mut best_score) = (0usize, f32::MIN);
    for lag in lo..=hi {
        let s = score(lag as f32);
        if s > best_score {
            (best, best_score) = (lag, s);
        }
    }
    if best_score <= 0.05 {
        return None;
    }
    // Affinage parabolique autour du meilleur retard.
    let (a, b, c) = (score(best as f32 - 1.0), best_score, score(best as f32 + 1.0));
    let denom = a - 2.0 * b + c;
    let shift = if denom.abs() > 1e-9 { (0.5 * (a - c) / denom).clamp(-0.5, 0.5) } else { 0.0 };
    let bpm = 60.0 * rate / (best as f32 + shift);
    Some(snap_to_loop(bpm, duration))
}

/// Tempo d'un signal mono.
pub fn detect_bpm(signal: &[f32], sample_rate: f32) -> Option<f32> {
    let a = OnsetAnalysis::new(signal, sample_rate);
    estimate_bpm(&a, signal.len() as f32 / sample_rate.max(1.0))
}

/// Si `duration` vaut à 4 % près un nombre entier de mesures (4 temps) au
/// tempo `bpm`, renvoie le tempo exact de cette boucle.
pub fn snap_to_loop(bpm: f32, duration: f32) -> f32 {
    if duration <= 0.0 {
        return bpm;
    }
    let beats = duration * bpm / 60.0;
    let bars = (beats / 4.0).round();
    if bars < 1.0 {
        return bpm;
    }
    let exact = bars * 4.0 * 60.0 / duration;
    if (exact / bpm - 1.0).abs() < 0.04 { exact } else { bpm }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::noise::WhiteNoise;

    const SR: f32 = 44_100.0;

    /// Batterie synthétique : grosse caisse sur 1 et 3, caisse claire sur 2 et
    /// 4, charleston sur les croches.
    fn beat(bpm: f32, seconds: f32) -> Vec<f32> {
        let len = (seconds * SR) as usize;
        let mut x = vec![0.0f32; len];
        let mut noise = WhiteNoise::new(5);
        let step = 60.0 / bpm * SR * 0.5;
        let mut k = 0;
        loop {
            let at = (k as f32 * step) as usize;
            if at >= len {
                break;
            }
            let (amp, decay, tone) = match k % 4 {
                0 => (0.9, 3_000.0, true),
                2 => (0.7, 2_000.0, false),
                _ => (0.25, 500.0, false),
            };
            for i in 0..(len - at).min(10_000) {
                let env = (-(i as f32) / decay).exp();
                let s = if tone { (i as f32 * 60.0 * std::f32::consts::TAU / SR).sin() } else { noise.next_sample() };
                x[at + i] += amp * env * s;
            }
            k += 1;
        }
        x
    }

    #[test]
    fn finds_common_tempos() {
        for bpm in [90.0, 120.0, 140.0, 174.0 / 2.0] {
            // Longueur qui ne tombe pas sur une boucle entière : pas d'aimantation.
            let x = beat(bpm, 7.3);
            let got = detect_bpm(&x, SR).expect("tempo");
            assert!((got / bpm - 1.0).abs() < 0.02, "{got} au lieu de {bpm}");
        }
    }

    #[test]
    fn exact_loops_snap() {
        let bpm = 93.0;
        let seconds = 8.0 * 60.0 / bpm; // 2 mesures
        let x = beat(bpm, seconds);
        let got = detect_bpm(&x, SR).expect("tempo");
        assert!((got - bpm).abs() < 0.05, "{got}");
    }

    #[test]
    fn no_rhythm_no_tempo() {
        assert_eq!(detect_bpm(&vec![0.0; 44_100], SR), None);
        assert_eq!(detect_bpm(&[0.1; 100], SR), None);
    }

    #[test]
    fn snap_only_when_close() {
        assert!((snap_to_loop(119.0, 4.0) - 120.0).abs() < 1e-3);
        assert_eq!(snap_to_loop(110.0, 4.0), 110.0);
    }
}
