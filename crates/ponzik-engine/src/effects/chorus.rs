use ponzik_dsp::delay::DelayLine;
use ponzik_dsp::lfo::{Lfo, LfoShape};
use ponzik_dsp::smooth::Smoother;
use ponzik_dsp::util::flush_denormal;

use super::{DEFAULT_SR, Values};
use crate::Effect;
use crate::param::{ParamInfo, Params, Unit};

const P_MODE: u32 = 0;
const P_RATE: u32 = 1;
const P_DEPTH: u32 = 2;
const P_FEEDBACK: u32 = 3;
const P_SPREAD: u32 = 4;
const P_MIX: u32 = 5;

static PARAMS: [ParamInfo; 6] = [
    ParamInfo::choice(P_MODE, "Mode", &["Chorus", "Flanger"], 0).group("Modulation"),
    ParamInfo::new(P_RATE, "Rate", 0.01, 10.0, 0.8, Unit::Hz).log().group("Modulation"),
    ParamInfo::new(P_DEPTH, "Depth", 0.0, 1.0, 0.5, Unit::Percent).group("Modulation"),
    ParamInfo::new(P_FEEDBACK, "Feedback", -0.95, 0.95, 0.0, Unit::Percent).group("Modulation"),
    ParamInfo::new(P_SPREAD, "Stereo Spread", 0.0, 1.0, 0.5, Unit::Percent).group("Output"),
    ParamInfo::new(P_MIX, "Mix", 0.0, 1.0, 0.5, Unit::Percent).group("Output"),
];

const MAX_MS: f32 = 25.0;

/// Retard central et excursion (ms) selon le mode.
fn sweep(flanger: bool, depth: f32) -> (f32, f32) {
    if flanger { (0.3 + 2.5 * depth, 2.5 * depth) } else { (12.0, 8.0 * depth) }
}

/// Chorus / flanger stéréo à ligne de retard modulée.
pub struct Chorus {
    values: Values<6>,
    sample_rate: f32,
    lines: [DelayLine; 2],
    lfo: Lfo,
    center: Smoother,
    swing: Smoother,
    feedback: Smoother,
    mix: Smoother,
}

impl Default for Chorus {
    fn default() -> Self {
        let mut c = Self {
            values: Values::new(&PARAMS),
            sample_rate: DEFAULT_SR,
            lines: [DelayLine::new(2), DelayLine::new(2)],
            lfo: Lfo::new(LfoShape::Sine),
            center: Smoother::new(0.0),
            swing: Smoother::new(0.0),
            feedback: Smoother::new(0.0),
            mix: Smoother::new(0.0),
        };
        c.set_sample_rate(DEFAULT_SR);
        c
    }
}

impl Chorus {
    fn update(&mut self) {
        let v = &self.values;
        let ms = self.sample_rate / 1000.0;
        let (center, swing) = sweep(v.index(P_MODE) == 1, v.get(P_DEPTH));
        self.center.set_target(center * ms);
        self.swing.set_target(swing * ms);
        self.feedback.set_target(v.get(P_FEEDBACK));
        self.mix.set_target(v.get(P_MIX));
        self.lfo.set_rate(v.get(P_RATE), self.sample_rate);
    }
}

impl Params for Chorus {
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

impl Effect for Chorus {
    fn kind(&self) -> &'static str {
        "chorus"
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        let max = (MAX_MS * 0.001 * sample_rate) as usize + 4;
        self.lines = [DelayLine::new(max), DelayLine::new(max)];
        for s in [&mut self.center, &mut self.swing, &mut self.feedback, &mut self.mix] {
            s.set_time(0.05, sample_rate);
        }
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        self.lines.iter_mut().for_each(DelayLine::clear);
        self.lfo.reset(0.0);
        for s in [&mut self.center, &mut self.swing, &mut self.feedback, &mut self.mix] {
            s.snap();
        }
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let offset = 0.5 * self.values.get(P_SPREAD);
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let center = self.center.next_value();
            let swing = self.swing.next_value();
            let fb = self.feedback.next_value();
            let mix = self.mix.next_value();
            let ml = self.lfo.value_at(0.0);
            let mr = self.lfo.value_at(offset);
            self.lfo.advance();
            let [ll, lr] = &mut self.lines;
            for (x, line, m) in [(l, ll, ml), (r, lr, mr)] {
                let wet = line.read_frac(center + swing * m);
                line.write(flush_denormal(*x + fb * wet));
                *x = *x * (1.0 - mix) + wet * mix;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::testing::*;

    #[test]
    fn static_delay_without_depth() {
        let mut c = Chorus::default();
        c.set_param(P_DEPTH, 0.0);
        c.set_param(P_MIX, 1.0);
        c.reset();
        let (l, r) = run_mono(&mut c, &impulse(2_000));
        assert!((l[576] - 1.0).abs() < 1e-5 && (r[576] - 1.0).abs() < 1e-5);
        assert!(peak(&l[..576]) < 1e-6);
    }

    #[test]
    fn flanger_is_shorter_and_spread_decorrelates() {
        let mut c = Chorus::default();
        c.set_param(P_MODE, 1.0);
        c.set_param(P_DEPTH, 0.0);
        c.set_param(P_MIX, 1.0);
        c.reset();
        let (l, _) = run_mono(&mut c, &impulse(100));
        assert!(peak(&l[10..20]) > 0.5);

        let mut c = Chorus::default();
        c.set_param(P_DEPTH, 1.0);
        c.set_param(P_SPREAD, 1.0);
        c.reset();
        let (l, r) = run_mono(&mut c, &sine(500.0, 0.5, 24_000));
        assert!(rms(&l.iter().zip(&r).map(|(a, b)| a - b).collect::<Vec<_>>()) > 0.05);
    }

    #[test]
    fn stable() {
        assert_stable_at_extremes(|| Box::new(Chorus::default()), 30.0);
    }
}
