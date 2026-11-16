use ponzik_dsp::delay::DelayLine;
use ponzik_dsp::smooth::Smoother;
use ponzik_dsp::util::{db_to_gain, decay_coef};

use super::{DEFAULT_SR, Values};
use crate::Effect;
use crate::param::{ParamInfo, Params, Unit};

const P_INPUT: u32 = 0;
const P_CEILING: u32 = 1;
const P_RELEASE: u32 = 2;

static PARAMS: [ParamInfo; 3] = [
    ParamInfo::new(P_INPUT, "Input Gain", 0.0, 24.0, 0.0, Unit::Db).group("Limiter"),
    ParamInfo::new(P_CEILING, "Ceiling", -24.0, 0.0, -0.3, Unit::Db).group("Limiter"),
    ParamInfo::new(P_RELEASE, "Release", 0.001, 1.0, 0.1, Unit::Seconds).log().group("Limiter"),
];

const LOOKAHEAD: f32 = 0.001_5;

/// Minimum glissant sur une fenêtre fixe (file monotone, sans allocation après création).
struct SlidingMin {
    vals: Vec<f32>,
    stamps: Vec<u64>,
    head: usize,
    len: usize,
    window: u64,
    now: u64,
}

impl SlidingMin {
    fn new(window: usize) -> Self {
        let cap = window + 1;
        Self { vals: vec![0.0; cap], stamps: vec![0; cap], head: 0, len: 0, window: window as u64, now: 0 }
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
        let back = (self.head + self.len) % cap;
        self.vals[back] = v;
        self.stamps[back] = self.now;
        self.len += 1;
        while self.stamps[self.head] + self.window <= self.now {
            self.head = (self.head + 1) % cap;
            self.len -= 1;
        }
        self.now += 1;
        self.vals[self.head]
    }
}

/// Limiteur à anticipation (1,5 ms) : le gain atteint la valeur requise avant
/// que la crête ne sorte, la sortie ne dépasse jamais le plafond.
pub struct Limiter {
    values: Values<3>,
    sample_rate: f32,
    lookahead: usize,
    delay: [DelayLine; 2],
    min: SlidingMin,
    ramp: Vec<f32>,
    ramp_pos: usize,
    ramp_sum: f64,
    env: f32,
    release: f32,
    input: Smoother,
}

impl Default for Limiter {
    fn default() -> Self {
        let mut l = Self {
            values: Values::new(&PARAMS),
            sample_rate: DEFAULT_SR,
            lookahead: 2,
            delay: [DelayLine::new(2), DelayLine::new(2)],
            min: SlidingMin::new(2),
            ramp: vec![1.0; 2],
            ramp_pos: 0,
            ramp_sum: 2.0,
            env: 1.0,
            release: 0.0,
            input: Smoother::new(1.0),
        };
        l.set_sample_rate(DEFAULT_SR);
        l
    }
}

impl Limiter {
    fn update(&mut self) {
        self.release = decay_coef(self.values.get(P_RELEASE), self.sample_rate);
        self.input.set_target(db_to_gain(self.values.get(P_INPUT)));
    }
}

impl Params for Limiter {
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

impl Effect for Limiter {
    fn kind(&self) -> &'static str {
        "limiter"
    }

    /// Anticipation du détecteur.
    fn latency(&self) -> usize {
        self.lookahead - 1
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        let n = ((LOOKAHEAD * sample_rate).round() as usize).max(2);
        self.lookahead = n;
        self.delay = [DelayLine::new(n), DelayLine::new(n)];
        self.min = SlidingMin::new(n);
        self.ramp = vec![1.0; n];
        self.input.set_time(0.02, sample_rate);
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        self.delay.iter_mut().for_each(DelayLine::clear);
        self.min.clear();
        self.ramp.fill(1.0);
        self.ramp_pos = 0;
        self.ramp_sum = self.ramp.len() as f64;
        self.env = 1.0;
        self.input.snap();
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let ceiling = db_to_gain(self.values.get(P_CEILING));
        let n = self.lookahead;
        let inv = 1.0 / n as f64;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let g_in = self.input.next_value();
            let (a, b) = (*l * g_in, *r * g_in);
            let peak = a.abs().max(b.abs());
            let required = if peak > ceiling { ceiling / peak } else { 1.0 };
            // Attaque instantanée, relâche exponentielle : l'enveloppe reste sous le gain requis.
            self.env = if required < self.env { required } else { required + (self.env - required) * self.release };
            let held = self.min.push(self.env);
            // Moyenne glissante du minimum tenu : rampe douce qui atteint le gain voulu à temps.
            self.ramp_sum += held as f64 - self.ramp[self.ramp_pos] as f64;
            self.ramp[self.ramp_pos] = held;
            self.ramp_pos = (self.ramp_pos + 1) % n;
            let g = (self.ramp_sum * inv) as f32;

            let [dl, dr] = &mut self.delay;
            dl.write(a);
            dr.write(b);
            *l = (dl.read(n) * g).clamp(-ceiling, ceiling);
            *r = (dr.read(n) * g).clamp(-ceiling, ceiling);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::testing::*;

    #[test]
    fn sliding_min() {
        let mut m = SlidingMin::new(3);
        let out: Vec<f32> = [5.0, 3.0, 4.0, 6.0, 7.0, 8.0, 1.0].into_iter().map(|v| m.push(v)).collect();
        assert_eq!(out, [5.0, 3.0, 3.0, 3.0, 4.0, 6.0, 1.0]);
    }

    #[test]
    fn ceiling_is_never_exceeded() {
        let mut lim = Limiter::default();
        lim.set_param(P_INPUT, 12.0);
        lim.set_param(P_CEILING, -1.0);
        lim.set_param(P_RELEASE, 0.01);
        let ceiling = db_to_gain(-1.0);
        let mut x = sine(150.0, 0.8, 48_000);
        for (i, s) in noise(1.0, 48_000, 11).into_iter().enumerate() {
            x[i] += if (i / 2_000) % 3 == 0 { s } else { 0.05 * s };
        }
        let (l, r) = run_mono(&mut lim, &x);
        assert!(peak(&l) <= ceiling + 1e-6 && peak(&r) <= ceiling + 1e-6);
        assert!(peak(&l) > 0.8 * ceiling);
        // Un écrêteur laisserait la plupart des échantillons collés au plafond.
        let touching = l.iter().filter(|s| s.abs() >= 0.999 * ceiling).count();
        assert!(touching < x.len() / 20, "{touching}");
    }

    #[test]
    fn quiet_signal_is_only_delayed() {
        let mut lim = Limiter::default();
        let x = sine(440.0, 0.25, 4_800);
        let (l, _) = run_mono(&mut lim, &x);
        let d = lim.latency();
        for i in d..x.len() {
            assert!((l[i] - x[i - d]).abs() < 1e-6);
        }
    }

    #[test]
    fn stable() {
        assert_stable_at_extremes(|| Box::new(Limiter::default()), 1.0 + 1e-6);
    }
}
