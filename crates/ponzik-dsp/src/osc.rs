use std::f32::consts::TAU;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Waveform {
    Sine,
    Saw,
    Square,
    Triangle,
}

/// Oscillateur à phase normalisée, anti-aliasé par PolyBLEP (dent de scie, carré).
#[derive(Clone, Debug)]
pub struct Oscillator {
    pub waveform: Waveform,
    phase: f32,
    inc: f32,
}

impl Oscillator {
    pub fn new(waveform: Waveform) -> Self {
        Self { waveform, phase: 0.0, inc: 0.0 }
    }

    pub fn set_frequency(&mut self, hz: f32, sample_rate: f32) {
        self.inc = (hz / sample_rate).clamp(0.0, 0.5);
    }

    /// Repositionne la phase (dans [0, 1)).
    pub fn reset(&mut self, phase: f32) {
        self.phase = phase.rem_euclid(1.0);
    }

    #[inline]
    pub fn next_sample(&mut self) -> f32 {
        let t = self.phase;
        let dt = self.inc;
        let out = match self.waveform {
            Waveform::Sine => (t * TAU).sin(),
            Waveform::Saw => 2.0 * t - 1.0 - poly_blep(t, dt),
            Waveform::Square => {
                let naive = if t < 0.5 { 1.0 } else { -1.0 };
                naive + poly_blep(t, dt) - poly_blep((t + 0.5).fract(), dt)
            }
            Waveform::Triangle => 1.0 - 4.0 * (t - 0.5).abs(),
        };
        self.phase += dt;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }
        out
    }
}

/// Correction polynomiale des discontinuités (Välimäki & Huovilainen).
#[inline]
fn poly_blep(t: f32, dt: f32) -> f32 {
    if dt <= 0.0 {
        0.0
    } else if t < dt {
        let x = t / dt;
        x + x - x * x - 1.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt;
        x * x + x + x + 1.0
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48_000.0;

    fn rising_zero_crossings(osc: &mut Oscillator, samples: usize) -> usize {
        let mut prev = osc.next_sample();
        let mut count = 0;
        for _ in 1..samples {
            let v = osc.next_sample();
            if prev < 0.0 && v >= 0.0 {
                count += 1;
            }
            prev = v;
        }
        count
    }

    #[test]
    fn sine_has_expected_frequency() {
        let mut osc = Oscillator::new(Waveform::Sine);
        osc.set_frequency(440.0, SR);
        let n = rising_zero_crossings(&mut osc, SR as usize);
        assert!((439..=441).contains(&n), "{n}");
    }

    #[test]
    fn all_waveforms_are_bounded() {
        for wf in [Waveform::Sine, Waveform::Saw, Waveform::Square, Waveform::Triangle] {
            let mut osc = Oscillator::new(wf);
            osc.set_frequency(3_000.0, SR);
            for _ in 0..10_000 {
                let v = osc.next_sample();
                assert!(v.abs() <= 1.01, "{wf:?} -> {v}");
            }
        }
    }
}
