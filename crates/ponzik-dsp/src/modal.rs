//! Modal synthesis: a struck or plucked object as a sum of exponentially
//! decaying resonances (bars, tines, reeds, tubes, plates).
//!
//! A [`Mode`] is a two-pole resonator tuned to one vibration mode: fed an
//! impulse it rings as a decaying sinusoid of unit amplitude, fed a short
//! contact pulse (a mallet, a hammer) it rings with the pulse's spectrum at
//! its frequency — a long soft contact barely excites the high modes, a hard
//! short one rings them all. Nothing allocates.

use std::f32::consts::{PI, TAU};

/// Frequency ratios of the transverse modes of an ideal clamped-free beam
/// (cantilever: tines, reeds, kalimba tines), relative to the first.
pub const CLAMPED_FREE: [f32; 4] = [1.0, 6.267, 17.548, 34.387];

/// Frequency ratios of the transverse modes of an ideal free-free beam
/// (untuned bar: glockenspiel, celesta), relative to the first.
pub const FREE_FREE: [f32; 5] = [1.0, 2.756, 5.404, 8.933, 13.344];

/// Two-pole resonator ringing at one frequency with a given decay.
#[derive(Clone, Copy, Debug, Default)]
pub struct Mode {
    a1: f32,
    a2: f32,
    /// Input gain: unit ringing amplitude for a unit impulse.
    b0: f32,
    y1: f32,
    y2: f32,
}

impl Mode {
    /// Tunes the mode to `hz`, ringing down by 60 dB in `t60` seconds. Above
    /// 0.48 × the sample rate the mode is muted (no aliased ringing). Keeps
    /// the current motion, so a retune glides.
    pub fn tune(&mut self, hz: f32, t60: f32, sample_rate: f32) {
        let w = TAU * hz / sample_rate;
        if !(w > 0.0 && w < PI * 0.96) {
            self.a1 = 0.0;
            self.a2 = 0.0;
            self.b0 = 0.0;
            return;
        }
        let r = pole_radius(t60, sample_rate);
        self.a1 = 2.0 * r * w.cos();
        self.a2 = r * r;
        self.b0 = w.sin();
    }

    /// Changes only the decay (the frequency stays): damping a ringing mode.
    pub fn set_decay(&mut self, t60: f32, sample_rate: f32) {
        let old = self.a2.sqrt();
        if old <= 0.0 {
            return;
        }
        let r = pole_radius(t60, sample_rate);
        self.a1 *= r / old;
        self.a2 = r * r;
    }

    /// True when muted by [`Mode::tune`] (frequency out of range).
    pub fn is_muted(&self) -> bool {
        self.b0 == 0.0
    }

    pub fn reset(&mut self) {
        self.y1 = 0.0;
        self.y2 = 0.0;
    }

    /// One sample: `x` is the force on the mode, the result its displacement.
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.a1 * self.y1 - self.a2 * self.y2 + self.b0 * x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }

    /// Rough level of the motion (upper bound of the ringing amplitude, up
    /// to a small factor): for detecting silence.
    #[inline]
    pub fn energy(&self) -> f32 {
        self.y1.abs() + self.y2.abs()
    }

    /// Clears tiny states that would otherwise decay into denormals.
    #[inline]
    pub fn flush(&mut self) {
        if self.energy() < 1e-12 {
            self.reset();
        }
    }
}

/// Pole radius of a resonance decaying by 60 dB in `t60` seconds.
#[inline]
pub fn pole_radius(t60: f32, sample_rate: f32) -> f32 {
    // 60 dB = ln(1000) nepers.
    (-6.907_755 / (t60.max(1e-4) * sample_rate)).exp()
}

/// Contact force of a mallet or hammer: a raised-cosine pulse whose length
/// sets the spectrum (its first spectral null is at 2 / length): a soft
/// mallet stays in contact for milliseconds, a hard one for a fraction.
#[derive(Clone, Copy, Debug, Default)]
pub struct Strike {
    phase: f32,
    inc: f32,
    gain: f32,
}

impl Strike {
    /// Starts a hit of `amplitude` lasting `contact` seconds. The pulse area
    /// is independent of its length: a soft mallet is not louder in the bass,
    /// only duller.
    pub fn hit(&mut self, amplitude: f32, contact: f32, sample_rate: f32) {
        let n = (contact * sample_rate).max(1.0);
        self.phase = 0.0;
        self.inc = 1.0 / n;
        // A raised cosine of n samples has an area of n / 2 (× gain): unit
        // area, so a mode far below 1 / contact rings at `amplitude`.
        self.gain = amplitude * 2.0 / n;
    }

    pub fn is_active(&self) -> bool {
        self.inc > 0.0
    }

    /// Next sample of the force (0 once the contact is over).
    #[inline]
    pub fn next_sample(&mut self) -> f32 {
        if self.inc <= 0.0 {
            return 0.0;
        }
        let x = 0.5 * (1.0 - (TAU * self.phase).cos()) * self.gain;
        self.phase += self.inc;
        if self.phase >= 1.0 {
            self.inc = 0.0;
        }
        x
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48_000.0;

    fn rising(s: &[f32]) -> usize {
        s.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count()
    }

    #[test]
    fn rings_at_its_frequency_with_unit_amplitude() {
        let mut m = Mode::default();
        m.tune(440.0, 2.0, SR);
        let out: Vec<f32> = (0..SR as usize).map(|i| m.process(if i == 0 { 1.0 } else { 0.0 })).collect();
        let peak = out[..200].iter().fold(0.0f32, |a, x| a.max(x.abs()));
        assert!((peak - 1.0).abs() < 0.02, "{peak}");
        let n = rising(&out);
        assert!((439..=441).contains(&n), "{n}");
    }

    #[test]
    fn decays_by_sixty_db_in_t60() {
        let mut m = Mode::default();
        m.tune(300.0, 0.5, SR);
        let out: Vec<f32> = (0..SR as usize).map(|i| m.process(if i == 0 { 1.0 } else { 0.0 })).collect();
        let at = |t: f32| {
            let i = (t * SR) as usize;
            out[i..i + 400].iter().fold(0.0f32, |a, x| a.max(x.abs()))
        };
        let db = 20.0 * (at(0.5) / at(0.0)).log10();
        assert!((db + 60.0).abs() < 1.5, "{db}");
        // Damping keeps the frequency, shortens the ring.
        m.tune(300.0, 5.0, SR);
        m.reset();
        m.process(1.0);
        m.set_decay(0.1, SR);
        let tail: Vec<f32> = (0..4_800).map(|_| m.process(0.0)).collect();
        assert!(tail[4_000..].iter().all(|x| x.abs() < 0.01));
        assert!((rising(&tail) as i32 - 30).abs() <= 1);
    }

    #[test]
    fn contact_length_sets_the_brightness() {
        // A long contact excites a high mode far less than a short one.
        let response = |contact: f32, hz: f32| {
            let mut m = Mode::default();
            m.tune(hz, 1.0, SR);
            let mut s = Strike::default();
            s.hit(1.0, contact, SR);
            (0..4_800).map(|_| m.process(s.next_sample()).abs()).fold(0.0f32, f32::max)
        };
        let low_soft = response(0.004, 100.0);
        let low_hard = response(0.0003, 100.0);
        assert!((low_soft / low_hard - 1.0).abs() < 0.15, "{low_soft} {low_hard}");
        assert!((low_hard - 1.0).abs() < 0.05, "{low_hard}");
        let high_soft = response(0.004, 3_000.0);
        let high_hard = response(0.0003, 3_000.0);
        assert!(high_hard > 8.0 * high_soft, "{high_soft} {high_hard}");
    }

    #[test]
    fn muted_out_of_range() {
        let mut m = Mode::default();
        m.tune(30_000.0, 1.0, SR);
        assert!(m.is_muted());
        assert_eq!(m.process(1.0), 0.0);
    }
}
