//! Enveloppe DAHDSR (delay, attack, hold, decay, sustain, release) à segments
//! courbés.
//!
//! Convention des courbes, la même pour chaque segment et pour l'affichage
//! (l'interface dessine avec [`env_segment`], donc ce qu'on voit est ce qu'on
//! entend) : une courbe **positive** bombe le segment **vers le haut**, il passe
//! au-dessus de la droite qui relie ses extrémités ; une courbe négative le creuse
//! vers le bas ; 0 donne la droite (le comportement historique, linéaire).
//! Pour une montée (attaque), courbe > 0 : départ rapide puis arrivée douce. Pour
//! une descente (déclin, relâchement), courbe > 0 : chute lente d'abord puis
//! rapide ; courbe < 0 : la chute « exponentielle » classique, rapide d'abord.

/// Paramètres d'enveloppe ; temps en secondes, `sustain` dans [0, 1], courbes
/// dans [−1, 1] (0 : linéaire, voir le module).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AdsrParams {
    pub attack: f32,
    pub decay: f32,
    pub sustain: f32,
    pub release: f32,
    /// Silence (le niveau de départ est tenu) entre la note et l'attaque.
    pub delay: f32,
    /// Temps tenu au niveau maximal après l'attaque, avant le déclin.
    pub hold: f32,
    pub attack_curve: f32,
    pub decay_curve: f32,
    pub release_curve: f32,
}

impl AdsrParams {
    /// ADSR sans délai, sans maintien, segments droits.
    pub const fn new(attack: f32, decay: f32, sustain: f32, release: f32) -> Self {
        Self { attack, decay, sustain, release, delay: 0.0, hold: 0.0, attack_curve: 0.0, decay_curve: 0.0, release_curve: 0.0 }
    }
}

impl Default for AdsrParams {
    fn default() -> Self {
        Self::new(0.005, 0.2, 1.0, 0.1)
    }
}

/// Étape d'une enveloppe. `index` donne la numérotation stable (0 à 6) des
/// sondes de l'interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum EnvStage {
    #[default]
    Idle,
    Delay,
    Attack,
    Hold,
    Decay,
    Sustain,
    Release,
}

impl EnvStage {
    pub const ALL: [EnvStage; 7] =
        [Self::Idle, Self::Delay, Self::Attack, Self::Hold, Self::Decay, Self::Sustain, Self::Release];

    /// 0 Idle, 1 Delay, 2 Attack, 3 Hold, 4 Decay, 5 Sustain, 6 Release.
    pub const fn index(self) -> u32 {
        self as u32
    }

    /// Inverse de [`index`](Self::index) ; `Idle` pour un nombre inconnu.
    pub fn from_index(i: u32) -> Self {
        Self::ALL.get(i as usize).copied().unwrap_or(Self::Idle)
    }
}

/// Forme d'un segment : progression `x` dans [0, 1] → [0, 1], croissante,
/// `env_shape(0, c) = 0`, `env_shape(1, c) = 1`, identité exacte pour `c = 0`.
/// `curve` dans [−1, 1] : positive, `env_shape(x) > x` (montée rapide puis
/// douce, courbe convexe vers le haut) ; négative, `env_shape(x) < x`.
/// Exponentielle normalisée `(e^{kx} − 1) / (e^k − 1)` avec `k = −6 · curve` ;
/// `env_shape(x, −c) = 1 − env_shape(1 − x, c)`.
#[inline]
pub fn env_shape(x: f32, curve: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    let c = if curve.is_finite() { curve.clamp(-1.0, 1.0) } else { 0.0 };
    let k = -6.0 * c;
    if k.abs() < 1e-4 {
        return x;
    }
    ((k * x).exp_m1() / k.exp_m1()).clamp(0.0, 1.0)
}

/// Niveau d'un segment qui va de `from` à `to`, à la progression `x` dans
/// [0, 1], avec la convention du module : `curve > 0` passe au-dessus de la
/// droite, que le segment monte ou descende.
#[inline]
pub fn env_segment(from: f32, to: f32, x: f32, curve: f32) -> f32 {
    let c = if to >= from { curve } else { -curve };
    from + (to - from) * env_shape(x, c)
}

/// Enveloppe DAHDSR. Un nouveau `gate_on` repart du niveau courant (pas de clic).
/// Avec délai et maintien nuls et des courbes nulles, c'est exactement
/// l'ancienne ADSR linéaire.
#[derive(Clone, Debug)]
pub struct Adsr {
    pub params: AdsrParams,
    sample_rate: f32,
    stage: EnvStage,
    level: f32,
    release_step: f32,
    /// Niveau au début de l'étape (attaque, relâchement).
    from: f32,
    /// Progression dans l'étape, [0, 1].
    pos: f32,
    /// Échantillons écoulés dans l'étape (délai, maintien).
    timer: u32,
}

impl Adsr {
    pub fn new(params: AdsrParams, sample_rate: f32) -> Self {
        Self { params, sample_rate, stage: EnvStage::Idle, level: 0.0, release_step: 0.0, from: 0.0, pos: 0.0, timer: 0 }
    }

    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
    }

    pub fn gate_on(&mut self) {
        self.from = self.level;
        self.pos = 0.0;
        self.timer = 0;
        self.stage = if self.count(self.params.delay) > 0 { EnvStage::Delay } else { EnvStage::Attack };
    }

    pub fn gate_off(&mut self) {
        if self.stage != EnvStage::Idle {
            self.stage = EnvStage::Release;
            self.from = self.level;
            self.pos = 0.0;
            self.timer = 0;
            self.release_step = self.level / self.samples(self.params.release);
        }
    }

    /// Coupe net, sans release.
    pub fn reset(&mut self) {
        self.stage = EnvStage::Idle;
        self.level = 0.0;
        self.pos = 0.0;
    }

    pub fn is_active(&self) -> bool {
        self.stage != EnvStage::Idle
    }

    pub fn is_gated(&self) -> bool {
        matches!(self.stage, EnvStage::Delay | EnvStage::Attack | EnvStage::Hold | EnvStage::Decay | EnvStage::Sustain)
    }

    pub fn stage(&self) -> EnvStage {
        self.stage
    }

    /// Progression dans l'étape minutée en cours (délai, attaque, maintien,
    /// déclin, relâchement), dans [0, 1] ; 0 au repos et en sustain.
    pub fn progress(&self) -> f32 {
        match self.stage {
            EnvStage::Delay => (self.timer as f32 / self.count(self.params.delay).max(1) as f32).min(1.0),
            EnvStage::Hold => (self.timer as f32 / self.count(self.params.hold).max(1) as f32).min(1.0),
            EnvStage::Attack | EnvStage::Decay | EnvStage::Release => self.pos.clamp(0.0, 1.0),
            EnvStage::Idle | EnvStage::Sustain => 0.0,
        }
    }

    /// Dernier niveau rendu, dans [0, 1].
    pub fn level(&self) -> f32 {
        self.level
    }

    #[inline]
    pub fn next_sample(&mut self) -> f32 {
        let p = self.params;
        match self.stage {
            EnvStage::Idle => self.level = 0.0,
            EnvStage::Delay => {
                self.timer += 1;
                if self.timer >= self.count(p.delay) {
                    self.stage = EnvStage::Attack;
                    self.from = self.level;
                    self.pos = 0.0;
                    self.timer = 0;
                }
            }
            EnvStage::Attack => {
                // L'attaque dure le temps qu'il faut pour monter de `from` à 1 à la pente nominale.
                self.advance((self.samples(p.attack) * (1.0 - self.from)).max(1.0));
                let done = if p.attack_curve == 0.0 {
                    self.level += 1.0 / self.samples(p.attack);
                    self.level >= 1.0
                } else {
                    self.level = env_segment(self.from, 1.0, self.pos, p.attack_curve);
                    self.pos >= 1.0
                };
                if done {
                    self.level = 1.0;
                    self.pos = 0.0;
                    self.timer = 0;
                    self.stage = if self.count(p.hold) > 0 { EnvStage::Hold } else { EnvStage::Decay };
                }
            }
            EnvStage::Hold => {
                self.level = 1.0;
                self.timer += 1;
                if self.timer >= self.count(p.hold) {
                    self.stage = EnvStage::Decay;
                    self.pos = 0.0;
                    self.timer = 0;
                }
            }
            EnvStage::Decay => {
                self.advance(self.samples(p.decay));
                let done = if p.decay_curve == 0.0 {
                    self.level -= (1.0 - p.sustain) / self.samples(p.decay);
                    self.level <= p.sustain
                } else {
                    self.level = env_segment(1.0, p.sustain, self.pos, p.decay_curve);
                    self.pos >= 1.0 || p.sustain >= 1.0
                };
                if done {
                    self.level = p.sustain;
                    self.stage = EnvStage::Sustain;
                }
            }
            EnvStage::Sustain => self.level = p.sustain,
            EnvStage::Release => {
                self.advance(self.samples(p.release));
                let done = if p.release_curve == 0.0 {
                    self.level -= self.release_step;
                    self.level <= 0.0
                } else {
                    self.level = env_segment(self.from, 0.0, self.pos, p.release_curve);
                    self.pos >= 1.0 || self.level <= 0.0
                };
                if done {
                    self.level = 0.0;
                    self.pos = 0.0;
                    self.stage = EnvStage::Idle;
                }
            }
        }
        self.level
    }

    /// Un échantillon de plus dans une étape de `len` échantillons.
    #[inline]
    fn advance(&mut self, len: f32) {
        self.timer = self.timer.saturating_add(1);
        self.pos = (self.timer as f32 / len).min(1.0);
    }

    #[inline]
    fn samples(&self, seconds: f32) -> f32 {
        (seconds * self.sample_rate).max(1.0)
    }

    /// Durée d'une étape tenue (délai, maintien), en échantillons entiers.
    #[inline]
    fn count(&self, seconds: f32) -> u32 {
        let n = (seconds * self.sample_rate).round();
        if n.is_nan() || n < 1.0 { 0 } else { n.min(u32::MAX as f32) as u32 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 1_000.0;
    const P: AdsrParams = AdsrParams::new(0.01, 0.01, 0.5, 0.01);

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

    /// The linear ADSR as it was before delay, hold and curves.
    struct OldAdsr {
        p: AdsrParams,
        sr: f32,
        stage: u8,
        level: f32,
        release_step: f32,
    }

    impl OldAdsr {
        fn samples(&self, s: f32) -> f32 {
            (s * self.sr).max(1.0)
        }
        fn gate_on(&mut self) {
            self.stage = 1;
        }
        fn gate_off(&mut self) {
            if self.stage != 0 {
                self.stage = 4;
                self.release_step = self.level / self.samples(self.p.release);
            }
        }
        fn next(&mut self) -> f32 {
            let p = self.p;
            match self.stage {
                0 => self.level = 0.0,
                1 => {
                    self.level += 1.0 / self.samples(p.attack);
                    if self.level >= 1.0 {
                        self.level = 1.0;
                        self.stage = 2;
                    }
                }
                2 => {
                    self.level -= (1.0 - p.sustain) / self.samples(p.decay);
                    if self.level <= p.sustain {
                        self.level = p.sustain;
                        self.stage = 3;
                    }
                }
                3 => self.level = p.sustain,
                _ => {
                    self.level -= self.release_step;
                    if self.level <= 0.0 {
                        self.level = 0.0;
                        self.stage = 0;
                    }
                }
            }
            self.level
        }
    }

    #[test]
    fn defaults_match_the_old_linear_envelope_sample_for_sample() {
        let sr = 48_000.0;
        for p in [
            AdsrParams::new(0.004, 0.2, 0.6, 0.12),
            AdsrParams::new(0.001, 0.25, 0.0, 0.2),
            AdsrParams::new(0.0, 0.0, 1.0, 0.0),
            AdsrParams::new(0.013, 0.031, 0.37, 0.077),
        ] {
            let mut new = Adsr::new(p, sr);
            let mut old = OldAdsr { p, sr, stage: 0, level: 0.0, release_step: 0.0 };
            // Gate on, off mid-decay, retrigger during the release, off in the attack, again.
            let script: [(usize, bool); 6] = [(0, true), (5_000, false), (2_000, true), (100, false), (300, true), (20_000, false)];
            for (wait, on) in script {
                for i in 0..wait {
                    assert_eq!(new.next_sample().to_bits(), old.next().to_bits(), "{p:?}, sample {i}");
                }
                if on {
                    new.gate_on();
                    old.gate_on();
                } else {
                    new.gate_off();
                    old.gate_off();
                }
            }
            for _ in 0..30_000 {
                assert_eq!(new.next_sample().to_bits(), old.next().to_bits());
            }
            assert!(!new.is_active());
        }
    }

    #[test]
    fn shape_endpoints_monotonic_and_identity() {
        for c in [-1.0, -0.7, -0.2, -1e-6, 0.0, 1e-6, 0.3, 0.8, 1.0] {
            assert_eq!(env_shape(0.0, c), 0.0, "{c}");
            assert!((env_shape(1.0, c) - 1.0).abs() < 1e-6, "{c}");
            let mut last = 0.0;
            for i in 0..=1_000 {
                let y = env_shape(i as f32 / 1_000.0, c);
                assert!((0.0..=1.0).contains(&y));
                assert!(y >= last, "{c}: not monotonic at {i}");
                last = y;
            }
        }
        for i in 0..=100 {
            let x = i as f32 / 100.0;
            assert_eq!(env_shape(x, 0.0), x);
        }
        assert_eq!(env_shape(0.5, f32::NAN), 0.5);
        assert!(env_shape(0.5, 5.0) == env_shape(0.5, 1.0));
    }

    #[test]
    fn positive_curves_lie_above_the_chord_for_every_segment() {
        for (from, to) in [(0.0, 1.0), (0.3, 1.0), (1.0, 0.5), (1.0, 0.0), (0.7, 0.0)] {
            for c in [0.1f32, 0.5, 1.0] {
                for i in 1..100 {
                    let x = i as f32 / 100.0;
                    let chord = from + (to - from) * x;
                    assert!(env_segment(from, to, x, c) > chord, "{from} → {to}, {c}, {x}");
                    assert!(env_segment(from, to, x, -c) < chord, "{from} → {to}, {}, {x}", -c);
                }
            }
        }
    }

    /// Level sampled at progress `x` of each curved segment, rendered.
    #[test]
    fn rendered_segments_follow_the_drawn_curves() {
        let mut p = AdsrParams::new(0.1, 0.1, 0.25, 0.1);
        p.attack_curve = 0.6;
        p.decay_curve = -0.5;
        p.release_curve = 0.4;
        let mut env = Adsr::new(p, SR);
        env.gate_on();
        let attack: Vec<f32> = (0..100).map(|_| env.next_sample()).collect();
        assert_eq!(env.stage(), EnvStage::Decay);
        for (i, &y) in attack.iter().enumerate() {
            assert!((y - env_segment(0.0, 1.0, (i + 1) as f32 / 100.0, 0.6)).abs() < 1e-4, "{i}");
            assert!(y >= i as f32 / 100.0);
        }
        let decay: Vec<f32> = (0..100).map(|_| env.next_sample()).collect();
        assert_eq!(env.stage(), EnvStage::Sustain);
        for (i, &y) in decay.iter().enumerate() {
            assert!((y - env_segment(1.0, 0.25, (i + 1) as f32 / 100.0, -0.5)).abs() < 1e-4, "{i}");
        }
        env.gate_off();
        let release: Vec<f32> = (0..100).map(|_| env.next_sample()).collect();
        for (i, &y) in release.iter().enumerate() {
            assert!((y - env_segment(0.25, 0.0, (i + 1) as f32 / 100.0, 0.4)).abs() < 1e-4, "{i}");
        }
        assert!(!env.is_active());
    }

    #[test]
    fn delay_and_hold_last_their_time_in_samples() {
        let mut p = AdsrParams::new(0.01, 0.01, 0.5, 0.01);
        p.delay = 0.02;
        p.hold = 0.015;
        let mut env = Adsr::new(p, SR);
        env.gate_on();
        assert_eq!(env.stage(), EnvStage::Delay);
        for i in 0..20 {
            assert_eq!(env.next_sample(), 0.0, "delay sample {i}");
            assert!(env.is_gated());
        }
        assert_eq!(env.stage(), EnvStage::Attack);
        // 10 samples of attack, the 10th reaches 1.
        for i in 0..10 {
            let y = env.next_sample();
            assert!(y > 0.0 && y <= 1.0, "{i}");
        }
        assert_eq!(env.level(), 1.0);
        assert_eq!(env.stage(), EnvStage::Hold);
        for i in 0..15 {
            assert_eq!(env.next_sample(), 1.0, "hold sample {i}");
        }
        assert_eq!(env.stage(), EnvStage::Decay);
        assert!(env.next_sample() < 1.0);
        for _ in 0..20 {
            env.next_sample();
        }
        assert_eq!(env.stage(), EnvStage::Sustain);
        assert_eq!(env.level(), 0.5);
    }

    #[test]
    fn progress_runs_through_each_timed_stage() {
        let mut p = AdsrParams::new(0.1, 0.1, 0.5, 0.1);
        p.delay = 0.1;
        p.hold = 0.1;
        p.attack_curve = 0.5;
        let mut env = Adsr::new(p, SR);
        assert_eq!((env.stage(), env.progress()), (EnvStage::Idle, 0.0));
        env.gate_on();
        let mut seen = Vec::new();
        for _ in 0..600 {
            env.next_sample();
            let (s, x) = (env.stage(), env.progress());
            assert!((0.0..=1.0).contains(&x), "{s:?} {x}");
            if seen.last() != Some(&s) {
                seen.push(s);
            }
            if s == EnvStage::Sustain {
                break;
            }
        }
        assert_eq!(seen, [EnvStage::Delay, EnvStage::Attack, EnvStage::Hold, EnvStage::Decay, EnvStage::Sustain]);
        env.gate_off();
        env.next_sample();
        assert_eq!(env.stage(), EnvStage::Release);
        assert!((env.progress() - 0.01).abs() < 1e-4);
        assert_eq!(EnvStage::from_index(EnvStage::Release.index()), EnvStage::Release);
        assert_eq!(EnvStage::from_index(99), EnvStage::Idle);
    }

    #[test]
    fn extreme_settings_stay_in_bounds() {
        for c in [-1.0, 1.0, f32::NAN, 1e9, -1e9] {
            for (d, h) in [(0.0, 0.0), (2.0, 2.0), (-1.0, f32::NAN)] {
                let mut p = AdsrParams::new(0.0001, 0.0001, 0.3, 0.0001);
                p.delay = d;
                p.hold = h;
                p.attack_curve = c;
                p.decay_curve = c;
                p.release_curve = c;
                let mut env = Adsr::new(p, 48_000.0);
                for round in 0..3 {
                    env.gate_on();
                    for _ in 0..(200_000 * round + 500) {
                        let y = env.next_sample();
                        assert!(y.is_finite() && (0.0..=1.0).contains(&y), "{c} {d} {h}: {y}");
                    }
                    env.gate_off();
                    for _ in 0..1_000 {
                        let y = env.next_sample();
                        assert!(y.is_finite() && (0.0..=1.0).contains(&y));
                    }
                }
                assert!(!env.is_active(), "{c} {d} {h}");
            }
        }
    }
}
