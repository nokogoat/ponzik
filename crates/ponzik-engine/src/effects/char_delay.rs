//! Délai à caractère : écho de bande, de circuit analogique ou numérique propre.
//!
//! Tout ce qui entre dans la boucle passe par les filtres et la saturation du
//! caractère choisi, si bien que chaque répétition s'use un peu plus que la
//! précédente. Le pleurage (lent) et le scintillement (rapide) modulent la
//! tête de lecture ; le ducking baisse les échos tant que le signal joue.

use ponzik_dsp::biquad::{Biquad, BiquadCoefs};
use ponzik_dsp::delay::DelayLine;
use ponzik_dsp::dynamics::EnvelopeFollower;
use ponzik_dsp::lfo::{Lfo, LfoShape};
use ponzik_dsp::smooth::Smoother;
use ponzik_dsp::util::flush_denormal;

use super::{DEFAULT_SR, DIVISIONS, MIN_BPM, OFF_ON, Values, division_beats};
use crate::Effect;
use crate::param::{ParamInfo, Params, Unit};

const P_CHARACTER: u32 = 0;
const P_DIVISION: u32 = 1;
const P_TIME: u32 = 2;
const P_FEEDBACK: u32 = 3;
const P_PINGPONG: u32 = 4;
const P_SATURATION: u32 = 5;
const P_LOW_CUT: u32 = 6;
const P_HIGH_CUT: u32 = 7;
const P_WOW: u32 = 8;
const P_FLUTTER: u32 = 9;
const P_DUCKING: u32 = 10;
const P_MIX: u32 = 11;

static PARAMS: [ParamInfo; 12] = [
    ParamInfo::choice(P_CHARACTER, "Character", &["Tape", "Analog", "Digital"], 0).group("Character"),
    ParamInfo::choice(P_DIVISION, "Sync", DIVISIONS, 8).group("Time"),
    ParamInfo::new(P_TIME, "Free Time", 0.001, MAX_FREE_TIME, 0.375, Unit::Seconds).log().group("Time"),
    ParamInfo::new(P_FEEDBACK, "Feedback", 0.0, 1.0, 0.45, Unit::Percent).group("Echoes"),
    ParamInfo::choice(P_PINGPONG, "Ping-pong", OFF_ON, 0).group("Echoes"),
    ParamInfo::new(P_SATURATION, "Saturation", 0.0, 1.0, 0.3, Unit::Percent).group("Echoes"),
    ParamInfo::new(P_LOW_CUT, "Low Cut", 20.0, 2_000.0, 100.0, Unit::Hz).log().group("Echoes"),
    ParamInfo::new(P_HIGH_CUT, "High Cut", 500.0, 20_000.0, 7_000.0, Unit::Hz).log().group("Echoes"),
    ParamInfo::new(P_WOW, "Wow", 0.0, 1.0, 0.2, Unit::Percent).group("Modulation"),
    ParamInfo::new(P_FLUTTER, "Flutter", 0.0, 1.0, 0.15, Unit::Percent).group("Modulation"),
    ParamInfo::new(P_DUCKING, "Ducking", 0.0, 1.0, 0.0, Unit::Percent).group("Output"),
    ParamInfo::new(P_MIX, "Mix", 0.0, 1.0, 0.3, Unit::Percent).group("Output"),
];

const MAX_FREE_TIME: f32 = 4.0;
/// Une mesure au tempo le plus lent.
const MAX_TIME: f32 = 4.0 * 60.0 / MIN_BPM;
/// Excursions maximales de la tête de lecture (s).
const WOW_DEPTH: f32 = 0.004;
const FLUTTER_DEPTH: f32 = 0.000_25;
const WOW_HZ: f32 = 0.55;
const FLUTTER_HZ: [f32; 2] = [6.3, 9.7];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Character {
    Tape,
    Analog,
    Digital,
}

const CHARACTERS: [Character; 3] = [Character::Tape, Character::Analog, Character::Digital];

impl Character {
    /// Part de la modulation réglée qui s'applique.
    fn modulation(self) -> f32 {
        match self {
            Character::Tape => 1.0,
            Character::Analog => 0.5,
            Character::Digital => 0.25,
        }
    }

    /// Facteur sur le coupe-haut : la ligne à capacités commutées est plus sombre.
    fn darkness(self) -> f32 {
        match self {
            Character::Analog => 0.5,
            _ => 1.0,
        }
    }

    /// Saturation de la boucle : gain unitaire en petit signal, sortie bornée par `c`.
    #[inline]
    fn saturate(self, x: f32, c: f32, bias: f32) -> f32 {
        match self {
            Character::Tape => c * (x / c).tanh(),
            // Asymétrique : harmoniques paires, la composante continue part au coupe-bas.
            Character::Analog => c * ((x / c + bias).tanh() - bias.tanh()) / (1.0 - bias.tanh() * bias.tanh()),
            Character::Digital => x.clamp(-c, c),
        }
    }
}

/// Écho stéréo à caractère, synchronisable au tempo, avec ping-pong et ducking.
pub struct CharDelay {
    values: Values<12>,
    sample_rate: f32,
    bpm: f32,
    character: Character,
    lines: [DelayLine; 2],
    low_cut: [Biquad; 2],
    high_cut: [Biquad; 2],
    wow: Lfo,
    flutter: [Lfo; 2],
    duck: EnvelopeFollower,
    time: Smoother,
    feedback: Smoother,
    mix: Smoother,
}

impl Default for CharDelay {
    fn default() -> Self {
        let mut d = Self {
            values: Values::new(&PARAMS),
            sample_rate: DEFAULT_SR,
            bpm: 120.0,
            character: Character::Tape,
            lines: [DelayLine::new(2), DelayLine::new(2)],
            low_cut: Default::default(),
            high_cut: Default::default(),
            wow: Lfo::new(LfoShape::Sine),
            flutter: [Lfo::new(LfoShape::Sine), Lfo::new(LfoShape::Sine)],
            duck: EnvelopeFollower::new(0.005, 0.25, DEFAULT_SR),
            time: Smoother::new(0.0),
            feedback: Smoother::new(0.0),
            mix: Smoother::new(0.0),
        };
        d.set_sample_rate(DEFAULT_SR);
        d
    }
}

impl CharDelay {
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
        self.character = CHARACTERS[v.index(P_CHARACTER).min(CHARACTERS.len() - 1)];
        self.time.set_target(self.time_seconds() * sr);
        self.feedback.set_target(v.get(P_FEEDBACK));
        self.mix.set_target(v.get(P_MIX));
        let lc = BiquadCoefs::highpass(v.get(P_LOW_CUT), 0.707, sr);
        let hc = BiquadCoefs::lowpass(v.get(P_HIGH_CUT) * self.character.darkness(), 0.707, sr);
        self.low_cut.iter_mut().for_each(|f| f.coefs = lc);
        self.high_cut.iter_mut().for_each(|f| f.coefs = hc);
    }
}

impl Params for CharDelay {
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

impl Effect for CharDelay {
    fn kind(&self) -> &'static str {
        "char_delay"
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        let max = ((MAX_TIME + WOW_DEPTH + FLUTTER_DEPTH) * sample_rate) as usize + 8;
        self.lines = [DelayLine::new(max), DelayLine::new(max)];
        self.wow.set_rate(WOW_HZ, sample_rate);
        for (lfo, hz) in self.flutter.iter_mut().zip(FLUTTER_HZ) {
            lfo.set_rate(hz, sample_rate);
        }
        self.duck.set_times(0.005, 0.25, sample_rate);
        self.time.set_time(0.1, sample_rate);
        self.feedback.set_time(0.02, sample_rate);
        self.mix.set_time(0.02, sample_rate);
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        self.lines.iter_mut().for_each(DelayLine::clear);
        self.low_cut.iter_mut().chain(&mut self.high_cut).for_each(Biquad::reset);
        self.wow.reset(0.0);
        self.flutter.iter_mut().for_each(|l| l.reset(0.0));
        self.duck.reset();
        self.time.snap();
        self.feedback.snap();
        self.mix.snap();
    }

    fn set_tempo(&mut self, bpm: f32) {
        self.bpm = bpm.clamp(MIN_BPM, 999.0);
        self.update();
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let v = &self.values;
        let character = self.character;
        let pingpong = v.on(P_PINGPONG);
        let sat = v.get(P_SATURATION);
        let ceiling = 1.5 - sat;
        let bias = 0.3 * sat;
        let scale = character.modulation() * self.sample_rate;
        // Excursions unipolaires : le retard ne descend jamais sous le temps réglé.
        let wow = v.get(P_WOW) * WOW_DEPTH * scale * 0.5;
        let flutter = v.get(P_FLUTTER) * FLUTTER_DEPTH * scale * 0.5;
        let ducking = v.get(P_DUCKING) * 8.0;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let t = self.time.next_value();
            let fb = self.feedback.next_value();
            let mix = self.mix.next_value();
            let [f0, f1] = &self.flutter;
            let wobble = |offset: f32| {
                wow * (1.0 + self.wow.value_at(offset))
                    + flutter * (1.0 + 0.6 * f0.value_at(offset) + 0.4 * f1.value_at(offset))
            };
            let (ml, mr) = (wobble(0.0), wobble(0.13));
            self.wow.advance();
            self.flutter.iter_mut().for_each(Lfo::advance);

            let [dl, dr] = &mut self.lines;
            let (el, er) = (dl.read_frac(t + ml), dr.read_frac(t + mr));
            let (fl, fr) = (fb * el, fb * er);
            let (in_l, in_r) = if pingpong { (0.5 * (*l + *r) + fr, fl) } else { (*l + fl, *r + fr) };
            let mut color = |ch: usize, x: f32| {
                let y = self.high_cut[ch].process(self.low_cut[ch].process(x));
                flush_denormal(character.saturate(y, ceiling, bias))
            };
            let (wl, wr) = (color(0, in_l), color(1, in_r));
            dl.write(wl);
            dr.write(wr);

            let level = self.duck.process(l.abs().max(r.abs()));
            let wet = mix / (1.0 + ducking * level);
            *l = *l * (1.0 - mix) + el * wet;
            *r = *r * (1.0 - mix) + er * wet;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::testing::*;

    fn clean(division: f32) -> CharDelay {
        let mut d = CharDelay::default();
        d.set_param(P_CHARACTER, 2.0);
        d.set_param(P_DIVISION, division);
        d.set_param(P_MIX, 1.0);
        d.set_param(P_FEEDBACK, 0.5);
        d.set_param(P_SATURATION, 0.0);
        d.set_param(P_LOW_CUT, 20.0);
        d.set_param(P_HIGH_CUT, 20_000.0);
        d.set_param(P_WOW, 0.0);
        d.set_param(P_FLUTTER, 0.0);
        d.set_tempo(120.0);
        d.reset();
        d
    }

    fn argmax(x: &[f32]) -> usize {
        (0..x.len()).max_by(|&a, &b| x[a].abs().total_cmp(&x[b].abs())).unwrap()
    }

    /// Impulsion lisse (Hann de 64 échantillons, centrée sur 32) : les filtres de la boucle ne l'étalent pas.
    fn pulse(frames: usize) -> Vec<f32> {
        let mut x = vec![0.0; frames];
        for (i, s) in x.iter_mut().take(65).enumerate() {
            *s = 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / 64.0).cos();
        }
        x
    }

    #[test]
    fn echoes_land_on_the_beat_at_120_bpm() {
        // Noire à 120 BPM = 0,5 s = 24 000 échantillons, croche = 12 000, croche pointée = 18 000.
        for (division, at) in [(11.0, 24_000), (8.0, 12_000), (9.0, 18_000)] {
            let mut d = clean(division);
            let (l, _) = run_mono(&mut d, &pulse(at + at / 2));
            let got = argmax(&l) - 32;
            assert!(got.abs_diff(at) <= 2, "{division} : {got} au lieu de {at}");
            // Le coupe-bas à 20 Hz entame à peine l'impulsion.
            assert!(peak(&l[at..at + 64]) > 0.9, "{}", peak(&l[at..at + 64]));
        }
        // La deuxième répétition arrive un temps plus tard, atténuée par la réinjection.
        let mut d = clean(11.0);
        let (l, _) = run_mono(&mut d, &pulse(60_000));
        assert!((argmax(&l[36_000..]) + 36_000 - 32).abs_diff(48_000) <= 3);
        let second = peak(&l[47_000..49_000]);
        assert!(second > 0.4 && second < 0.55, "{second}");
    }

    #[test]
    fn tape_saturation_bounds_runaway_feedback() {
        let mut d = CharDelay::default();
        d.set_param(P_DIVISION, 11.0);
        d.set_param(P_MIX, 1.0);
        d.set_param(P_FEEDBACK, 1.0);
        d.set_param(P_SATURATION, 1.0);
        d.set_param(P_WOW, 1.0);
        d.set_tempo(120.0);
        d.reset();
        let x = noise(1.0, 4 * SR as usize, 4);
        let (l, r) = run_mono(&mut d, &x);
        assert!(l.iter().chain(&r).all(|s| s.is_finite() && s.abs() <= 0.5 * 1.3));
        assert!(rms(&l[3 * SR as usize..]) > 0.05, "la bande s'entretient");
    }

    #[test]
    fn pingpong_alternates_sides() {
        let mut d = clean(11.0);
        d.set_param(P_PINGPONG, 1.0);
        d.reset();
        let mut l = pulse(50_000);
        let mut r = vec![0.0; 50_000];
        run(&mut d, &mut l, &mut r);
        assert!(peak(&l[23_000..25_000]) > 0.4 && peak(&r[23_000..25_000]) < 1e-3);
        assert!(peak(&r[47_000..49_000]) > 0.1 && peak(&l[47_000..49_000]) < 1e-2);
    }

    #[test]
    fn stable() {
        assert_stable_at_extremes(|| Box::new(CharDelay::default()), 10.0);
    }
}
