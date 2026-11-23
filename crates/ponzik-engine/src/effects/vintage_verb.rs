//! Réverbe algorithmique : salle, plaque ou nuage de diffusion, modulation
//! riche et queues jusqu'à une minute.
//!
//! Huit lignes rebouclées par une matrice de Householder (orthogonale), dont
//! les têtes de lecture sont modulées chacune par son propre LFO : la queue
//! bouge au lieu de sonner métallique. Le mode « Nuage » ajoute un passe-tout
//! dans chaque ligne et une longue diffusion d'entrée, pour une attaque floue.
//! « Figer » coupe l'entrée de la queue et la fait tourner sans perte.

use ponzik_dsp::delay::{Allpass, DelayLine};
use ponzik_dsp::filter::OnePole;
use ponzik_dsp::lfo::{Lfo, LfoShape};
use ponzik_dsp::smooth::Smoother;
use ponzik_dsp::util::flush_denormal;

use super::{DEFAULT_SR, OFF_ON, Values};
use crate::Effect;
use crate::param::{ParamInfo, Params, Unit};

const P_MODE: u32 = 0;
const P_PREDELAY: u32 = 1;
const P_SIZE: u32 = 2;
const P_DIFFUSION: u32 = 3;
const P_DECAY: u32 = 4;
const P_DAMPING: u32 = 5;
const P_LOW_CUT: u32 = 6;
const P_FREEZE: u32 = 7;
const P_MOD_DEPTH: u32 = 8;
const P_MOD_RATE: u32 = 9;
const P_WIDTH: u32 = 10;
const P_MIX: u32 = 11;

static PARAMS: [ParamInfo; 12] = [
    ParamInfo::choice(P_MODE, "Character", &["Hall", "Plate", "Cloud"], 0).group("Character"),
    ParamInfo::new(P_PREDELAY, "Pre-delay", 0.0, MAX_PREDELAY, 0.02, Unit::Seconds).group("Space"),
    ParamInfo::new(P_SIZE, "Size", 0.0, 1.0, 0.6, Unit::Percent).group("Space"),
    ParamInfo::new(P_DIFFUSION, "Diffusion", 0.0, 1.0, 0.7, Unit::Percent).group("Space"),
    ParamInfo::new(P_DECAY, "Decay", 0.2, 60.0, 3.0, Unit::Seconds).log().group("Tail"),
    ParamInfo::new(P_DAMPING, "Damping", 1_000.0, 20_000.0, 8_000.0, Unit::Hz).log().group("Tail"),
    ParamInfo::new(P_LOW_CUT, "Low Cut", 20.0, 1_000.0, 80.0, Unit::Hz).log().group("Tail"),
    ParamInfo::choice(P_FREEZE, "Freeze", OFF_ON, 0).group("Tail"),
    ParamInfo::new(P_MOD_DEPTH, "Depth", 0.0, 1.0, 0.4, Unit::Percent).group("Modulation"),
    ParamInfo::new(P_MOD_RATE, "Rate", 0.05, 5.0, 0.6, Unit::Hz).log().group("Modulation"),
    ParamInfo::new(P_WIDTH, "Width", 0.0, 1.0, 1.0, Unit::Percent).group("Output"),
    ParamInfo::new(P_MIX, "Mix", 0.0, 1.0, 0.3, Unit::Percent).group("Output"),
];

const LINES: usize = 8;
const MAX_PREDELAY: f32 = 0.25;
/// Longueurs des lignes à taille nominale (ms) pour chaque caractère.
const LINE_MS: [[f32; LINES]; 3] = [
    [43.7, 50.3, 57.1, 63.7, 71.9, 79.3, 87.1, 97.3],
    [11.3, 13.7, 16.1, 18.9, 21.7, 24.1, 27.3, 30.7],
    [61.3, 73.9, 87.7, 101.3, 117.1, 131.9, 149.3, 167.9],
];
/// Passe-tout dans les lignes (mode « Nuage »), en ms.
const LOOP_DIFFUSER_MS: [f32; LINES] = [5.3, 6.1, 7.3, 8.9, 9.7, 11.3, 12.7, 13.9];
/// Diffuseurs d'entrée (ms) par canal ; les deux derniers servent au seul « Nuage ».
const DIFFUSER_MS: [[f32; 6]; 2] = [[4.77, 3.59, 12.73, 9.31, 31.7, 47.3], [4.99, 3.73, 12.31, 9.67, 33.1, 45.7]];
/// Excursion maximale des têtes de lecture (ms) par caractère.
const MOD_MS: [f32; 3] = [0.6, 0.3, 1.5];
/// Facteur de taille maximal (taille 1).
const MAX_SIZE_FACTOR: f32 = 1.25;
/// Gain de boucle figé : la perte reste sous 1 % en dix secondes.
const FREEZE_GAIN: f32 = 0.999_95;
/// Part de la modulation retirée pendant le gel.
const FREEZE_MOD_CUT: f32 = 0.8;
/// Sorties : deux lignes orthogonales d'une matrice de Hadamard.
const OUT_L: [f32; LINES] = [1.0, -1.0, 1.0, -1.0, 1.0, -1.0, 1.0, -1.0];
const OUT_R: [f32; LINES] = [1.0, 1.0, -1.0, -1.0, 1.0, 1.0, -1.0, -1.0];

fn size_factor(size: f32) -> f32 {
    0.25 + (MAX_SIZE_FACTOR - 0.25) * size
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Hall,
    Plate,
    Cloud,
}

const MODES: [Mode; 3] = [Mode::Hall, Mode::Plate, Mode::Cloud];

/// Réverbération algorithmique modulée à huit lignes, trois caractères et gel de la queue.
pub struct VintageVerb {
    values: Values<12>,
    sample_rate: f32,
    mode: Mode,
    predelay: [DelayLine; 2],
    diffusers: [[Allpass; 6]; 2],
    lines: [DelayLine; LINES],
    loop_diffusers: [Allpass; LINES],
    lfos: [Lfo; LINES],
    damping: [OnePole; LINES],
    low_cut: [OnePole; LINES],
    predelay_samples: Smoother,
    size: Smoother,
    freeze: Smoother,
    mix: Smoother,
}

impl Default for VintageVerb {
    fn default() -> Self {
        let mut r = Self {
            values: Values::new(&PARAMS),
            sample_rate: DEFAULT_SR,
            mode: Mode::Hall,
            predelay: [DelayLine::new(2), DelayLine::new(2)],
            diffusers: std::array::from_fn(|_| std::array::from_fn(|_| Allpass::new(1, 0.6))),
            lines: std::array::from_fn(|_| DelayLine::new(2)),
            loop_diffusers: std::array::from_fn(|_| Allpass::new(1, 0.5)),
            lfos: std::array::from_fn(|_| Lfo::new(LfoShape::Sine)),
            damping: Default::default(),
            low_cut: Default::default(),
            predelay_samples: Smoother::new(0.0),
            size: Smoother::new(1.0),
            freeze: Smoother::new(0.0),
            mix: Smoother::new(0.0),
        };
        r.set_sample_rate(DEFAULT_SR);
        r
    }
}

impl VintageVerb {
    fn update(&mut self) {
        let sr = self.sample_rate;
        let v = &self.values;
        self.mode = MODES[v.index(P_MODE).min(MODES.len() - 1)];
        self.predelay_samples.set_target(v.get(P_PREDELAY) * sr);
        self.size.set_target(size_factor(v.get(P_SIZE)));
        self.freeze.set_target(if v.on(P_FREEZE) { 1.0 } else { 0.0 });
        self.mix.set_target(v.get(P_MIX));
        let diffusion = v.get(P_DIFFUSION);
        let input_gain = match self.mode {
            Mode::Hall => 0.6,
            Mode::Plate => 0.75,
            Mode::Cloud => 0.8,
        } * diffusion;
        self.diffusers.iter_mut().flatten().for_each(|a| a.gain = input_gain);
        self.loop_diffusers.iter_mut().for_each(|a| a.gain = 0.6 * diffusion);
        // La plaque est plus brillante que la salle pour un même réglage.
        let bright = if self.mode == Mode::Plate { 1.5 } else { 1.0 };
        let (damp, cut) = (v.get(P_DAMPING) * bright, v.get(P_LOW_CUT));
        let rate = v.get(P_MOD_RATE);
        for (i, ((d, c), lfo)) in self.damping.iter_mut().zip(&mut self.low_cut).zip(&mut self.lfos).enumerate() {
            d.set_cutoff(damp, sr);
            c.set_cutoff(cut, sr);
            // Vitesses légèrement différentes : les modulations ne se resynchronisent jamais.
            lfo.set_rate(rate * (1.0 + 0.071 * i as f32), sr);
        }
    }
}

impl Params for VintageVerb {
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

impl Effect for VintageVerb {
    fn kind(&self) -> &'static str {
        "vintage_verb"
    }

    fn tail(&self) -> f32 {
        self.values.get(P_PREDELAY)
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        let samples = |ms: f32| ((ms * 0.001 * sample_rate).round() as usize).max(1);
        let pd = (MAX_PREDELAY * sample_rate) as usize + 4;
        self.predelay = [DelayLine::new(pd), DelayLine::new(pd)];
        self.diffusers = DIFFUSER_MS.map(|ch| ch.map(|ms| Allpass::new(samples(ms), 0.6)));
        let max_mod = MOD_MS.iter().fold(0.0f32, |m, &v| m.max(v));
        self.lines = std::array::from_fn(|i| {
            let longest = LINE_MS.iter().fold(0.0f32, |m, mode| m.max(mode[i]));
            DelayLine::new(samples(longest * MAX_SIZE_FACTOR + 2.0 * max_mod) + 8)
        });
        self.loop_diffusers = LOOP_DIFFUSER_MS.map(|ms| Allpass::new(samples(ms), 0.5));
        self.predelay_samples.set_time(0.05, sample_rate);
        self.size.set_time(0.2, sample_rate);
        self.freeze.set_time(0.05, sample_rate);
        self.mix.set_time(0.02, sample_rate);
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        self.predelay.iter_mut().for_each(DelayLine::clear);
        self.diffusers.iter_mut().flatten().for_each(Allpass::clear);
        self.lines.iter_mut().for_each(DelayLine::clear);
        self.loop_diffusers.iter_mut().for_each(Allpass::clear);
        for (i, lfo) in self.lfos.iter_mut().enumerate() {
            lfo.reset(i as f32 / LINES as f32);
        }
        self.damping.iter_mut().chain(&mut self.low_cut).for_each(OnePole::reset);
        for s in [&mut self.predelay_samples, &mut self.size, &mut self.freeze, &mut self.mix] {
            s.snap();
        }
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let sr = self.sample_rate;
        let mode_index = self.mode as usize;
        let cloud = self.mode == Mode::Cloud;
        let diffuser_count = if cloud { 6 } else { 4 };
        let lengths = LINE_MS[mode_index].map(|ms| ms * 0.001 * sr);
        let decay = self.values.get(P_DECAY) * sr;
        let depth = self.values.get(P_MOD_DEPTH) * MOD_MS[mode_index] * 0.001 * sr;
        let width = self.values.get(P_WIDTH);
        // Gains de boucle (−60 dB après « déclin ») calculés pour la taille en début de bloc.
        let factor = self.size.value();
        let gains = lengths.map(|len| 10f32.powf(-3.0 * len * factor / decay));
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let pd = self.predelay_samples.next_value();
            let factor = self.size.next_value();
            let frozen = self.freeze.next_value();
            let mix = self.mix.next_value();
            let [pl, pr] = &mut self.predelay;
            let (mut il, mut ir) = (pl.read_frac(pd), pr.read_frac(pd));
            pl.write(*l);
            pr.write(*r);
            let [dl, dr] = &mut self.diffusers;
            for (a, b) in dl.iter_mut().zip(dr.iter_mut()).take(diffuser_count) {
                il = a.process(il);
                ir = b.process(ir);
            }
            // Figée, la queue n'accepte plus rien ; la modulation se calme, car chaque
            // lecture interpolée entame un peu les aigus et la queue s'userait.
            let feed = 0.5 * (1.0 - frozen);
            let depth = depth * (1.0 - FREEZE_MOD_CUT * frozen);

            let mut y = [0.0; LINES];
            let (mut wl, mut wr) = (0.0, 0.0);
            for i in 0..LINES {
                let wobble = depth * (1.0 + self.lfos[i].next_sample());
                let raw = self.lines[i].read_frac(lengths[i] * factor + wobble + 2.0);
                wl += OUT_L[i] * raw;
                wr += OUT_R[i] * raw;
                let colored = self.low_cut[i].highpass(self.damping[i].lowpass(raw));
                let g = gains[i] + (FREEZE_GAIN - gains[i]) * frozen;
                y[i] = (colored + (raw - colored) * frozen) * g;
                if cloud {
                    y[i] = self.loop_diffusers[i].process(y[i]);
                }
            }
            let household = y.iter().sum::<f32>() * (2.0 / LINES as f32);
            for (i, line) in self.lines.iter_mut().enumerate() {
                let input = if i % 2 == 0 { il } else { ir };
                line.write(flush_denormal(y[i] - household + feed * input));
            }

            let (wl, wr) = (wl * 0.3, wr * 0.3);
            let mid = 0.5 * (wl + wr);
            let side = 0.5 * (wl - wr) * width;
            *l = *l * (1.0 - mix) + (mid + side) * mix;
            *r = *r * (1.0 - mix) + (mid - side) * mix;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::testing::*;

    fn wet(mode: f32) -> VintageVerb {
        let mut r = VintageVerb::default();
        r.set_param(P_MODE, mode);
        r.set_param(P_MIX, 1.0);
        r.set_param(P_PREDELAY, 0.0);
        r.set_param(P_DECAY, 1.0);
        r.reset();
        r
    }

    fn window(x: &[f32], from: f32, to: f32) -> f32 {
        rms(&x[(from * SR) as usize..(to * SR) as usize])
    }

    #[test]
    fn freeze_sustains_the_tail() {
        for mode in 0..3 {
            let mut r = wet(mode as f32);
            let x = noise(0.5, (SR * 0.5) as usize, 8);
            run_mono(&mut r, &x);
            r.set_param(P_FREEZE, 1.0);
            // Pendant le gel, l'entrée ne nourrit plus la queue.
            let (l, _) = run_mono(&mut r, &noise(0.5, (SR * 4.0) as usize, 9));
            let (early, late) = (window(&l, 0.3, 0.8), window(&l, 3.5, 4.0));
            assert!(early > 0.01 && late > 0.8 * early && late < 1.2 * early, "mode {mode} : {early} → {late}");

            // Sans gel, la même queue s'éteint.
            let mut r = wet(mode as f32);
            run_mono(&mut r, &x);
            let (l, _) = run_mono(&mut r, &vec![0.0; (SR * 4.0) as usize]);
            assert!(window(&l, 3.5, 4.0) < 0.01 * window(&l, 0.3, 0.8), "mode {mode}");
        }
    }

    #[test]
    fn tail_decays_at_the_set_rate() {
        let mut r = wet(0.0);
        r.set_param(P_DAMPING, 20_000.0);
        r.set_param(P_LOW_CUT, 20.0);
        r.set_param(P_MOD_DEPTH, 0.0);
        r.reset();
        let (l, rr) = run_mono(&mut r, &impulse(SR as usize));
        let drop = 20.0 * (window(&l, 0.6, 0.65) / window(&l, 0.1, 0.15)).log10();
        assert!((-40.0..-20.0).contains(&drop), "{drop}");
        assert!(l.iter().zip(&rr).any(|(a, b)| (a - b).abs() > 1e-4), "pas de stéréo");
    }

    #[test]
    fn very_long_tails_stay_bounded() {
        let mut r = wet(2.0);
        r.set_param(P_DECAY, 60.0);
        r.set_param(P_SIZE, 1.0);
        r.set_param(P_DAMPING, 20_000.0);
        r.set_param(P_MOD_DEPTH, 1.0);
        let (l, rr) = run_mono(&mut r, &noise(1.0, 10 * SR as usize, 5));
        assert!(l.iter().chain(&rr).all(|s| s.is_finite()));
        assert!(peak(&l) < 30.0 && peak(&rr) < 30.0, "{} {}", peak(&l), peak(&rr));
    }

    #[test]
    fn stable() {
        assert_stable_at_extremes(|| Box::new(VintageVerb::default()), 30.0);
    }
}
