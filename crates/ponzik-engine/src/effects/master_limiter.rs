//! Limiteur de mastering : dernier maillon de la chaîne du mix.
//!
//! Trois algorithmes : « Transparent » (relâche qui s'allonge sur les passages
//! denses, pour ne pas pomper), « Punchy » (relâche courte, transitoires
//! préservées) et « Agressif » (écrêteur doux suréchantillonné avant le
//! limiteur, relâche très courte). La détection peut mesurer la crête vraie
//! (suréchantillonnage 4×) : les crêtes entre échantillons respectent aussi le
//! plafond, comme l'exige la diffusion.

use ponzik_dsp::delay::DelayLine;
use ponzik_dsp::loudness::TruePeak;
use ponzik_dsp::oversample::{LATENCY as OS_LATENCY, Oversampler2x};
use ponzik_dsp::smooth::Smoother;
use ponzik_dsp::util::{db_to_gain, decay_coef, gain_to_db};

use super::{DEFAULT_SR, OFF_ON, Values};
use crate::Effect;
use crate::param::{ParamInfo, Params, Unit};

const P_ALGORITHM: u32 = 0;
const P_INPUT: u32 = 1;
const P_CEILING: u32 = 2;
const P_RELEASE: u32 = 3;
const P_LOOKAHEAD: u32 = 4;
const P_TRUE_PEAK: u32 = 5;

static PARAMS: [ParamInfo; 6] = [
    ParamInfo::choice(P_ALGORITHM, "Algorithme", &["Transparent", "Punchy", "Agressif"], 0).group("Limiteur"),
    ParamInfo::new(P_INPUT, "Gain d'entrée", 0.0, 24.0, 0.0, Unit::Db).group("Limiteur"),
    ParamInfo::new(P_CEILING, "Plafond", -12.0, 0.0, -1.0, Unit::Db).group("Limiteur"),
    ParamInfo::new(P_RELEASE, "Relâche", 0.001, 1.0, 0.08, Unit::Seconds).log().group("Temps"),
    ParamInfo::new(P_LOOKAHEAD, "Anticipation", MIN_LOOKAHEAD, MAX_LOOKAHEAD, 0.002, Unit::Seconds).group("Temps"),
    ParamInfo::choice(P_TRUE_PEAK, "Crête vraie", OFF_ON, 1).group("Détection"),
];

const MIN_LOOKAHEAD: f32 = 0.000_5;
const MAX_LOOKAHEAD: f32 = 0.005;
/// Échantillons qui contribuent à une valeur interpolée du détecteur de crête vraie.
const TP_SPAN: usize = 12;
/// Début du genou de l'écrêteur « Agressif », en fraction du plafond.
const CLIP_KNEE: f32 = 0.7;
/// Temps d'intégration de la densité du programme (algorithme transparent).
const DENSITY_TIME: f32 = 0.5;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Algorithm {
    Transparent,
    Punchy,
    Aggressive,
}

const ALGORITHMS: [Algorithm; 3] = [Algorithm::Transparent, Algorithm::Punchy, Algorithm::Aggressive];

impl Algorithm {
    fn release_scale(self) -> f32 {
        match self {
            Algorithm::Transparent => 1.0,
            Algorithm::Punchy => 0.5,
            Algorithm::Aggressive => 0.25,
        }
    }
}

/// Écrêteur doux : linéaire jusqu'à `CLIP_KNEE × c`, tend vers `c` sans le dépasser.
#[inline]
fn soft_clip_at(x: f32, c: f32) -> f32 {
    let knee = CLIP_KNEE * c;
    let a = x.abs();
    if a <= knee { x } else { (knee + (c - knee) * ((a - knee) / (c - knee)).tanh()).copysign(x) }
}

/// Minimum glissant sur une fenêtre réglable (file monotone, capacité fixe).
struct SlidingMin {
    vals: Vec<f32>,
    stamps: Vec<u64>,
    head: usize,
    len: usize,
    window: u64,
    now: u64,
}

impl SlidingMin {
    /// Alloue : à appeler hors du thread audio.
    fn new(max_window: usize) -> Self {
        let cap = max_window + 1;
        Self { vals: vec![0.0; cap], stamps: vec![0; cap], head: 0, len: 0, window: max_window as u64, now: 0 }
    }

    fn set_window(&mut self, window: usize) {
        self.window = (window as u64).clamp(1, self.vals.len() as u64 - 1);
    }

    fn clear(&mut self) {
        self.len = 0;
    }

    #[inline]
    fn push(&mut self, v: f32) -> f32 {
        let cap = self.vals.len();
        while self.len > 0 && self.vals[(self.head + self.len - 1) % cap] >= v {
            self.len -= 1;
        }
        while self.len > 0 && self.stamps[self.head] + self.window <= self.now {
            self.head = (self.head + 1) % cap;
            self.len -= 1;
        }
        let back = (self.head + self.len) % cap;
        self.vals[back] = v;
        self.stamps[back] = self.now;
        self.len += 1;
        self.now += 1;
        self.vals[self.head]
    }
}

struct Channel {
    os: Oversampler2x,
    /// Signal direct retardé comme l'écrêteur, quand celui-ci est inactif.
    dry: [f32; OS_LATENCY],
    tp: TruePeak,
    delay: DelayLine,
}

impl Channel {
    fn new(max_delay: usize) -> Self {
        Self {
            os: Oversampler2x::default(),
            dry: [0.0; OS_LATENCY],
            tp: TruePeak::new(),
            delay: DelayLine::new(max_delay),
        }
    }

    fn reset(&mut self) {
        self.os.reset();
        self.dry = [0.0; OS_LATENCY];
        self.tp.reset();
        self.delay.clear();
    }
}

/// Limiteur de mastering à anticipation, crête vraie et trois algorithmes.
///
/// Le gain atteint la valeur requise avant que la crête ne sorte, et reste tenu
/// sur tous les échantillons qui forment une crête entre échantillons.
pub struct MasterLimiter {
    values: Values<6>,
    sample_rate: f32,
    algorithm: Algorithm,
    channels: [Channel; 2],
    dry_pos: usize,
    /// Longueur de la rampe d'attaque (anticipation), en échantillons.
    ramp_len: usize,
    min: SlidingMin,
    ramp: Vec<f32>,
    ramp_pos: usize,
    ramp_sum: f64,
    env: f32,
    gain: f32,
    density: f32,
    density_coef: f32,
    release: f32,
    input: Smoother,
}

impl Default for MasterLimiter {
    fn default() -> Self {
        let mut l = Self {
            values: Values::new(&PARAMS),
            sample_rate: DEFAULT_SR,
            algorithm: Algorithm::Transparent,
            channels: [Channel::new(2), Channel::new(2)],
            dry_pos: 0,
            ramp_len: 2,
            min: SlidingMin::new(2),
            ramp: vec![1.0; 2],
            ramp_pos: 0,
            ramp_sum: 2.0,
            env: 1.0,
            gain: 1.0,
            density: 0.0,
            density_coef: 0.0,
            release: 0.1,
            input: Smoother::new(1.0),
        };
        l.set_sample_rate(DEFAULT_SR);
        l
    }
}

impl MasterLimiter {
    /// Latence introduite, en échantillons : écrêteur, détecteur et anticipation.
    pub fn latency(&self) -> usize {
        OS_LATENCY + self.ramp_len + TP_SPAN - 1
    }

    /// Réduction de gain courante en dB (positive), pour l'afficheur.
    pub fn gain_reduction_db(&self) -> f32 {
        -gain_to_db(self.gain)
    }

    fn update(&mut self) {
        let v = &self.values;
        self.algorithm = ALGORITHMS[v.index(P_ALGORITHM).min(ALGORITHMS.len() - 1)];
        self.release = v.get(P_RELEASE) * self.algorithm.release_scale();
        self.input.set_target(db_to_gain(v.get(P_INPUT)));
        let n = ((v.get(P_LOOKAHEAD) * self.sample_rate).round() as usize).clamp(2, self.ramp.len());
        if n != self.ramp_len {
            // Nouvelle rampe remplie du gain courant : pas de saut.
            self.ramp_len = n;
            self.ramp[..n].fill(self.gain);
            self.ramp_pos = 0;
            self.ramp_sum = self.gain as f64 * n as f64;
            self.min.set_window(n + TP_SPAN);
        }
    }
}

impl Params for MasterLimiter {
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

impl Effect for MasterLimiter {
    fn kind(&self) -> &'static str {
        "master_limiter"
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        let max_n = ((MAX_LOOKAHEAD * sample_rate).round() as usize).max(2);
        self.channels = [Channel::new(max_n + TP_SPAN + 2), Channel::new(max_n + TP_SPAN + 2)];
        self.min = SlidingMin::new(max_n + TP_SPAN);
        self.ramp = vec![1.0; max_n];
        self.ramp_len = 0;
        self.density_coef = decay_coef(DENSITY_TIME, sample_rate);
        self.input.set_time(0.02, sample_rate);
        self.gain = 1.0;
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        self.channels.iter_mut().for_each(Channel::reset);
        self.dry_pos = 0;
        self.min.clear();
        self.ramp.fill(1.0);
        self.ramp_pos = 0;
        self.ramp_sum = self.ramp_len as f64;
        self.env = 1.0;
        self.gain = 1.0;
        self.density = 0.0;
        self.input.snap();
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let ceiling = db_to_gain(self.values.get(P_CEILING));
        let true_peak = self.values.on(P_TRUE_PEAK);
        let algorithm = self.algorithm;
        let clip = algorithm == Algorithm::Aggressive;
        let n = self.ramp_len;
        let inv = 1.0 / n as f64;
        let delay = n + TP_SPAN;
        let sr = self.sample_rate;
        let fixed_release = decay_coef(self.release, sr);
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let g_in = self.input.next_value();
            let pos = self.dry_pos;
            let mut peak = 0.0f32;
            let mut staged = [0.0f32; 2];
            for ((x, ch), out) in [&*l, &*r].into_iter().zip(&mut self.channels).zip(&mut staged) {
                let x = *x * g_in;
                let clipped = ch.os.process(x, |v| soft_clip_at(v, ceiling));
                let dry = std::mem::replace(&mut ch.dry[pos], x);
                let s = if clip { clipped } else { dry };
                let detected = ch.tp.process(s);
                peak = peak.max(if true_peak { detected } else { s.abs() });
                ch.delay.write(s);
                *out = ch.delay.read(delay);
            }
            self.dry_pos = (pos + 1) % OS_LATENCY;

            let required = if peak > ceiling { ceiling / peak } else { 1.0 };
            let release = if algorithm == Algorithm::Transparent {
                // Passage dense : relâche jusqu'à 4× plus longue, le limiteur ne pompe pas.
                self.density = (1.0 - self.env) + (self.density - (1.0 - self.env)) * self.density_coef;
                decay_coef(self.release * (1.0 + 3.0 * self.density.min(1.0)), sr)
            } else {
                fixed_release
            };
            self.env = if required < self.env { required } else { required + (self.env - required) * release };
            // Minimum tenu sur la rampe et toute l'étendue du détecteur, puis moyenne glissante.
            let held = self.min.push(self.env);
            self.ramp_sum += held as f64 - self.ramp[self.ramp_pos] as f64;
            self.ramp[self.ramp_pos] = held;
            self.ramp_pos = (self.ramp_pos + 1) % n;
            let g = (self.ramp_sum * inv).min(1.0) as f32;
            self.gain = g;
            *l = (staged[0] * g).clamp(-ceiling, ceiling);
            *r = (staged[1] * g).clamp(-ceiling, ceiling);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::f32::consts::{FRAC_PI_4, TAU};

    use super::*;
    use crate::effects::testing::*;

    fn true_peak_of(x: &[f32]) -> f32 {
        let mut tp = TruePeak::new();
        x.iter().fold(0.0f32, |m, &s| m.max(tp.process(s)))
    }

    #[test]
    fn sliding_min() {
        let mut m = SlidingMin::new(3);
        let out: Vec<f32> = [5.0, 3.0, 4.0, 6.0, 7.0, 8.0, 1.0].into_iter().map(|v| m.push(v)).collect();
        assert_eq!(out, [5.0, 3.0, 3.0, 3.0, 4.0, 6.0, 1.0]);
    }

    #[test]
    fn true_peak_ceiling_holds_on_inter_sample_peaks() {
        // Sinus à fs/4 déphasé de 45° : les échantillons valent ±0,707 (sous le plafond
        // de −1 dB), mais la crête reconstruite vaut 1.
        let x: Vec<f32> = (0..48_000).map(|n| (TAU * 0.25 * n as f32 + FRAC_PI_4).sin()).collect();
        assert!(peak(&x) < db_to_gain(-1.0) && true_peak_of(&x) > 0.99);
        let ceiling = db_to_gain(-1.0);
        for algorithm in 0..3 {
            let mut lim = MasterLimiter::default();
            lim.set_param(P_ALGORITHM, algorithm as f32);
            lim.set_param(P_CEILING, -1.0);
            lim.reset();
            let (l, _) = run_mono(&mut lim, &x);
            let tp = true_peak_of(&l[4_800..]);
            assert!(tp <= ceiling * 1.005 && tp > 0.9 * ceiling, "algorithme {algorithm} : {}", gain_to_db(tp));
        }
        // Sans la crête vraie, le même signal dépasse le plafond entre les échantillons.
        let mut lim = MasterLimiter::default();
        lim.set_param(P_TRUE_PEAK, 0.0);
        lim.set_param(P_CEILING, -1.0);
        let (l, _) = run_mono(&mut lim, &x);
        assert!(true_peak_of(&l[4_800..]) > 1.05 * ceiling);
    }

    #[test]
    fn true_peak_ceiling_holds_on_loud_program() {
        let ceiling = db_to_gain(-0.5);
        let mut x = sine(3_100.0, 0.7, 96_000);
        for (i, s) in noise(1.0, 96_000, 3).into_iter().enumerate() {
            x[i] += if (i / 3_000) % 2 == 0 { s } else { 0.1 * s };
        }
        for algorithm in 0..3 {
            let mut lim = MasterLimiter::default();
            lim.set_param(P_ALGORITHM, algorithm as f32);
            lim.set_param(P_INPUT, 12.0);
            lim.set_param(P_CEILING, -0.5);
            lim.reset();
            let (l, _) = run_mono(&mut lim, &x);
            let tp = true_peak_of(&l);
            assert!(tp <= ceiling * 1.012, "algorithme {algorithm} : {} dB", gain_to_db(tp));
            assert!(lim.gain_reduction_db() >= 0.0);
        }
    }

    #[test]
    fn quiet_signal_is_only_delayed() {
        for lookahead in [MIN_LOOKAHEAD, 0.002, MAX_LOOKAHEAD] {
            let mut lim = MasterLimiter::default();
            lim.set_param(P_LOOKAHEAD, lookahead);
            lim.reset();
            let x = sine(440.0, 0.25, 4_800);
            let (l, _) = run_mono(&mut lim, &x);
            let d = lim.latency();
            for i in d..x.len() {
                assert!((l[i] - x[i - d]).abs() < 1e-6, "{lookahead} : {i}");
            }
        }
    }

    #[test]
    fn stable() {
        assert_stable_at_extremes(|| Box::new(MasterLimiter::default()), 1.0 + 1e-6);
    }
}
