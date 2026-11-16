use std::f32::consts::PI;

use ponzik_dsp::delay::DelayLine;
use ponzik_dsp::smooth::Smoother;

use super::{DEFAULT_SR, Values};
use crate::Effect;
use crate::param::{ParamInfo, Params, Unit};

const P_SEMITONES: u32 = 0;
const P_CENTS: u32 = 1;
const P_WINDOW: u32 = 2;
const P_MIX: u32 = 3;

static PARAMS: [ParamInfo; 4] = [
    ParamInfo::new(P_SEMITONES, "Transpose", -24.0, 24.0, 0.0, Unit::Semitones).group("Pitch"),
    ParamInfo::new(P_CENTS, "Fine", -100.0, 100.0, 0.0, Unit::Cents).group("Pitch"),
    ParamInfo::new(P_WINDOW, "Window", MIN_WINDOW, MAX_WINDOW, 0.06, Unit::Seconds).log().group("Grains"),
    ParamInfo::new(P_MIX, "Mix", 0.0, 1.0, 1.0, Unit::Percent).group("Output"),
];

const MIN_WINDOW: f32 = 0.01;
const MAX_WINDOW: f32 = 0.2;

/// Transposition granulaire : deux têtes de lecture glissent dans une ligne de
/// retard à la vitesse `rapport`, décalées d'une demi-fenêtre ; chacune est
/// pondérée par une fenêtre de Hann (somme constante) et saute quand elle est muette.
#[derive(Clone, Debug)]
pub(crate) struct GrainShifter {
    line: DelayLine,
    phase: f32,
}

impl GrainShifter {
    /// Alloue : à appeler hors du thread audio.
    pub fn new(max_window: usize) -> Self {
        Self { line: DelayLine::new(max_window + 8), phase: 0.0 }
    }

    pub fn clear(&mut self) {
        self.line.clear();
        self.phase = 0.0;
    }

    /// `ratio` : rapport de fréquences ; `window` : taille des grains en échantillons.
    #[inline]
    pub fn process(&mut self, x: f32, ratio: f32, window: f32) -> f32 {
        let window = window.clamp(8.0, self.line.max_delay() as f32 - 4.0);
        self.line.write(x);
        self.phase = (self.phase + (1.0 - ratio) / window).rem_euclid(1.0);
        let mut y = 0.0;
        for offset in [0.0, 0.5] {
            let p = (self.phase + offset).fract();
            let w = (PI * p).sin();
            y += w * w * self.line.read_frac(2.0 + p * window);
        }
        y
    }
}

/// Transposeur ±24 demi-tons, latence d'environ une demi-fenêtre.
pub struct PitchShifter {
    values: Values<4>,
    sample_rate: f32,
    shifters: [GrainShifter; 2],
    ratio: f32,
    window: Smoother,
    mix: Smoother,
}

impl Default for PitchShifter {
    fn default() -> Self {
        let mut p = Self {
            values: Values::new(&PARAMS),
            sample_rate: DEFAULT_SR,
            shifters: [GrainShifter::new(8), GrainShifter::new(8)],
            ratio: 1.0,
            window: Smoother::new(0.0),
            mix: Smoother::new(1.0),
        };
        p.set_sample_rate(DEFAULT_SR);
        p
    }
}

impl PitchShifter {
    fn update(&mut self) {
        let v = &self.values;
        self.ratio = 2f32.powf((v.get(P_SEMITONES).round() + v.get(P_CENTS) / 100.0) / 12.0);
        self.window.set_target(v.get(P_WINDOW) * self.sample_rate);
        self.mix.set_target(v.get(P_MIX));
    }
}

impl Params for PitchShifter {
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

impl Effect for PitchShifter {
    fn kind(&self) -> &'static str {
        "pitch"
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        let max = (MAX_WINDOW * sample_rate) as usize;
        self.shifters = [GrainShifter::new(max), GrainShifter::new(max)];
        self.window.set_time(0.05, sample_rate);
        self.mix.set_time(0.02, sample_rate);
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        self.shifters.iter_mut().for_each(GrainShifter::clear);
        self.window.snap();
        self.mix.snap();
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let window = self.window.next_value();
            let mix = self.mix.next_value();
            let [sl, sr] = &mut self.shifters;
            let (wl, wr) = (sl.process(*l, self.ratio, window), sr.process(*r, self.ratio, window));
            *l += (wl - *l) * mix;
            *r += (wr - *r) * mix;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::testing::*;

    fn crossings(x: &[f32]) -> usize {
        x.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count()
    }

    fn shifted(semitones: f32, hz: f32) -> Vec<f32> {
        let mut p = PitchShifter::default();
        p.set_param(P_SEMITONES, semitones);
        p.reset();
        run_mono(&mut p, &sine(hz, 0.5, 2 * SR as usize)).0
    }

    #[test]
    fn octave_up_doubles_the_frequency() {
        for hz in [220.0, 440.0, 1_000.0] {
            let n = crossings(&shifted(12.0, hz)[SR as usize..]) as f32;
            assert!((n / (2.0 * hz) - 1.0).abs() < 0.05, "{hz} Hz : {n}");
        }
        let n = crossings(&shifted(-12.0, 440.0)[SR as usize..]) as f32;
        assert!((n / 220.0 - 1.0).abs() < 0.05, "{n}");
    }

    #[test]
    fn keeps_the_level() {
        let y = shifted(7.0, 440.0);
        let level = rms(&y[SR as usize..]) / rms(&sine(440.0, 0.5, 4_800));
        assert!((0.6..1.2).contains(&level), "{level}");
    }

    #[test]
    fn stable() {
        assert_stable_at_extremes(|| Box::new(PitchShifter::default()), 4.0);
    }
}
