use ponzik_dsp::dynamics::EnvelopeFollower;
use ponzik_dsp::smooth::Smoother;
use ponzik_dsp::util::{db_to_gain, decay_coef, gain_to_db};

use super::{DEFAULT_SR, KeyInput, Values};
use crate::Effect;
use crate::param::{ParamInfo, Params, Unit};

const P_THRESHOLD: u32 = 0;
const P_RATIO: u32 = 1;
const P_KNEE: u32 = 2;
const P_DETECTOR: u32 = 3;
const P_ATTACK: u32 = 4;
const P_RELEASE: u32 = 5;
const P_MAKEUP: u32 = 6;
const P_MIX: u32 = 7;

static PARAMS: [ParamInfo; 8] = [
    ParamInfo::new(P_THRESHOLD, "Seuil", -60.0, 0.0, -18.0, Unit::Db).group("Compression"),
    ParamInfo::new(P_RATIO, "Ratio", 1.0, 20.0, 4.0, Unit::Ratio).log().group("Compression"),
    ParamInfo::new(P_KNEE, "Genou", 0.0, 24.0, 6.0, Unit::Db).group("Compression"),
    ParamInfo::choice(P_DETECTOR, "Détection", &["Crête", "RMS"], 0).group("Compression"),
    ParamInfo::new(P_ATTACK, "Attaque", 0.000_1, 0.2, 0.01, Unit::Seconds).log().group("Temps"),
    ParamInfo::new(P_RELEASE, "Relâche", 0.01, 2.0, 0.15, Unit::Seconds).log().group("Temps"),
    ParamInfo::new(P_MAKEUP, "Compensation", 0.0, 24.0, 0.0, Unit::Db).group("Sortie"),
    ParamInfo::new(P_MIX, "Mix", 0.0, 1.0, 1.0, Unit::Percent).group("Sortie"),
];

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

/// Compresseur stéréo lié, détection crête ou RMS, mix parallèle.
///
/// Avec une piste clé branchée, le détecteur écoute la clé au lieu du signal.
pub struct Compressor {
    values: Values<8>,
    key: KeyInput,
    sample_rate: f32,
    rms: EnvelopeFollower,
    attack: f32,
    release: f32,
    reduction: f32,
    makeup: Smoother,
    mix: Smoother,
}

impl Default for Compressor {
    fn default() -> Self {
        let mut c = Self {
            values: Values::new(&PARAMS),
            key: KeyInput::new(),
            sample_rate: DEFAULT_SR,
            rms: EnvelopeFollower::new(0.01, 0.01, DEFAULT_SR),
            attack: 0.0,
            release: 0.0,
            reduction: 0.0,
            makeup: Smoother::new(1.0),
            mix: Smoother::new(1.0),
        };
        c.set_sample_rate(DEFAULT_SR);
        c
    }
}

impl Compressor {
    /// Réduction de gain courante en dB, pour l'afficheur.
    pub fn gain_reduction_db(&self) -> f32 {
        self.reduction
    }

    fn update(&mut self) {
        let sr = self.sample_rate;
        self.attack = decay_coef(self.values.get(P_ATTACK), sr);
        self.release = decay_coef(self.values.get(P_RELEASE), sr);
        self.makeup.set_target(db_to_gain(self.values.get(P_MAKEUP)));
        self.mix.set_target(self.values.get(P_MIX));
    }
}

impl Params for Compressor {
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

impl Effect for Compressor {
    fn kind(&self) -> &'static str {
        "compressor"
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.rms.set_times(0.01, 0.01, sample_rate);
        self.makeup.set_time(0.02, sample_rate);
        self.mix.set_time(0.02, sample_rate);
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        self.rms.reset();
        self.reduction = 0.0;
        self.makeup.snap();
        self.mix.snap();
    }

    fn sidechain(&mut self, left: &[f32], right: &[f32]) {
        self.key.store(left, right);
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let v = &self.values;
        let (threshold, ratio, knee) = (v.get(P_THRESHOLD), v.get(P_RATIO), v.get(P_KNEE));
        let use_rms = v.index(P_DETECTOR) == 1;
        let keyed = self.key.take();
        for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
            let (dl, dr) = if keyed { self.key.at(i) } else { (*l, *r) };
            let level =
                if use_rms { self.rms.process(0.5 * (dl * dl + dr * dr)).sqrt() } else { dl.abs().max(dr.abs()) };
            let target = gain_reduction(gain_to_db(level), threshold, ratio, knee);
            let coef = if target > self.reduction { self.attack } else { self.release };
            self.reduction = target + (self.reduction - target) * coef;
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
    fn static_curve() {
        assert_eq!(gain_reduction(-30.0, -20.0, 4.0, 0.0), 0.0);
        assert!((gain_reduction(0.0, -20.0, 4.0, 0.0) - 15.0).abs() < 1e-4);
        let soft = gain_reduction(-20.0, -20.0, 4.0, 10.0);
        assert!(soft > 0.0 && soft < 3.0);
        assert!((gain_reduction(-10.0, -20.0, 4.0, 10.0) - 7.5).abs() < 1e-4);
    }

    #[test]
    fn reduces_loud_and_leaves_quiet() {
        let mut c = Compressor::default();
        c.set_param(P_THRESHOLD, -20.0);
        c.set_param(P_RATIO, 10.0);
        c.set_param(P_KNEE, 0.0);
        let (l, _) = run_mono(&mut c, &sine(200.0, 1.0, 24_000));
        assert!(gain_to_db(peak(&l[12_000..])) < -15.0, "{}", peak(&l[12_000..]));

        c.reset();
        let quiet = sine(200.0, 0.01, 24_000);
        let (l, _) = run_mono(&mut c, &quiet);
        assert!((peak(&l[12_000..]) - 0.01).abs() < 1e-4);
    }

    #[test]
    fn rms_detector_and_parallel_mix() {
        let mut c = Compressor::default();
        c.set_param(P_THRESHOLD, -30.0);
        c.set_param(P_RATIO, 20.0);
        c.set_param(P_DETECTOR, 1.0);
        let (full, _) = run_mono(&mut c, &sine(200.0, 1.0, 24_000));
        c.set_param(P_MIX, 0.5);
        c.reset();
        let (half, _) = run_mono(&mut c, &sine(200.0, 1.0, 24_000));
        let (pf, ph) = (peak(&full[12_000..]), peak(&half[12_000..]));
        assert!(pf < 0.1 && (ph - (0.5 + 0.5 * pf)).abs() < 0.02, "{pf} {ph}");
    }

    #[test]
    fn key_drives_the_detector() {
        let mut c = Compressor::default();
        c.set_param(P_THRESHOLD, -30.0);
        c.set_param(P_RATIO, 20.0);
        c.set_param(P_KNEE, 0.0);
        let x = sine(200.0, 0.1, 256);
        let run_keyed = |c: &mut Compressor, key: &[f32]| {
            let mut out = 0.0;
            for _ in 0..100 {
                let (mut l, mut r) = (x.clone(), x.clone());
                c.sidechain(key, key);
                c.process(&mut l, &mut r);
                out = peak(&l);
            }
            out
        };
        // Clé forte : le signal (−20 dB, au-dessus du seuil) est fortement réduit.
        let loud = run_keyed(&mut c, &sine(100.0, 1.0, 256));
        assert!(loud < 0.02, "{loud}");
        // Clé silencieuse : il passe intact.
        c.reset();
        let quiet = run_keyed(&mut c, &[0.0; 256]);
        assert!((quiet - 0.1).abs() < 1e-3, "{quiet}");
    }

    #[test]
    fn stable() {
        assert_stable_at_extremes(|| Box::new(Compressor::default()), 100.0);
    }
}
