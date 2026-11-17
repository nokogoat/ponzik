//! Chromagram: how much of each of the twelve pitch classes a recording
//! holds, for key detection. Off the audio thread: it allocates.
//!
//! Each frame (4096 samples, Hann window, half overlap) keeps the spectral
//! peaks between [`LOW_HZ`] and [`HIGH_HZ`]; a peak's frequency, refined by
//! parabolic interpolation, gives its pitch class, and its log-compressed
//! magnitude adds to that class. Frames far below the loudest are skipped.

use crate::fft::{RealFft, hann};

const FRAME: usize = 4096;
const HOP: usize = FRAME / 2;
/// Lowest frequency heard: under it, bins are too wide to tell semitones apart.
pub const LOW_HZ: f32 = 60.0;
/// Highest frequency heard: above it, harmonics blur the classes.
pub const HIGH_HZ: f32 = 5_000.0;
/// Peaks this far under the frame's strongest (dB) are ignored.
const PEAK_RANGE_DB: f32 = 40.0;
/// Frames this far under the loudest frame (dB) are silence.
const GATE_DB: f32 = 50.0;

/// Pitch class (0 = C) of a frequency, A at 440 Hz.
pub fn pitch_class(hz: f32) -> usize {
    let note = 69.0 + 12.0 * (hz / 440.0).log2();
    (note.round() as i64).rem_euclid(12) as usize
}

/// Chromagram of a mono signal, normalized so that its largest class is 1
/// (all zeros for silence).
pub fn chromagram(signal: &[f32], sample_rate: f32) -> [f32; 12] {
    let mut out = [0.0f32; 12];
    if signal.is_empty() || sample_rate <= 0.0 {
        return out;
    }
    let mut fft = RealFft::new(FRAME);
    let mut window = vec![0.0; FRAME];
    hann(&mut window);
    let bins = fft.bins();
    let (mut re, mut im) = (vec![0.0; bins], vec![0.0; bins]);
    let mut buf = vec![0.0f32; FRAME];
    let mut mag = vec![0.0f32; bins];
    let bin_hz = sample_rate / FRAME as f32;
    let lo = ((LOW_HZ / bin_hz).floor() as usize).max(1);
    let hi = ((HIGH_HZ / bin_hz).ceil() as usize).min(bins - 2);
    // Frame levels first, for the gate.
    let starts: Vec<usize> = (0..signal.len().max(1)).step_by(HOP).collect();
    let rms: Vec<f32> = starts
        .iter()
        .map(|&s| {
            let part = &signal[s..(s + FRAME).min(signal.len())];
            (part.iter().map(|x| x * x).sum::<f32>() / FRAME as f32).sqrt()
        })
        .collect();
    let loudest = rms.iter().copied().fold(0.0f32, f32::max);
    if loudest <= 1e-9 {
        return out;
    }
    let gate = loudest * 10f32.powf(-GATE_DB / 20.0);
    for (&s, &level) in starts.iter().zip(&rms) {
        if level < gate {
            continue;
        }
        for (i, b) in buf.iter_mut().enumerate() {
            *b = signal.get(s + i).copied().unwrap_or(0.0) * window[i];
        }
        fft.forward(&buf, &mut re, &mut im);
        for k in 0..bins {
            mag[k] = (re[k] * re[k] + im[k] * im[k]).sqrt();
        }
        let top = mag[lo..=hi].iter().copied().fold(0.0f32, f32::max);
        if top <= 0.0 {
            continue;
        }
        let floor = top * 10f32.powf(-PEAK_RANGE_DB / 20.0);
        for k in lo..=hi {
            let (a, b, c) = (mag[k - 1], mag[k], mag[k + 1]);
            if b < floor || b < a || b <= c {
                continue;
            }
            // Parabolic interpolation of the peak, in log magnitude.
            let (la, lb, lc) = ((a + 1e-12).ln(), (b + 1e-12).ln(), (c + 1e-12).ln());
            let d = la - 2.0 * lb + lc;
            let shift = if d.abs() > 1e-12 { (0.5 * (la - lc) / d).clamp(-0.5, 0.5) } else { 0.0 };
            let hz = (k as f32 + shift) * bin_hz;
            out[pitch_class(hz)] += (1.0 + b / floor).ln();
        }
    }
    let max = out.iter().copied().fold(0.0f32, f32::max);
    if max > 0.0 {
        out.iter_mut().for_each(|v| *v /= max);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(notes: &[u8], secs: f32, sr: f32) -> Vec<f32> {
        let n = (secs * sr) as usize;
        (0..n)
            .map(|i| {
                let t = i as f32 / sr;
                notes.iter().map(|&k| (std::f32::consts::TAU * 440.0 * ((k as f32 - 69.0) / 12.0).exp2() * t).sin()).sum::<f32>() * 0.2
            })
            .collect()
    }

    #[test]
    fn classes_of_tones() {
        assert_eq!(pitch_class(440.0), 9);
        assert_eq!(pitch_class(261.63), 0);
        assert_eq!(pitch_class(466.16), 10);
        let sr = 44_100.0;
        let c = chromagram(&tone(&[60, 64, 67], 1.0, sr), sr);
        let mut order: Vec<usize> = (0..12).collect();
        order.sort_by(|&a, &b| c[b].total_cmp(&c[a]));
        let mut top: Vec<usize> = order[..3].to_vec();
        top.sort();
        assert_eq!(top, [0, 4, 7], "{c:?}");
        assert!(c[1] < 0.3 && c[6] < 0.3, "{c:?}");
        assert_eq!(chromagram(&vec![0.0; 10_000], sr), [0.0; 12]);
        assert_eq!(chromagram(&[], sr), [0.0; 12]);
    }
}
