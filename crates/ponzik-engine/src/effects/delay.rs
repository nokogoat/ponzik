use ponzik_dsp::biquad::{Biquad, BiquadCoefs};
use ponzik_dsp::delay::DelayLine;
use ponzik_dsp::smooth::Smoother;
use ponzik_dsp::util::flush_denormal;

use super::{DEFAULT_SR, DIVISIONS, MIN_BPM, OFF_ON, Values, division_beats};
use crate::Effect;
use crate::param::{ParamInfo, Params, Unit};

const P_DIVISION: u32 = 0;
const P_TIME: u32 = 1;
const P_FEEDBACK: u32 = 2;
const P_PINGPONG: u32 = 3;
const P_LOW_CUT: u32 = 4;
const P_HIGH_CUT: u32 = 5;
const P_MIX: u32 = 6;

static PARAMS: [ParamInfo; 7] = [
    ParamInfo::choice(P_DIVISION, "Sync", DIVISIONS, 8).group("Time"),
    ParamInfo::new(P_TIME, "Free Time", 0.001, MAX_FREE_TIME, 0.25, Unit::Seconds).log().group("Time"),
    ParamInfo::new(P_FEEDBACK, "Feedback", 0.0, 0.95, 0.4, Unit::Percent).group("Echoes"),
    ParamInfo::choice(P_PINGPONG, "Ping-pong", OFF_ON, 0).group("Echoes"),
    ParamInfo::new(P_LOW_CUT, "Low Cut", 20.0, 2_000.0, 80.0, Unit::Hz).log().group("Echoes"),
    ParamInfo::new(P_HIGH_CUT, "High Cut", 1_000.0, 20_000.0, 12_000.0, Unit::Hz).log().group("Echoes"),
    ParamInfo::new(P_MIX, "Mix", 0.0, 1.0, 0.3, Unit::Percent).group("Output"),
];

const MAX_FREE_TIME: f32 = 4.0;
/// Une mesure au tempo le plus lent.
const MAX_TIME: f32 = 4.0 * 60.0 / MIN_BPM;

/// Écho stéréo synchronisable au tempo ; les filtres agissent dans la boucle de
/// réinjection (la première répétition reste intacte).
pub struct Delay {
    values: Values<7>,
    sample_rate: f32,
    bpm: f32,
    lines: [DelayLine; 2],
    low_cut: [Biquad; 2],
    high_cut: [Biquad; 2],
    time: Smoother,
    feedback: Smoother,
    mix: Smoother,
}

impl Default for Delay {
    fn default() -> Self {
        let mut d = Self {
            values: Values::new(&PARAMS),
            sample_rate: DEFAULT_SR,
            bpm: 120.0,
            lines: [DelayLine::new(0), DelayLine::new(0)],
            low_cut: Default::default(),
            high_cut: Default::default(),
            time: Smoother::new(0.0),
            feedback: Smoother::new(0.0),
            mix: Smoother::new(0.0),
        };
        d.set_sample_rate(DEFAULT_SR);
        d
    }
}

impl Delay {
    /// Retard courant en secondes.
    pub fn time_seconds(&self) -> f32 {
        let t = match division_beats(self.values.get(P_DIVISION)) {
            Some(beats) => beats * 60.0 / self.bpm,
            None => self.values.get(P_TIME),
        };
        t.min(MAX_TIME)
    }

    fn update(&mut self) {
        let sr = self.sample_rate;
        let v = &self.values;
        let time = self.time_seconds() * sr;
        self.time.set_target(time);
        // L'anneau grandit avec le retard (sans allocation) : un écho court ne
        // touche pas les secondes réservées.
        for line in &mut self.lines {
            line.reserve(time as usize + 4);
        }
        self.feedback.set_target(v.get(P_FEEDBACK));
        self.mix.set_target(v.get(P_MIX));
        let lc = BiquadCoefs::highpass(v.get(P_LOW_CUT), 0.707, sr);
        let hc = BiquadCoefs::lowpass(v.get(P_HIGH_CUT), 0.707, sr);
        for f in &mut self.low_cut {
            f.coefs = lc;
        }
        for f in &mut self.high_cut {
            f.coefs = hc;
        }
    }
}

impl Params for Delay {
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

impl Effect for Delay {
    fn kind(&self) -> &'static str {
        "delay"
    }

    fn tail(&self) -> f32 {
        self.time_seconds()
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        let max = (MAX_TIME * sample_rate) as usize + 4;
        // Même fréquence (la ligne existe déjà) : rien à réallouer.
        if sample_rate != self.sample_rate || self.lines[0].max_delay() < 4 {
            self.lines = [DelayLine::growable(max), DelayLine::growable(max)];
        }
        self.sample_rate = sample_rate;
        self.time.set_time(0.1, sample_rate);
        self.feedback.set_time(0.02, sample_rate);
        self.mix.set_time(0.02, sample_rate);
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        self.lines.iter_mut().for_each(DelayLine::clear);
        self.low_cut.iter_mut().chain(&mut self.high_cut).for_each(Biquad::reset);
        self.time.snap();
        self.feedback.snap();
        self.mix.snap();
    }

    fn set_tempo(&mut self, bpm: f32) {
        self.bpm = bpm.clamp(MIN_BPM, 999.0);
        self.update();
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let pingpong = self.values.on(P_PINGPONG);
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let t = self.time.next_value();
            let fb = self.feedback.next_value();
            let mix = self.mix.next_value();
            let [dl, dr] = &mut self.lines;
            let (el, er) = (dl.read_frac(t), dr.read_frac(t));
            let mut loop_back = |ch: usize, x: f32| fb * self.high_cut[ch].process(self.low_cut[ch].process(x));
            let (fl, fr) = (loop_back(0, el), loop_back(1, er));
            if pingpong {
                dl.write(flush_denormal(0.5 * (*l + *r) + fr));
                dr.write(flush_denormal(fl));
            } else {
                dl.write(flush_denormal(*l + fl));
                dr.write(flush_denormal(*r + fr));
            }
            *l = *l * (1.0 - mix) + el * mix;
            *r = *r * (1.0 - mix) + er * mix;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::testing::*;

    fn wet(division: f32) -> Delay {
        let mut d = Delay::default();
        d.set_param(P_DIVISION, division);
        d.set_param(P_MIX, 1.0);
        d.set_param(P_FEEDBACK, 0.5);
        d.set_param(P_LOW_CUT, 20.0);
        d.set_param(P_HIGH_CUT, 20_000.0);
        d.set_tempo(120.0);
        d.reset();
        d
    }

    fn argmax(x: &[f32]) -> usize {
        (0..x.len()).max_by(|&a, &b| x[a].abs().total_cmp(&x[b].abs())).unwrap()
    }

    #[test]
    fn synced_echo_lands_on_the_beat() {
        // Noire à 120 BPM = 0,5 s = 24 000 échantillons ; croche pointée = 18 000.
        let mut d = wet(11.0);
        let (l, _) = run_mono(&mut d, &impulse(30_000));
        assert_eq!(argmax(&l), 24_000);
        assert!((l[24_000] - 1.0).abs() < 1e-5);
        let mut d = wet(9.0);
        let (l, _) = run_mono(&mut d, &impulse(20_000));
        assert_eq!(argmax(&l), 18_000);
        let mut d = wet(10.0);
        let (l, _) = run_mono(&mut d, &impulse(20_000));
        assert_eq!(argmax(&l), 16_000);
    }

    #[test]
    fn free_time_and_feedback_repeats() {
        let mut d = wet(0.0);
        d.set_param(P_TIME, 0.1);
        d.reset();
        let (l, _) = run_mono(&mut d, &impulse(12_000));
        assert_eq!(argmax(&l[..6_000]), 4_800);
        let second = peak(&l[9_000..10_000]);
        assert!(second > 0.2 && second < 0.55, "{second}");
    }

    #[test]
    fn pingpong_alternates_sides() {
        let mut d = wet(11.0);
        d.set_param(P_PINGPONG, 1.0);
        d.reset();
        let mut l = impulse(50_000);
        let mut r = vec![0.0; 50_000];
        run(&mut d, &mut l, &mut r);
        assert!(peak(&l[23_000..25_000]) > 0.4 && peak(&r[23_000..25_000]) < 1e-6);
        assert!(peak(&r[47_000..49_000]) > 0.1 && peak(&l[47_000..49_000]) < 1e-3);
    }

    #[test]
    fn stable() {
        assert_stable_at_extremes(|| Box::new(Delay::default()), 30.0);
    }
}
