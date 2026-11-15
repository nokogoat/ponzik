use std::f32::consts::TAU;

use crate::noise::WhiteNoise;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LfoShape {
    Sine,
    Triangle,
    Square,
    /// Échantillonné-bloqué : une valeur au hasard par cycle.
    Random,
}

/// Oscillateur basse fréquence bipolaire ([-1, 1]), non anti-aliasé.
#[derive(Clone, Debug)]
pub struct Lfo {
    pub shape: LfoShape,
    phase: f32,
    inc: f32,
    noise: WhiteNoise,
    held: f32,
}

impl Lfo {
    pub fn new(shape: LfoShape) -> Self {
        Self { shape, phase: 0.0, inc: 0.0, noise: WhiteNoise::default(), held: 0.0 }
    }

    pub fn set_rate(&mut self, hz: f32, sample_rate: f32) {
        self.inc = (hz / sample_rate).clamp(0.0, 0.5);
    }

    pub fn reset(&mut self, phase: f32) {
        self.phase = phase.rem_euclid(1.0);
    }

    /// Valeur à la phase courante décalée de `offset` cycles, sans avancer.
    #[inline]
    pub fn value_at(&self, offset: f32) -> f32 {
        let t = (self.phase + offset).rem_euclid(1.0);
        match self.shape {
            LfoShape::Sine => (t * TAU).sin(),
            LfoShape::Triangle => 4.0 * ((t - 0.25).rem_euclid(1.0) - 0.5).abs() - 1.0,
            LfoShape::Square if t < 0.5 => 1.0,
            LfoShape::Square => -1.0,
            LfoShape::Random => self.held,
        }
    }

    #[inline]
    pub fn advance(&mut self) {
        self.phase += self.inc;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
            self.held = self.noise.next_sample();
        }
    }

    #[inline]
    pub fn next_sample(&mut self) -> f32 {
        let v = self.value_at(0.0);
        self.advance();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_are_bounded_and_periodic() {
        for shape in [LfoShape::Sine, LfoShape::Triangle, LfoShape::Square, LfoShape::Random] {
            let mut lfo = Lfo::new(shape);
            lfo.set_rate(10.0, 1_000.0);
            for _ in 0..1_000 {
                assert!(lfo.next_sample().abs() <= 1.0);
            }
        }
        let lfo = Lfo::new(LfoShape::Triangle);
        assert!(lfo.value_at(0.0).abs() < 1e-6);
        assert!((lfo.value_at(0.25) - 1.0).abs() < 1e-6);
        assert!((lfo.value_at(0.75) + 1.0).abs() < 1e-6);
    }
}
