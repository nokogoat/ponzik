use ponzik_dsp::util::{db_to_gain, decay_coef};

use super::{DEFAULT_SR, KeyInput, Values};
use crate::Effect;
use crate::param::{ParamInfo, Params, Unit};

const P_THRESHOLD: u32 = 0;
const P_ATTACK: u32 = 1;
const P_HOLD: u32 = 2;
const P_RELEASE: u32 = 3;
const P_RANGE: u32 = 4;

static PARAMS: [ParamInfo; 5] = [
    ParamInfo::new(P_THRESHOLD, "Seuil", -80.0, 0.0, -40.0, Unit::Db).group("Porte"),
    ParamInfo::new(P_ATTACK, "Attaque", 0.000_1, 0.1, 0.001, Unit::Seconds).log().group("Temps"),
    ParamInfo::new(P_HOLD, "Maintien", 0.0, 0.5, 0.05, Unit::Seconds).group("Temps"),
    ParamInfo::new(P_RELEASE, "Relâche", 0.005, 2.0, 0.1, Unit::Seconds).log().group("Temps"),
    ParamInfo::new(P_RANGE, "Plage", -80.0, 0.0, -80.0, Unit::Db).group("Porte"),
];

/// Hystérésis : la porte se referme 4 dB sous le seuil d'ouverture.
const HYSTERESIS: f32 = 0.63;

/// Porte de bruit stéréo liée, avec maintien et atténuation réglable.
///
/// Avec une piste clé branchée, c'est la clé qui ouvre la porte.
pub struct Gate {
    values: Values<5>,
    key: KeyInput,
    sample_rate: f32,
    detector: f32,
    detector_release: f32,
    open: bool,
    hold_left: u32,
    gain: f32,
    attack: f32,
    release: f32,
}

impl Default for Gate {
    fn default() -> Self {
        let mut g = Self {
            values: Values::new(&PARAMS),
            key: KeyInput::new(),
            sample_rate: DEFAULT_SR,
            detector: 0.0,
            detector_release: 0.0,
            open: false,
            hold_left: 0,
            gain: 0.0,
            attack: 0.0,
            release: 0.0,
        };
        g.set_sample_rate(DEFAULT_SR);
        g
    }
}

impl Gate {
    fn floor(&self) -> f32 {
        let range = self.values.get(P_RANGE);
        if range <= -79.9 { 0.0 } else { db_to_gain(range) }
    }

    fn update(&mut self) {
        let sr = self.sample_rate;
        self.attack = decay_coef(self.values.get(P_ATTACK), sr);
        self.release = decay_coef(self.values.get(P_RELEASE), sr);
    }
}

impl Params for Gate {
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

impl Effect for Gate {
    fn kind(&self) -> &'static str {
        "gate"
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.detector_release = decay_coef(0.005, sample_rate);
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        self.detector = 0.0;
        self.open = false;
        self.hold_left = 0;
        self.gain = self.floor();
    }

    fn sidechain(&mut self, left: &[f32], right: &[f32]) {
        self.key.store(left, right);
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let keyed = self.key.take();
        let open_at = db_to_gain(self.values.get(P_THRESHOLD));
        let close_at = open_at * HYSTERESIS;
        let hold = (self.values.get(P_HOLD) * self.sample_rate) as u32;
        let floor = self.floor();
        for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
            let (dl, dr) = if keyed { self.key.at(i) } else { (*l, *r) };
            let level = dl.abs().max(dr.abs());
            self.detector = level.max(self.detector * self.detector_release);
            if self.detector >= open_at {
                self.open = true;
                self.hold_left = hold;
            } else if self.detector < close_at {
                if self.hold_left > 0 {
                    self.hold_left -= 1;
                } else {
                    self.open = false;
                }
            }
            let (target, coef) = if self.open { (1.0, self.attack) } else { (floor, self.release) };
            self.gain = target + (self.gain - target) * coef;
            *l *= self.gain;
            *r *= self.gain;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::testing::*;

    #[test]
    fn passes_loud_and_silences_quiet() {
        let mut g = Gate::default();
        g.set_param(P_THRESHOLD, -30.0);
        let (l, _) = run_mono(&mut g, &sine(200.0, 0.5, 9_600));
        assert!((peak(&l[4_800..]) - 0.5).abs() < 1e-3);
        let (l, _) = run_mono(&mut g, &sine(200.0, 0.01, 48_000));
        assert!(peak(&l[40_000..]) < 1e-5, "{}", peak(&l[40_000..]));
    }

    #[test]
    fn range_and_hold() {
        let mut g = Gate::default();
        g.set_param(P_THRESHOLD, -30.0);
        g.set_param(P_RANGE, -20.0);
        g.set_param(P_HOLD, 0.1);
        g.set_param(P_RELEASE, 0.005);
        let mut x = sine(200.0, 0.5, 4_800);
        x.extend(sine(200.0, 0.01, 24_000));
        let (l, _) = run_mono(&mut g, &x);
        // Pendant le maintien, le signal faible passe encore intact.
        assert!((peak(&l[5_000..8_000]) - 0.01).abs() < 1e-3);
        // Ensuite atténué de 20 dB, pas coupé.
        assert!((peak(&l[20_000..]) - 0.001).abs() < 1e-4, "{}", peak(&l[20_000..]));
    }

    #[test]
    fn key_opens_the_gate() {
        let mut g = Gate::default();
        g.set_param(P_THRESHOLD, -30.0);
        // Signal faible, sous le seuil : seule la clé peut ouvrir.
        let x = sine(200.0, 0.01, 256);
        let run_keyed = |g: &mut Gate, key: &[f32]| {
            let mut out = 0.0;
            for _ in 0..400 {
                let (mut l, mut r) = (x.clone(), x.clone());
                g.sidechain(key, key);
                g.process(&mut l, &mut r);
                out = peak(&l);
            }
            out
        };
        assert!((run_keyed(&mut g, &sine(100.0, 0.5, 256)) - 0.01).abs() < 1e-4);
        assert!(run_keyed(&mut g, &[0.0; 256]) < 1e-5);
    }

    #[test]
    fn stable() {
        assert_stable_at_extremes(|| Box::new(Gate::default()), 2.0);
    }
}
