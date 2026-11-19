//! Sympathetic resonance: the strings of every key, as cheap waveguides
//! (delay line, fractional allpass, loss filter), listening to the bridge.
//!
//! With its damper lifted (sustain pedal, key held, top keys without dampers)
//! a string picks up from the bridge whatever falls on its harmonics and rings
//! along: the halo of a pedaled piano, the octave that sings when its key is
//! held silently. With its damper down it dies within a fraction of a second,
//! and a quiet damped string is not computed at all.

use super::keys::{KEYS, LOWEST};

/// Fractional part of the delay given to the allpass, in [0.5, 1.5).
const MIN_FRACTION: f32 = 0.5;

#[derive(Clone, Copy, Debug, Default)]
struct Line {
    start: usize,
    cap: usize,
    pos: usize,
    /// Whole samples of delay.
    len: usize,
    /// Allpass (fractional delay) and its state.
    ap: f32,
    ap_x: f32,
    ap_y: f32,
    /// Loss filter (one-pole lowpass): coefficient and state.
    lp_a: f32,
    lp: f32,
    /// Loop gain per period, damper lifted and resting.
    g_free: f32,
    g_damped: f32,
    g: f32,
    damper: f32,
    /// Mean square output over the last block.
    energy: f32,
    /// Samples to keep computing after an input, until it comes out.
    wake: usize,
    pan_l: f32,
    pan_r: f32,
}

pub struct Resonance {
    buf: Vec<f32>,
    lines: [Line; KEYS],
    sample_rate: f32,
    /// Energy rendered since the last reset (tests).
    #[cfg(test)]
    pub total: f64,
}

impl Resonance {
    /// Allocates the lines for `sample_rate`, long enough for a flat tuning
    /// (A4 at 415 Hz and a full semitone of stretch).
    pub fn new(sample_rate: f32) -> Self {
        let mut lines = [Line::default(); KEYS];
        let mut start = 0;
        for (k, l) in lines.iter_mut().enumerate() {
            let hz = 415.0 * ((k as f32 + LOWEST as f32 - 69.0 - 1.0) / 12.0).exp2();
            let cap = (sample_rate / hz).ceil() as usize + 4;
            *l = Line { start, cap, len: cap - 2, lp_a: 0.5, g_free: 0.0, g_damped: 0.0, damper: 1.0, pan_l: 1.0, pan_r: 1.0, ..Line::default() };
            start += cap;
        }
        Self {
            buf: vec![0.0; start],
            lines,
            sample_rate,
            #[cfg(test)]
            total: 0.0,
        }
    }

    pub fn clear(&mut self) {
        self.buf.fill(0.0);
        for l in &mut self.lines {
            l.ap_x = 0.0;
            l.ap_y = 0.0;
            l.lp = 0.0;
            l.energy = 0.0;
            l.wake = 0;
        }
    }

    /// Tunes line `k` to `hz` with its decay times (lifted and damped) and pan.
    pub fn tune(&mut self, k: usize, hz: f32, t60: f32, damped_t60: f32, pan: (f32, f32)) {
        let sr = self.sample_rate;
        let Some(l) = self.lines.get_mut(k) else { return };
        let period = (sr / hz.max(1.0)).clamp(4.0, l.cap as f32 - 2.0);
        // Brighter loss filters up the keyboard, where periods are short.
        let octave = (hz / 27.5).max(1.0).log2();
        let a = (0.35 + 0.08 * octave).clamp(0.35, 0.95);
        let w = std::f32::consts::TAU / period;
        let (sin, cos) = w.sin_cos();
        let pole = 1.0 - a;
        let lp_delay = (pole * sin).atan2(1.0 - pole * cos) / w;
        let rest = (period - lp_delay).max(MIN_FRACTION + 2.0);
        let len = ((rest - MIN_FRACTION).floor() as usize).clamp(1, l.cap - 1);
        let d = rest - len as f32;
        l.len = len;
        if l.pos >= len {
            l.pos = 0;
        }
        l.ap = (1.0 - d) / (1.0 + d);
        l.lp_a = a;
        // Not compensated for the loss filter: its gain is 1 at DC, which
        // would then ring longer than the strings.
        let gain = |t: f32| 10f32.powf(-3.0 * period / (t.max(0.01) * sr)).min(0.9995);
        l.g_free = gain(t60);
        l.g_damped = gain(damped_t60);
        l.g = l.g_free + (l.g_damped - l.g_free) * l.damper;
        (l.pan_l, l.pan_r) = pan;
    }

    /// Damper of line `k`: 0 lifted, 1 resting.
    pub fn set_damper(&mut self, k: usize, e: f32) {
        if let Some(l) = self.lines.get_mut(k) {
            l.damper = e;
            l.g = l.g_free + (l.g_damped - l.g_free) * e;
        }
    }

    /// Adds the response of the strings to `feed` (bridge motion, scaled by
    /// `coupling`) into `left` and `right`, times `amount`.
    pub fn render(&mut self, feed: &[f32], coupling: f32, amount: f32, left: &mut [f32], right: &mut [f32]) {
        let n = feed.len().min(left.len()).min(right.len());
        if n == 0 {
            return;
        }
        let quiet_feed = feed[..n].iter().all(|x| x.abs() < 1e-7);
        let inv = 1.0 / n as f32;
        for l in &mut self.lines {
            // A damped string with nothing left is not worth computing; with
            // a silent bridge neither is a silent lifted one.
            if l.energy < 1e-14 && l.wake == 0 && (l.damper > 0.9 || quiet_feed) {
                continue;
            }
            l.wake = l.wake.saturating_sub(n);
            let line = &mut self.buf[l.start..l.start + l.cap];
            let (mut pos, len, ap, lp_a, g) = (l.pos, l.len, l.ap, l.lp_a, l.g);
            let (mut ap_x, mut ap_y, mut lp) = (l.ap_x, l.ap_y, l.lp);
            let mut energy = 0.0;
            for i in 0..n {
                let x = line[pos];
                let y = ap * (x - ap_y) + ap_x;
                ap_x = x;
                ap_y = y;
                lp += lp_a * (y - lp);
                let out = lp * g;
                line[pos] = out + feed[i] * coupling;
                pos += 1;
                if pos >= len {
                    pos = 0;
                }
                energy += out * out;
                left[i] += out * amount * l.pan_l;
                right[i] += out * amount * l.pan_r;
            }
            l.pos = pos;
            l.ap_x = flush(ap_x);
            l.ap_y = flush(ap_y);
            l.lp = flush(lp);
            l.energy = energy * inv;
            // Pulses come round once a period: stay awake over one after
            // any sound in or out.
            if l.energy >= 1e-14 || !quiet_feed {
                l.wake = len + n + 2;
            }
            #[cfg(test)]
            {
                self.total += energy as f64;
            }
        }
    }

    /// True when no string rings.
    pub fn is_quiet(&self) -> bool {
        self.lines.iter().all(|l| l.energy < 1e-12 && l.wake == 0)
    }
}

#[inline]
fn flush(x: f32) -> f32 {
    if x.abs() < 1e-20 { 0.0 } else { x }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A lifted line excited by an impulse rings at its pitch.
    #[test]
    fn line_rings_at_its_pitch() {
        let sr = 48_000.0;
        let mut r = Resonance::new(sr);
        let k = (57 - LOWEST) as usize; // A3, 220 Hz
        r.tune(k, 220.0, 5.0, 0.2, (1.0, 1.0));
        r.set_damper(k, 0.0);
        let mut feed = vec![0.0f32; 48_000];
        feed[0] = 1.0;
        let (mut l, mut rr) = (vec![0.0f32; 48_000], vec![0.0f32; 48_000]);
        for ((f, a), b) in feed.chunks(64).zip(l.chunks_mut(64)).zip(rr.chunks_mut(64)) {
            r.render(f, 1.0, 1.0, a, b);
        }
        // Period from the autocorrelation peak near 218 samples.
        let s = &l[4_800..24_000];
        let ac = |lag: usize| s.iter().zip(&s[lag..]).map(|(a, b)| a * b).sum::<f32>();
        let best = (200..240).max_by(|&a, &b| ac(a).total_cmp(&ac(b))).unwrap_or(0);
        assert!((best as f32 - sr / 220.0).abs() <= 1.0, "{best}");
        // Damped, it dies fast.
        let early: f32 = l[4_800..9_600].iter().map(|x| x * x).sum();
        r.set_damper(k, 1.0);
        let silent = vec![0.0f32; 24_000];
        let (mut l2, mut r2) = (vec![0.0f32; 24_000], vec![0.0f32; 24_000]);
        r.render(&silent[..4_800], 1.0, 1.0, &mut l2[..4_800], &mut r2[..4_800]);
        for i in 0..4 {
            let a = 4_800 + i * 4_800;
            r.render(&silent[a..a + 4_800], 1.0, 1.0, &mut l2[a..a + 4_800], &mut r2[a..a + 4_800]);
        }
        let late: f32 = l2[19_200..24_000].iter().map(|x| x * x).sum();
        assert!(late < early * 1e-3, "{early} {late}");
    }
}
