/// Bruit blanc rapide et déterministe (xorshift32).
#[derive(Clone, Debug)]
pub struct WhiteNoise {
    state: u32,
}

impl WhiteNoise {
    pub fn new(seed: u32) -> Self {
        Self { state: seed.max(1) }
    }

    /// Échantillon uniforme dans [-1, 1].
    #[inline]
    pub fn next_sample(&mut self) -> f32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.state = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

impl Default for WhiteNoise {
    fn default() -> Self {
        Self::new(0x2545_F491)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_and_roughly_centered() {
        let mut n = WhiteNoise::default();
        let mut sum = 0.0f64;
        for _ in 0..100_000 {
            let v = n.next_sample();
            assert!((-1.0..=1.0).contains(&v));
            sum += v as f64;
        }
        assert!((sum / 100_000.0).abs() < 0.02);
    }
}
