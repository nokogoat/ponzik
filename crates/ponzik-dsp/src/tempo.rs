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

/// Tempo estimate with its octave doubt.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TempoEstimate {
    /// Most likely tempo, in BPM.
    pub bpm: f32,
    /// Strength of the beat, 0 to 1 (autocorrelation at the chosen period).
    pub confidence: f32,
    /// Half or double tempo that fits almost as well (a drum and bass loop
    /// at 174 also reads as 87): offered to the user rather than guessed.
    pub alternative: Option<f32>,
}

/// Tempo estimé (BPM) d'une fonction d'attaque ; `None` sans rythme net.
pub fn estimate_bpm(analysis: &OnsetAnalysis, duration: f32) -> Option<f32> {
    estimate(analysis, duration).map(|e| e.bpm)
}

/// Tempo, confidence and octave alternative of an onset function.
pub fn estimate(analysis: &OnsetAnalysis, duration: f32) -> Option<TempoEstimate> {
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
    let comb = |lag: f32| -> f32 { at(lag) + 0.5 * at(2.0 * lag) + 0.25 * at(4.0 * lag) + 0.2 * at(lag * 0.5) };
    let score = |lag: f32| -> f32 {
        let bpm = 60.0 * rate / lag;
        let octave = (bpm / 120.0).log2();
        comb(lag) * (-0.5 * (octave / 0.9).powi(2)).exp()
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
    let mut lag = best as f32 + shift;
    // A loop cut on whole bars tells its tempo by its length: among those
    // exact tempos, one that scores nearly as well as the best wins (a short
    // loop has few periods, and its autocorrelation alone may err).
    let exact = (1..=64)
        .map(|bars| bars as f32 * 240.0 / duration.max(1e-3))
        .filter(|b| (MIN_BPM..=MAX_BPM).contains(b))
        .map(|b| (60.0 * rate / b, score(60.0 * rate / b)))
        .filter(|&(_, s)| s >= 0.85 * best_score)
        .max_by(|a, b| a.1.total_cmp(&b.1));
    if let Some((l, _)) = exact {
        lag = l;
    }
    let bpm = 60.0 * rate / lag;
    // Octave doubt: half and double tempo, at their own autocorrelation peak,
    // judged on the plain comb (no preference for 120). Most beats fit both
    // fairly well; the closer one is offered when it scores 60 % or more.
    let own = comb(lag).max(1e-6);
    let peak_near = |l: f32| -> f32 {
        let mut b = (l, at(l));
        for d in -3..=3 {
            let c = l + d as f32;
            if c > 1.0 && at(c) > b.1 {
                b = (c, at(c));
            }
        }
        b.0
    };
    let alternative = [0.5f32, 2.0]
        .into_iter()
        .filter_map(|k| {
            let l = peak_near(lag * k);
            let alt = 60.0 * rate / l;
            let ratio = comb(l) / own;
            (l > 1.0 && (50.0..=230.0).contains(&alt) && ratio >= 0.6).then_some((alt, ratio))
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))
        // Rounded to the exact octave of the chosen tempo.
        .map(|(alt, _)| if alt > bpm { 2.0 * bpm } else { 0.5 * bpm })
        .map(|alt| snap_to_loop(alt, duration));
    Some(TempoEstimate { bpm: snap_to_loop(bpm, duration), confidence: (own / 1.95).clamp(0.0, 1.0), alternative })
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

    /// Every tempo is found, or its half or double with the right one offered.
    #[test]
    fn finds_90_120_140_174_or_flags_the_octave() {
        // Exact loops of 2 and 4 bars, and excerpts that are no loop at all.
        let cases = [90.0f32, 120.0, 140.0, 174.0].into_iter().flat_map(|b| [(b, 8.0 * 60.0 / b), (b, 16.0 * 60.0 / b), (b, 7.3)]);
        for (bpm, seconds) in cases {
            let x = beat(bpm, seconds);
            let a = OnsetAnalysis::new(&x, SR);
            let e = estimate(&a, x.len() as f32 / SR).expect("tempo");
            let near = |v: f32| (v / bpm - 1.0).abs() < 0.02;
            let ok = near(e.bpm) || ((near(e.bpm * 2.0) || near(e.bpm * 0.5)) && e.alternative.is_some_and(near));
            eprintln!("{bpm} ({seconds:.2} s): {:.2} (alt {:?}, confidence {:.2})", e.bpm, e.alternative, e.confidence);
            assert!(ok, "{bpm} ({seconds} s): {e:?}");
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
