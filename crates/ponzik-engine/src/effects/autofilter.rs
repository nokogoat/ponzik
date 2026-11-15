use ponzik_dsp::dynamics::EnvelopeFollower;
use ponzik_dsp::filter::{FilterMode, Svf};
use ponzik_dsp::lfo::{Lfo, LfoShape};
use ponzik_dsp::smooth::Smoother;

use super::{DEFAULT_SR, DIVISIONS, Values, division_beats};
use crate::Effect;
use crate::param::{ParamInfo, Params, Unit};

const P_MODE: u32 = 0;
const P_CUTOFF: u32 = 1;
const P_RESONANCE: u32 = 2;
const P_SHAPE: u32 = 3;
const P_SYNC: u32 = 4;
const P_RATE: u32 = 5;
const P_LFO_DEPTH: u32 = 6;
const P_ENV_AMOUNT: u32 = 7;
const P_ENV_ATTACK: u32 = 8;
const P_ENV_RELEASE: u32 = 9;
const P_MIX: u32 = 10;

static PARAMS: [ParamInfo; 11] = [
    ParamInfo::choice(P_MODE, "Type", &["Passe-bas", "Passe-haut", "Passe-bande", "Coupe-bande"], 0).group("Filtre"),
    ParamInfo::new(P_CUTOFF, "Coupure", 20.0, 20_000.0, 1_000.0, Unit::Hz).log().group("Filtre"),
    ParamInfo::new(P_RESONANCE, "Résonance", 0.5, 20.0, 0.707, Unit::None).log().group("Filtre"),
    ParamInfo::choice(P_SHAPE, "Forme", &["Sinus", "Triangle", "Carré", "Aléatoire"], 0).group("LFO"),
    ParamInfo::choice(P_SYNC, "Synchro", DIVISIONS, 0).group("LFO"),
    ParamInfo::new(P_RATE, "Vitesse", 0.01, 20.0, 1.0, Unit::Hz).log().group("LFO"),
    ParamInfo::new(P_LFO_DEPTH, "Profondeur", 0.0, 1.0, 0.0, Unit::Percent).group("LFO"),
    ParamInfo::new(P_ENV_AMOUNT, "Quantité", -1.0, 1.0, 0.0, Unit::Percent).group("Enveloppe"),
    ParamInfo::new(P_ENV_ATTACK, "Attaque", 0.001, 0.5, 0.01, Unit::Seconds).log().group("Enveloppe"),
    ParamInfo::new(P_ENV_RELEASE, "Relâche", 0.01, 2.0, 0.2, Unit::Seconds).log().group("Enveloppe"),
    ParamInfo::new(P_MIX, "Mix", 0.0, 1.0, 1.0, Unit::Percent).group("Sortie"),
];

const MODES: [FilterMode; 4] = [FilterMode::LowPass, FilterMode::HighPass, FilterMode::BandPass, FilterMode::Notch];
const SHAPES: [LfoShape; 4] = [LfoShape::Sine, LfoShape::Triangle, LfoShape::Square, LfoShape::Random];
/// Excursion maximale de la coupure (LFO ou enveloppe), en octaves.
const OCTAVES: f32 = 4.0;

/// Filtre à variables d'état modulé par un LFO (libre ou synchronisé) et par
/// l'enveloppe du signal d'entrée.
pub struct AutoFilter {
    values: Values<11>,
    sample_rate: f32,
    bpm: f32,
    filters: [Svf; 2],
    lfo: Lfo,
    env: EnvelopeFollower,
    cutoff: Smoother,
    mix: Smoother,
}

impl Default for AutoFilter {
    fn default() -> Self {
        let svf = || Svf::new(FilterMode::LowPass, 1_000.0, 0.707, DEFAULT_SR);
        let mut f = Self {
            values: Values::new(&PARAMS),
            sample_rate: DEFAULT_SR,
            bpm: 120.0,
            filters: [svf(), svf()],
            lfo: Lfo::new(LfoShape::Sine),
            env: EnvelopeFollower::new(0.01, 0.2, DEFAULT_SR),
            cutoff: Smoother::new(10.0),
            mix: Smoother::new(1.0),
        };
        f.set_sample_rate(DEFAULT_SR);
        f
    }
}

impl AutoFilter {
    /// Fréquence du LFO en hertz, synchronisée ou libre.
    pub fn lfo_rate(&self) -> f32 {
        match division_beats(self.values.get(P_SYNC)) {
            Some(beats) => self.bpm / 60.0 / beats,
            None => self.values.get(P_RATE),
        }
    }

    fn update(&mut self) {
        let v = &self.values;
        let sr = self.sample_rate;
        let mode = MODES[v.index(P_MODE).min(MODES.len() - 1)];
        self.filters.iter_mut().for_each(|f| f.mode = mode);
        self.lfo.shape = SHAPES[v.index(P_SHAPE).min(SHAPES.len() - 1)];
        self.lfo.set_rate(self.lfo_rate(), sr);
        self.env.set_times(v.get(P_ENV_ATTACK), v.get(P_ENV_RELEASE), sr);
        self.cutoff.set_target(v.get(P_CUTOFF).log2());
        self.mix.set_target(v.get(P_MIX));
    }
}

impl Params for AutoFilter {
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

impl Effect for AutoFilter {
    fn kind(&self) -> &'static str {
        "autofilter"
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.cutoff.set_time(0.02, sample_rate);
        self.mix.set_time(0.02, sample_rate);
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        self.filters.iter_mut().for_each(Svf::reset);
        self.lfo.reset(0.0);
        self.env.reset();
        self.cutoff.snap();
        self.mix.snap();
    }

    fn set_tempo(&mut self, bpm: f32) {
        self.bpm = bpm.clamp(super::MIN_BPM, 999.0);
        self.lfo.set_rate(self.lfo_rate(), self.sample_rate);
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let v = &self.values;
        let (q, lfo_depth, env_amount) = (v.get(P_RESONANCE), v.get(P_LFO_DEPTH), v.get(P_ENV_AMOUNT));
        let sr = self.sample_rate;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let env = self.env.process(l.abs().max(r.abs())).min(1.0);
            let lfo = self.lfo.next_sample();
            let octave = self.cutoff.next_value() + OCTAVES * (lfo * lfo_depth + env * env_amount);
            let hz = octave.exp2().clamp(20.0, 20_000.0);
            let mix = self.mix.next_value();
            for (x, f) in [l, r].into_iter().zip(&mut self.filters) {
                f.set_params(hz, q, sr);
                *x = *x * (1.0 - mix) + f.process(*x) * mix;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::testing::*;

    fn windows(x: &[f32]) -> Vec<f32> {
        x.chunks(2_400).map(rms).collect()
    }

    #[test]
    fn lowpass_cuts_highs() {
        let mut f = AutoFilter::default();
        f.set_param(P_CUTOFF, 500.0);
        let (l, _) = run_mono(&mut f, &sine(5_000.0, 0.5, 9_600));
        assert!(rms(&l[4_800..]) < 0.01);
        f.reset();
        let (l, _) = run_mono(&mut f, &sine(100.0, 0.5, 9_600));
        assert!(rms(&l[4_800..]) > 0.3);
    }

    #[test]
    fn lfo_sweeps_the_cutoff() {
        let mut f = AutoFilter::default();
        f.set_param(P_CUTOFF, 1_000.0);
        f.set_param(P_RATE, 2.0);
        f.set_param(P_LFO_DEPTH, 1.0);
        let (l, _) = run_mono(&mut f, &sine(3_000.0, 0.5, 48_000));
        let w = windows(&l);
        let (lo, hi) = w.iter().fold((f32::MAX, 0.0f32), |(a, b), &x| (a.min(x), b.max(x)));
        assert!(hi > 0.3 && lo < 0.05, "{lo} {hi}");
    }

    #[test]
    fn envelope_opens_the_filter() {
        let x = sine(4_000.0, 0.8, 9_600);
        let mut f = AutoFilter::default();
        f.set_param(P_CUTOFF, 300.0);
        let closed = rms(&run_mono(&mut f, &x).0[4_800..]);
        f.set_param(P_ENV_AMOUNT, 1.0);
        f.reset();
        let open = rms(&run_mono(&mut f, &x).0[4_800..]);
        assert!(open > 10.0 * closed, "{open} {closed}");
    }

    #[test]
    fn tempo_sync_rate() {
        let mut f = AutoFilter::default();
        f.set_param(P_SYNC, 11.0);
        f.set_tempo(120.0);
        assert!((f.lfo_rate() - 2.0).abs() < 1e-6);
        f.set_param(P_SYNC, 16.0);
        assert!((f.lfo_rate() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn stable() {
        assert_stable_at_extremes(|| Box::new(AutoFilter::default()), 100.0);
    }
}
