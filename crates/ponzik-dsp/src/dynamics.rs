use crate::util::decay_coef;

/// Suiveur d'enveloppe à attaque et relâche séparées, sur un signal déjà
/// redressé (`|x|` pour la crête, `x²` pour la puissance).
#[derive(Clone, Debug)]
pub struct EnvelopeFollower {
    attack: f32,
    release: f32,
    env: f32,
}

impl EnvelopeFollower {
    pub fn new(attack: f32, release: f32, sample_rate: f32) -> Self {
        let mut f = Self { attack: 0.0, release: 0.0, env: 0.0 };
        f.set_times(attack, release, sample_rate);
        f
    }

    pub fn set_times(&mut self, attack: f32, release: f32, sample_rate: f32) {
        self.attack = decay_coef(attack, sample_rate);
        self.release = decay_coef(release, sample_rate);
    }

    pub fn reset(&mut self) {
        self.env = 0.0;
    }

    pub fn value(&self) -> f32 {
        self.env
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let coef = if x > self.env { self.attack } else { self.release };
        self.env = x + (self.env - x) * coef;
        self.env
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rises_fast_and_falls_slowly() {
        let mut f = EnvelopeFollower::new(0.001, 0.1, 48_000.0);
        for _ in 0..480 {
            f.process(1.0);
        }
        assert!(f.value() > 0.99);
        for _ in 0..480 {
            f.process(0.0);
        }
        assert!(f.value() > 0.85 && f.value() < 0.95, "{}", f.value());
    }
}
