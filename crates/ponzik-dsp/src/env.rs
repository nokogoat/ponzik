/// Paramètres d'enveloppe ; temps en secondes, `sustain` dans [0, 1].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AdsrParams {
    pub attack: f32,
    pub decay: f32,
    pub sustain: f32,
    pub release: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
}

/// Enveloppe ADSR linéaire. Un nouveau `gate_on` repart du niveau courant (pas de clic).
#[derive(Clone, Debug)]
pub struct Adsr {
    pub params: AdsrParams,
    sample_rate: f32,
    stage: Stage,
    level: f32,
    release_step: f32,
}

impl Adsr {
    pub fn new(params: AdsrParams, sample_rate: f32) -> Self {
        Self { params, sample_rate, stage: Stage::Idle, level: 0.0, release_step: 0.0 }
    }

    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
    }

    pub fn gate_on(&mut self) {
        self.stage = Stage::Attack;
    }

    pub fn gate_off(&mut self) {
        if self.stage != Stage::Idle {
            self.stage = Stage::Release;
            self.release_step = self.level / self.samples(self.params.release);
        }
    }

    /// Coupe net, sans release.
    pub fn reset(&mut self) {
        self.stage = Stage::Idle;
        self.level = 0.0;
    }

    pub fn is_active(&self) -> bool {
        self.stage != Stage::Idle
    }

    pub fn is_gated(&self) -> bool {
        matches!(self.stage, Stage::Attack | Stage::Decay | Stage::Sustain)
    }

    #[inline]
    pub fn next_sample(&mut self) -> f32 {
        let p = self.params;
        match self.stage {
            Stage::Idle => self.level = 0.0,
            Stage::Attack => {
                self.level += 1.0 / self.samples(p.attack);
                if self.level >= 1.0 {
                    self.level = 1.0;
                    self.stage = Stage::Decay;
                }
            }
            Stage::Decay => {
                self.level -= (1.0 - p.sustain) / self.samples(p.decay);
                if self.level <= p.sustain {
                    self.level = p.sustain;
                    self.stage = Stage::Sustain;
                }
            }
            Stage::Sustain => self.level = p.sustain,
            Stage::Release => {
                self.level -= self.release_step;
                if self.level <= 0.0 {
                    self.level = 0.0;
                    self.stage = Stage::Idle;
                }
            }
        }
        self.level
    }

    #[inline]
    fn samples(&self, seconds: f32) -> f32 {
        (seconds * self.sample_rate).max(1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 1_000.0;
    const P: AdsrParams = AdsrParams { attack: 0.01, decay: 0.01, sustain: 0.5, release: 0.01 };

    #[test]
    fn full_cycle() {
        let mut env = Adsr::new(P, SR);
        assert!(!env.is_active());
        env.gate_on();
        for _ in 0..10 {
            env.next_sample();
        }
        assert!((env.next_sample() - 1.0).abs() < 0.11);
        for _ in 0..20 {
            env.next_sample();
        }
        assert_eq!(env.next_sample(), 0.5);
        env.gate_off();
        for _ in 0..11 {
            env.next_sample();
        }
        assert_eq!(env.next_sample(), 0.0);
        assert!(!env.is_active());
    }

    #[test]
    fn retrigger_starts_from_current_level() {
        let mut env = Adsr::new(P, SR);
        env.gate_on();
        for _ in 0..30 {
            env.next_sample();
        }
        env.gate_off();
        env.next_sample();
        let before = env.next_sample();
        env.gate_on();
        assert!(env.next_sample() > before);
    }
}
