//! Noise reduction by spectral gating, offline.
//!
//! First a [`NoiseProfile`] is learnt from a stretch of noise alone (hiss,
//! hum, room tone): its mean power in each FFT bin. Then [`denoise`] runs a
//! short-time Fourier transform (2048 points, 75 % overlap, square-root Hann
//! windows on both sides) and multiplies every bin by a gain:
//!
//! - the gain is a Wiener gain on the *a priori* signal to noise ratio,
//!   estimated by the decision-directed rule of Ephraim and Malah: the ratio
//!   follows the previous frame's clean estimate, so lone noise peaks do not
//!   open the gate for one frame (the "musical noise" of plain spectral
//!   subtraction);
//! - the gain is then smoothed across neighbouring bins and, when falling,
//!   over time (a short release), and never goes under the reduction floor:
//!   the noise is lowered by at most `reduction_db`, never carved to nothing.
//!
//! Everything allocates: an editing tool, not audio thread code.

use crate::fft::{Fft, hann};

/// FFT size of the analysis.
pub const FFT: usize = 2048;
/// Hop between frames (75 % overlap).
const HOP: usize = FFT / 4;

/// Mean noise power per bin, learnt from a noise-only selection.
#[derive(Clone, Debug, PartialEq)]
pub struct NoiseProfile {
    pub sample_rate: f32,
    /// Power of bins 0 to `FFT / 2`.
    pub power: Vec<f32>,
}

/// How hard [`denoise`] works.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DenoiseSettings {
    /// Most the noise is lowered, in dB (6 to 40).
    pub reduction_db: f32,
    /// Margin over the learnt noise, in dB: higher removes more, at the risk
    /// of quiet details (−6 to 12).
    pub threshold_db: f32,
    /// Smoothing, 0 to 1: more is calmer (no musical noise), less is more precise.
    pub smoothing: f32,
}

impl Default for DenoiseSettings {
    fn default() -> Self {
        Self { reduction_db: 24.0, threshold_db: 3.0, smoothing: 0.5 }
    }
}

/// Square-root Hann window: analysis and synthesis together make a Hann
/// window, whose copies at 75 % overlap sum to 2.
fn window() -> Vec<f32> {
    let mut w = vec![0.0; FFT];
    hann(&mut w);
    w.iter_mut().for_each(|v| *v = v.sqrt());
    w
}

/// Frames of `x` (zero-padded at both ends), one spectrum each.
fn frames(len: usize) -> usize {
    (len + FFT).div_ceil(HOP)
}

/// Windowed frame `k` of `x`: frame 0 starts `FFT` before the signal.
fn load(x: &[f32], k: usize, win: &[f32], re: &mut [f32], im: &mut [f32]) {
    let start = (k * HOP) as isize - FFT as isize;
    for i in 0..FFT {
        let at = start + i as isize;
        re[i] = if at >= 0 && (at as usize) < x.len() { x[at as usize] * win[i] } else { 0.0 };
        im[i] = 0.0;
    }
}

impl NoiseProfile {
    /// Learns the noise of a mono signal; `None` if it is shorter than one
    /// analysis frame or silent.
    pub fn learn(x: &[f32], sample_rate: f32) -> Option<Self> {
        Self::learn_channels(&[x], sample_rate)
    }

    /// Learns from several channels at once (their powers are averaged).
    pub fn learn_channels(channels: &[&[f32]], sample_rate: f32) -> Option<Self> {
        let bins = FFT / 2 + 1;
        let fft = Fft::new(FFT);
        let win = window();
        let (mut re, mut im) = (vec![0.0f32; FFT], vec![0.0f32; FFT]);
        let mut power = vec![0.0f64; bins];
        let mut count = 0usize;
        for x in channels {
            if x.len() < FFT {
                continue;
            }
            // Whole frames only: the padded edges would bias the profile low.
            let mut start = 0;
            while start + FFT <= x.len() {
                for i in 0..FFT {
                    re[i] = x[start + i] * win[i];
                    im[i] = 0.0;
                }
                fft.forward(&mut re, &mut im);
                for (b, p) in power.iter_mut().enumerate() {
                    *p += (re[b] * re[b] + im[b] * im[b]) as f64;
                }
                count += 1;
                start += HOP;
            }
        }
        if count == 0 {
            return None;
        }
        let power: Vec<f32> = power.iter().map(|p| (p / count as f64) as f32).collect();
        (power.iter().any(|&p| p > 1e-14)).then_some(Self { sample_rate, power })
    }

    /// Noise level per bin in dB (for display), relative to a full-scale sine.
    pub fn db(&self) -> Vec<f32> {
        // A full-scale sine through the sqrt-Hann window peaks at about FFT/π in magnitude.
        let full = (FFT as f32 / std::f32::consts::PI).powi(2);
        self.power.iter().map(|&p| 10.0 * (p / full + 1e-12).log10()).collect()
    }
}

/// Removes the learnt noise from `x`; the result has the same length.
pub fn denoise(x: &[f32], profile: &NoiseProfile, settings: DenoiseSettings) -> Vec<f32> {
    let n = x.len();
    let bins = FFT / 2 + 1;
    if n == 0 || profile.power.len() != bins {
        return x.to_vec();
    }
    let fft = Fft::new(FFT);
    let win = window();
    let floor = 10f32.powf(-settings.reduction_db.clamp(0.0, 60.0) / 20.0);
    let thr = 10f32.powf(settings.threshold_db.clamp(-20.0, 20.0) / 10.0);
    let smooth = settings.smoothing.clamp(0.0, 1.0);
    // Decision-directed weight: closer to 1 is smoother.
    let beta = 0.90 + 0.08 * smooth;
    // Release per frame of a falling gain (the attack is immediate).
    let release = smooth * 0.6;
    // Frequency smoothing half width, in bins.
    let spread = 1 + (smooth * 3.0) as usize;
    let noise: Vec<f32> = profile.power.iter().map(|&p| (p * thr).max(1e-20)).collect();

    let (mut re, mut im) = (vec![0.0f32; FFT], vec![0.0f32; FFT]);
    let mut out = vec![0.0f32; n + 2 * FFT];
    let mut clean_prev = vec![0.0f32; bins];
    let mut gain = vec![1.0f32; bins];
    let mut raw = vec![0.0f32; bins];
    let mut prev_gain = vec![1.0f32; bins];
    for k in 0..frames(n) {
        load(x, k, &win, &mut re, &mut im);
        fft.forward(&mut re, &mut im);
        for b in 0..bins {
            let p = re[b] * re[b] + im[b] * im[b];
            let post = p / noise[b];
            let prio = beta * clean_prev[b] / noise[b] + (1.0 - beta) * (post - 1.0).max(0.0);
            raw[b] = prio / (1.0 + prio);
        }
        // Across bins: a triangular average, then never under the floor.
        for b in 0..bins {
            let (lo, hi) = (b.saturating_sub(spread), (b + spread + 1).min(bins));
            let (mut acc, mut wsum) = (0.0f32, 0.0f32);
            for (j, r) in raw.iter().enumerate().take(hi).skip(lo) {
                let w = (spread + 1 - j.abs_diff(b)) as f32;
                acc += w * r;
                wsum += w;
            }
            // The bin's own gain wins when it is higher: a strong partial stays whole.
            let g = (acc / wsum).max(raw[b] * 0.9);
            // Over time: falls no faster than the release allows.
            let g = g.max(prev_gain[b] * release);
            gain[b] = g.clamp(floor, 1.0);
        }
        for b in 0..bins {
            let g = gain[b];
            let p = re[b] * re[b] + im[b] * im[b];
            clean_prev[b] = g * g * p;
            prev_gain[b] = g;
            re[b] *= g;
            im[b] *= g;
            if b > 0 && b < FFT / 2 {
                re[FFT - b] *= g;
                im[FFT - b] *= g;
            }
        }
        fft.inverse(&mut re, &mut im);
        let start = k * HOP;
        for i in 0..FFT {
            if let Some(o) = out.get_mut(start + i) {
                *o += re[i] * win[i] * 0.5;
            }
        }
    }
    out[FFT..FFT + n].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::noise::WhiteNoise;

    const SR: f32 = 48_000.0;

    fn tone(n: usize) -> Vec<f32> {
        (0..n).map(|i| 0.3 * (std::f32::consts::TAU * 1_000.0 * i as f32 / SR).sin()).collect()
    }

    fn noise(n: usize, amp: f32, seed: u32) -> Vec<f32> {
        let mut w = WhiteNoise::new(seed);
        (0..n).map(|_| amp * w.next_sample()).collect()
    }

    /// Level of the 1 kHz tone in `y` (least-squares fit of a sine and a
    /// cosine) and of the rest, both as power.
    fn split(y: &[f32]) -> (f64, f64) {
        let w = std::f64::consts::TAU * 1_000.0 / SR as f64;
        let (mut s, mut c) = (0.0f64, 0.0f64);
        for (i, &v) in y.iter().enumerate() {
            s += v as f64 * (w * i as f64).sin();
            c += v as f64 * (w * i as f64).cos();
        }
        let n = y.len() as f64;
        let (a, b) = (2.0 * s / n, 2.0 * c / n);
        let mut rest = 0.0f64;
        for (i, &v) in y.iter().enumerate() {
            let fit = a * (w * i as f64).sin() + b * (w * i as f64).cos();
            rest += (v as f64 - fit).powi(2);
        }
        ((a * a + b * b) / 2.0, rest / n)
    }

    #[test]
    fn improves_snr_by_more_than_10_db_without_hurting_the_tone() {
        let n = 96_000;
        let learn = noise(48_000, 0.1, 7);
        let profile = NoiseProfile::learn(&learn, SR).unwrap();
        let clean = tone(n);
        let hiss = noise(n, 0.1, 99);
        let dirty: Vec<f32> = clean.iter().zip(&hiss).map(|(a, b)| a + b).collect();
        let out = denoise(&dirty, &profile, DenoiseSettings::default());
        assert_eq!(out.len(), n);
        // Measured away from the edges.
        let mid = |v: &[f32]| v[4_800..n - 4_800].to_vec();
        let (t_in, n_in) = split(&mid(&dirty));
        let (t_out, n_out) = split(&mid(&out));
        let snr_in = 10.0 * (t_in / n_in).log10();
        let snr_out = 10.0 * (t_out / n_out).log10();
        let tone_drop = 10.0 * (t_in / t_out).log10();
        eprintln!("SNR {snr_in:.1} dB → {snr_out:.1} dB (+{:.1}), tone −{tone_drop:.2} dB", snr_out - snr_in);
        assert!(snr_out - snr_in > 10.0, "SNR {snr_in:.1} → {snr_out:.1}");
        assert!(tone_drop < 1.0, "tone dropped by {tone_drop:.2} dB");
    }

    #[test]
    fn leaves_a_clean_signal_alone_and_noise_down_to_the_floor() {
        let n = 48_000;
        let profile = NoiseProfile::learn(&noise(24_000, 0.001, 3), SR).unwrap();
        let clean = tone(n);
        let out = denoise(&clean, &profile, DenoiseSettings::default());
        let err = clean[2_048..n - 2_048].iter().zip(&out[2_048..n - 2_048]).fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
        assert!(err < 0.01, "{err}");
        // Pure noise: lowered by about the reduction, not more.
        let hiss = noise(n, 0.1, 11);
        let profile = NoiseProfile::learn(&noise(24_000, 0.1, 12), SR).unwrap();
        let s = DenoiseSettings { reduction_db: 20.0, ..DenoiseSettings::default() };
        let out = denoise(&hiss, &profile, s);
        let level = |v: &[f32]| 10.0 * (v.iter().map(|x| x * x).sum::<f32>() / v.len() as f32).log10();
        let drop = level(&hiss[4_800..n - 4_800]) - level(&out[4_800..n - 4_800]);
        assert!(drop > 14.0 && drop < 24.0, "noise lowered by {drop:.1} dB");
    }

    #[test]
    fn learning_needs_a_frame_of_sound() {
        assert!(NoiseProfile::learn(&[0.1; 100], SR).is_none());
        assert!(NoiseProfile::learn(&[0.0; 10_000], SR).is_none());
        let p = NoiseProfile::learn(&noise(10_000, 0.1, 1), SR).unwrap();
        assert_eq!(p.power.len(), FFT / 2 + 1);
        assert!(p.db().iter().all(|d| d.is_finite()));
    }
}
