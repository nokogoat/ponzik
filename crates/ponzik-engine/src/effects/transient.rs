use ponzik_dsp::dynamics::EnvelopeFollower;
use ponzik_dsp::smooth::Smoother;
use ponzik_dsp::util::{db_to_gain, gain_to_db};

use super::{DEFAULT_SR, Values};
use crate::Effect;
use crate::param::{ParamInfo, Params, Unit};

const P_ATTACK: u32 = 0;
const P_SUSTAIN: u32 = 1;
const P_OUTPUT: u32 = 2;
const P_MIX: u32 = 3;

static PARAMS: [ParamInfo; 4] = [
    ParamInfo::new(P_ATTACK, "Attaque", -1.0, 1.0, 0.0, Unit::Percent).group("Transitoires"),
    ParamInfo::new(P_SUSTAIN, "Maintien", -1.0, 1.0, 0.0, Unit::Percent).group("Transitoires"),
    ParamInfo::new(P_OUTPUT, "Sortie", -24.0, 24.0, 0.0, Unit::Db).group("Sortie"),
    ParamInfo::new(P_MIX, "Mix", 0.0, 1.0, 1.0, Unit::Percent).group("Sortie"),
];

/// dB de gain par dB d'écart entre suiveurs, à 100 %.
const SENSITIVITY: f32 = 1.5;
const MAX_BOOST: f32 = 18.0;
const MAX_CUT: f32 = -30.0;

/// Façonneur de transitoires : l'écart entre un suiveur d'enveloppe rapide et un
/// lent repère les attaques (le rapide monte d'abord) et les queues (le lent
/// redescend après), que l'on renforce ou atténue séparément.
pub struct Transient {
    values: Values<4>,
    sample_rate: f32,
    /// Enveloppe rapide, commune aux deux détecteurs.
    fast: EnvelopeFollower,
    /// Suit `fast` avec une attaque lente : en retard sur les attaques.
    slow_attack: EnvelopeFollower,
    /// Suit `fast` avec une relâche lente : en retard sur les queues.
    slow_release: EnvelopeFollower,
    attack: Smoother,
    sustain: Smoother,
    output: Smoother,
    mix: Smoother,
}

impl Default for Transient {
    fn default() -> Self {
        let follower = || EnvelopeFollower::new(0.01, 0.1, DEFAULT_SR);
        let mut t = Self {
            values: Values::new(&PARAMS),
            sample_rate: DEFAULT_SR,
            fast: follower(),
            slow_attack: follower(),
            slow_release: follower(),
            attack: Smoother::new(0.0),
            sustain: Smoother::new(0.0),
            output: Smoother::new(1.0),
            mix: Smoother::new(1.0),
        };
        t.set_sample_rate(DEFAULT_SR);
        t
    }
}

impl Transient {
    fn update(&mut self) {
        let v = &self.values;
        self.attack.set_target(v.get(P_ATTACK));
        self.sustain.set_target(v.get(P_SUSTAIN));
        self.output.set_target(db_to_gain(v.get(P_OUTPUT)));
        self.mix.set_target(v.get(P_MIX));
    }

    fn smoothers(&mut self) -> [&mut Smoother; 4] {
        [&mut self.attack, &mut self.sustain, &mut self.output, &mut self.mix]
    }
}

impl Params for Transient {
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

impl Effect for Transient {
    fn kind(&self) -> &'static str {
        "transient"
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.fast.set_times(0.000_5, 0.05, sample_rate);
        self.slow_attack.set_times(0.025, 0.05, sample_rate);
        self.slow_release.set_times(0.000_5, 0.3, sample_rate);
        self.smoothers().into_iter().for_each(|s| s.set_time(0.02, sample_rate));
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        for f in [&mut self.fast, &mut self.slow_attack, &mut self.slow_release] {
            f.reset();
        }
        self.smoothers().into_iter().for_each(Smoother::snap);
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let x = l.abs().max(r.abs());
            let fast = self.fast.process(x);
            let onset = gain_to_db(fast) - gain_to_db(self.slow_attack.process(fast));
            let tail = gain_to_db(self.slow_release.process(fast)) - gain_to_db(fast);
            let (attack, sustain) = (self.attack.next_value(), self.sustain.next_value());
            let db = SENSITIVITY * (attack * onset.max(0.0) + sustain * tail.max(0.0));
            let mix = self.mix.next_value();
            let g = (db_to_gain(db.clamp(MAX_CUT, MAX_BOOST)) * mix + 1.0 - mix) * self.output.next_value();
            *l *= g;
            *r *= g;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::testing::*;

    /// Silence puis note qui décroît : une attaque nette et une queue.
    fn hit() -> Vec<f32> {
        let mut x = vec![0.0; 4_800];
        x.extend(sine(150.0, 0.5, 24_000).iter().enumerate().map(|(n, s)| s * (-(n as f32) / 6_000.0).exp()));
        x
    }

    fn shaped(attack: f32, sustain: f32) -> Vec<f32> {
        let mut t = Transient::default();
        t.set_param(P_ATTACK, attack);
        t.set_param(P_SUSTAIN, sustain);
        t.reset();
        run_mono(&mut t, &hit()).0
    }

    #[test]
    fn neutral_by_default() {
        assert_eq!(shaped(0.0, 0.0), hit());
    }

    #[test]
    fn attack_moves_the_onset_peak() {
        let onset = |y: &[f32]| peak(&y[4_800..5_300]);
        let (dry, boosted, cut) = (onset(&hit()), onset(&shaped(1.0, 0.0)), onset(&shaped(-1.0, 0.0)));
        assert!(boosted > 1.5 * dry, "{dry} {boosted}");
        assert!(cut < 0.8 * dry, "{dry} {cut}");
        // La queue n'est presque pas touchée par l'attaque.
        let tail = |y: &[f32]| rms(&y[19_200..24_000]);
        assert!((tail(&shaped(1.0, 0.0)) / tail(&hit()) - 1.0).abs() < 0.15);
    }

    #[test]
    fn sustain_moves_the_tail() {
        let tail = |y: &[f32]| rms(&y[19_200..28_800]);
        let dry = tail(&hit());
        assert!(tail(&shaped(0.0, 1.0)) > 1.3 * dry);
        assert!(tail(&shaped(0.0, -1.0)) < 0.8 * dry);
    }

    #[test]
    fn stable() {
        assert_stable_at_extremes(|| Box::new(Transient::default()), 100.0);
    }
}
