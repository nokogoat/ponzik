//! Compresseur de bus : la « colle » d'un groupe de pistes ou du mix entier.
//!
//! Trois caractères : « Propre » (crête, genou moyen, temps exacts), « Opto »
//! (RMS, genou large, relâche à deux étages qui dépend du programme) et « VCA »
//! (crête, genou serré, réaction franche). La relâche automatique combine une
//! relâche rapide pour les transitoires et une lente pour la compression tenue.

use ponzik_dsp::biquad::{Biquad, BiquadCoefs};
use ponzik_dsp::dynamics::EnvelopeFollower;
use ponzik_dsp::smooth::Smoother;
use ponzik_dsp::util::{db_to_gain, decay_coef, gain_to_db};

use super::{DEFAULT_SR, KeyInput, OFF_ON, Values};
use crate::Effect;
use crate::param::{ParamInfo, Params, Unit};

const P_MODE: u32 = 0;
const P_THRESHOLD: u32 = 1;
const P_RATIO: u32 = 2;
const P_ATTACK: u32 = 3;
const P_RELEASE: u32 = 4;
const P_AUTO: u32 = 5;
const P_SC_CUT: u32 = 6;
const P_MAKEUP: u32 = 7;
const P_MIX: u32 = 8;

static PARAMS: [ParamInfo; 9] = [
    ParamInfo::choice(P_MODE, "Caractère", &["Propre", "Opto", "VCA"], 0).group("Compression"),
    ParamInfo::new(P_THRESHOLD, "Seuil", -40.0, 0.0, -12.0, Unit::Db).group("Compression"),
    ParamInfo::new(P_RATIO, "Ratio", 1.0, 10.0, 4.0, Unit::Ratio).log().group("Compression"),
    ParamInfo::new(P_ATTACK, "Attaque", 0.000_1, 0.1, 0.01, Unit::Seconds).log().group("Temps"),
    ParamInfo::new(P_RELEASE, "Relâche", 0.05, 2.0, 0.3, Unit::Seconds).log().group("Temps"),
    ParamInfo::choice(P_AUTO, "Relâche auto", OFF_ON, 0).group("Temps"),
    ParamInfo::new(P_SC_CUT, "Coupe-bas détecteur", 20.0, 500.0, 20.0, Unit::Hz).log().group("Détection"),
    ParamInfo::new(P_MAKEUP, "Compensation", 0.0, 24.0, 0.0, Unit::Db).group("Sortie"),
    ParamInfo::new(P_MIX, "Mix", 0.0, 1.0, 1.0, Unit::Percent).group("Sortie"),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Clean,
    Opto,
    Vca,
}

const MODES: [Mode; 3] = [Mode::Clean, Mode::Opto, Mode::Vca];

impl Mode {
    /// Largeur du genou en dB.
    fn knee(self) -> f32 {
        match self {
            Mode::Clean => 6.0,
            Mode::Opto => 12.0,
            Mode::Vca => 2.0,
        }
    }
}

/// Relâche automatique : étage rapide et étage lent (attaque lente, relâche longue).
const AUTO_FAST_RELEASE: f32 = 0.1;
const AUTO_SLOW_ATTACK: f32 = 0.4;
const AUTO_SLOW_RELEASE: f32 = 1.2;
/// L'opto n'attaque jamais plus vite que 10 ms.
const OPTO_MIN_ATTACK: f32 = 0.01;

/// Réduction de gain (dB, positive) d'un niveau `x` dB, genou doux de largeur `knee`.
fn gain_reduction(x: f32, threshold: f32, ratio: f32, knee: f32) -> f32 {
    let over = x - threshold;
    let slope = 1.0 - 1.0 / ratio;
    if 2.0 * over <= -knee {
        0.0
    } else if knee > 0.0 && 2.0 * over.abs() <= knee {
        slope * (over + knee / 2.0).powi(2) / (2.0 * knee)
    } else {
        slope * over
    }
}

/// Un étage de lissage de la réduction (dB) : attaque et relâche en coefficients.
#[derive(Clone, Copy, Default)]
struct Stage {
    gr: f32,
    attack: f32,
    release: f32,
}

impl Stage {
    #[inline]
    fn follow(&mut self, target: f32) -> f32 {
        let coef = if target > self.gr { self.attack } else { self.release };
        self.gr = target + (self.gr - target) * coef;
        self.gr
    }
}

/// Compresseur stéréo lié pour bus et mix, avec relâche automatique.
///
/// Avec une piste clé branchée, le détecteur écoute la clé au lieu du signal.
pub struct BusComp {
    values: Values<9>,
    key: KeyInput,
    sample_rate: f32,
    mode: Mode,
    sc_filter: [Biquad; 2],
    rms: EnvelopeFollower,
    fast: Stage,
    slow: Stage,
    dual: bool,
    reduction: f32,
    makeup: Smoother,
    mix: Smoother,
}

impl Default for BusComp {
    fn default() -> Self {
        let mut c = Self {
            values: Values::new(&PARAMS),
            key: KeyInput::new(),
            sample_rate: DEFAULT_SR,
            mode: Mode::Clean,
            sc_filter: Default::default(),
            rms: EnvelopeFollower::new(0.02, 0.02, DEFAULT_SR),
            fast: Stage::default(),
            slow: Stage::default(),
            dual: false,
            reduction: 0.0,
            makeup: Smoother::new(1.0),
            mix: Smoother::new(1.0),
        };
        c.set_sample_rate(DEFAULT_SR);
        c
    }
}

impl BusComp {
    /// Réduction de gain courante en dB (positive), pour l'afficheur et son historique.
    pub fn gain_reduction_db(&self) -> f32 {
        self.reduction
    }

    fn update(&mut self) {
        let sr = self.sample_rate;
        let v = &self.values;
        self.mode = MODES[v.index(P_MODE).min(MODES.len() - 1)];
        let (attack, release) = (v.get(P_ATTACK), v.get(P_RELEASE));
        let auto = v.on(P_AUTO);
        let coef = |t: f32| decay_coef(t, sr);
        match (self.mode, auto) {
            (Mode::Opto, _) => {
                // Cellule optique : la relâche s'allonge quand la compression dure.
                let attack = attack.max(OPTO_MIN_ATTACK);
                self.fast.attack = coef(attack);
                self.fast.release = coef(release * 0.3);
                self.slow.attack = coef(AUTO_SLOW_ATTACK);
                self.slow.release = coef(release * 3.0);
                self.dual = true;
            }
            (_, true) => {
                self.fast.attack = coef(attack);
                self.fast.release = coef(AUTO_FAST_RELEASE);
                self.slow.attack = coef(AUTO_SLOW_ATTACK.max(attack));
                self.slow.release = coef(AUTO_SLOW_RELEASE);
                self.dual = true;
            }
            (_, false) => {
                self.fast.attack = coef(attack);
                self.fast.release = coef(release);
                self.dual = false;
            }
        }
        let hp = BiquadCoefs::highpass(v.get(P_SC_CUT), 0.707, sr);
        self.sc_filter.iter_mut().for_each(|f| f.coefs = hp);
        self.makeup.set_target(db_to_gain(v.get(P_MAKEUP)));
        self.mix.set_target(v.get(P_MIX));
    }
}

impl Params for BusComp {
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

impl Effect for BusComp {
    fn kind(&self) -> &'static str {
        "bus_comp"
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.rms.set_times(0.02, 0.02, sample_rate);
        self.makeup.set_time(0.02, sample_rate);
        self.mix.set_time(0.02, sample_rate);
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        self.sc_filter.iter_mut().for_each(Biquad::reset);
        self.rms.reset();
        self.fast.gr = 0.0;
        self.slow.gr = 0.0;
        self.reduction = 0.0;
        self.makeup.snap();
        self.mix.snap();
    }

    fn sidechain(&mut self, left: &[f32], right: &[f32]) {
        self.key.store(left, right);
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let v = &self.values;
        let (threshold, ratio) = (v.get(P_THRESHOLD), v.get(P_RATIO));
        let knee = self.mode.knee();
        let use_rms = self.mode == Mode::Opto;
        let keyed = self.key.take();
        for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
            let (kl, kr) = if keyed { self.key.at(i) } else { (*l, *r) };
            let [fl, fr] = &mut self.sc_filter;
            let (dl, dr) = (fl.process(kl), fr.process(kr));
            let level =
                if use_rms { self.rms.process(0.5 * (dl * dl + dr * dr)).sqrt() } else { dl.abs().max(dr.abs()) };
            let target = gain_reduction(gain_to_db(level), threshold, ratio, knee);
            let fast = self.fast.follow(target);
            self.reduction = if self.dual { fast.max(self.slow.follow(target)) } else { fast };
            let mix = self.mix.next_value();
            let g = db_to_gain(-self.reduction) * self.makeup.next_value() * mix + (1.0 - mix);
            *l *= g;
            *r *= g;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::testing::*;

    #[test]
    fn compresses_loud_and_leaves_quiet() {
        for mode in 0..3 {
            let mut c = BusComp::default();
            c.set_param(P_MODE, mode as f32);
            c.set_param(P_THRESHOLD, -20.0);
            c.set_param(P_RATIO, 10.0);
            c.reset();
            let (l, _) = run_mono(&mut c, &sine(200.0, 1.0, 48_000));
            assert!(gain_to_db(peak(&l[36_000..])) < -10.0, "mode {mode} : {}", peak(&l[36_000..]));
            assert!(c.gain_reduction_db() > 10.0);

            c.reset();
            let (l, _) = run_mono(&mut c, &sine(200.0, 0.01, 24_000));
            assert!((peak(&l[12_000..]) - 0.01).abs() < 1e-4, "mode {mode}");
        }
    }

    #[test]
    fn auto_release_depends_on_the_program() {
        // Après un coup bref, la réduction retombe vite ; après une compression tenue, elle se maintient.
        let after = |burst: usize| {
            let mut c = BusComp::default();
            c.set_param(P_THRESHOLD, -30.0);
            c.set_param(P_RATIO, 10.0);
            c.set_param(P_ATTACK, 0.001);
            c.set_param(P_AUTO, 1.0);
            c.reset();
            let mut x = sine(200.0, 1.0, burst);
            x.extend(std::iter::repeat_n(0.0, 14_400));
            run_mono(&mut c, &x);
            c.gain_reduction_db()
        };
        let (short, long) = (after(960), after(96_000));
        assert!(long > 4.0 * short && long > 10.0, "{short} {long}");
    }

    #[test]
    fn stable() {
        assert_stable_at_extremes(|| Box::new(BusComp::default()), 100.0);
    }
}
