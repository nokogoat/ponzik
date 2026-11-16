use ponzik_dsp::biquad::{BUTTERWORTH4_Q, Biquad, BiquadCoefs};
use ponzik_dsp::smooth::Smoother;
use ponzik_dsp::util::db_to_gain;

use super::{DEFAULT_SR, OFF_ON, Values};
use crate::Effect;
use crate::param::{ParamInfo, Params, Unit};

const SLOPES: &[&str] = &["12 dB/oct", "24 dB/oct"];

const P_LC_ON: u32 = 0;
const P_LC_FREQ: u32 = 1;
const P_LC_SLOPE: u32 = 2;
const P_LS_FREQ: u32 = 3;
const P_LS_GAIN: u32 = 4;
/// Première bande en cloche : fréquence, gain, Q ; les deux suivantes suivent.
const P_BAND: u32 = 5;
const P_HS_FREQ: u32 = 14;
const P_HS_GAIN: u32 = 15;
const P_HC_ON: u32 = 16;
const P_HC_FREQ: u32 = 17;
const P_HC_SLOPE: u32 = 18;
const P_OUTPUT: u32 = 19;

const fn band(id: u32, group: &'static str, hz: f32) -> [ParamInfo; 3] {
    [
        ParamInfo::new(id, "Frequency", 20.0, 20_000.0, hz, Unit::Hz).log().group(group),
        ParamInfo::new(id + 1, "Gain", -18.0, 18.0, 0.0, Unit::Db).group(group),
        ParamInfo::new(id + 2, "Q", 0.1, 18.0, 1.0, Unit::None).log().group(group),
    ]
}

const B1: [ParamInfo; 3] = band(P_BAND, "Band 1", 250.0);
const B2: [ParamInfo; 3] = band(P_BAND + 3, "Band 2", 1_000.0);
const B3: [ParamInfo; 3] = band(P_BAND + 6, "Band 3", 4_000.0);

static PARAMS: [ParamInfo; 20] = [
    ParamInfo::choice(P_LC_ON, "Low Cut", OFF_ON, 0).group("Low Cut"),
    ParamInfo::new(P_LC_FREQ, "Frequency", 20.0, 2_000.0, 30.0, Unit::Hz).log().group("Low Cut"),
    ParamInfo::choice(P_LC_SLOPE, "Slope", SLOPES, 0).group("Low Cut"),
    ParamInfo::new(P_LS_FREQ, "Frequency", 20.0, 1_000.0, 100.0, Unit::Hz).log().group("Low"),
    ParamInfo::new(P_LS_GAIN, "Gain", -18.0, 18.0, 0.0, Unit::Db).group("Low"),
    B1[0],
    B1[1],
    B1[2],
    B2[0],
    B2[1],
    B2[2],
    B3[0],
    B3[1],
    B3[2],
    ParamInfo::new(P_HS_FREQ, "Frequency", 1_000.0, 20_000.0, 8_000.0, Unit::Hz).log().group("High"),
    ParamInfo::new(P_HS_GAIN, "Gain", -18.0, 18.0, 0.0, Unit::Db).group("High"),
    ParamInfo::choice(P_HC_ON, "High Cut", OFF_ON, 0).group("High Cut"),
    ParamInfo::new(P_HC_FREQ, "Frequency", 1_000.0, 20_000.0, 18_000.0, Unit::Hz).log().group("High Cut"),
    ParamInfo::choice(P_HC_SLOPE, "Slope", SLOPES, 0).group("High Cut"),
    ParamInfo::new(P_OUTPUT, "Output", -24.0, 24.0, 0.0, Unit::Db).group("Output"),
];

/// Étages : coupe-bas ×2, plateau grave, 3 cloches, plateau aigu, coupe-haut ×2.
const STAGES: usize = 9;

/// Égaliseur paramétrique à biquads RBJ.
pub struct Eq {
    values: Values<20>,
    sample_rate: f32,
    coefs: [BiquadCoefs; STAGES],
    active: [bool; STAGES],
    filters: [[Biquad; STAGES]; 2],
    gain: Smoother,
}

impl Default for Eq {
    fn default() -> Self {
        let mut eq = Self {
            values: Values::new(&PARAMS),
            sample_rate: DEFAULT_SR,
            coefs: [BiquadCoefs::IDENTITY; STAGES],
            active: [false; STAGES],
            filters: Default::default(),
            gain: Smoother::new(1.0),
        };
        eq.set_sample_rate(DEFAULT_SR);
        eq
    }
}

impl Eq {
    fn update(&mut self) {
        let v = &self.values;
        let sr = self.sample_rate;
        let mut coefs = [BiquadCoefs::IDENTITY; STAGES];
        let mut active = [false; STAGES];

        let cut = |on: u32, freq: u32, slope: u32, high: bool| {
            let hz = v.get(freq);
            let make = |q| if high { BiquadCoefs::lowpass(hz, q, sr) } else { BiquadCoefs::highpass(hz, q, sr) };
            let steep = v.index(slope) == 1;
            let qs = if steep { BUTTERWORTH4_Q } else { [std::f32::consts::FRAC_1_SQRT_2, 0.0] };
            ([make(qs[0]), make(qs[1])], [v.on(on), v.on(on) && steep])
        };
        let (c, a) = cut(P_LC_ON, P_LC_FREQ, P_LC_SLOPE, false);
        coefs[..2].copy_from_slice(&c);
        active[..2].copy_from_slice(&a);
        let (c, a) = cut(P_HC_ON, P_HC_FREQ, P_HC_SLOPE, true);
        coefs[7..].copy_from_slice(&c);
        active[7..].copy_from_slice(&a);

        coefs[2] = BiquadCoefs::low_shelf(v.get(P_LS_FREQ), 0.707, v.get(P_LS_GAIN), sr);
        active[2] = v.get(P_LS_GAIN) != 0.0;
        for b in 0..3 {
            let id = P_BAND + 3 * b as u32;
            coefs[3 + b] = BiquadCoefs::peaking(v.get(id), v.get(id + 2), v.get(id + 1), sr);
            active[3 + b] = v.get(id + 1) != 0.0;
        }
        coefs[6] = BiquadCoefs::high_shelf(v.get(P_HS_FREQ), 0.707, v.get(P_HS_GAIN), sr);
        active[6] = v.get(P_HS_GAIN) != 0.0;

        for ch in &mut self.filters {
            for (s, f) in ch.iter_mut().enumerate() {
                if active[s] && !self.active[s] {
                    f.reset();
                }
                f.coefs = coefs[s];
            }
        }
        self.coefs = coefs;
        self.active = active;
        self.gain.set_target(db_to_gain(self.values.get(P_OUTPUT)));
    }
}

impl Params for Eq {
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

impl Effect for Eq {
    fn kind(&self) -> &'static str {
        "eq"
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.gain.set_time(0.02, sample_rate);
        self.update();
        self.gain.snap();
    }

    fn reset(&mut self) {
        self.filters.iter_mut().flatten().for_each(Biquad::reset);
        self.gain.snap();
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let active = self.active;
        let [fl, fr] = &mut self.filters;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let g = self.gain.next_value();
            let (mut a, mut b) = (*l, *r);
            for s in (0..STAGES).filter(|&s| active[s]) {
                a = fl[s].process(a);
                b = fr[s].process(b);
            }
            *l = a * g;
            *r = b * g;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::testing::*;

    fn level(eq: &mut Eq, hz: f32) -> f32 {
        eq.reset();
        let (l, _) = run_mono(eq, &sine(hz, 0.5, 9_600));
        rms(&l[4_800..]) / rms(&sine(hz, 0.5, 9_600)[4_800..])
    }

    #[test]
    fn neutral_by_default() {
        let mut eq = Eq::default();
        let x = noise(0.5, 4_096, 3);
        let (l, r) = run_mono(&mut eq, &x);
        assert_eq!(l, x);
        assert_eq!(r, x);
    }

    #[test]
    fn peaking_cut_reduces_only_its_band() {
        let mut eq = Eq::default();
        eq.set_param(P_BAND + 3, 1_000.0);
        eq.set_param(P_BAND + 4, -18.0);
        eq.set_param(P_BAND + 5, 2.0);
        assert!(level(&mut eq, 1_000.0) < 0.15);
        assert!((level(&mut eq, 100.0) - 1.0).abs() < 0.03);
    }

    #[test]
    fn cuts_and_shelves() {
        let mut eq = Eq::default();
        eq.set_param(P_LC_ON, 1.0);
        eq.set_param(P_LC_FREQ, 400.0);
        eq.set_param(P_LC_SLOPE, 1.0);
        assert!(level(&mut eq, 50.0) < 0.001);
        eq.set_param(P_HC_ON, 1.0);
        eq.set_param(P_HC_FREQ, 2_000.0);
        assert!(level(&mut eq, 12_000.0) < 0.05);
        assert!(level(&mut eq, 900.0) > 0.85);
        eq.set_param(P_HS_GAIN, 12.0);
        eq.set_param(P_HC_ON, 0.0);
        assert!(level(&mut eq, 18_000.0) > 3.5);
    }

    #[test]
    fn stable() {
        assert_stable_at_extremes(|| Box::new(Eq::default()), 1e5);
    }
}
