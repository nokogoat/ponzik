use ponzik_dsp::biquad::{Biquad, BiquadCoefs};
use ponzik_dsp::smooth::Smoother;
use ponzik_dsp::util::{balance_gains, db_to_gain};

use super::{DEFAULT_SR, OFF_ON, Values};
use crate::Effect;
use crate::param::{ParamInfo, Params, Unit};

const P_GAIN: u32 = 0;
const P_PAN: u32 = 1;
const P_WIDTH: u32 = 2;
const P_BASS_MONO: u32 = 3;
const P_BASS_FREQ: u32 = 4;
const P_INVERT_L: u32 = 5;
const P_INVERT_R: u32 = 6;
const P_MUTE: u32 = 7;

static PARAMS: [ParamInfo; 8] = [
    ParamInfo::new(P_GAIN, "Gain", -80.0, 24.0, 0.0, Unit::Db).group("Level"),
    ParamInfo::new(P_PAN, "Balance", -1.0, 1.0, 0.0, Unit::Pan).group("Level"),
    ParamInfo::new(P_WIDTH, "Width", 0.0, 2.0, 1.0, Unit::Percent).group("Stereo"),
    ParamInfo::choice(P_BASS_MONO, "Bass Mono", OFF_ON, 0).group("Stereo"),
    ParamInfo::new(P_BASS_FREQ, "Mono Frequency", 20.0, 500.0, 120.0, Unit::Hz).log().group("Stereo"),
    ParamInfo::choice(P_INVERT_L, "Invert L", OFF_ON, 0).group("Phase"),
    ParamInfo::choice(P_INVERT_R, "Invert R", OFF_ON, 0).group("Phase"),
    ParamInfo::choice(P_MUTE, "Mute", OFF_ON, 0).group("Level"),
];

/// Séparation Linkwitz-Riley d'ordre 4 : graves + aigus = passe-tout.
#[derive(Default)]
struct Crossover {
    low: [Biquad; 2],
    high: [Biquad; 2],
}

impl Crossover {
    fn set(&mut self, hz: f32, sample_rate: f32) {
        let q = std::f32::consts::FRAC_1_SQRT_2;
        let (lp, hp) = (BiquadCoefs::lowpass(hz, q, sample_rate), BiquadCoefs::highpass(hz, q, sample_rate));
        self.low.iter_mut().for_each(|f| f.coefs = lp);
        self.high.iter_mut().for_each(|f| f.coefs = hp);
    }

    fn reset(&mut self) {
        self.low.iter_mut().chain(&mut self.high).for_each(Biquad::reset);
    }

    #[inline]
    fn split(&mut self, x: f32) -> (f32, f32) {
        let [l0, l1] = &mut self.low;
        let [h0, h1] = &mut self.high;
        (l1.process(l0.process(x)), h1.process(h0.process(x)))
    }
}

/// Outils de piste : gain, balance, largeur stéréo, graves en mono, phase, muet.
pub struct Utility {
    values: Values<8>,
    sample_rate: f32,
    crossover: [Crossover; 2],
    gain_l: Smoother,
    gain_r: Smoother,
    width: Smoother,
}

impl Default for Utility {
    fn default() -> Self {
        let mut u = Self {
            values: Values::new(&PARAMS),
            sample_rate: DEFAULT_SR,
            crossover: Default::default(),
            gain_l: Smoother::new(1.0),
            gain_r: Smoother::new(1.0),
            width: Smoother::new(1.0),
        };
        u.set_sample_rate(DEFAULT_SR);
        u
    }
}

impl Utility {
    fn update(&mut self) {
        let v = &self.values;
        let gain = if v.on(P_MUTE) || v.get(P_GAIN) <= -79.9 { 0.0 } else { db_to_gain(v.get(P_GAIN)) };
        let (bl, br) = balance_gains(v.get(P_PAN));
        let sign = |id| if v.on(id) { -1.0 } else { 1.0 };
        self.gain_l.set_target(gain * bl * sign(P_INVERT_L));
        self.gain_r.set_target(gain * br * sign(P_INVERT_R));
        self.width.set_target(v.get(P_WIDTH));
        let hz = v.get(P_BASS_FREQ);
        self.crossover.iter_mut().for_each(|c| c.set(hz, self.sample_rate));
    }
}

impl Params for Utility {
    fn params(&self) -> &'static [ParamInfo] {
        &PARAMS
    }

    fn get_param(&self, id: u32) -> f32 {
        self.values.get(id)
    }

    fn set_param(&mut self, id: u32, value: f32) {
        let was_mono = self.values.on(P_BASS_MONO);
        if self.values.set(&PARAMS, id, value) {
            if self.values.on(P_BASS_MONO) && !was_mono {
                self.crossover.iter_mut().for_each(Crossover::reset);
            }
            self.update();
        }
    }
}

impl Effect for Utility {
    fn kind(&self) -> &'static str {
        "utility"
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        for s in [&mut self.gain_l, &mut self.gain_r, &mut self.width] {
            s.set_time(0.02, sample_rate);
        }
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        self.crossover.iter_mut().for_each(Crossover::reset);
        for s in [&mut self.gain_l, &mut self.gain_r, &mut self.width] {
            s.snap();
        }
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let bass_mono = self.values.on(P_BASS_MONO);
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let width = self.width.next_value();
            let (mut a, mut b) = (*l, *r);
            if bass_mono {
                let [cl, cr] = &mut self.crossover;
                let ((lo_l, hi_l), (lo_r, hi_r)) = (cl.split(a), cr.split(b));
                let mono = 0.5 * (lo_l + lo_r);
                (a, b) = (mono + hi_l, mono + hi_r);
            }
            let mid = 0.5 * (a + b);
            let side = 0.5 * (a - b) * width;
            *l = (mid + side) * self.gain_l.next_value();
            *r = (mid - side) * self.gain_r.next_value();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::testing::*;

    #[test]
    fn transparent_by_default() {
        let mut u = Utility::default();
        let (mut l, mut r) = (noise(0.5, 1_000, 1), noise(0.5, 1_000, 2));
        let (x, y) = (l.clone(), r.clone());
        run(&mut u, &mut l, &mut r);
        assert!(l.iter().zip(&x).chain(r.iter().zip(&y)).all(|(a, b)| (a - b).abs() < 1e-6));
    }

    #[test]
    fn zero_width_is_mono() {
        let mut u = Utility::default();
        u.set_param(P_WIDTH, 0.0);
        u.reset();
        let (mut l, mut r) = (noise(0.5, 1_000, 1), noise(0.5, 1_000, 2));
        run(&mut u, &mut l, &mut r);
        assert_eq!(l, r);
    }

    #[test]
    fn invert_mute_and_balance() {
        let mut u = Utility::default();
        u.set_param(P_INVERT_L, 1.0);
        u.set_param(P_PAN, 1.0);
        u.reset();
        let (mut l, mut r) = (vec![0.5; 100], vec![0.5; 100]);
        run(&mut u, &mut l, &mut r);
        assert!(l.iter().all(|s| s.abs() < 1e-6));
        assert!(r.iter().all(|s| (s - 0.5).abs() < 1e-6));
        u.set_param(P_PAN, 0.0);
        u.reset();
        let (mut l, mut r) = (vec![0.5; 100], vec![0.5; 100]);
        run(&mut u, &mut l, &mut r);
        assert!((l[50] + 0.5).abs() < 1e-6);
        u.set_param(P_MUTE, 1.0);
        u.reset();
        run(&mut u, &mut l, &mut r);
        assert!(peak(&l) == 0.0 && peak(&r) == 0.0);
    }

    #[test]
    fn bass_mono_removes_low_side_only() {
        let mut u = Utility::default();
        u.set_param(P_BASS_MONO, 1.0);
        u.set_param(P_BASS_FREQ, 200.0);
        let side = |hz: f32, u: &mut Utility| {
            u.reset();
            let mut l = sine(hz, 0.5, 19_200);
            let mut r: Vec<f32> = l.iter().map(|s| -s).collect();
            run(u, &mut l, &mut r);
            rms(&l[9_600..])
        };
        assert!(side(40.0, &mut u) < 0.02);
        assert!(side(3_000.0, &mut u) > 0.34);
    }

    #[test]
    fn stable() {
        assert_stable_at_extremes(|| Box::new(Utility::default()), 100.0);
    }
}
