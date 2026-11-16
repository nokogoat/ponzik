use ponzik_dsp::util::{db_to_gain, decay_coef};

use super::{DEFAULT_SR, KeyInput, Values};
use crate::Effect;
use crate::param::{ParamInfo, Params, Unit};

const P_THRESHOLD: u32 = 0;
const P_AMOUNT: u32 = 1;
const P_ATTACK: u32 = 2;
const P_HOLD: u32 = 3;
const P_RELEASE: u32 = 4;

static PARAMS: [ParamInfo; 5] = [
    ParamInfo::new(P_THRESHOLD, "Seuil", -60.0, 0.0, -30.0, Unit::Db).group("Ducking"),
    ParamInfo::new(P_AMOUNT, "Quantité", 0.0, 1.0, 0.8, Unit::Percent).group("Ducking"),
    ParamInfo::new(P_ATTACK, "Attaque", 0.000_1, 0.1, 0.005, Unit::Seconds).log().group("Temps"),
    ParamInfo::new(P_HOLD, "Maintien", 0.0, 1.0, 0.05, Unit::Seconds).group("Temps"),
    ParamInfo::new(P_RELEASE, "Relâche", 0.005, 2.0, 0.2, Unit::Seconds).log().group("Temps"),
];

/// Ducking par entrée latérale : le volume baisse de « Quantité » tant que la
/// piste clé dépasse le seuil (puis pendant le maintien), et remonte ensuite.
///
/// Sans piste clé, le signal passe tel quel.
pub struct Ducker {
    values: Values<5>,
    key: KeyInput,
    sample_rate: f32,
    detector: f32,
    detector_release: f32,
    hold_left: u32,
    gain: f32,
    attack: f32,
    release: f32,
}

impl Default for Ducker {
    fn default() -> Self {
        let mut d = Self {
            values: Values::new(&PARAMS),
            key: KeyInput::new(),
            sample_rate: DEFAULT_SR,
            detector: 0.0,
            detector_release: 0.0,
            hold_left: 0,
            gain: 1.0,
            attack: 0.0,
            release: 0.0,
        };
        d.set_sample_rate(DEFAULT_SR);
        d
    }
}

impl Ducker {
    /// Gain appliqué en ce moment, dans [0, 1], pour l'afficheur.
    pub fn current_gain(&self) -> f32 {
        self.gain
    }

    fn update(&mut self) {
        let sr = self.sample_rate;
        self.attack = decay_coef(self.values.get(P_ATTACK), sr);
        self.release = decay_coef(self.values.get(P_RELEASE), sr);
    }
}

impl Params for Ducker {
    fn params(&self) -> &'static [ParamInfo] {
        &PARAMS
    }

    fn get_param(&self, id: u32) -> f32 {
        self.values.get(id)
    }

    fn set_param(&mut self, id: u32, value: f32) {
        if self.values.set(&PARAMS, id, value) {
            self.update();
        }
    }
}

impl Effect for Ducker {
    fn kind(&self) -> &'static str {
        "ducker"
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.detector_release = decay_coef(0.005, sample_rate);
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        self.detector = 0.0;
        self.hold_left = 0;
        self.gain = 1.0;
    }

    fn sidechain(&mut self, left: &[f32], right: &[f32]) {
        self.key.store(left, right);
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let keyed = self.key.take();
        let threshold = db_to_gain(self.values.get(P_THRESHOLD));
        let ducked = 1.0 - self.values.get(P_AMOUNT);
        let hold = (self.values.get(P_HOLD) * self.sample_rate) as u32;
        for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
            let (kl, kr) = if keyed { self.key.at(i) } else { (0.0, 0.0) };
            self.detector = kl.abs().max(kr.abs()).max(self.detector * self.detector_release);
            let active = if self.detector >= threshold {
                self.hold_left = hold;
                true
            } else if self.hold_left > 0 {
                self.hold_left -= 1;
                true
            } else {
                false
            };
            let (target, coef) = if active { (ducked, self.attack) } else { (1.0, self.release) };
            self.gain = target + (self.gain - target) * coef;
            *l *= self.gain;
            *r *= self.gain;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::testing::*;

    fn run_keyed(d: &mut Ducker, x: &[f32], key: &[f32], blocks: usize) -> f32 {
        let mut out = 0.0;
        for _ in 0..blocks {
            let (mut l, mut r) = (x.to_vec(), x.to_vec());
            d.sidechain(key, key);
            d.process(&mut l, &mut r);
            out = peak(&l);
        }
        out
    }

    #[test]
    fn ducks_while_the_key_plays_then_recovers() {
        let mut d = Ducker::default();
        d.set_param(P_AMOUNT, 0.75);
        let x = sine(200.0, 0.5, 256);
        let ducked = run_keyed(&mut d, &x, &sine(100.0, 0.5, 256), 40);
        assert!((ducked - 0.125).abs() < 0.01, "{ducked}");
        // Clé muette : retour au plein volume après maintien et relâche.
        let back = run_keyed(&mut d, &x, &[0.0; 256], 400);
        assert!((back - 0.5).abs() < 1e-3, "{back}");
    }

    #[test]
    fn without_key_passes_through() {
        let mut d = Ducker::default();
        let x = sine(200.0, 0.9, 4_800);
        let (l, _) = run_mono(&mut d, &x);
        assert_eq!(l, x);
    }

    #[test]
    fn hold_keeps_ducking() {
        let mut d = Ducker::default();
        d.set_param(P_HOLD, 0.5);
        d.set_param(P_RELEASE, 0.005);
        let x = sine(200.0, 0.5, 256);
        run_keyed(&mut d, &x, &[0.5; 256], 20);
        // 0,2 s après la clé : toujours baissé (maintien 0,5 s).
        let held = run_keyed(&mut d, &x, &[0.0; 256], 37);
        assert!(held < 0.11, "{held}");
    }

    #[test]
    fn stable() {
        assert_stable_at_extremes(|| Box::new(Ducker::default()), 2.0);
    }
}
