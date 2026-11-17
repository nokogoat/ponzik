//! Time-varying warping of audio clips, off the audio thread.
//!
//! A warp is described by a map from output samples to source positions
//! (fractional source samples, both at the same rate): the clip follows any
//! tempo curve, ramps included. Three algorithms:
//!
//! - [`WarpAlgo::Beats`]: the source is cut at its transients (see
//!   [`crate::onset`]); every segment starts exactly where its transient
//!   belongs and plays its first milliseconds untouched, only the tail between
//!   two hits is stretched (WSOLA). Drums keep their punch, no doubled hits.
//! - [`WarpAlgo::Tones`]: phase vocoder with identity phase locking (Laroche
//!   and Dolson), instantaneous frequencies measured on every frame, phases of
//!   both channels locked to the mid signal: smooth on chords, pads and voices.
//! - [`WarpAlgo::RePitch`]: plain resampling, the pitch follows the speed.
//!
//! In Beats and Tones, `pitch` transposes independently of the time map: the
//! signal is warped to `pitch` times the length, then resampled back.
//!
//! Everything here allocates: these are editing tools, not audio thread code.

use std::f64::consts::PI;

use crate::fft::{Fft, RealFft, hann};
use crate::onset::{OnsetAnalysis, mono};

/// How a warped clip is stretched.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum WarpAlgo {
    /// Transient-preserving segments, for drums and percussive loops.
    #[default]
    Beats,
    /// Phase vocoder, for tonal material.
    Tones,
    /// Resampling: tempo and pitch move together, like a turntable.
    RePitch,
}

impl WarpAlgo {
    pub const ALL: [WarpAlgo; 3] = [WarpAlgo::Beats, WarpAlgo::Tones, WarpAlgo::RePitch];

    pub fn id(self) -> &'static str {
        match self {
            WarpAlgo::Beats => "beats",
            WarpAlgo::Tones => "tones",
            WarpAlgo::RePitch => "repitch",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.id() == id)
    }
}

/// Untouched part of a segment after its transient, in seconds.
const TRANSIENT_SECONDS: f64 = 0.03;
/// WSOLA frame of the Beats tails, in seconds.
const BEATS_FRAME_SECONDS: f32 = 0.024;
/// Search span of a WSOLA splice, as a share of the frame.
const TOLERANCE: f32 = 0.25;
/// Fade at the end of a stretched segment, in samples.
const SEGMENT_FADE: usize = 48;

/// Source sample at `i`: wrapped around when `looped`, silent outside otherwise.
#[inline]
fn tap(x: &[f32], i: isize, looped: bool) -> f32 {
    if x.is_empty() {
        return 0.0;
    }
    if looped {
        x[i.rem_euclid(x.len() as isize) as usize]
    } else if i >= 0 && (i as usize) < x.len() {
        x[i as usize]
    } else {
        0.0
    }
}

/// Cubic Hermite read at a fractional position.
pub fn read_cubic(x: &[f32], pos: f64, looped: bool) -> f32 {
    let i = pos.floor();
    let t = (pos - i) as f32;
    let i = i as isize;
    let (y0, y1, y2, y3) = (
        tap(x, i - 1, looped),
        tap(x, i, looped),
        tap(x, i + 1, looped),
        tap(x, i + 2, looped),
    );
    let c1 = 0.5 * (y2 - y0);
    let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
    let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
    ((c3 * t + c2) * t + c1) * t + y1
}

/// Renders `out_len` samples of a warped stereo source. `map(k)` is the source
/// position of output sample `k` (it must not decrease); positions past the
/// source wrap around when `looped`. `pitch` is a frequency ratio (2.0: an
/// octave up), ignored in Re-Pitch.
#[allow(clippy::too_many_arguments)]
pub fn warp_stereo(
    left: &[f32],
    right: &[f32],
    sample_rate: f32,
    out_len: usize,
    map: &dyn Fn(f64) -> f64,
    looped: bool,
    algo: WarpAlgo,
    pitch: f64,
) -> (Vec<f32>, Vec<f32>) {
    let len = left.len().min(right.len());
    let (left, right) = (&left[..len], &right[..len]);
    if out_len == 0 || len == 0 {
        return (vec![0.0; out_len], vec![0.0; out_len]);
    }
    if algo == WarpAlgo::RePitch {
        let render = |x: &[f32]| {
            (0..out_len)
                .map(|k| read_cubic(x, map(k as f64), looped))
                .collect()
        };
        return (render(left), render(right));
    }
    let pitch = if pitch.is_finite() && pitch > 0.0 {
        pitch.clamp(0.25, 4.0)
    } else {
        1.0
    };
    let shifted = (pitch - 1.0).abs() > 1e-9;
    // Intermediate signal: `pitch` times longer, same pitch, resampled at the end.
    let inner_len = if shifted {
        (out_len as f64 * pitch).ceil() as usize + 4
    } else {
        out_len
    };
    let inner_map = |j: f64| map(j / pitch);
    let (l, r) = match algo {
        WarpAlgo::Beats => beats(left, right, sample_rate, inner_len, &inner_map, looped),
        _ => vocoder(left, right, sample_rate, inner_len, &inner_map, looped),
    };
    if !shifted {
        return (l, r);
    }
    let back = |x: &[f32]| {
        (0..out_len)
            .map(|k| read_cubic(x, k as f64 * pitch, false))
            .collect()
    };
    (back(&l), back(&r))
}

// --- WSOLA ---

/// Time-varying WSOLA: output frames of `n` samples at 50 % overlap (Hann),
/// each taken near its mapped source position, at the offset that continues
/// the previous frame best. The first frame sits exactly on its position.
/// Source samples from `end` on are left out (the next hit of a Beats segment).
fn wsola_map(
    left: &[f32],
    right: &[f32],
    out_len: usize,
    map: &dyn Fn(f64) -> f64,
    n: usize,
    looped: bool,
    end: f64,
) -> (Vec<f32>, Vec<f32>) {
    let hs = n / 2;
    let delta = (n as f32 * TOLERANCE) as usize;
    let mono: Vec<f32> = mono(left, right);
    let window: Vec<f32> = (0..n)
        .map(|i| (0.5 - 0.5 * (2.0 * PI * i as f64 / n as f64).cos()) as f32)
        .collect();
    let mut out_l = vec![0.0f32; out_len + n];
    let mut out_r = vec![0.0f32; out_len + n];
    let mut norm = vec![0.0f32; out_len + n];
    let mut prev: isize = 0;
    let frames = out_len.div_ceil(hs) + 1;
    for k in 0..frames {
        // Frame k is centred on output sample k·hs.
        let out_start = k as isize * hs as isize - hs as isize;
        let nominal = map((k * hs) as f64).round() as isize - hs as isize;
        let start = if k == 0 {
            nominal
        } else {
            best_offset(&mono, prev + hs as isize, nominal, delta, n, looped)
        };
        for (i, &w) in window.iter().enumerate() {
            let o = out_start + i as isize;
            if o < 0 || o as usize >= out_l.len() {
                continue;
            }
            let src = start + i as isize;
            norm[o as usize] += w;
            if src as f64 >= end {
                continue;
            }
            out_l[o as usize] += w * tap(left, src, looped);
            out_r[o as usize] += w * tap(right, src, looped);
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

/// Frame start in `[nominal − delta, nominal + delta]` most similar to
/// `natural` (the continuation of the previous frame): coarse decimated
/// search, then a fine one around the best point.
fn best_offset(
    x: &[f32],
    natural: isize,
    nominal: isize,
    delta: usize,
    n: usize,
    looped: bool,
) -> isize {
    const COARSE: usize = 4;
    if natural == nominal {
        return nominal;
    }
    let d = delta as isize;
    let corr = |cand: isize, step: usize| -> f32 {
        let (mut xy, mut yy) = (0.0f32, 0.0f32);
        let mut i = 0;
        while i < n {
            let a = tap(x, natural + i as isize, looped);
            let b = tap(x, cand + i as isize, looped);
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

// --- Beats ---

/// Output position where the source reaches `src` (bisection on the map).
fn inverse(map: &dyn Fn(f64) -> f64, src: f64, out_len: usize) -> f64 {
    let (mut lo, mut hi) = (0.0f64, out_len as f64);
    if map(lo) >= src {
        return lo;
    }
    if map(hi) <= src {
        return hi;
    }
    for _ in 0..48 {
        let mid = 0.5 * (lo + hi);
        if map(mid) < src {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    hi
}

fn beats(
    left: &[f32],
    right: &[f32],
    sample_rate: f32,
    out_len: usize,
    map: &dyn Fn(f64) -> f64,
    looped: bool,
) -> (Vec<f32>, Vec<f32>) {
    let len = left.len();
    let sr = sample_rate.max(8_000.0);
    let n = (((sr * BEATS_FRAME_SECONDS) as usize) / 2 * 2).max(64);
    let signal = mono(left, right);
    let analysis = OnsetAnalysis::new(&signal, sr);
    let onsets: Vec<f64> = analysis
        .pick(&signal, 0.6, 1 << 16)
        .iter()
        .map(|o| o.pos as f64)
        .collect();
    // Hits met by the clip, as source positions (repeated in a loop).
    let (s0, s1) = (map(0.0), map(out_len as f64));
    let mut hits: Vec<f64> = Vec::new();
    if !onsets.is_empty() {
        let period = len as f64;
        let mut lap = if looped { (s0 / period).floor() } else { 0.0 };
        loop {
            let base = lap * period;
            if base > s1 {
                break;
            }
            hits.extend(
                onsets
                    .iter()
                    .map(|o| o + base)
                    .filter(|&p| p > s0 && p < s1),
            );
            if !looped {
                break;
            }
            lap += 1.0;
        }
    }
    // Segment bounds: output start, source start.
    let mut bounds: Vec<(usize, f64)> = vec![(0, s0)];
    for h in hits {
        let at = inverse(map, h, out_len).round() as usize;
        if at > bounds.last().map_or(0, |b| b.0) && at < out_len {
            bounds.push((at, h));
        }
    }
    let transient = TRANSIENT_SECONDS * sr as f64;
    let mut out_l = vec![0.0f32; out_len];
    let mut out_r = vec![0.0f32; out_len];
    for (i, &(ua, sa)) in bounds.iter().enumerate() {
        let (ub, sb) = bounds.get(i + 1).copied().unwrap_or((out_len, s1));
        let seg = ub - ua;
        if seg == 0 {
            continue;
        }
        let span = (sb - sa).max(0.0);
        let l = seg as f64;
        // Untouched transient, then the tail fitted to the next hit.
        let keep = transient.min(0.5 * span).min(l);
        let sub = move |v: f64| {
            if v <= keep || l <= keep {
                sa + v
            } else {
                sa + keep + (v - keep) * (span - keep) / (l - keep)
            }
        };
        let natural = (span - l).abs() < 1.0;
        let (sl, sr_) = if natural {
            let copy = |x: &[f32]| {
                (0..seg)
                    .map(|v| read_cubic(x, sa + v as f64, looped))
                    .collect::<Vec<f32>>()
            };
            (copy(left), copy(right))
        } else {
            wsola_map(left, right, seg, &sub, n, looped, sb)
        };
        let fade = if natural || i + 1 == bounds.len() {
            0
        } else {
            SEGMENT_FADE.min(seg / 4)
        };
        for v in 0..seg {
            let g = if v + fade >= seg && fade > 0 {
                (seg - v) as f32 / fade as f32
            } else {
                1.0
            };
            out_l[ua + v] = sl[v] * g;
            out_r[ua + v] = sr_[v] * g;
        }
    }
    (out_l, out_r)
}

// --- Tones: phase vocoder ---

/// Wraps a phase into [−π, π].
fn princarg(p: f64) -> f64 {
    p - 2.0 * PI * ((p + PI) / (2.0 * PI)).floor()
}

/// Spectrum of an `n`-sample Hann-windowed frame centred on `centre`.
struct Frame {
    re: Vec<f32>,
    im: Vec<f32>,
}

struct Analyzer {
    fft: RealFft,
    window: Vec<f32>,
    buf: Vec<f32>,
}

impl Analyzer {
    fn new(n: usize) -> Self {
        let mut window = vec![0.0; n];
        hann(&mut window);
        Self {
            fft: RealFft::new(n),
            window,
            buf: vec![0.0; n],
        }
    }

    fn analyze(&mut self, x: &[f32], centre: isize, looped: bool, out: &mut Frame) {
        let n = self.buf.len();
        let start = centre - (n / 2) as isize;
        for (i, b) in self.buf.iter_mut().enumerate() {
            *b = tap(x, start + i as isize, looped) * self.window[i];
        }
        self.fft.forward(&self.buf, &mut out.re, &mut out.im);
    }
}

fn vocoder(
    left: &[f32],
    right: &[f32],
    sample_rate: f32,
    out_len: usize,
    map: &dyn Fn(f64) -> f64,
    looped: bool,
) -> (Vec<f32>, Vec<f32>) {
    // ~85 ms frames: 4096 samples at 44.1 and 48 kHz.
    let n = ((sample_rate.max(8_000.0) * 0.085) as usize)
        .next_power_of_two()
        .clamp(512, 8192);
    let hs = n / 4;
    let bins = n / 2 + 1;
    let mid = mono(left, right);
    let mut analyzer = Analyzer::new(n);
    let new_frame = || Frame {
        re: vec![0.0; bins],
        im: vec![0.0; bins],
    };
    let (mut fm, mut fm_prev, mut fl, mut fr) =
        (new_frame(), new_frame(), new_frame(), new_frame());
    let ifft = Fft::new(n);
    let (mut re, mut im) = (vec![0.0f32; n], vec![0.0f32; n]);
    let mut window = vec![0.0f32; n];
    hann(&mut window);
    let mut out_l = vec![0.0f32; out_len + n];
    let mut out_r = vec![0.0f32; out_len + n];
    let mut norm = vec![0.0f32; out_len + n];
    let mut synth = vec![0.0f64; bins];
    let mut mag = vec![0.0f64; bins];
    let mut phase = vec![0.0f64; bins];
    let mut peak_of = vec![0usize; bins];
    let mut peaks: Vec<usize> = Vec::with_capacity(bins);
    let frames = out_len.div_ceil(hs) + 4;
    for m in 0..frames {
        let centre = (m * hs) as f64;
        let a = map(centre).round() as isize;
        analyzer.analyze(&mid, a, looped, &mut fm);
        // A second look `hs` earlier gives each bin's instantaneous frequency.
        analyzer.analyze(&mid, a - hs as isize, looped, &mut fm_prev);
        for k in 0..bins {
            let (r, i) = (fm.re[k] as f64, fm.im[k] as f64);
            mag[k] = (r * r + i * i).sqrt();
            phase[k] = i.atan2(r);
        }
        if m == 0 {
            synth.copy_from_slice(&phase);
        } else {
            // Peaks advance at their own frequency; the bins around them follow.
            peaks.clear();
            for k in 0..bins {
                let lo = k.saturating_sub(2);
                let hi = (k + 3).min(bins);
                if (lo..hi).all(|j| j == k || mag[j] < mag[k] || (j > k && mag[j] <= mag[k])) {
                    peaks.push(k);
                }
            }
            if peaks.is_empty() {
                peaks.push(0);
            }
            let mut p = 0;
            for (k, owner) in peak_of.iter_mut().enumerate() {
                while p + 1 < peaks.len()
                    && (peaks[p + 1] as isize - k as isize).abs()
                        < (peaks[p] as isize - k as isize).abs()
                {
                    p += 1;
                }
                *owner = peaks[p];
            }
            let mut advanced = synth.clone();
            for &k in &peaks {
                let omega = 2.0 * PI * k as f64 / n as f64;
                let prev = (fm_prev.im[k] as f64).atan2(fm_prev.re[k] as f64);
                let freq = omega + princarg(phase[k] - prev - omega * hs as f64) / hs as f64;
                advanced[k] = synth[k] + freq * hs as f64;
            }
            for k in 0..bins {
                let pk = peak_of[k];
                synth[k] = if pk == k {
                    advanced[k]
                } else {
                    advanced[pk] + phase[k] - phase[pk]
                };
            }
        }
        analyzer.analyze(left, a, looped, &mut fl);
        analyzer.analyze(right, a, looped, &mut fr);
        let start = m as isize * hs as isize - (n / 2) as isize;
        for (ch, f) in [(0usize, &fl), (1, &fr)] {
            // Each channel keeps its phase offset from the mid signal.
            for k in 0..bins {
                let (r, i) = (f.re[k] as f64, f.im[k] as f64);
                let amp = (r * r + i * i).sqrt();
                let theta = synth[k] + i.atan2(r) - phase[k];
                re[k] = (amp * theta.cos()) as f32;
                im[k] = (amp * theta.sin()) as f32;
            }
            im[0] = 0.0;
            im[n / 2] = 0.0;
            for k in 1..n / 2 {
                re[n - k] = re[k];
                im[n - k] = -im[k];
            }
            ifft.inverse(&mut re, &mut im);
            let out = if ch == 0 { &mut out_l } else { &mut out_r };
            for (i, &w) in window.iter().enumerate() {
                let o = start + i as isize;
                if o < 0 || o as usize >= out.len() {
                    continue;
                }
                out[o as usize] += re[i] * w;
                if ch == 0 {
                    norm[o as usize] += w * w;
                }
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tuner::Tuner;

    const SR: f32 = 48_000.0;

    fn sine(freq: f32, len: usize) -> Vec<f32> {
        (0..len)
            .map(|i| 0.5 * (i as f32 * freq * std::f32::consts::TAU / SR).sin())
            .collect()
    }

    /// Median YIN pitch over the middle of a signal.
    fn pitch_of(x: &[f32]) -> f32 {
        let mut tuner = Tuner::new(SR, 2048);
        let mut found = Vec::new();
        let (a, b) = (x.len() / 5, x.len() * 4 / 5);
        for chunk in x[a..b].chunks(1024) {
            tuner.push(chunk);
            if let Some(p) = tuner.detect() {
                found.push(p.hz);
            }
        }
        assert!(!found.is_empty(), "no pitch found");
        found.sort_by(f32::total_cmp);
        found[found.len() / 2]
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    fn linear(ratio: f64) -> impl Fn(f64) -> f64 {
        move |k| k * ratio
    }

    #[test]
    fn beats_and_tones_keep_the_pitch() {
        let x = sine(440.0, 96_000);
        for algo in [WarpAlgo::Beats, WarpAlgo::Tones] {
            for ratio in [0.7, 1.3] {
                let out_len = (x.len() as f64 / ratio) as usize;
                let (l, r) = warp_stereo(&x, &x, SR, out_len, &linear(ratio), false, algo, 1.0);
                assert_eq!(l.len(), out_len);
                assert_eq!(l, r);
                let hz = pitch_of(&l);
                assert!(
                    (hz / 440.0 - 1.0).abs() < 0.01,
                    "{algo:?} ×{ratio}: {hz} Hz"
                );
                let level = rms(&l[out_len / 5..out_len * 4 / 5]) / rms(&x);
                assert!(
                    (0.8..1.15).contains(&level),
                    "{algo:?} ×{ratio}: level {level}"
                );
            }
        }
    }

    #[test]
    fn repitch_follows_the_speed() {
        let x = sine(440.0, 96_000);
        let (l, _) = warp_stereo(
            &x,
            &x,
            SR,
            60_000,
            &linear(1.25),
            false,
            WarpAlgo::RePitch,
            2.0,
        );
        let hz = pitch_of(&l);
        assert!((hz / 550.0 - 1.0).abs() < 0.01, "{hz}");
    }

    #[test]
    fn transpose_is_independent_of_the_speed() {
        let x = sine(330.0, 96_000);
        let up = 2f64.powf(5.0 / 12.0);
        for algo in [WarpAlgo::Beats, WarpAlgo::Tones] {
            for ratio in [0.8, 1.25] {
                let out_len = (x.len() as f64 / ratio) as usize;
                let (l, _) = warp_stereo(&x, &x, SR, out_len, &linear(ratio), false, algo, up);
                assert_eq!(l.len(), out_len);
                let hz = pitch_of(&l);
                assert!(
                    (hz / (330.0 * up as f32) - 1.0).abs() < 0.012,
                    "{algo:?} ×{ratio}: {hz} Hz"
                );
            }
        }
    }

    #[test]
    fn beats_puts_every_hit_on_its_new_place() {
        // Four clicks a quarter second apart, slowed down and sped up.
        let mut x = vec![0.0f32; 48_000];
        for k in 0..4 {
            for i in 0..600 {
                x[k * 12_000 + 1_000 + i] =
                    (1.0 - i as f32 / 600.0) * if i % 2 == 0 { 0.8 } else { -0.8 };
            }
        }
        for ratio in [0.75, 1.25] {
            let out_len = (x.len() as f64 / ratio) as usize;
            let (l, _) = warp_stereo(
                &x,
                &x,
                SR,
                out_len,
                &linear(ratio),
                false,
                WarpAlgo::Beats,
                1.0,
            );
            let a = OnsetAnalysis::new(&l, SR);
            let hits = a.pick(&l, 0.5, 64);
            assert_eq!(hits.len(), 4, "{hits:?}");
            for (k, h) in hits.iter().enumerate() {
                let want = ((k * 12_000 + 1_000) as f64 / ratio) as isize;
                assert!(
                    (h.pos as isize - want).abs() < 64,
                    "{} instead of {want}",
                    h.pos
                );
                // The attack itself is copied untouched.
                let at = want as usize;
                assert!((l[at + 2] - x[k * 12_000 + 1_002]).abs() < 0.05, "hit {k}");
            }
        }
    }

    #[test]
    fn identity_map_is_transparent() {
        let x = sine(220.0, 20_000);
        for algo in [WarpAlgo::Beats, WarpAlgo::RePitch] {
            let (l, _) = warp_stereo(&x, &x, SR, x.len(), &linear(1.0), false, algo, 1.0);
            for (a, b) in l.iter().zip(&x).skip(10).take(19_000) {
                assert!((a - b).abs() < 1e-3, "{algo:?}");
            }
        }
    }

    #[test]
    fn loops_wrap_around() {
        let x = sine(480.0, 4_800); // 48 whole periods
        let (l, _) = warp_stereo(
            &x,
            &x,
            SR,
            19_200,
            &linear(1.0),
            true,
            WarpAlgo::RePitch,
            1.0,
        );
        assert!(rms(&l[14_400..]) > 0.3);
        let (l, _) = warp_stereo(
            &x,
            &x,
            SR,
            19_200,
            &linear(1.0),
            false,
            WarpAlgo::RePitch,
            1.0,
        );
        assert!(rms(&l[14_400..]) < 1e-6);
    }

    #[test]
    fn ids_round_trip() {
        for a in WarpAlgo::ALL {
            assert_eq!(WarpAlgo::from_id(a.id()), Some(a));
        }
        assert_eq!(WarpAlgo::from_id("granular"), None);
    }
}
