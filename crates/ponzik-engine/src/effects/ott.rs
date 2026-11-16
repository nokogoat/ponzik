use ponzik_dsp::dynamics::EnvelopeFollower;
use ponzik_dsp::smooth::Smoother;
use ponzik_dsp::util::{db_to_gain, gain_to_db};

use super::{DEFAULT_SR, ThreeBand, Values};
use crate::Effect;
use crate::param::{ParamInfo, Params, Unit};

const P_DEPTH: u32 = 0;
const P_TIME: u32 = 1;
const P_UPWARD: u32 = 2;
const P_DOWNWARD: u32 = 3;
const P_LOW: u32 = 4;
const P_MID: u32 = 5;
const P_HIGH: u32 = 6;
const P_INPUT: u32 = 7;
const P_OUTPUT: u32 = 8;

static PARAMS: [ParamInfo; 9] = [
    ParamInfo::new(P_DEPTH, "Depth", 0.0, 1.0, 0.4, Unit::Percent).group("Compression"),
    ParamInfo::new(P_TIME, "Time", 0.1, 10.0, 1.0, Unit::Percent).log().group("Compression"),
    ParamInfo::new(P_UPWARD, "Upward", 0.0, 1.0, 1.0, Unit::Percent).group("Compression"),
    ParamInfo::new(P_DOWNWARD, "Downward", 0.0, 1.0, 1.0, Unit::Percent).group("Compression"),
    ParamInfo::new(P_LOW, "Low", -24.0, 24.0, 0.0, Unit::Db).group("Bands"),
    ParamInfo::new(P_MID, "Mid", -24.0, 24.0, 0.0, Unit::Db).group("Bands"),
    ParamInfo::new(P_HIGH, "High", -24.0, 24.0, 0.0, Unit::Db).group("Bands"),
    ParamInfo::new(P_INPUT, "Input", -24.0, 24.0, 0.0, Unit::Db).group("Level"),
    ParamInfo::new(P_OUTPUT, "Output", -24.0, 24.0, 0.0, Unit::Db).group("Level"),
];

const CROSSOVER_LOW: f32 = 120.0;
const CROSSOVER_HIGH: f32 = 2_500.0;
/// Temps de base par bande (graves plus lents), multipliés par « Temps ».
const ATTACK: [f32; 3] = [0.012, 0.006, 0.003];
const RELEASE: [f32; 3] = [0.2, 0.13, 0.08];
const DOWN_THRESHOLD: f32 = -24.0;
const DOWN_SLOPE: f32 = 1.0 - 1.0 / 10.0;
const UP_THRESHOLD: f32 = -30.0;
const UP_SLOPE: f32 = 1.0 - 1.0 / 3.0;
const MAX_UP: f32 = 24.0;

/// Gain (dB) d'une bande au niveau `level` dB : compression vers le bas au-dessus du
/// seuil haut, vers le haut sous le seuil bas ; le silence n'est pas remonté.
fn band_gain(level: f32, up: f32, down: f32) -> f32 {
    let lowered = -(level - DOWN_THRESHOLD).max(0.0) * DOWN_SLOPE * down;
    let floor = ((level + 78.0) / 12.0).clamp(0.0, 1.0);
    let raised = ((UP_THRESHOLD - level).max(0.0) * UP_SLOPE).min(MAX_UP) * floor * up;
    lowered + raised
}

/// Compression multibande agressive : trois bandes (Linkwitz-Riley 4, somme plate),
/// chacune comprimée vers le haut et vers le bas ; « Profondeur » dose l'effet.
pub struct Ott {
    values: Values<9>,
    sample_rate: f32,
    split: [ThreeBand; 2],
    detectors: [EnvelopeFollower; 3],
    depth: Smoother,
    band_gain: [Smoother; 3],
    input: Smoother,
    output: Smoother,
}

impl Default for Ott {
    fn default() -> Self {
        let mut o = Self {
            values: Values::new(&PARAMS),
            sample_rate: DEFAULT_SR,
            split: Default::default(),
            detectors: std::array::from_fn(|_| EnvelopeFollower::new(0.01, 0.1, DEFAULT_SR)),
            depth: Smoother::new(0.0),
            band_gain: std::array::from_fn(|_| Smoother::new(1.0)),
            input: Smoother::new(1.0),
            output: Smoother::new(1.0),
        };
        o.set_sample_rate(DEFAULT_SR);
        o
    }
}

impl Ott {
    fn update(&mut self) {
        let (v, sr) = (&self.values, self.sample_rate);
        let scale = v.get(P_TIME);
        for (i, d) in self.detectors.iter_mut().enumerate() {
            d.set_times(ATTACK[i] * scale, RELEASE[i] * scale, sr);
        }
        self.depth.set_target(v.get(P_DEPTH));
        for (s, id) in self.band_gain.iter_mut().zip([P_LOW, P_MID, P_HIGH]) {
            s.set_target(db_to_gain(v.get(id)));
        }
        self.input.set_target(db_to_gain(v.get(P_INPUT)));
        self.output.set_target(db_to_gain(v.get(P_OUTPUT)));
    }

    fn smoothers(&mut self) -> impl Iterator<Item = &mut Smoother> {
        [&mut self.depth, &mut self.input, &mut self.output].into_iter().chain(&mut self.band_gain)
    }
}

impl Params for Ott {
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

impl Effect for Ott {
    fn kind(&self) -> &'static str {
        "ott"
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.split.iter_mut().for_each(|s| s.set(CROSSOVER_LOW, CROSSOVER_HIGH, sample_rate));
        self.smoothers().for_each(|s| s.set_time(0.02, sample_rate));
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        self.split.iter_mut().for_each(ThreeBand::reset);
        self.detectors.iter_mut().for_each(EnvelopeFollower::reset);
        self.smoothers().for_each(Smoother::snap);
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let (up, down) = (self.values.get(P_UPWARD), self.values.get(P_DOWNWARD));
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let input = self.input.next_value();
            let depth = self.depth.next_value();
            let [sl, sr] = &mut self.split;
            let (bl, br) = (sl.split(*l * input), sr.split(*r * input));
            let (mut ol, mut or) = (0.0, 0.0);
            for i in 0..3 {
                let level = self.detectors[i].process(bl[i].abs().max(br[i].abs()));
                let g = db_to_gain(depth * band_gain(gain_to_db(level), up, down)) * self.band_gain[i].next_value();
                ol += bl[i] * g;
                or += br[i] * g;
            }
            let out = self.output.next_value();
            *l = ol * out;
            *r = or * out;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::testing::*;

    #[test]
    fn static_curve() {
        assert_eq!(band_gain(-27.0, 1.0, 1.0), 0.0);
        assert!((band_gain(-14.0, 1.0, 1.0) + 9.0).abs() < 1e-4);
        assert!((band_gain(-36.0, 1.0, 1.0) - 4.0).abs() < 1e-4);
        assert_eq!(band_gain(-120.0, 1.0, 1.0), 0.0);
        assert_eq!(band_gain(-36.0, 0.0, 1.0), 0.0);
    }

    #[test]
    fn transparent_at_zero_depth() {
        let mut o = Ott::default();
        o.set_param(P_DEPTH, 0.0);
        o.reset();
        let (l, r) = run_mono(&mut o, &impulse(48_000));
        let energy: f32 = l.iter().map(|s| s * s).sum();
        assert!((energy - 1.0).abs() < 1e-3, "{energy}");
        assert_eq!(l, r);
        for hz in [50.0, 500.0, 2_500.0, 8_000.0] {
            o.reset();
            let x = sine(hz, 0.5, 24_000);
            let (y, _) = run_mono(&mut o, &x);
            let ratio = rms(&y[12_000..]) / rms(&x[12_000..]);
            assert!((ratio - 1.0).abs() < 0.01, "{hz} Hz : {ratio}");
        }
    }

    #[test]
    fn full_depth_raises_quiet_and_lowers_loud() {
        let mut o = Ott::default();
        o.set_param(P_DEPTH, 1.0);
        o.reset();
        let quiet = sine(1_000.0, 0.01, 48_000);
        let (y, _) = run_mono(&mut o, &quiet);
        assert!(rms(&y[24_000..]) > 1.5 * rms(&quiet[24_000..]), "{}", rms(&y[24_000..]));
        o.reset();
        let loud = sine(1_000.0, 0.9, 48_000);
        let (y, _) = run_mono(&mut o, &loud);
        assert!(rms(&y[24_000..]) < 0.5 * rms(&loud[24_000..]), "{}", rms(&y[24_000..]));
    }

    #[test]
    fn stable() {
        assert_stable_at_extremes(|| Box::new(Ott::default()), 100.0);
    }
}
