//! Spectrogram of a stretch of audio, for display (offline, allocates).
//!
//! One FFT column per requested column, centred on its share of the range,
//! read into rows spaced logarithmically from [`LOW_HZ`] to the Nyquist
//! frequency: each row takes the strongest bin it covers, or interpolates
//! between bins when it is narrower than one (the bass rows).

use crate::fft::{RealFft, hann};

/// Frequency of the lowest row.
pub const LOW_HZ: f32 = 30.0;

/// Levels in dB relative to a full-scale sine, column after column.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Spectrogram {
    pub columns: usize,
    pub rows: usize,
    /// `data[c * rows + r]`, row 0 the lowest frequency.
    pub data: Vec<f32>,
}

impl Spectrogram {
    pub fn at(&self, column: usize, row: usize) -> f32 {
        self.data.get(column * self.rows + row).copied().unwrap_or(-120.0)
    }
}

/// Frequency at the bottom of `row` (`row` may be fractional; `rows` at the top: Nyquist).
pub fn row_hz(row: f32, rows: usize, sample_rate: f32) -> f32 {
    let top = (sample_rate * 0.5).max(LOW_HZ * 2.0);
    LOW_HZ * (top / LOW_HZ).powf(row / rows.max(1) as f32)
}

/// Spectrogram of `x[start..end]` (fractional frames) in `columns` × `rows`;
/// `fft` is a power of two (2048 suits music at 44.1 or 48 kHz).
pub fn compute(x: &[f32], sample_rate: f32, start: f64, end: f64, columns: usize, rows: usize, fft: usize) -> Spectrogram {
    let mut out = Spectrogram { columns, rows, data: vec![-120.0; columns * rows] };
    if columns == 0 || rows == 0 || x.is_empty() || end <= start {
        return out;
    }
    let mut f = RealFft::new(fft);
    let mut win = vec![0.0f32; fft];
    hann(&mut win);
    let bins = f.bins();
    let (mut re, mut im) = (vec![0.0f32; bins], vec![0.0f32; bins]);
    let mut buf = vec![0.0f32; fft];
    let mut power = vec![0.0f32; bins];
    // A full-scale sine through a Hann window peaks at N / 4 in magnitude.
    let full = (fft as f32 / 4.0).powi(2);
    let hz = sample_rate / fft as f32;
    // Row edges in (fractional) bins.
    let edges: Vec<f32> = (0..=rows).map(|r| row_hz(r as f32, rows, sample_rate) / hz).collect();
    let step = (end - start) / columns as f64;
    for c in 0..columns {
        let center = start + (c as f64 + 0.5) * step;
        let first = center.round() as isize - (fft / 2) as isize;
        for (i, b) in buf.iter_mut().enumerate() {
            let at = first + i as isize;
            *b = if at >= 0 && (at as usize) < x.len() { x[at as usize] * win[i] } else { 0.0 };
        }
        f.forward(&buf, &mut re, &mut im);
        for b in 0..bins {
            power[b] = re[b] * re[b] + im[b] * im[b];
        }
        let col = &mut out.data[c * rows..(c + 1) * rows];
        for (r, cell) in col.iter_mut().enumerate() {
            let (lo, hi) = (edges[r], edges[r + 1]);
            let p = if hi - lo < 1.0 {
                // Narrower than a bin: interpolated at the row centre.
                let m = 0.5 * (lo + hi);
                let i = (m.floor() as usize).min(bins - 2);
                let t = (m - i as f32).clamp(0.0, 1.0);
                power[i] * (1.0 - t) + power[i + 1] * t
            } else {
                let (a, b) = ((lo.round() as usize).min(bins - 1), (hi.round() as usize).clamp(lo.round() as usize + 1, bins));
                power[a..b].iter().fold(0.0f32, |m, &v| m.max(v))
            };
            *cell = 10.0 * (p / full + 1e-12).log10();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48_000.0;

    #[test]
    fn a_tone_lights_its_row_at_its_level() {
        let x: Vec<f32> = (0..48_000).map(|i| 0.5 * (std::f32::consts::TAU * 1_000.0 * i as f32 / SR).sin()).collect();
        let s = compute(&x, SR, 0.0, 48_000.0, 20, 96, 2048);
        assert_eq!(s.data.len(), 20 * 96);
        let col = 10;
        let best = (0..96).max_by(|&a, &b| s.at(col, a).total_cmp(&s.at(col, b))).unwrap();
        let (lo, hi) = (row_hz(best as f32, 96, SR), row_hz(best as f32 + 1.0, 96, SR));
        assert!(lo <= 1_030.0 && hi >= 970.0, "{lo}..{hi}");
        // −6 dB for half scale, within the window's scalloping.
        assert!((s.at(col, best) + 6.0).abs() < 1.5, "{}", s.at(col, best));
        // Far rows are quiet.
        assert!(s.at(col, 5) < -60.0 && s.at(col, 90) < -60.0);
    }

    #[test]
    fn silence_and_empty_ranges() {
        let s = compute(&[0.0; 4_096], SR, 0.0, 4_096.0, 4, 8, 1024);
        assert!(s.data.iter().all(|&d| d <= -100.0));
        assert!(compute(&[], SR, 0.0, 1.0, 4, 8, 1024).data.iter().all(|&d| d == -120.0));
        assert!((row_hz(0.0, 10, SR) - LOW_HZ).abs() < 1e-3 && (row_hz(10.0, 10, SR) - 24_000.0).abs() < 1.0);
    }
}
