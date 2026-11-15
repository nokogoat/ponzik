use crate::util::decay_coef;

/// Lissage exponentiel (un pôle) d'un paramètre vers sa cible : évite les clics
/// quand un gain ou un temps de retard change brutalement.
#[derive(Clone, Debug)]
pub struct Smoother {
    value: f32,
    target: f32,
    coef: f32,
}

impl Smoother {
    /// Lisseur instantané tant que [`Smoother::set_time`] n'a pas été appelé.
    pub fn new(value: f32) -> Self {
        Self { value, target: value, coef: 0.0 }
    }

    /// Constante de temps en secondes.
    pub fn set_time(&mut self, seconds: f32, sample_rate: f32) {
        self.coef = decay_coef(seconds, sample_rate);
    }

    pub fn set_target(&mut self, target: f32) {
        self.target = target;
    }

    /// Saute directement à la cible.
    pub fn snap(&mut self) {
        self.value = self.target;
    }

    pub fn target(&self) -> f32 {
        self.target
    }

    pub fn value(&self) -> f32 {
        self.value
    }

    #[inline]
    pub fn next_value(&mut self) -> f32 {
        self.value = self.target + (self.value - self.target) * self.coef;
        self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converges_without_overshoot() {
        let mut s = Smoother::new(0.0);
        s.set_time(0.01, 1_000.0);
        s.set_target(1.0);
        let mut prev = 0.0;
        for _ in 0..100 {
            let v = s.next_value();
            assert!(v >= prev && v <= 1.0);
            prev = v;
        }
        assert!((prev - 1.0).abs() < 1e-3);
    }
}
