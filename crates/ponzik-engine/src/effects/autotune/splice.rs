//! Classic mode: the Auto-Tune 5 era shifter. A read head moves through a
//! short delay line at the pitch ratio; when it drifts out of its window it
//! jumps by whole periods of the voice (aligned by correlation), crossfading
//! between two heads that read the same wave a period apart. Very low latency,
//! a tight, slightly buzzy hard tune; formants move with the pitch.
//!
//! Unvoiced input is read at rate 1 (no shift), the head easing back to the
//! middle of its window.

use super::frames::Frames;
use super::line::Line;

#[derive(Clone, Copy, Debug, Default)]
struct Head {
    d: f64,
    next: f64,
    fade: f64,
    fading: bool,
}

pub struct Splice {
    sample_rate: f32,
    heads: [Head; 2],
    center: f64,
    lo: f64,
    hi: f64,
    xfade: f64,
    p_max: f64,
}

impl Splice {
    pub fn new(sample_rate: f32) -> Self {
        Self { sample_rate, heads: [Head::default(); 2], center: 64.0, lo: 32.0, hi: 96.0, xfade: 16.0, p_max: 64.0 }
    }

    /// Crossfade length (samples) and the window under the lowest heads may go.
    pub fn floor(sample_rate: f32) -> f64 {
        let xfade = (0.002 * f64::from(sample_rate)).round();
        1.25 * xfade + 8.0
    }

    /// `center`: the latency (middle of the window); `p_max`: longest period.
    pub fn configure(&mut self, center: usize, p_max: f32) {
        self.xfade = (0.002 * f64::from(self.sample_rate)).round();
        self.p_max = f64::from(p_max);
        self.center = center as f64;
        self.lo = (self.center - 0.5 * self.p_max).max(Self::floor(self.sample_rate));
        self.hi = self.lo + self.p_max;
        self.reset();
    }

    pub fn reset(&mut self) {
        self.heads = [Head { d: self.center, ..Head::default() }; 2];
    }

    /// Offset within ±`reach` of `to` whose past best matches the past at `from`.
    fn align(line: &Line, ch: usize, t: f64, from: f64, to: f64, reach: f64, len: i64) -> f64 {
        let a = (t - from).round() as i64;
        let b0 = (t - to).round() as i64;
        let reach = reach.round() as i64;
        let mut best = (0i64, f32::MIN);
        for d in -reach..=reach {
            let (mut xy, mut yy) = (0.0f32, 1e-9f32);
            for k in 0..len {
                let y = line.get(ch, b0 + d - k);
                xy += y * line.get(ch, a - k);
                yy += y * y;
            }
            let v = xy / yy.sqrt();
            if v > best.1 {
                best = (d, v);
            }
        }
        to - best.0 as f64
    }

    /// Output sample of channel `ch`; `spread`: ratio on top of the correction.
    pub fn step(&mut self, ch: usize, line: &Line, frames: &Frames, now: u64, spread: f64) -> f32 {
        let t = now as f64 - 1.0;
        let h = self.heads[ch];
        let at = t - h.d;
        let f = frames.at(at).unwrap_or_default();
        let voiced = f.voiced && f.hz > 0.0;
        let rate = if voiced {
            let r = 2f64.powf(f64::from(frames.shift_at(at)) / 12.0) * spread;
            // A nudge of 0.1 % at most keeps the head near the middle.
            r * (1.0 + ((h.d - self.center) * 1e-5).clamp(-0.001, 0.001))
        } else {
            1.0 + ((h.d - self.center) * 0.002).clamp(-0.03, 0.03)
        }
        .clamp(0.4, 2.5);
        let mut h = h;
        h.d += 1.0 - rate;
        if h.fading {
            h.next += 1.0 - rate;
        }
        let hard_lo = Self::floor(self.sample_rate) - 1.25 * self.xfade + 4.0;
        if !h.fading && (h.d < self.lo || h.d > self.hi) {
            let p = if voiced { (f64::from(self.sample_rate) / f64::from(f.hz)).min(self.p_max) } else { self.p_max };
            let mut k = ((self.center - h.d) / p).round();
            if k == 0.0 {
                k = if h.d < self.lo { 1.0 } else { -1.0 };
            }
            let to = h.d + k * p;
            let len = (p.round() as i64).clamp(16, 192);
            h.next = if voiced { Self::align(line, ch, t, h.d, to, (p / 8.0).min(48.0), len) } else { to };
            h.next = h.next.clamp(hard_lo, self.hi + 0.5 * self.p_max);
            h.fade = 0.0;
            h.fading = true;
        }
        let y = if h.fading {
            let g = 0.5 - 0.5 * (std::f64::consts::PI * h.fade / self.xfade).cos();
            let y = (1.0 - g) as f32 * line.interp(ch, t - h.d.max(hard_lo)) + g as f32 * line.interp(ch, t - h.next);
            h.fade += 1.0;
            if h.fade >= self.xfade {
                h.d = h.next;
                h.fading = false;
            }
            y
        } else {
            line.interp(ch, t - h.d.max(hard_lo))
        };
        self.heads[ch] = h;
        y
    }
}
