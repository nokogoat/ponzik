use std::f32::consts::{PI, TAU};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterMode {
    LowPass,
    HighPass,
    BandPass,
    Notch,
}

/// Filtre à variables d'état en topologie TPT (Zavalishin / Simper) :
/// stable même en modulant la coupure à chaque échantillon.
#[derive(Clone, Debug)]
pub struct Svf {
    pub mode: FilterMode,
    k: f32,
    a1: f32,
    a2: f32,
    a3: f32,
    ic1eq: f32,
    ic2eq: f32,
}

impl Svf {
    pub fn new(mode: FilterMode, cutoff_hz: f32, q: f32, sample_rate: f32) -> Self {
        let mut f = Self { mode, k: 0.0, a1: 0.0, a2: 0.0, a3: 0.0, ic1eq: 0.0, ic2eq: 0.0 };
        f.set_params(cutoff_hz, q, sample_rate);
        f
    }

    pub fn set_params(&mut self, cutoff_hz: f32, q: f32, sample_rate: f32) {
        let cutoff = cutoff_hz.clamp(10.0, sample_rate * 0.49);
        let g = (PI * cutoff / sample_rate).tan();
        self.k = 1.0 / q.max(0.05);
        self.a1 = 1.0 / (1.0 + g * (g + self.k));
        self.a2 = g * self.a1;
        self.a3 = g * self.a2;
    }

    pub fn reset(&mut self) {
        self.ic1eq = 0.0;
        self.ic2eq = 0.0;
    }

    #[inline]
    pub fn process(&mut self, v0: f32) -> f32 {
        let v3 = v0 - self.ic2eq;
        let v1 = self.a1 * self.ic1eq + self.a2 * v3;
        let v2 = self.ic2eq + self.a2 * self.ic1eq + self.a3 * v3;
        self.ic1eq = 2.0 * v1 - self.ic1eq;
        self.ic2eq = 2.0 * v2 - self.ic2eq;
        match self.mode {
            FilterMode::LowPass => v2,
            FilterMode::BandPass => v1,
            FilterMode::HighPass => v0 - self.k * v1 - v2,
            FilterMode::Notch => v0 - self.k * v1,
        }
    }
}

/// Passe-bas à un pôle, sans dépassement ; le passe-haut s'en déduit.
#[derive(Clone, Debug, Default)]
pub struct OnePole {
    a: f32,
    z: f32,
}

impl OnePole {
    pub fn new(cutoff_hz: f32, sample_rate: f32) -> Self {
        let mut f = Self::default();
        f.set_cutoff(cutoff_hz, sample_rate);
        f
    }

    pub fn set_cutoff(&mut self, cutoff_hz: f32, sample_rate: f32) {
        self.a = 1.0 - (-TAU * cutoff_hz.clamp(0.1, sample_rate * 0.49) / sample_rate).exp();
    }

    /// Réglage direct du coefficient (0 = figé, 1 = transparent).
    pub fn set_coef(&mut self, a: f32) {
        self.a = a.clamp(0.0, 1.0);
    }

    pub fn reset(&mut self) {
        self.z = 0.0;
    }

    #[inline]
    pub fn lowpass(&mut self, x: f32) -> f32 {
        self.z += self.a * (x - self.z);
        self.z
    }

    #[inline]
    pub fn highpass(&mut self, x: f32) -> f32 {
        x - self.lowpass(x)
    }
}

/// Bloqueur de composante continue (passe-haut ~10 Hz).
#[derive(Clone, Debug)]
pub struct DcBlocker {
    r: f32,
    x1: f32,
    y1: f32,
}

impl DcBlocker {
    pub fn new(sample_rate: f32) -> Self {
        Self { r: 1.0 - TAU * 10.0 / sample_rate, x1: 0.0, y1: 0.0 }
    }

    pub fn reset(&mut self) {
        self.x1 = 0.0;
        self.y1 = 0.0;
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = x - self.x1 + self.r * self.y1;
        self.x1 = x;
        self.y1 = y;
        y
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::osc::{Oscillator, Waveform};

    const SR: f32 = 48_000.0;

    fn rms_through(filter: &mut Svf, hz: f32) -> f32 {
        let mut osc = Oscillator::new(Waveform::Sine);
        osc.set_frequency(hz, SR);
        for _ in 0..4_800 {
            filter.process(osc.next_sample());
        }
        let n = 9_600;
        let sum: f32 = (0..n).map(|_| filter.process(osc.next_sample()).powi(2)).sum();
        (sum / n as f32).sqrt()
    }

    #[test]
    fn lowpass_passes_lows_and_cuts_highs() {
        let mut f = Svf::new(FilterMode::LowPass, 500.0, 0.707, SR);
        let low = rms_through(&mut f, 50.0);
        f.reset();
        let high = rms_through(&mut f, 10_000.0);
        assert!(low > 0.65, "{low}");
        assert!(high < 0.01, "{high}");
    }

    #[test]
    fn highpass_cuts_lows() {
        let mut f = Svf::new(FilterMode::HighPass, 5_000.0, 0.707, SR);
        assert!(rms_through(&mut f, 100.0) < 0.01);
    }

    #[test]
    fn notch_removes_its_center() {
        let mut f = Svf::new(FilterMode::Notch, 1_000.0, 2.0, SR);
        assert!(rms_through(&mut f, 1_000.0) < 0.01);
        f.reset();
        assert!(rms_through(&mut f, 100.0) > 0.65);
    }

    #[test]
    fn one_pole_and_dc_blocker() {
        let mut lp = OnePole::new(100.0, SR);
        let mut dc = DcBlocker::new(SR);
        let (mut a, mut b) = (0.0, 0.0);
        for _ in 0..48_000 {
            a = lp.lowpass(1.0);
            b = dc.process(1.0);
        }
        assert!((a - 1.0).abs() < 1e-4);
        assert!(b.abs() < 1e-3);
    }

    #[test]
    fn stays_finite_at_extreme_settings() {
        let mut f = Svf::new(FilterMode::LowPass, 30_000.0, 50.0, SR);
        assert!(rms_through(&mut f, 20_000.0).is_finite());
    }
}
