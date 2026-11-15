use ponzik_dsp::delay::{Allpass, DelayLine};
use ponzik_dsp::filter::OnePole;
use ponzik_dsp::lfo::{Lfo, LfoShape};
use ponzik_dsp::smooth::Smoother;
use ponzik_dsp::util::flush_denormal;

use super::pitch::GrainShifter;
use super::{DEFAULT_SR, Values};
use crate::Effect;
use crate::param::{ParamInfo, Params, Unit};

const P_SIZE: u32 = 0;
const P_DECAY: u32 = 1;
const P_SHIMMER: u32 = 2;
const P_DAMPING: u32 = 3;
const P_MODULATION: u32 = 4;
const P_MIX: u32 = 5;

static PARAMS: [ParamInfo; 6] = [
    ParamInfo::new(P_SIZE, "Taille", 0.0, 1.0, 0.8, Unit::Percent).group("Espace"),
    ParamInfo::new(P_DECAY, "Déclin", 0.5, 20.0, 5.0, Unit::Seconds).log().group("Queue"),
    ParamInfo::new(P_SHIMMER, "Shimmer", 0.0, 1.0, 0.5, Unit::Percent).group("Queue"),
    ParamInfo::new(P_DAMPING, "Amortissement", 0.0, 1.0, 0.4, Unit::Percent).group("Queue"),
    ParamInfo::new(P_MODULATION, "Modulation", 0.0, 1.0, 0.3, Unit::Percent).group("Queue"),
    ParamInfo::new(P_MIX, "Mix", 0.0, 1.0, 0.35, Unit::Percent).group("Sortie"),
];

const LINES: usize = 8;
/// Lignes plus longues que celles de la réverb : espace plus vaste.
const LINE_MS: [f32; LINES] = [61.3, 69.7, 78.1, 87.3, 95.9, 104.3, 113.9, 124.7];
const DIFFUSER_MS: [[f32; 2]; 2] = [[4.77, 3.59], [4.99, 3.73]];
const LFO_HZ: [f32; LINES] = [0.13, 0.21, 0.29, 0.37, 0.43, 0.53, 0.61, 0.71];
/// Excursion maximale de la modulation des lignes.
const MOD_DEPTH: f32 = 0.002;
const SHIFT_WINDOW: f32 = 0.1;
const OUT_L: [f32; LINES] = [1.0, -1.0, 1.0, -1.0, 1.0, -1.0, 1.0, -1.0];
const OUT_R: [f32; LINES] = [1.0, 1.0, -1.0, -1.0, 1.0, 1.0, -1.0, -1.0];

fn size_factor(size: f32) -> f32 {
    0.3 + 0.7 * size
}

/// Saturation douce bornée à ±1 : garde la boucle transposée sous contrôle.
#[inline]
fn soft_clip(x: f32) -> f32 {
    x / (1.0 + x.abs())
}

/// Réverbération scintillante : le réseau à 8 lignes de la réverb (matrice de
/// Householder), lignes modulées, et une transposition d'une octave vers le haut
/// réinjectée dans la boucle ; chaque passage monte d'une octave.
pub struct Shimmer {
    values: Values<6>,
    sample_rate: f32,
    diffusers: [[Allpass; 2]; 2],
    lines: [DelayLine; LINES],
    lengths: [f32; LINES],
    damping: [OnePole; LINES],
    lfos: [Lfo; LINES],
    shifter: GrainShifter,
    /// Coupe-bas de la boucle transposée.
    shift_hp: OnePole,
    size: Smoother,
    shimmer: Smoother,
    modulation: Smoother,
    mix: Smoother,
}

impl Default for Shimmer {
    fn default() -> Self {
        let mut s = Self {
            values: Values::new(&PARAMS),
            sample_rate: DEFAULT_SR,
            diffusers: std::array::from_fn(|_| std::array::from_fn(|_| Allpass::new(1, 0.6))),
            lines: std::array::from_fn(|_| DelayLine::new(2)),
            lengths: [0.0; LINES],
            damping: Default::default(),
            lfos: std::array::from_fn(|_| Lfo::new(LfoShape::Sine)),
            shifter: GrainShifter::new(8),
            shift_hp: OnePole::default(),
            size: Smoother::new(1.0),
            shimmer: Smoother::new(0.0),
            modulation: Smoother::new(0.0),
            mix: Smoother::new(0.0),
        };
        s.set_sample_rate(DEFAULT_SR);
        s
    }
}

impl Shimmer {
    fn update(&mut self) {
        let v = &self.values;
        self.size.set_target(size_factor(v.get(P_SIZE)));
        self.shimmer.set_target(v.get(P_SHIMMER));
        self.modulation.set_target(v.get(P_MODULATION) * MOD_DEPTH * self.sample_rate);
        self.mix.set_target(v.get(P_MIX));
        let a = 1.0 - 0.9 * v.get(P_DAMPING);
        self.damping.iter_mut().for_each(|d| d.set_coef(a));
    }

    fn smoothers(&mut self) -> [&mut Smoother; 4] {
        [&mut self.size, &mut self.shimmer, &mut self.modulation, &mut self.mix]
    }
}

impl Params for Shimmer {
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

impl Effect for Shimmer {
    fn kind(&self) -> &'static str {
        "shimmer"
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        let samples = |ms: f32| (ms * 0.001 * sample_rate).round() as usize;
        let modulation = (MOD_DEPTH * sample_rate) as usize;
        self.diffusers = DIFFUSER_MS.map(|ch| ch.map(|ms| Allpass::new(samples(ms), 0.6)));
        self.lengths = LINE_MS.map(|ms| samples(ms) as f32);
        self.lines = self.lengths.map(|len| DelayLine::new(len as usize + modulation + 4));
        for (lfo, hz) in self.lfos.iter_mut().zip(LFO_HZ) {
            lfo.set_rate(hz, sample_rate);
        }
        self.shifter = GrainShifter::new((SHIFT_WINDOW * sample_rate) as usize);
        self.shift_hp.set_cutoff(250.0, sample_rate);
        self.smoothers().into_iter().for_each(|s| s.set_time(0.05, sample_rate));
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        self.diffusers.iter_mut().flatten().for_each(Allpass::clear);
        self.lines.iter_mut().for_each(DelayLine::clear);
        self.damping.iter_mut().for_each(OnePole::reset);
        for (i, lfo) in self.lfos.iter_mut().enumerate() {
            lfo.reset(i as f32 / LINES as f32);
        }
        self.shifter.clear();
        self.shift_hp.reset();
        self.smoothers().into_iter().for_each(Smoother::snap);
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let decay = self.values.get(P_DECAY) * self.sample_rate;
        let factor = self.size.value();
        // L'octave réinjectée ajoute de l'énergie : on raccourcit un peu la boucle en compensation.
        let trim = 1.0 - 0.1 * self.shimmer.value();
        let gains = self.lengths.map(|len| 10f32.powf(-3.0 * len * factor / decay) * trim);
        let window = SHIFT_WINDOW * self.sample_rate;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let factor = self.size.next_value();
            let depth = self.modulation.next_value();
            let shimmer = self.shimmer.next_value();
            let mix = self.mix.next_value();
            let [[dl0, dl1], [dr0, dr1]] = &mut self.diffusers;
            let il = dl1.process(dl0.process(*l));
            let ir = dr1.process(dr0.process(*r));

            let mut y = [0.0; LINES];
            let (mut wl, mut wr) = (0.0, 0.0);
            for i in 0..LINES {
                let d = self.lengths[i] * factor + depth * (1.0 + self.lfos[i].next_sample());
                y[i] = self.lines[i].read_frac(d);
                wl += OUT_L[i] * y[i];
                wr += OUT_R[i] * y[i];
                y[i] = self.damping[i].lowpass(y[i]) * gains[i];
            }
            let (wl, wr) = (wl * 0.25, wr * 0.25);
            let octave = self.shifter.process(0.5 * (wl + wr), 2.0, window);
            let octave = 0.5 * shimmer * soft_clip(self.shift_hp.highpass(octave));

            let household = y.iter().sum::<f32>() * (2.0 / LINES as f32);
            for (i, line) in self.lines.iter_mut().enumerate() {
                let input = if i % 2 == 0 { il } else { ir };
                // Signes alternés comme les sorties : l'octave n'est pas annulée par les prises.
                let sign = if i % 2 == 0 { OUT_L[i] } else { OUT_R[i] };
                line.write(flush_denormal(y[i] - household + 0.5 * input + sign * octave));
            }

            *l += (wl - *l) * mix;
            *r += (wr - *r) * mix;
        }
    }
}

#[cfg(test)]
mod tests {
    use ponzik_dsp::biquad::{Biquad, BiquadCoefs};

    use super::*;
    use crate::effects::testing::*;

    fn wet() -> Shimmer {
        let mut s = Shimmer::default();
        s.set_param(P_MIX, 1.0);
        s.reset();
        s
    }

    #[test]
    fn bounded_at_maximum_settings() {
        let mut s = wet();
        s.set_param(P_DECAY, 20.0);
        s.set_param(P_SIZE, 1.0);
        s.set_param(P_SHIMMER, 1.0);
        s.set_param(P_DAMPING, 0.0);
        s.set_param(P_MODULATION, 1.0);
        s.reset();
        let x = noise(1.0, 10 * SR as usize, 5);
        let (l, r) = run_mono(&mut s, &x);
        assert!(l.iter().chain(&r).all(|v| v.is_finite()));
        assert!(peak(&l) < 30.0 && peak(&r) < 30.0, "{} {}", peak(&l), peak(&r));
        // Après l'arrêt de l'entrée, la queue finit par s'éteindre.
        let (tail, _) = run_mono(&mut s, &vec![0.0; 30 * SR as usize]);
        assert!(
            rms(&tail[tail.len() - 4_800..]) < 0.1 * rms(&l[l.len() - 48_000..]),
            "{}",
            rms(&tail[tail.len() - 4_800..])
        );
    }

    #[test]
    fn shimmer_adds_upper_octave() {
        // Énergie autour de 880 Hz dans la queue d'une sinusoïde à 440 Hz.
        let high_energy = |amount: f32| {
            let mut s = wet();
            s.set_param(P_SHIMMER, amount);
            s.set_param(P_MODULATION, 0.0);
            s.reset();
            let mut x = sine(440.0, 0.5, SR as usize);
            x.extend(vec![0.0; SR as usize]);
            let (l, _) = run_mono(&mut s, &x);
            let mut bp = Biquad::new(BiquadCoefs::bandpass(880.0, 4.0, SR));
            let tail: Vec<f32> = l.iter().map(|&v| bp.process(v)).collect();
            rms(&tail[SR as usize..])
        };
        assert!(high_energy(1.0) > 1.5 * high_energy(0.0), "{} {}", high_energy(1.0), high_energy(0.0));
    }

    #[test]
    fn dry_when_mix_is_zero() {
        let mut s = Shimmer::default();
        s.set_param(P_MIX, 0.0);
        s.reset();
        let x = noise(0.5, 4_800, 2);
        assert_eq!(run_mono(&mut s, &x).0, x);
    }

    #[test]
    fn stable() {
        assert_stable_at_extremes(|| Box::new(Shimmer::default()), 30.0);
    }
}
