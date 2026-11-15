use std::f32::consts::{PI, TAU};

/// Coefficients normalisés (`a0 = 1`) d'un biquad, recettes RBJ (« Audio EQ Cookbook »).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BiquadCoefs {
    pub b0: f32,
    pub b1: f32,
    pub b2: f32,
    pub a1: f32,
    pub a2: f32,
}

/// `(cos w0, alpha)`, fréquence bornée sous Nyquist.
fn omega(hz: f32, q: f32, sample_rate: f32) -> (f32, f32) {
    let w0 = TAU * hz.clamp(1.0, sample_rate * 0.49) / sample_rate;
    let (sin, cos) = w0.sin_cos();
    (cos, sin / (2.0 * q.max(0.01)))
}

impl BiquadCoefs {
    pub const IDENTITY: Self = Self { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0 };

    fn normalized(b: [f32; 3], a: [f32; 3]) -> Self {
        Self { b0: b[0] / a[0], b1: b[1] / a[0], b2: b[2] / a[0], a1: a[1] / a[0], a2: a[2] / a[0] }
    }

    pub fn lowpass(hz: f32, q: f32, sample_rate: f32) -> Self {
        let (cos, alpha) = omega(hz, q, sample_rate);
        let b = (1.0 - cos) / 2.0;
        Self::normalized([b, 2.0 * b, b], [1.0 + alpha, -2.0 * cos, 1.0 - alpha])
    }

    pub fn highpass(hz: f32, q: f32, sample_rate: f32) -> Self {
        let (cos, alpha) = omega(hz, q, sample_rate);
        let b = (1.0 + cos) / 2.0;
        Self::normalized([b, -2.0 * b, b], [1.0 + alpha, -2.0 * cos, 1.0 - alpha])
    }

    /// Passe-bande à gain unitaire au centre.
    pub fn bandpass(hz: f32, q: f32, sample_rate: f32) -> Self {
        let (cos, alpha) = omega(hz, q, sample_rate);
        Self::normalized([alpha, 0.0, -alpha], [1.0 + alpha, -2.0 * cos, 1.0 - alpha])
    }

    pub fn notch(hz: f32, q: f32, sample_rate: f32) -> Self {
        let (cos, alpha) = omega(hz, q, sample_rate);
        Self::normalized([1.0, -2.0 * cos, 1.0], [1.0 + alpha, -2.0 * cos, 1.0 - alpha])
    }

    pub fn allpass(hz: f32, q: f32, sample_rate: f32) -> Self {
        let (cos, alpha) = omega(hz, q, sample_rate);
        Self::normalized([1.0 - alpha, -2.0 * cos, 1.0 + alpha], [1.0 + alpha, -2.0 * cos, 1.0 - alpha])
    }

    pub fn peaking(hz: f32, q: f32, gain_db: f32, sample_rate: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let (cos, alpha) = omega(hz, q, sample_rate);
        Self::normalized([1.0 + alpha * a, -2.0 * cos, 1.0 - alpha * a], [1.0 + alpha / a, -2.0 * cos, 1.0 - alpha / a])
    }

    /// Plateau grave ; `q = 0,707` donne la pente maximale sans bosse.
    pub fn low_shelf(hz: f32, q: f32, gain_db: f32, sample_rate: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let (cos, alpha) = omega(hz, q, sample_rate);
        let k = 2.0 * a.sqrt() * alpha;
        Self::normalized(
            [
                a * ((a + 1.0) - (a - 1.0) * cos + k),
                2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                a * ((a + 1.0) - (a - 1.0) * cos - k),
            ],
            [(a + 1.0) + (a - 1.0) * cos + k, -2.0 * ((a - 1.0) + (a + 1.0) * cos), (a + 1.0) + (a - 1.0) * cos - k],
        )
    }

    pub fn high_shelf(hz: f32, q: f32, gain_db: f32, sample_rate: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let (cos, alpha) = omega(hz, q, sample_rate);
        let k = 2.0 * a.sqrt() * alpha;
        Self::normalized(
            [
                a * ((a + 1.0) + (a - 1.0) * cos + k),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                a * ((a + 1.0) + (a - 1.0) * cos - k),
            ],
            [(a + 1.0) - (a - 1.0) * cos + k, 2.0 * ((a - 1.0) - (a + 1.0) * cos), (a + 1.0) - (a - 1.0) * cos - k],
        )
    }

    /// Gain linéaire à la fréquence `hz` (courbes de l'interface, tests).
    pub fn magnitude(&self, hz: f32, sample_rate: f32) -> f32 {
        let w = TAU * hz / sample_rate;
        let (s1, c1) = w.sin_cos();
        let (s2, c2) = (2.0 * w).sin_cos();
        let num = (self.b0 + self.b1 * c1 + self.b2 * c2).hypot(self.b1 * s1 + self.b2 * s2);
        let den = (1.0 + self.a1 * c1 + self.a2 * c2).hypot(self.a1 * s1 + self.a2 * s2);
        num / den.max(1e-12)
    }
}

/// Q des deux étages d'un Butterworth d'ordre 4.
pub const BUTTERWORTH4_Q: [f32; 2] = [0.541_196_1, 1.306_563];

/// Biquad en forme directe transposée II.
#[derive(Clone, Debug)]
pub struct Biquad {
    pub coefs: BiquadCoefs,
    z1: f32,
    z2: f32,
}

impl Default for Biquad {
    fn default() -> Self {
        Self::new(BiquadCoefs::IDENTITY)
    }
}

impl Biquad {
    pub fn new(coefs: BiquadCoefs) -> Self {
        Self { coefs, z1: 0.0, z2: 0.0 }
    }

    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let c = &self.coefs;
        let y = c.b0 * x + self.z1;
        self.z1 = c.b1 * x - c.a1 * y + self.z2;
        self.z2 = c.b2 * x - c.a2 * y;
        y
    }
}

/// Coefficient d'un passe-tout du premier ordre (`y = a·x + z ; z = x − a·y`),
/// déphasé de 90° à `hz`.
#[inline]
pub fn allpass1_coef(hz: f32, sample_rate: f32) -> f32 {
    let t = (PI * hz.clamp(1.0, sample_rate * 0.49) / sample_rate).tan();
    (t - 1.0) / (t + 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48_000.0;

    fn db(c: &BiquadCoefs, hz: f32) -> f32 {
        20.0 * c.magnitude(hz, SR).log10()
    }

    #[test]
    fn cookbook_responses() {
        let lp = BiquadCoefs::lowpass(1_000.0, 0.707, SR);
        assert!(db(&lp, 50.0).abs() < 0.1);
        assert!((db(&lp, 1_000.0) + 3.0).abs() < 0.1);
        assert!(db(&lp, 10_000.0) < -35.0);
        let hp = BiquadCoefs::highpass(1_000.0, 0.707, SR);
        assert!(db(&hp, 100.0) < -35.0);
        let pk = BiquadCoefs::peaking(2_000.0, 1.0, -12.0, SR);
        assert!((db(&pk, 2_000.0) + 12.0).abs() < 0.05);
        assert!(db(&pk, 50.0).abs() < 0.2);
        let ls = BiquadCoefs::low_shelf(200.0, 0.707, 6.0, SR);
        assert!((db(&ls, 20.0) - 6.0).abs() < 0.1);
        assert!(db(&ls, 10_000.0).abs() < 0.1);
        let hs = BiquadCoefs::high_shelf(5_000.0, 0.707, -6.0, SR);
        assert!((db(&hs, 20_000.0) + 6.0).abs() < 0.3);
        assert!(db(&hs, 100.0).abs() < 0.1);
        let nt = BiquadCoefs::notch(1_000.0, 2.0, SR);
        assert!(db(&nt, 1_000.0) < -60.0);
        let ap = BiquadCoefs::allpass(1_000.0, 0.707, SR);
        assert!(db(&ap, 3_000.0).abs() < 0.01);
    }

    #[test]
    fn filter_matches_magnitude() {
        let coefs = BiquadCoefs::peaking(1_000.0, 2.0, 9.0, SR);
        let mut f = Biquad::new(coefs);
        let w = TAU * 1_000.0 / SR;
        let mut peak = 0.0f32;
        for n in 0..9_600 {
            let y = f.process((w * n as f32).sin());
            if n > 4_800 {
                peak = peak.max(y.abs());
            }
        }
        assert!((peak - coefs.magnitude(1_000.0, SR)).abs() < 0.02, "{peak}");
    }

    #[test]
    fn allpass1_is_quarter_turn_at_cutoff() {
        assert!(allpass1_coef(SR / 4.0, SR).abs() < 1e-6);
    }
}
