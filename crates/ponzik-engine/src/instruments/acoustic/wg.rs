//! Waveguide building blocks shared by the modeled acoustic instruments:
//! fractional delay lines, loop filters whose phase delay is known (to keep
//! the loops in tune), resonator banks for instrument bodies, controller
//! state and the spread of the players of a section.

use std::f32::consts::{PI, TAU};

use ponzik_dsp::biquad::{Biquad, BiquadCoefs};
use ponzik_dsp::util::flush_denormal;

use crate::Controller;

/// Circular delay line read with 4-point Lagrange interpolation: its gain
/// never exceeds 1, so a loop through it stays stable, and it keeps the
/// highs much better than a linear read.
pub(crate) struct Delay {
    buf: Vec<f32>,
    mask: usize,
    pos: usize,
}

impl Delay {
    /// Room for delays up to `max` samples.
    pub fn new(max: usize) -> Self {
        let len = (max + 8).next_power_of_two();
        Self { buf: vec![0.0; len], mask: len - 1, pos: 0 }
    }

    pub fn clear(&mut self) {
        self.buf.fill(0.0);
    }

    /// Scales everything in the line (damping a string that is struck again).
    pub fn scale(&mut self, g: f32) {
        self.buf.iter_mut().for_each(|x| *x *= g);
    }

    /// Longest delay `read` serves.
    pub fn max_delay(&self) -> f32 {
        (self.mask - 3) as f32
    }

    #[inline]
    pub fn write(&mut self, x: f32) {
        self.buf[self.pos] = x;
        self.pos = (self.pos + 1) & self.mask;
    }

    #[inline]
    fn at(&self, k: usize) -> f32 {
        self.buf[(self.pos.wrapping_sub(k)) & self.mask]
    }

    /// The sample written `d` steps ago, `d` ≥ 2 (fractional).
    #[inline]
    pub fn read(&self, d: f32) -> f32 {
        let d = d.clamp(2.0, self.max_delay());
        let i = d as usize;
        // Taps at i − 1 … i + 2: the fraction sits in the middle interval.
        let t = d - i as f32 + 1.0;
        let (x0, x1, x2, x3) = (self.at(i - 1), self.at(i), self.at(i + 1), self.at(i + 2));
        let (a, b, c, e) = (t, t - 1.0, t - 2.0, t - 3.0);
        -x0 * b * c * e / 6.0 + x1 * a * c * e / 2.0 - x2 * a * b * e / 2.0 + x3 * a * b * c / 6.0
    }
}

/// One-pole lowpass with a gain: the loss filter of a waveguide loop.
/// `y = g·(1 − a)·x + a·y₁`.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Loss {
    a: f32,
    b: f32,
    y: f32,
}

impl Loss {
    /// `gain` at DC (≤ 1 for a stable loop), `pole` in [0, 1) (more: darker).
    pub fn set(&mut self, gain: f32, pole: f32) {
        self.a = pole.clamp(0.0, 0.99);
        self.b = gain * (1.0 - self.a);
    }

    pub fn reset(&mut self) {
        self.y = 0.0;
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        self.y = flush_denormal(self.b * x + self.a * self.y);
        self.y
    }

    /// Phase delay in samples at `hz`.
    pub fn delay(&self, hz: f32, sample_rate: f32) -> f32 {
        lowpass_delay(self.a, hz, sample_rate)
    }
}

/// Phase delay (samples) of `y = (1 − a)·x + a·y₁` at `hz`.
pub(crate) fn lowpass_delay(a: f32, hz: f32, sample_rate: f32) -> f32 {
    let w = (TAU * hz / sample_rate).clamp(1e-4, PI - 1e-3);
    (a * w.sin()).atan2(1.0 - a * w.cos()) / w
}

/// First-order allpass `(c + z⁻¹) / (1 + c·z⁻¹)`: with `c < 0` the highs
/// travel faster than the lows, like on a stiff string.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Allpass1 {
    pub c: f32,
    x1: f32,
    y1: f32,
}

impl Allpass1 {
    pub fn reset(&mut self) {
        self.x1 = 0.0;
        self.y1 = 0.0;
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = flush_denormal(self.c * x + self.x1 - self.c * self.y1);
        self.x1 = x;
        self.y1 = y;
        y
    }

    /// Phase delay in samples at `hz`.
    pub fn delay(&self, hz: f32, sample_rate: f32) -> f32 {
        let w = (TAU * hz / sample_rate).clamp(1e-4, PI - 1e-3);
        1.0 - 2.0 * (self.c * w.sin()).atan2(1.0 + self.c * w.cos()) / w
    }
}

/// Body resonances: a direct path plus parallel bandpass resonators.
pub(crate) struct Body<const N: usize> {
    filters: [Biquad; N],
    gains: [f32; N],
    /// Weight of the direct (dry) path.
    pub direct: f32,
    /// Overall weight of the resonances.
    pub amount: f32,
}

impl<const N: usize> Default for Body<N> {
    fn default() -> Self {
        Self { filters: std::array::from_fn(|_| Biquad::default()), gains: [0.0; N], direct: 1.0, amount: 0.0 }
    }
}

impl<const N: usize> Body<N> {
    /// Modes `(hz, q, gain)`, scaled in frequency by `scale`; unused slots go silent.
    pub fn set(&mut self, modes: &[(f32, f32, f32)], scale: f32, sample_rate: f32) {
        for (i, (f, g)) in self.filters.iter_mut().zip(self.gains.iter_mut()).enumerate() {
            match modes.get(i) {
                Some(&(hz, q, gain)) => {
                    f.coefs = BiquadCoefs::bandpass((hz * scale).min(sample_rate * 0.45), q, sample_rate);
                    *g = gain;
                }
                None => *g = 0.0,
            }
        }
    }

    pub fn reset(&mut self) {
        self.filters.iter_mut().for_each(Biquad::reset);
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let mut y = 0.0;
        for (f, &g) in self.filters.iter_mut().zip(&self.gains) {
            if g != 0.0 {
                y += f.process(x) * g;
            }
        }
        flush_denormal(x * self.direct + y * self.amount)
    }
}

/// Small deterministic generator (xorshift), for noise and humanizing.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Rng(u32);

impl Rng {
    pub fn new(seed: u32) -> Self {
        Self(seed.max(1))
    }

    /// Uniform in [−1, 1).
    #[inline]
    pub fn next(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x >> 8) as f32 / (1u32 << 23) as f32 - 1.0
    }
}

/// Channel controllers as the acoustic instruments read them.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Controls {
    /// Pitch bend in semitones.
    pub bend: f32,
    /// Mod wheel (CC 1), [0, 1].
    pub modwheel: f32,
    /// Breath (CC 2), [0, 1].
    pub breath: f32,
    /// Expression (CC 11), [0, 1], 1 by default.
    pub expression: f32,
    /// Sustain pedal (CC 64).
    pub sustain: bool,
    /// True once CC 1 or CC 2 moved: the wheel then drives the dynamics.
    pub wheel_used: bool,
}

impl Default for Controls {
    fn default() -> Self {
        Self { bend: 0.0, modwheel: 0.0, breath: 0.0, expression: 1.0, sustain: false, wheel_used: false }
    }
}

impl Controls {
    /// Applies a controller; returns true when the sustain pedal was released.
    pub fn apply(&mut self, ctl: Controller) -> bool {
        match ctl {
            Controller::PitchBend(b) => self.bend = if b.is_finite() { b.clamp(-1.0, 1.0) * crate::PITCH_BEND_RANGE } else { 0.0 },
            Controller::Cc(cc, v) => {
                let v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
                match cc {
                    1 => {
                        self.modwheel = v;
                        self.wheel_used = true;
                    }
                    2 => {
                        self.breath = v;
                        self.wheel_used = true;
                    }
                    11 => self.expression = v,
                    64 => {
                        let was = self.sustain;
                        self.sustain = v >= 0.5;
                        return was && !self.sustain;
                    }
                    _ => {}
                }
            }
        }
        false
    }

    /// Continuous dynamics from the wheel or breath, once they are used.
    pub fn wheel_dynamics(&self) -> Option<f32> {
        self.wheel_used.then(|| self.modwheel.max(self.breath))
    }
}

/// How one player of a section differs from the written note.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Player {
    /// Detune in cents.
    pub cents: f32,
    /// Late start, in seconds.
    pub delay: f32,
    /// Pan in [−1, 1].
    pub pan: f32,
    /// Vibrato rate factor around 1.
    pub vib_rate: f32,
    /// Vibrato phase in turns.
    pub vib_phase: f32,
    /// Slow pitch drift rate (Hz) and its phase.
    pub drift_rate: f32,
    pub drift_phase: f32,
    /// Level factor.
    pub gain: f32,
}

/// Spreads `n` players: player 0 is the leader, the others are detuned by
/// up to `detune` cents, late by up to `timing` seconds and spread over
/// `width` in the stereo field.
pub(crate) fn spread_players(out: &mut [Player], n: usize, detune: f32, timing: f32, width: f32, rng: &mut Rng) {
    let n = n.clamp(1, out.len());
    for (i, p) in out.iter_mut().enumerate().take(n) {
        let side = if n == 1 { 0.0 } else { 2.0 * i as f32 / (n - 1) as f32 - 1.0 };
        let lead = i == 0 && n == 1;
        *p = Player {
            cents: if lead { 0.0 } else { rng.next() * detune },
            delay: if lead { 0.0 } else { (0.5 + 0.5 * rng.next()) * timing },
            pan: (side * width + 0.15 * rng.next() * width).clamp(-1.0, 1.0),
            vib_rate: 1.0 + 0.12 * rng.next(),
            vib_phase: 0.5 + 0.5 * rng.next(),
            drift_rate: 0.15 + 0.1 * rng.next(),
            drift_phase: 0.5 + 0.5 * rng.next(),
            gain: if lead { 1.0 } else { 0.85 + 0.15 * rng.next() },
        };
    }
}

/// Vibrato that waits, then fades in: `(delay, fade)` in seconds since the
/// note started; returns the depth factor in [0, 1].
#[inline]
pub(crate) fn vibrato_depth(age: f32, delay: f32, fade: f32) -> f32 {
    ((age - delay) / fade.max(1e-3)).clamp(0.0, 1.0)
}

#[cfg(test)]
pub(crate) mod test_util {
    use ponzik_dsp::tuner::Tuner;

    use crate::Instrument;

    /// Renders in blocks of 128 frames, like the engine; checks every sample
    /// is finite and not denormal.
    pub fn render(inst: &mut dyn Instrument, frames: usize) -> (Vec<f32>, Vec<f32>) {
        let mut l = vec![0.0; frames];
        let mut r = vec![0.0; frames];
        for (a, b) in l.chunks_mut(128).zip(r.chunks_mut(128)) {
            inst.render(a, b);
        }
        for &s in l.iter().chain(&r) {
            assert!(s.is_finite(), "not finite: {s}");
            assert!(s == 0.0 || s.abs() > 1e-30, "denormal: {s}");
        }
        (l, r)
    }

    pub fn mono(l: &[f32], r: &[f32]) -> Vec<f32> {
        l.iter().zip(r).map(|(a, b)| 0.5 * (a + b)).collect()
    }

    pub fn peak(s: &[f32]) -> f32 {
        s.iter().fold(0.0f32, |m, x| m.max(x.abs()))
    }

    pub fn rms(s: &[f32]) -> f32 {
        (s.iter().map(|x| x * x).sum::<f32>() / s.len().max(1) as f32).sqrt()
    }

    /// Median YIN pitch (Hz) over the signal, in windows of 4096 samples.
    pub fn pitch(s: &[f32], sample_rate: f32) -> Option<f32> {
        let mut t = Tuner::new(sample_rate, 4096);
        t.set_range(25.0, 4200.0);
        let mut found: Vec<f32> = Vec::new();
        for chunk in s.chunks(1024) {
            t.push(chunk);
            if let Some(p) = t.detect() {
                found.push(p.hz);
            }
        }
        if found.is_empty() {
            return None;
        }
        found.sort_by(f32::total_cmp);
        Some(found[found.len() / 2])
    }

    /// Error in cents of `hz` against MIDI note `note`.
    pub fn cents_off(hz: f32, note: f32) -> f32 {
        1_200.0 * (hz / ponzik_dsp::util::midi_to_hz(note)).log2()
    }

    /// Spectral centroid (Hz) of a frame, by DFT over a Hann window.
    pub fn centroid(s: &[f32], sample_rate: f32) -> f32 {
        let n = if s.len() >= 4096 { 4096 } else { s.len().next_power_of_two() / 2 };
        let mut re = vec![0.0f32; n];
        let mut im = vec![0.0f32; n];
        for (i, x) in s[..n].iter().enumerate() {
            let w = 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos();
            re[i] = x * w;
        }
        ponzik_dsp::fft::Fft::new(n).forward(&mut re, &mut im);
        let (mut num, mut den) = (0.0, 0.0);
        for k in 1..n / 2 {
            let m = (re[k] * re[k] + im[k] * im[k]).sqrt();
            num += m * k as f32 * sample_rate / n as f32;
            den += m;
        }
        if den > 0.0 { num / den } else { 0.0 }
    }

    /// Time (s) for the smoothed envelope to first reach `frac` of its peak.
    pub fn attack_time(s: &[f32], sample_rate: f32, frac: f32) -> f32 {
        let env = envelope(s, sample_rate);
        let top = peak(&env);
        env.iter().position(|&e| e >= frac * top).unwrap_or(env.len()) as f32 / sample_rate
    }

    /// Envelope follower (5 ms release).
    pub fn envelope(s: &[f32], sample_rate: f32) -> Vec<f32> {
        let k = (-1.0 / (0.005 * sample_rate)).exp();
        let mut e = 0.0f32;
        s.iter()
            .map(|x| {
                e = x.abs().max(e * k);
                e
            })
            .collect()
    }

    /// Asserts a voice on every few notes stays bounded at every velocity.
    pub fn sweep_stable(make: &dyn Fn() -> Box<dyn Instrument>, notes: std::ops::RangeInclusive<u8>, step: usize) {
        for n in notes.step_by(step) {
            for vel in [0.05f32, 0.5, 1.0] {
                let mut inst = make();
                inst.note_on(n, vel);
                let (l, r) = render(inst.as_mut(), 24_000);
                inst.note_off(n);
                let (l2, r2) = render(inst.as_mut(), 12_000);
                let p = peak(&l).max(peak(&r)).max(peak(&l2)).max(peak(&r2));
                assert!(p < 2.5, "note {n} vel {vel}: peak {p}");
            }
        }
    }

    #[test]
    fn delay_line_reads_fractions() {
        let mut d = super::Delay::new(64);
        for i in 0..64 {
            d.write(i as f32);
        }
        // Last written: 63 (d = 1). A ramp interpolates exactly.
        assert!((d.read(2.0) - 62.0).abs() < 1e-4);
        assert!((d.read(10.25) - 53.75).abs() < 1e-3, "{}", d.read(10.25));
    }

    #[test]
    fn loss_and_allpass_delays_are_measured() {
        let mut l = super::Loss::default();
        l.set(1.0, 0.0);
        assert!(l.delay(100.0, 48_000.0).abs() < 1e-4);
        l.set(1.0, 0.5);
        // a / (1 − a) at DC.
        assert!((l.delay(10.0, 48_000.0) - 1.0).abs() < 1e-2);
        let ap = super::Allpass1 { c: -0.3, ..Default::default() };
        assert!((ap.delay(10.0, 48_000.0) - 1.3 / 0.7).abs() < 1e-2);
    }
}
