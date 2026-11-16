//! Bruits et texture : vieillit un son comme un disque usé ou une cassette fatiguée.
//!
//! Le signal passe dans une tête de lecture modulée (pleurage lent,
//! scintillement rapide), un filtre d'usure qui referme le haut et le bas du
//! spectre, puis reçoit le souffle de bande et les craquements de vinyle. La
//! perte de bits tire au sort, bloc par bloc, une résolution plus ou moins
//! dégradée et quelques décrochages.

use ponzik_dsp::biquad::{Biquad, BiquadCoefs};
use ponzik_dsp::delay::DelayLine;
use ponzik_dsp::filter::OnePole;
use ponzik_dsp::lfo::{Lfo, LfoShape};
use ponzik_dsp::noise::WhiteNoise;
use ponzik_dsp::smooth::Smoother;
use ponzik_dsp::util::{db_to_gain, decay_coef};

use super::{DEFAULT_SR, Values};
use crate::Effect;
use crate::param::{ParamInfo, Params, Unit};

const P_CRACKLE: u32 = 0;
const P_HISS: u32 = 1;
const P_WOW: u32 = 2;
const P_FLUTTER: u32 = 3;
const P_BIT_LOSS: u32 = 4;
const P_AGE: u32 = 5;
const P_MIX: u32 = 6;

static PARAMS: [ParamInfo; 7] = [
    ParamInfo::new(P_CRACKLE, "Crackle", 0.0, 1.0, 0.3, Unit::Percent).group("Vinyl"),
    ParamInfo::new(P_HISS, "Hiss", 0.0, 1.0, 0.2, Unit::Percent).group("Tape"),
    ParamInfo::new(P_WOW, "Wow", 0.0, 1.0, 0.2, Unit::Percent).group("Tape"),
    ParamInfo::new(P_FLUTTER, "Flutter", 0.0, 1.0, 0.1, Unit::Percent).group("Tape"),
    ParamInfo::new(P_BIT_LOSS, "Bit Loss", 0.0, 1.0, 0.0, Unit::Percent).group("Digital"),
    ParamInfo::new(P_AGE, "Age", 0.0, 1.0, 0.3, Unit::Percent).group("Wear"),
    ParamInfo::new(P_MIX, "Mix", 0.0, 1.0, 1.0, Unit::Percent).group("Output"),
];

/// Souffle à fond : −36 dBFS efficaces.
const HISS_DB: f32 = -36.0;
/// Valeur efficace d'un bruit blanc uniforme sur [−1, 1].
const WHITE_RMS: f32 = 0.577_35;
/// Coupe-bas du souffle (le souffle de bande est surtout aigu).
const HISS_LOW_CUT: f32 = 400.0;
/// Craquements par seconde à fond, et leur durée.
const CRACKLE_RATE: f32 = 60.0;
const CRACKLE_TAU: f32 = 0.000_25;
const CRACKLE_LEVEL: f32 = 0.6;
/// Excursions de la tête de lecture (s).
const WOW_DEPTH: f32 = 0.003;
const FLUTTER_DEPTH: f32 = 0.000_3;
const WOW_HZ: f32 = 0.45;
/// Ramène la dérive filtrée (écart type ≈ 0,008) vers ±1.
const DRIFT_SCALE: f32 = 40.0;
const FLUTTER_HZ: f32 = 7.5;
/// Bloc de la perte de bits (s) et résolution la plus basse atteinte.
const BLOCK_TIME: f32 = 0.025;
const MIN_BITS: f32 = 4.0;
const MAX_BITS: f32 = 16.0;
/// Filtre d'usure : de 20 kHz / 20 Hz (neuf) à 2,5 kHz / 150 Hz (usé).
const AGE_LOW_PASS: [f32; 2] = [20_000.0, 2_500.0];
const AGE_HIGH_PASS: [f32; 2] = [20.0, 150.0];

/// `a` à 0, `b` à 1, en progression géométrique.
fn geometric(range: [f32; 2], t: f32) -> f32 {
    range[0] * (range[1] / range[0]).powf(t)
}

struct Channel {
    line: DelayLine,
    low_pass: Biquad,
    high_pass: Biquad,
    hiss: WhiteNoise,
    hiss_filter: OnePole,
    crackle: WhiteNoise,
    crackle_env: f32,
}

impl Channel {
    fn new(max_delay: usize, seed: u32) -> Self {
        Self {
            line: DelayLine::new(max_delay),
            low_pass: Biquad::default(),
            high_pass: Biquad::default(),
            hiss: WhiteNoise::new(seed),
            hiss_filter: OnePole::default(),
            crackle: WhiteNoise::new(seed.rotate_left(13) ^ 0x5bd1_e995),
            crackle_env: 0.0,
        }
    }

    fn reset(&mut self) {
        self.line.clear();
        self.low_pass.reset();
        self.high_pass.reset();
        self.hiss_filter.reset();
        self.crackle_env = 0.0;
    }
}

/// Vieillissement : pleurage, scintillement, usure, souffle, craquements et perte de bits.
pub struct Lofi {
    values: Values<7>,
    sample_rate: f32,
    channels: [Channel; 2],
    /// Retard central de la tête de lecture (échantillons).
    base_delay: usize,
    wow: Lfo,
    flutter: Lfo,
    /// Dérive aléatoire lente du scintillement.
    drift: OnePole,
    dice: WhiteNoise,
    block_len: usize,
    block_pos: usize,
    step: f32,
    dropout: Smoother,
    crackle_coef: f32,
    aged: bool,
    mix: Smoother,
}

impl Default for Lofi {
    fn default() -> Self {
        let mut l = Self {
            values: Values::new(&PARAMS),
            sample_rate: DEFAULT_SR,
            channels: [Channel::new(4, 0x1234_5679), Channel::new(4, 0x9e37_79b9)],
            base_delay: 2,
            wow: Lfo::new(LfoShape::Sine),
            flutter: Lfo::new(LfoShape::Sine),
            drift: OnePole::default(),
            dice: WhiteNoise::new(0x2f6b_1d03),
            block_len: 1,
            block_pos: 0,
            step: 0.0,
            dropout: Smoother::new(1.0),
            crackle_coef: 0.0,
            aged: false,
            mix: Smoother::new(1.0),
        };
        l.set_sample_rate(DEFAULT_SR);
        l
    }
}

impl Lofi {
    fn update(&mut self) {
        let sr = self.sample_rate;
        let age = self.values.get(P_AGE);
        self.aged = age > 1e-3;
        let lp = BiquadCoefs::lowpass(geometric(AGE_LOW_PASS, age), 0.707, sr);
        let hp = BiquadCoefs::highpass(geometric(AGE_HIGH_PASS, age), 0.707, sr);
        for ch in &mut self.channels {
            ch.low_pass.coefs = lp;
            ch.high_pass.coefs = hp;
        }
        self.mix.set_target(self.values.get(P_MIX));
    }

    /// Nouveau bloc de perte de bits : résolution et décrochage tirés au sort.
    fn roll_block(&mut self, amount: f32) {
        if amount <= 0.0 {
            self.step = 0.0;
            self.dropout.set_target(1.0);
            return;
        }
        let r = 0.5 + 0.5 * self.dice.next_sample();
        let bits = MAX_BITS - (MAX_BITS - MIN_BITS) * amount * r;
        self.step = 2.0 / bits.exp2();
        let drop = 0.5 + 0.5 * self.dice.next_sample() < 0.1 * amount;
        self.dropout.set_target(if drop { 0.25 } else { 1.0 });
    }
}

impl Params for Lofi {
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

impl Effect for Lofi {
    fn kind(&self) -> &'static str {
        "lofi"
    }

    /// Tête de lecture de la bande, au repos.
    fn latency(&self) -> usize {
        self.base_delay - 1
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.base_delay = ((WOW_DEPTH + FLUTTER_DEPTH) * sample_rate).ceil() as usize + 4;
        let max = 2 * self.base_delay + 4;
        self.channels = [Channel::new(max, 0x1234_5679), Channel::new(max, 0x9e37_79b9)];
        for ch in &mut self.channels {
            ch.hiss_filter.set_cutoff(HISS_LOW_CUT, sample_rate);
        }
        self.wow.set_rate(WOW_HZ, sample_rate);
        self.flutter.set_rate(FLUTTER_HZ, sample_rate);
        self.drift.set_cutoff(3.0, sample_rate);
        self.block_len = ((BLOCK_TIME * sample_rate) as usize).max(1);
        self.crackle_coef = decay_coef(CRACKLE_TAU, sample_rate);
        self.dropout.set_time(0.003, sample_rate);
        self.mix.set_time(0.02, sample_rate);
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        self.channels.iter_mut().for_each(Channel::reset);
        self.wow.reset(0.0);
        self.flutter.reset(0.0);
        self.drift.reset();
        self.block_pos = 0;
        self.step = 0.0;
        self.dropout.set_target(1.0);
        self.dropout.snap();
        self.mix.snap();
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let v = &self.values;
        let sr = self.sample_rate;
        let crackle = v.get(P_CRACKLE);
        let crackle_p = crackle * CRACKLE_RATE / sr;
        let hiss = v.get(P_HISS) * db_to_gain(HISS_DB) / WHITE_RMS;
        let wow = v.get(P_WOW) * WOW_DEPTH * sr;
        let flutter = v.get(P_FLUTTER) * FLUTTER_DEPTH * sr;
        let bit_loss = v.get(P_BIT_LOSS);
        let aged = self.aged;
        let base = self.base_delay;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            if self.block_pos == 0 {
                self.roll_block(bit_loss);
            }
            self.block_pos = (self.block_pos + 1) % self.block_len;
            let drift = self.drift.lowpass(self.dice.next_sample());
            let jitter = (DRIFT_SCALE * drift).clamp(-1.0, 1.0);
            let excursion = wow * self.wow.next_sample() + flutter * (0.7 * self.flutter.next_sample() + 0.3 * jitter);
            let delay = base as f32 + excursion;
            let dropout = self.dropout.next_value();
            let mix = self.mix.next_value();
            let step = self.step;
            for (x, ch) in [l, r].into_iter().zip(&mut self.channels) {
                ch.line.write(*x);
                let dry = ch.line.read(base);
                let mut wet = ch.line.read_frac(delay);
                if aged {
                    wet = ch.low_pass.process(ch.high_pass.process(wet));
                }
                wet *= dropout;
                if hiss > 0.0 {
                    wet += hiss * ch.hiss_filter.highpass(ch.hiss.next_sample());
                }
                if crackle_p > 0.0 {
                    let roll = 0.5 + 0.5 * ch.crackle.next_sample();
                    if roll < crackle_p {
                        // Amplitudes très inégales : beaucoup de poussières, quelques gros grains.
                        let a = (0.5 + 0.5 * ch.crackle.next_sample()).powi(4);
                        ch.crackle_env = ch.crackle_env.max(a * CRACKLE_LEVEL * (0.3 + 0.7 * crackle));
                    }
                    wet += ch.crackle_env * ch.crackle.next_sample();
                    ch.crackle_env *= self.crackle_coef;
                }
                if step > 0.0 {
                    wet = (wet / step).round() * step;
                }
                *x = dry * (1.0 - mix) + wet * mix;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use ponzik_dsp::util::gain_to_db;

    use super::*;
    use crate::effects::testing::*;

    fn bare() -> Lofi {
        let mut l = Lofi::default();
        for id in [P_CRACKLE, P_HISS, P_WOW, P_FLUTTER, P_BIT_LOSS, P_AGE] {
            l.set_param(id, 0.0);
        }
        l.reset();
        l
    }

    #[test]
    fn hiss_alone_sets_the_noise_floor() {
        let silence = vec![0.0; 96_000];
        let mut l = bare();
        let (a, b) = run_mono(&mut l, &silence);
        assert!(a.iter().chain(&b).all(|&s| s == 0.0), "tout à zéro : silence");

        let floor = |hiss: f32| {
            let mut l = bare();
            l.set_param(P_HISS, hiss);
            let (a, b) = run_mono(&mut l, &silence);
            (gain_to_db(rms(&a)), gain_to_db(rms(&b)), a, b)
        };
        let (dl, dr, a, b) = floor(1.0);
        assert!((dl - HISS_DB).abs() < 1.5 && (dr - HISS_DB).abs() < 1.5, "{dl} {dr}");
        // Souffle stéréo : deux bruits indépendants.
        let corr = a.iter().zip(&b).map(|(x, y)| x * y).sum::<f32>() / (rms(&a) * rms(&b) * a.len() as f32);
        assert!(corr.abs() < 0.1, "{corr}");
        let (quarter, _, _, _) = floor(0.25);
        assert!((dl - quarter - 12.0).abs() < 1.0, "{quarter}");
    }

    #[test]
    fn bare_settings_only_delay_the_signal() {
        let mut l = bare();
        let x = noise(0.5, 4_800, 3);
        let (y, _) = run_mono(&mut l, &x);
        let d = l.latency();
        for i in d..x.len() {
            assert!((y[i] - x[i - d]).abs() < 1e-6, "{i}");
        }
    }

    #[test]
    fn crackle_and_age_do_their_job() {
        let silence = vec![0.0; 96_000];
        let mut l = bare();
        l.set_param(P_CRACKLE, 1.0);
        let (a, _) = run_mono(&mut l, &silence);
        // Des grains épars : crête nette au-dessus d'un fond presque nul.
        assert!(peak(&a) > 0.05 && peak(&a) > 6.0 * rms(&a), "{} {}", peak(&a), rms(&a));

        let mut l = bare();
        l.set_param(P_AGE, 1.0);
        let (y, _) = run_mono(&mut l, &sine(10_000.0, 0.5, 24_000));
        assert!(peak(&y[4_800..]) < 0.05, "les aigus disparaissent : {}", peak(&y[4_800..]));
    }

    #[test]
    fn stable() {
        assert_stable_at_extremes(|| Box::new(Lofi::default()), 4.0);
    }
}
