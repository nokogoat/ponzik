use ponzik_dsp::biquad::allpass1_coef;
use ponzik_dsp::lfo::{Lfo, LfoShape};
use ponzik_dsp::smooth::Smoother;
use ponzik_dsp::util::flush_denormal;

use super::{DEFAULT_SR, Values};
use crate::Effect;
use crate::param::{ParamInfo, Params, Unit};

const P_STAGES: u32 = 0;
const P_FREQ: u32 = 1;
const P_RATE: u32 = 2;
const P_DEPTH: u32 = 3;
const P_FEEDBACK: u32 = 4;
const P_SPREAD: u32 = 5;
const P_MIX: u32 = 6;

const STAGE_COUNTS: [usize; 5] = [4, 6, 8, 10, 12];

static PARAMS: [ParamInfo; 7] = [
    ParamInfo::choice(P_STAGES, "Étages", &["4", "6", "8", "10", "12"], 1).group("Phaser"),
    ParamInfo::new(P_FREQ, "Fréquence", 100.0, 5_000.0, 700.0, Unit::Hz).log().group("Phaser"),
    ParamInfo::new(P_RATE, "Vitesse", 0.01, 10.0, 0.5, Unit::Hz).log().group("Modulation"),
    ParamInfo::new(P_DEPTH, "Profondeur", 0.0, 1.0, 0.7, Unit::Percent).group("Modulation"),
    ParamInfo::new(P_FEEDBACK, "Réinjection", -0.95, 0.95, 0.5, Unit::Percent).group("Modulation"),
    ParamInfo::new(P_SPREAD, "Écart stéréo", 0.0, 1.0, 0.25, Unit::Percent).group("Sortie"),
    ParamInfo::new(P_MIX, "Mix", 0.0, 1.0, 0.5, Unit::Percent).group("Sortie"),
];

/// Balayage de ±3 octaves à profondeur maximale.
const OCTAVES: f32 = 3.0;

#[derive(Default)]
struct Chain {
    z: [f32; 12],
    last: f32,
}

impl Chain {
    #[inline]
    fn process(&mut self, x: f32, a: f32, stages: usize, fb: f32) -> f32 {
        let mut s = x + fb * self.last;
        for z in &mut self.z[..stages] {
            let y = a * s + *z;
            *z = flush_denormal(s - a * y);
            s = y;
        }
        self.last = s;
        s
    }
}

/// Phaser stéréo à passe-tout du premier ordre.
pub struct Phaser {
    values: Values<7>,
    sample_rate: f32,
    chains: [Chain; 2],
    lfo: Lfo,
    freq: Smoother,
    depth: Smoother,
    feedback: Smoother,
    mix: Smoother,
}

impl Default for Phaser {
    fn default() -> Self {
        let mut p = Self {
            values: Values::new(&PARAMS),
            sample_rate: DEFAULT_SR,
            chains: Default::default(),
            lfo: Lfo::new(LfoShape::Sine),
            freq: Smoother::new(0.0),
            depth: Smoother::new(0.0),
            feedback: Smoother::new(0.0),
            mix: Smoother::new(0.0),
        };
        p.set_sample_rate(DEFAULT_SR);
        p
    }
}

impl Phaser {
    fn update(&mut self) {
        let v = &self.values;
        self.freq.set_target(v.get(P_FREQ).log2());
        self.depth.set_target(v.get(P_DEPTH) * OCTAVES);
        self.feedback.set_target(v.get(P_FEEDBACK));
        self.mix.set_target(v.get(P_MIX));
        self.lfo.set_rate(v.get(P_RATE), self.sample_rate);
    }
}

impl Params for Phaser {
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

impl Effect for Phaser {
    fn kind(&self) -> &'static str {
        "phaser"
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        for s in [&mut self.freq, &mut self.depth, &mut self.feedback, &mut self.mix] {
            s.set_time(0.05, sample_rate);
        }
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        self.chains = Default::default();
        self.lfo.reset(0.0);
        for s in [&mut self.freq, &mut self.depth, &mut self.feedback, &mut self.mix] {
            s.snap();
        }
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let stages = STAGE_COUNTS[self.values.index(P_STAGES).min(STAGE_COUNTS.len() - 1)];
        let offset = 0.5 * self.values.get(P_SPREAD);
        let sr = self.sample_rate;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let center = self.freq.next_value();
            let depth = self.depth.next_value();
            let fb = self.feedback.next_value();
            let mix = self.mix.next_value();
            let coef = |m: f32| allpass1_coef((center + depth * m).exp2(), sr);
            let (al, ar) = (coef(self.lfo.value_at(0.0)), coef(self.lfo.value_at(offset)));
            self.lfo.advance();
            let [cl, cr] = &mut self.chains;
            let (wl, wr) = (cl.process(*l, al, stages, fb), cr.process(*r, ar, stages, fb));
            *l = *l * (1.0 - mix) + wl * mix;
            *r = *r * (1.0 - mix) + wr * mix;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::testing::*;

    fn level(p: &mut Phaser, hz: f32) -> f32 {
        p.reset();
        let (l, _) = run_mono(p, &sine(hz, 0.5, 9_600));
        rms(&l[4_800..])
    }

    #[test]
    fn static_notch_where_phase_is_half_a_turn() {
        let mut p = Phaser::default();
        p.set_param(P_STAGES, 0.0);
        p.set_param(P_FREQ, 1_000.0);
        p.set_param(P_DEPTH, 0.0);
        p.set_param(P_FEEDBACK, 0.0);
        p.set_param(P_MIX, 0.5);
        // 4 étages à −45° chacun : tan(ω/2) = tan(22,5°) · tan(ωc/2).
        let notch = 48_000.0 / std::f32::consts::PI
            * ((std::f32::consts::PI / 8.0).tan() * (std::f32::consts::PI * 1_000.0 / 48_000.0).tan()).atan();
        assert!(level(&mut p, notch) < 0.05 * level(&mut p, 1_000.0));
    }

    #[test]
    fn stable() {
        assert_stable_at_extremes(|| Box::new(Phaser::default()), 50.0);
    }
}
