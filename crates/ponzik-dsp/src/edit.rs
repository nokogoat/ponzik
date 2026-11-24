//! Offline audio editing, the operations of the audio editor (Edison style).
//!
//! Every function works on plain sample slices of one channel, or on a
//! stereo pair, and allocates its result when it has one: these are editing
//! tools for the interface thread, never audio thread code.
//!
//! - levels: [`peak`], [`rms`], [`apply_gain`], [`peak_normalize_gain`],
//!   [`integrated_lufs`] and [`lufs_normalize_gain`];
//! - fades with curve shapes ([`FadeShape`], [`fade_in`], [`fade_out`]);
//! - cleaning: [`remove_dc`], [`silence_bounds`] (trim silence),
//!   [`declick`] (isolated spikes), [`declip`] (flattened peaks);
//! - loops: [`nearest_zero_crossing`], [`crossfade_loop`];
//! - time and pitch: [`stretch`], [`pitch_shift`] (the warp engine's
//!   transient-safe and phase-vocoder stretchers) and [`resample`]
//!   (windowed sinc, for sample rate conversion).

use crate::loudness::LoudnessMeter;
use crate::warp::{WarpAlgo, warp_stereo};

/// Curve of a fade.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum FadeShape {
    /// Straight line.
    #[default]
    Linear,
    /// Constant power (sine quarter): the natural crossfade.
    EqualPower,
    /// Starts slowly, ends fast (exponential): a fade in that blooms late.
    Slow,
    /// Starts fast, ends slowly (logarithmic): a fade out that drops at once.
    Fast,
    /// Slow at both ends (raised cosine).
    SCurve,
}

impl FadeShape {
    pub const ALL: [FadeShape; 5] = [FadeShape::Linear, FadeShape::EqualPower, FadeShape::Slow, FadeShape::Fast, FadeShape::SCurve];

    /// Gain at `t` in [0, 1] of a rising fade (0 at 0, 1 at 1).
    pub fn gain(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            FadeShape::Linear => t,
            FadeShape::EqualPower => (t * std::f32::consts::FRAC_PI_2).sin(),
            FadeShape::Slow => t * t * t,
            FadeShape::Fast => 1.0 - (1.0 - t) * (1.0 - t) * (1.0 - t),
            FadeShape::SCurve => 0.5 - 0.5 * (t * std::f32::consts::PI).cos(),
        }
    }
}

/// Highest absolute sample.
pub fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0f32, |m, v| m.max(v.abs()))
}

/// Root mean square level.
pub fn rms(x: &[f32]) -> f32 {
    if x.is_empty() {
        return 0.0;
    }
    (x.iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>() / x.len() as f64).sqrt() as f32
}

pub fn apply_gain(x: &mut [f32], gain: f32) {
    for v in x {
        *v *= gain;
    }
}

/// Gain that brings the highest peak of the channels to `target_db` dBFS;
/// `None` when they are silent.
pub fn peak_normalize_gain(channels: &[&[f32]], target_db: f32) -> Option<f32> {
    let p = channels.iter().fold(0.0f32, |m, c| m.max(peak(c)));
    (p > 1e-9).then(|| 10f32.powf(target_db / 20.0) / p)
}

/// Integrated loudness (BS.1770, LUFS) of a stereo signal; `None` when it is
/// too short (under 400 ms) or below the absolute gate.
pub fn integrated_lufs(left: &[f32], right: &[f32], sample_rate: f32) -> Option<f32> {
    let mut meter = LoudnessMeter::new(sample_rate, 2);
    meter.process_stereo(left, right);
    let l = meter.integrated();
    l.is_finite().then_some(l)
}

/// Gain that brings the integrated loudness to `target_lufs`.
pub fn lufs_normalize_gain(left: &[f32], right: &[f32], sample_rate: f32, target_lufs: f32) -> Option<f32> {
    integrated_lufs(left, right, sample_rate).map(|l| 10f32.powf((target_lufs - l) / 20.0))
}

/// Fades the whole slice in: the first sample is silent, the last untouched.
pub fn fade_in(x: &mut [f32], shape: FadeShape) {
    let n = x.len();
    if n == 0 {
        return;
    }
    for (i, v) in x.iter_mut().enumerate() {
        *v *= shape.gain(i as f32 / n as f32);
    }
}

/// Fades the whole slice out: the first sample is untouched, the one after
/// the last would be silent.
pub fn fade_out(x: &mut [f32], shape: FadeShape) {
    let n = x.len();
    for (i, v) in x.iter_mut().enumerate() {
        *v *= shape.gain((n - i) as f32 / n as f32 - 1.0 / n as f32);
    }
}

/// Removes the DC offset (the mean) of the slice and returns it. Over the
/// first and last `ramp` frames the correction fades in and out, so a
/// selection inside a longer sound does not get a step at its edges.
pub fn remove_dc(x: &mut [f32], ramp: usize) -> f32 {
    let n = x.len();
    if n == 0 {
        return 0.0;
    }
    let mean = (x.iter().map(|&v| v as f64).sum::<f64>() / n as f64) as f32;
    let ramp = ramp.min(n / 2);
    for (i, v) in x.iter_mut().enumerate() {
        let edge = i.min(n - 1 - i);
        let w = if edge < ramp { (edge + 1) as f32 / (ramp + 1) as f32 } else { 1.0 };
        *v -= mean * w;
    }
    mean
}

/// First and last frame above `threshold_db` dBFS in either channel, widened
/// by `pad` frames on each side; `None` when everything is below.
pub fn silence_bounds(left: &[f32], right: &[f32], threshold_db: f32, pad: usize) -> Option<(usize, usize)> {
    let thr = 10f32.powf(threshold_db / 20.0);
    let len = left.len().min(right.len());
    let loud = |i: usize| left[i].abs() > thr || right[i].abs() > thr;
    let first = (0..len).find(|&i| loud(i))?;
    let last = (0..len).rev().find(|&i| loud(i))?;
    Some((first.saturating_sub(pad), (last + 1 + pad).min(len)))
}

/// Zero crossing nearest to `pos` within `radius` frames (a sign change, or an
/// exact zero); `pos` itself when there is none.
pub fn nearest_zero_crossing(x: &[f32], pos: usize, radius: usize) -> usize {
    if x.len() < 2 {
        return pos.min(x.len());
    }
    let pos = pos.min(x.len() - 1);
    let crosses = |i: usize| i > 0 && i < x.len() && (x[i] == 0.0 || (x[i - 1] < 0.0) != (x[i] < 0.0));
    for d in 0..=radius {
        if pos >= d && crosses(pos - d) {
            return pos - d;
        }
        if crosses(pos + d) {
            return pos + d;
        }
    }
    pos
}

/// Makes `[a, b)` loop seamlessly and returns the loop: the last `xfade`
/// frames of the loop are crossfaded (equal power) with the frames that lead
/// into `a`, so that the end flows into the start. Without enough material
/// before `a`, the start is crossfaded with what follows `b` instead.
pub fn crossfade_loop(x: &[f32], a: usize, b: usize, xfade: usize) -> Vec<f32> {
    let b = b.min(x.len());
    if a >= b {
        return Vec::new();
    }
    let mut out = x[a..b].to_vec();
    let len = b - a;
    let before = a.min(len / 2).min(xfade);
    let after = (x.len() - b).min(len / 2).min(xfade);
    if before >= after && before > 0 {
        let n = before;
        for i in 0..n {
            let t = (i + 1) as f32 / (n + 1) as f32;
            let (fi, fo) = (FadeShape::EqualPower.gain(t), FadeShape::EqualPower.gain(1.0 - t));
            let j = len - n + i;
            out[j] = out[j] * fo + x[a - n + i] * fi;
        }
    } else if after > 0 {
        let n = after;
        for i in 0..n {
            let t = (i + 1) as f32 / (n + 1) as f32;
            let (fi, fo) = (FadeShape::EqualPower.gain(t), FadeShape::EqualPower.gain(1.0 - t));
            out[i] = out[i] * fi + x[b + i] * fo;
        }
    }
    out
}

/// Prediction residual: what a straight line through the two previous
/// samples misses.
fn residual(x: &[f32], i: usize) -> f32 {
    if i < 2 { 0.0 } else { x[i] - (2.0 * x[i - 1] - x[i - 2]) }
}

/// Fills `x[a..b]` with a cubic Hermite curve from the slope before `a` to
/// the slope after `b`.
fn bridge(x: &mut [f32], a: usize, b: usize) {
    if a < 2 || b + 1 >= x.len() || b <= a {
        return;
    }
    let (p0, p1) = (x[a - 1], x[b]);
    let (m0, m1) = (x[a - 1] - x[a - 2], x[b + 1] - x[b]);
    let n = (b - a + 1) as f32;
    for (k, v) in x[a..b].iter_mut().enumerate() {
        let t = (k + 1) as f32 / n;
        let (t2, t3) = (t * t, t * t * t);
        let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
        let h10 = t3 - 2.0 * t2 + t;
        let h01 = -2.0 * t3 + 3.0 * t2;
        let h11 = t3 - t2;
        *v = h00 * p0 + h10 * n * m0 + h01 * p1 + h11 * n * m1;
    }
}

/// Repairs clicks: short bursts (up to about 1 ms) where the signal jumps far
/// more than its neighbourhood predicts, bridged by a smooth curve.
/// `sensitivity` in [0, 1] (0.5: default). Returns the number of clicks fixed.
pub fn declick(x: &mut [f32], sample_rate: f32, sensitivity: f32) -> usize {
    const BLOCK: usize = 512;
    let n = x.len();
    if n < 8 {
        return 0;
    }
    let k = 14.0 - 10.0 * sensitivity.clamp(0.0, 1.0);
    let max_run = ((sample_rate * 0.001) as usize).max(4);
    // Local scale of the residual: median of |residual| per block, spread to ±2 blocks.
    let blocks = n.div_ceil(BLOCK);
    let med: Vec<f32> = (0..blocks)
        .map(|bk| {
            let mut v: Vec<f32> = (bk * BLOCK..((bk + 1) * BLOCK).min(n)).map(|i| residual(x, i).abs()).collect();
            v.sort_unstable_by(f32::total_cmp);
            v[v.len() / 2]
        })
        .collect();
    let scale = |i: usize| {
        let bk = i / BLOCK;
        let (lo, hi) = (bk.saturating_sub(2), (bk + 3).min(blocks));
        let mut w = med[lo..hi].to_vec();
        w.sort_unstable_by(f32::total_cmp);
        w[w.len() / 2].max(1e-5)
    };
    let mut fixed = 0;
    let mut i = 2;
    while i < n - 2 {
        let s = scale(i);
        if residual(x, i).abs() <= k * s {
            i += 1;
            continue;
        }
        // Burst: as long as the residual stays high (with a gap of 2 allowed).
        let mut end = i + 1;
        let mut quiet = 0;
        while end < n - 2 && end - i <= max_run + 2 {
            if residual(x, end).abs() > k * s * 0.5 {
                quiet = 0;
            } else {
                quiet += 1;
                if quiet > 2 {
                    break;
                }
            }
            end += 1;
        }
        let end = end - quiet;
        let (a, b) = (i.saturating_sub(1).max(2), (end + 1).min(n - 2));
        if b - a <= max_run + 2 {
            bridge(x, a, b);
            fixed += 1;
        }
        i = end + 2;
    }
    fixed
}

/// Rebuilds clipped peaks: runs of at least two samples stuck at the
/// highest level are replaced by a cubic curve that follows the slopes
/// around them (the peak may then go over full scale: lower the gain after).
/// Returns the number of runs rebuilt.
pub fn declip(x: &mut [f32]) -> usize {
    let top = peak(x);
    if top < 0.05 {
        return 0;
    }
    let level = top * 0.998;
    let n = x.len();
    let mut fixed = 0;
    let mut i = 2;
    while i < n {
        if x[i].abs() < level {
            i += 1;
            continue;
        }
        let sign = x[i].signum();
        let mut end = i;
        while end < n && x[end].abs() >= level && x[end].signum() == sign {
            end += 1;
        }
        if end - i >= 2 && end + 1 < n {
            bridge(x, i, end);
            // A peak keeps its side: the curve can only rise past the flat top.
            for v in &mut x[i..end] {
                if v.abs() < level || v.signum() != sign {
                    *v = sign * level;
                }
            }
            fixed += 1;
        }
        i = end + 1;
    }
    fixed
}

/// Changes the duration by `factor` (2.0: twice as long) without changing
/// the pitch. `algo`: [`WarpAlgo::Beats`] keeps transients, `Tones` suits
/// chords and voices, `RePitch` changes the speed like a turntable.
pub fn stretch(left: &[f32], right: &[f32], sample_rate: f32, factor: f64, algo: WarpAlgo) -> (Vec<f32>, Vec<f32>) {
    let len = left.len().min(right.len());
    let factor = if factor.is_finite() { factor.clamp(0.05, 20.0) } else { 1.0 };
    let out_len = (len as f64 * factor).round() as usize;
    let map = |k: f64| k / factor;
    warp_stereo(left, right, sample_rate, out_len, &map, false, algo, 1.0)
}

/// Transposes by `semitones` without changing the duration.
pub fn pitch_shift(left: &[f32], right: &[f32], sample_rate: f32, semitones: f32, algo: WarpAlgo) -> (Vec<f32>, Vec<f32>) {
    let len = left.len().min(right.len());
    if algo == WarpAlgo::RePitch {
        // Re-pitch: the speed follows the pitch, the length changes.
        let ratio = (semitones as f64 / 12.0).exp2();
        let out_len = (len as f64 / ratio).round() as usize;
        let map = |k: f64| k * ratio;
        return warp_stereo(left, right, sample_rate, out_len, &map, false, algo, 1.0);
    }
    let map = |k: f64| k;
    warp_stereo(left, right, sample_rate, len, &map, false, algo, (semitones as f64 / 12.0).exp2())
}

/// Half width of the sinc kernel, in input samples at the lower rate.
const SINC_TAPS: usize = 32;

/// Converts `x` from `from` Hz to `to` Hz: windowed sinc (Blackman), with
/// its cutoff just under the lower Nyquist frequency so nothing aliases.
pub fn resample(x: &[f32], from: f32, to: f32) -> Vec<f32> {
    if x.is_empty() || from <= 0.0 || to <= 0.0 {
        return Vec::new();
    }
    if (from - to).abs() < 1e-3 {
        return x.to_vec();
    }
    let ratio = to as f64 / from as f64;
    let out_len = (x.len() as f64 * ratio).round() as usize;
    // Cutoff relative to the input rate.
    let cutoff = ratio.min(1.0) * 0.94;
    let half = (SINC_TAPS as f64 / cutoff).ceil() as isize;
    let step = 1.0 / ratio;
    let pi = std::f64::consts::PI;
    (0..out_len)
        .map(|j| {
            let pos = j as f64 * step;
            let center = pos.floor() as isize;
            let mut acc = 0.0f64;
            let mut norm = 0.0f64;
            for i in center - half + 1..=center + half {
                let d = pos - i as f64;
                let w = d / (half as f64);
                if w.abs() >= 1.0 {
                    continue;
                }
                let win = 0.42 + 0.5 * (pi * w).cos() + 0.08 * (2.0 * pi * w).cos();
                let arg = pi * cutoff * d;
                let sinc = if arg.abs() < 1e-9 { 1.0 } else { arg.sin() / arg };
                let k = cutoff * sinc * win;
                norm += k;
                if i >= 0 && (i as usize) < x.len() {
                    acc += k * x[i as usize] as f64;
                }
            }
            // Normalised by the kernel sum: DC passes at exactly 1.
            (if norm.abs() > 1e-12 { acc / norm } else { 0.0 }) as f32
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::noise::WhiteNoise;
    use crate::tuner::Tuner;

    const SR: f32 = 48_000.0;

    fn sine(hz: f32, amp: f32, n: usize) -> Vec<f32> {
        (0..n).map(|i| amp * (std::f32::consts::TAU * hz * i as f32 / SR).sin()).collect()
    }

    /// Median pitch (Hz) measured by the YIN tuner over the middle of `x`.
    fn pitch_of(x: &[f32]) -> f32 {
        let mut t = Tuner::new(SR, 4096);
        let mut found = Vec::new();
        let mid = &x[x.len() / 4..x.len() * 3 / 4];
        for chunk in mid.chunks(2048) {
            t.push(chunk);
            if let Some(p) = t.detect() {
                found.push(p.hz);
            }
        }
        found.sort_unstable_by(f32::total_cmp);
        found[found.len() / 2]
    }

    fn cents(a: f32, b: f32) -> f32 {
        1200.0 * (a / b).log2()
    }

    #[test]
    fn peak_normalize_hits_the_target() {
        let x = sine(440.0, 0.3, 4800);
        let g = peak_normalize_gain(&[&x], -1.0).unwrap();
        let mut y = x.clone();
        apply_gain(&mut y, g);
        assert!((20.0 * peak(&y).log10() + 1.0).abs() < 0.01, "{}", 20.0 * peak(&y).log10());
        assert!(peak_normalize_gain(&[&[0.0; 100]], 0.0).is_none());
    }

    #[test]
    fn lufs_normalize_hits_the_target() {
        let mut noise = WhiteNoise::new(4);
        let mut l: Vec<f32> = sine(1000.0, 0.1, 48_000 * 3);
        for v in &mut l {
            *v += 0.05 * noise.next_sample();
        }
        let r = l.clone();
        for target in [-23.0f32, -14.0, -9.0] {
            let g = lufs_normalize_gain(&l, &r, SR, target).unwrap();
            let (mut a, mut b) = (l.clone(), r.clone());
            apply_gain(&mut a, g);
            apply_gain(&mut b, g);
            let got = integrated_lufs(&a, &b, SR).unwrap();
            assert!((got - target).abs() < 0.1, "{got} for {target}");
        }
        assert!(integrated_lufs(&[0.1; 1000], &[0.1; 1000], SR).is_none(), "too short");
    }

    /// Largest jump between neighbours, relative to the largest jump of the
    /// source (a click shows as a jump far bigger than the waveform has).
    fn worst_step(x: &[f32]) -> f32 {
        x.windows(2).fold(0.0f32, |m, w| m.max((w[1] - w[0]).abs()))
    }

    #[test]
    fn fades_are_click_free_with_every_shape() {
        let x = sine(220.0, 0.9, 9_600);
        let src_step = worst_step(&x);
        for shape in FadeShape::ALL {
            // Fade in from silence, fade out to silence, in the middle of a loud tone.
            let mut y = x.clone();
            fade_in(&mut y[..4_800], shape);
            fade_out(&mut y[4_800..], shape);
            assert!(y[0].abs() < 1e-6, "{shape:?} starts at {}", y[0]);
            assert!(y[9_599].abs() < 0.01, "{shape:?} ends at {}", y[9_599]);
            assert!(worst_step(&y) <= src_step * 1.05, "{shape:?}: step {} vs {}", worst_step(&y), src_step);
            // The joint in the middle is continuous: both fades are near unity there.
            assert!((y[4_799] - x[4_799]).abs() < 0.01 && (y[4_800] - x[4_800]).abs() < 0.01, "{shape:?}");
            // Rising and monotonic.
            let g: Vec<f32> = (0..=100).map(|i| shape.gain(i as f32 / 100.0)).collect();
            assert!(g.windows(2).all(|w| w[1] >= w[0] - 1e-6) && g[0] == 0.0 && (g[100] - 1.0).abs() < 1e-6, "{shape:?}");
        }
    }

    #[test]
    fn dc_offset_is_removed_without_touching_the_tone() {
        let mut x: Vec<f32> = sine(200.0, 0.5, 48_000).iter().map(|v| v + 0.2).collect();
        let off = remove_dc(&mut x, 0);
        assert!((off - 0.2).abs() < 1e-3, "{off}");
        let mean = x.iter().sum::<f32>() / x.len() as f32;
        assert!(mean.abs() < 1e-3, "{mean}");
        let mid = &x[4_800..43_200];
        assert!((rms(mid) - 0.5 / 2f32.sqrt()).abs() < 0.01, "{}", rms(mid));
    }

    #[test]
    fn silence_bounds_find_the_sound() {
        let mut l = vec![0.0f32; 10_000];
        l[3_000..6_000].copy_from_slice(&sine(300.0, 0.5, 3_000));
        let r = vec![0.0001f32; 10_000];
        let (a, b) = silence_bounds(&l, &r, -48.0, 10).unwrap();
        assert!((2_990..=3_000).contains(&a) && (6_000..=6_010).contains(&b), "{a} {b}");
        assert!(silence_bounds(&r, &r, -48.0, 0).is_none());
    }

    #[test]
    fn zero_crossings_are_found_nearby() {
        let x = sine(100.0, 1.0, 4_800);
        // Crossings every 240 frames.
        assert_eq!(nearest_zero_crossing(&x, 250, 100), 240);
        assert_eq!(nearest_zero_crossing(&x, 470, 100), 480);
        assert_eq!(nearest_zero_crossing(&x, 360, 10), 360, "none in reach");
    }

    #[test]
    fn crossfaded_loop_wraps_smoothly() {
        // 441 Hz does not fit a 4 800-frame loop: a hard loop clicks.
        let x = sine(441.0, 0.8, 24_000);
        // Half a period off: the plain loop jumps from one peak to the other.
        let (a, b) = (9_000, 13_854);
        let hard = (x[a] - x[b]).abs();
        let looped = crossfade_loop(&x, a, b, 1_000);
        assert_eq!(looped.len(), b - a);
        let wrap = (looped[0] - looped[looped.len() - 1]).abs();
        let step = worst_step(&x);
        assert!(hard > 2.0 * step, "the plain loop should click: {hard} vs {step}");
        assert!(wrap <= step * 1.05, "wrap {wrap} vs step {step}");
        // Not enough before: uses what follows.
        let early = crossfade_loop(&x, 10, 4_864, 1_000);
        assert_eq!(early.len(), 4_854);
        assert!((early[0] - early[early.len() - 1]).abs() <= step * 1.05, "{}", (early[0] - early[early.len() - 1]).abs());
    }

    #[test]
    fn declick_repairs_spikes_and_leaves_clean_audio_alone() {
        let clean = sine(440.0, 0.5, 48_000);
        let mut y = clean.clone();
        assert_eq!(declick(&mut y, SR, 0.5), 0, "nothing to fix in a sine");
        assert_eq!(y, clean);
        let mut dirty = clean.clone();
        for &at in &[5_000usize, 17_321, 30_000, 41_111] {
            dirty[at] += 0.7;
            dirty[at + 1] -= 0.4;
        }
        let fixed = declick(&mut dirty, SR, 0.5);
        assert_eq!(fixed, 4);
        let err = dirty.iter().zip(&clean).fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
        assert!(err < 0.02, "worst error after repair {err}");
    }

    #[test]
    fn declip_rebuilds_flattened_peaks() {
        let truth = sine(300.0, 1.4, 9_600);
        let clipped: Vec<f32> = truth.iter().map(|v| v.clamp(-1.0, 1.0)).collect();
        let mut y = clipped.clone();
        let runs = declip(&mut y);
        assert!(runs > 10, "{runs}");
        let err = |a: &[f32]| (a.iter().zip(&truth).map(|(a, b)| (a - b) * (a - b)).sum::<f32>() / a.len() as f32).sqrt();
        assert!(err(&y) < err(&clipped) * 0.5, "{} vs {}", err(&y), err(&clipped));
        assert!(peak(&y) > 1.2);
    }

    #[test]
    fn stretch_keeps_the_pitch() {
        let x = sine(440.0, 0.5, 48_000);
        for (factor, algo) in [(1.5, WarpAlgo::Tones), (0.7, WarpAlgo::Tones), (1.3, WarpAlgo::Beats)] {
            let (l, _) = stretch(&x, &x, SR, factor, algo);
            assert_eq!(l.len(), (48_000.0 * factor).round() as usize);
            let c = cents(pitch_of(&l), 440.0);
            assert!(c.abs() < 5.0, "{factor} {algo:?}: {c} cents");
        }
    }

    #[test]
    fn pitch_shift_moves_by_semitones() {
        let x = sine(440.0, 0.5, 48_000);
        for st in [3.0f32, -5.0, 12.0] {
            let (l, _) = pitch_shift(&x, &x, SR, st, WarpAlgo::Tones);
            assert_eq!(l.len(), x.len());
            let want = 440.0 * (st / 12.0).exp2();
            let c = cents(pitch_of(&l), want);
            assert!(c.abs() < 5.0, "{st}: {c} cents");
        }
        let (l, _) = pitch_shift(&x, &x, SR, 12.0, WarpAlgo::RePitch);
        assert!((l.len() as i64 - 24_000).abs() <= 1);
    }

    #[test]
    fn resample_keeps_pitch_and_stops_aliasing() {
        let x: Vec<f32> = (0..44_100).map(|i| 0.5 * (std::f32::consts::TAU * 1000.0 * i as f32 / 44_100.0).sin()).collect();
        let y = resample(&x, 44_100.0, 48_000.0);
        assert!((y.len() as i64 - 48_000).abs() <= 1);
        // A 1 kHz sine at 48 kHz: compare to the ideal one.
        let ideal = sine(1000.0, 0.5, y.len());
        let err = y[1_000..47_000].iter().zip(&ideal[1_000..47_000]).fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
        assert!(err < 0.005, "{err}");
        // 20 kHz cannot exist at 22.05 kHz: it must vanish, not fold to 2.05 kHz.
        let hi = sine(20_000.0, 0.5, 48_000);
        let down = resample(&hi, 48_000.0, 22_050.0);
        let level = rms(&down[1_000..down.len() - 1_000]);
        assert!(20.0 * (level / (0.5 / 2f32.sqrt())).log10() < -40.0, "{}", level);
    }
}
