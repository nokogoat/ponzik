use ponzik_dsp::delay::{Allpass, DelayLine};
use ponzik_dsp::filter::OnePole;
use ponzik_dsp::smooth::Smoother;
use ponzik_dsp::util::flush_denormal;

use super::{DEFAULT_SR, Values};
use crate::Effect;
use crate::param::{ParamInfo, Params, Unit};

const P_PREDELAY: u32 = 0;
const P_SIZE: u32 = 1;
const P_DECAY: u32 = 2;
const P_DAMPING: u32 = 3;
const P_WIDTH: u32 = 4;
const P_MIX: u32 = 5;

static PARAMS: [ParamInfo; 6] = [
    ParamInfo::new(P_PREDELAY, "Pre-delay", 0.0, 0.25, 0.02, Unit::Seconds).group("Space"),
    ParamInfo::new(P_SIZE, "Size", 0.0, 1.0, 0.6, Unit::Percent).group("Space"),
    ParamInfo::new(P_DECAY, "Decay", 0.1, 20.0, 2.0, Unit::Seconds).log().group("Tail"),
    ParamInfo::new(P_DAMPING, "Damping", 0.0, 1.0, 0.5, Unit::Percent).group("Tail"),
    ParamInfo::new(P_WIDTH, "Width", 0.0, 1.0, 1.0, Unit::Percent).group("Output"),
    ParamInfo::new(P_MIX, "Mix", 0.0, 1.0, 0.3, Unit::Percent).group("Output"),
];

const LINES: usize = 8;
/// Longueurs des lignes à taille maximale (ms), premières entre elles une fois échantillonnées.
const LINE_MS: [f32; LINES] = [43.7, 50.3, 57.1, 63.7, 71.9, 79.3, 87.1, 97.3];
const DIFFUSER_MS: [[f32; 2]; 2] = [[4.77, 3.59], [4.99, 3.73]];
const MAX_PREDELAY: f32 = 0.25;
/// Sorties : deux lignes orthogonales d'une matrice de Hadamard.
const OUT_L: [f32; LINES] = [1.0, -1.0, 1.0, -1.0, 1.0, -1.0, 1.0, -1.0];
const OUT_R: [f32; LINES] = [1.0, 1.0, -1.0, -1.0, 1.0, 1.0, -1.0, -1.0];

fn size_factor(size: f32) -> f32 {
    0.2 + 0.8 * size
}

/// Fréquence où le déclin vaut exactement le réglage « Déclin » (RT60 des médiums).
pub(super) const DECAY_REF_HZ: f32 = 500.0;

/// Fréquence où l'amortissement divise le déclin par deux, pour un réglage 0 à 1.
pub(super) fn damping_hz(damping: f32) -> f32 {
    1_200.0 * (20_000.0f32 / 1_200.0).powf(1.0 - damping)
}

/// Pertes (dB, positives) du passe-bas à un pôle `y += (1 − b)(x − y)` à la pulsation `w`.
fn lowpass_loss(b: f32, w: f32) -> f32 {
    let s = (0.5 * w).sin();
    10.0 * (1.0 + 4.0 * b * s * s / ((1.0 - b) * (1.0 - b))).log10()
}

/// Pôle du passe-bas à un pôle qui perd `loss` dB à la pulsation `w`.
fn lowpass_pole(loss: f32, w: f32) -> f32 {
    let s = (0.5 * w).sin();
    let m = (10f32.powf(0.1 * loss.max(0.0)) - 1.0) / (4.0 * s * s).max(1e-9);
    if m < 1e-7 {
        return m;
    }
    ((2.0 * m + 1.0 - (4.0 * m + 1.0).sqrt()) / (2.0 * m)).clamp(0.0, 0.999)
}

/// Gain et pôle d'amortissement d'une ligne rebouclée (méthode de Jot).
///
/// `loop_db` : perte voulue par passage (dB, négative) pour un RT60 égal au
/// réglage à [`DECAY_REF_HZ`] ; `half_hz` : fréquence où le déclin est moitié
/// moindre (aigus amortis), `None` sans amortissement ; `other_db` : réponse
/// (dB) des autres filtres de la boucle à la fréquence de référence. Les
/// pertes de l'amortissement à la référence sont rendues par le gain, toujours
/// inférieur à 1 : le déclin des médiums ne dépend ni de l'amortissement ni de
/// la longueur des lignes, et les aigus s'éteignent dans le même rapport quel
/// que soit le déclin. À appeler hors de la boucle par échantillon.
pub(super) fn loop_decay(loop_db: f32, half_hz: Option<f32>, other_db: f32, sample_rate: f32) -> (f32, f32) {
    let w_ref = std::f32::consts::TAU * DECAY_REF_HZ / sample_rate;
    let mut g = loop_db - other_db;
    let mut pole = 0.0;
    if let Some(hz) = half_hz.filter(|&hz| hz < 0.45 * sample_rate) {
        let w_half = std::f32::consts::TAU * hz.max(2.0 * DECAY_REF_HZ) / sample_rate;
        // À `half_hz`, la perte totale vaut le double de la perte voulue.
        // Queues très courtes et lignes longues : l'amortissement prendrait plus de la
        // moitié de la perte aux médiums ; on le borne plutôt que de fausser le déclin.
        let max_pole = lowpass_pole(-0.5 * (loop_db - other_db), w_ref);
        for _ in 0..4 {
            pole = lowpass_pole(g - 2.0 * loop_db, w_half).min(max_pole);
            g = loop_db - other_db + lowpass_loss(pole, w_ref);
        }
    }
    // Toujours une perte : la boucle reste stable même si les filtres rognent les médiums.
    (10f32.powf(g.min(0.1 * loop_db) / 20.0), pole)
}

/// Réverbération algorithmique : réseau de 8 lignes rebouclées par une matrice
/// de Householder (orthogonale, donc stable pour tout réglage).
pub struct Reverb {
    values: Values<6>,
    sample_rate: f32,
    predelay: [DelayLine; 2],
    diffusers: [[Allpass; 2]; 2],
    lines: [DelayLine; LINES],
    lengths: [f32; LINES],
    damping: [OnePole; LINES],
    predelay_samples: Smoother,
    size: Smoother,
    mix: Smoother,
}

impl Default for Reverb {
    fn default() -> Self {
        let mut r = Self {
            values: Values::new(&PARAMS),
            sample_rate: DEFAULT_SR,
            predelay: [DelayLine::new(2), DelayLine::new(2)],
            diffusers: std::array::from_fn(|_| std::array::from_fn(|_| Allpass::new(1, 0.6))),
            lines: std::array::from_fn(|_| DelayLine::new(2)),
            lengths: [0.0; LINES],
            damping: Default::default(),
            predelay_samples: Smoother::new(0.0),
            size: Smoother::new(1.0),
            mix: Smoother::new(0.0),
        };
        r.set_sample_rate(DEFAULT_SR);
        r
    }
}

impl Reverb {
    fn update(&mut self) {
        let v = &self.values;
        self.predelay_samples.set_target(v.get(P_PREDELAY) * self.sample_rate);
        self.size.set_target(size_factor(v.get(P_SIZE)));
        self.mix.set_target(v.get(P_MIX));
    }
}

impl Params for Reverb {
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

impl Effect for Reverb {
    fn kind(&self) -> &'static str {
        "reverb"
    }

    fn tail(&self) -> f32 {
        self.values.get(P_PREDELAY)
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        let samples = |ms: f32| (ms * 0.001 * sample_rate).round() as usize;
        let pd = (MAX_PREDELAY * sample_rate) as usize + 4;
        self.predelay = [DelayLine::new(pd), DelayLine::new(pd)];
        self.diffusers = DIFFUSER_MS.map(|ch| ch.map(|ms| Allpass::new(samples(ms), 0.6)));
        self.lengths = LINE_MS.map(|ms| samples(ms) as f32);
        self.lines = self.lengths.map(|len| DelayLine::new(len as usize + 4));
        self.predelay_samples.set_time(0.05, sample_rate);
        self.size.set_time(0.2, sample_rate);
        self.mix.set_time(0.02, sample_rate);
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        self.predelay.iter_mut().for_each(DelayLine::clear);
        self.diffusers.iter_mut().flatten().for_each(Allpass::clear);
        self.lines.iter_mut().for_each(DelayLine::clear);
        self.damping.iter_mut().for_each(OnePole::reset);
        self.predelay_samples.snap();
        self.size.snap();
        self.mix.snap();
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let decay = self.values.get(P_DECAY) * self.sample_rate;
        let width = self.values.get(P_WIDTH);
        let damping = self.values.get(P_DAMPING);
        let half = (damping > 0.0).then(|| damping_hz(damping));
        // Gains de boucle (−60 dB à 500 Hz après « déclin ») calculés pour la taille en début de bloc.
        let factor = self.size.value();
        let mut gains = [0.0; LINES];
        for ((gain, filter), len) in gains.iter_mut().zip(&mut self.damping).zip(self.lengths) {
            let (g, pole) = loop_decay(-60.0 * len * factor / decay, half, 0.0, self.sample_rate);
            *gain = g;
            filter.set_coef(1.0 - pole);
        }
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let pd = self.predelay_samples.next_value();
            let factor = self.size.next_value();
            let mix = self.mix.next_value();
            let [pl, pr] = &mut self.predelay;
            let (il, ir) = (pl.read_frac(pd), pr.read_frac(pd));
            pl.write(*l);
            pr.write(*r);
            let [[dl0, dl1], [dr0, dr1]] = &mut self.diffusers;
            let il = dl1.process(dl0.process(il));
            let ir = dr1.process(dr0.process(ir));

            let mut y = [0.0; LINES];
            let (mut wl, mut wr) = (0.0, 0.0);
            for i in 0..LINES {
                y[i] = self.lines[i].read_frac(self.lengths[i] * factor);
                wl += OUT_L[i] * y[i];
                wr += OUT_R[i] * y[i];
                y[i] = self.damping[i].lowpass(y[i]) * gains[i];
            }
            let household = y.iter().sum::<f32>() * (2.0 / LINES as f32);
            for (i, line) in self.lines.iter_mut().enumerate() {
                let input = if i % 2 == 0 { il } else { ir };
                line.write(flush_denormal(y[i] - household + 0.5 * input));
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

    fn wet() -> Reverb {
        let mut r = Reverb::default();
        r.set_param(P_MIX, 1.0);
        r.set_param(P_PREDELAY, 0.0);
        r.reset();
        r
    }

    #[test]
    fn bounded_with_maximum_decay() {
        let mut r = wet();
        r.set_param(P_DECAY, 20.0);
        r.set_param(P_SIZE, 1.0);
        r.set_param(P_DAMPING, 0.0);
        let x = noise(1.0, 10 * SR as usize, 5);
        let (l, rr) = run_mono(&mut r, &x);
        assert!(l.iter().chain(&rr).all(|s| s.is_finite()));
        assert!(peak(&l) < 30.0 && peak(&rr) < 30.0, "{} {}", peak(&l), peak(&rr));
    }

    #[test]
    fn decay_is_the_rt60() {
        for damping in [0.0, 0.5, 1.0] {
            for size in [0.0, 0.6, 1.0] {
                for decay in [0.3, 1.0, 2.0, 5.0, 20.0] {
                    let mut r = wet();
                    r.set_param(P_DAMPING, damping);
                    r.set_param(P_SIZE, size);
                    r.set_param(P_DECAY, decay);
                    r.reset();
                    let rt = reverb_rt60(&mut r, decay);
                    assert!((rt / decay - 1.0).abs() < 0.1, "amortissement {damping}, taille {size} : {decay} s → {rt} s");
                }
            }
        }
    }

    #[test]
    fn damping_shortens_the_highs_only() {
        let measure = |damping: f32, band: f32| {
            let mut r = wet();
            r.set_param(P_DAMPING, damping);
            r.set_param(P_DECAY, 3.0);
            r.reset();
            let (l, _) = run_mono(&mut r, &impulse((3.3 * SR) as usize));
            rt60(&l, Some(band))
        };
        let (open, damped) = (measure(0.0, 4_000.0), measure(1.0, 4_000.0));
        assert!(damped < 0.4 * open, "{open} → {damped}");
        assert!((measure(1.0, 500.0) / 3.0 - 1.0).abs() < 0.1);
    }

    #[test]
    fn stereo_tail() {
        let mut r = wet();
        let (l, rr) = run_mono(&mut r, &impulse(SR as usize));
        assert!(l.iter().zip(&rr).any(|(a, b)| (a - b).abs() > 1e-4), "pas de stéréo");
    }

    #[test]
    fn dry_when_mix_is_zero() {
        let mut r = Reverb::default();
        r.set_param(P_MIX, 0.0);
        r.reset();
        let x = noise(0.5, 4_800, 2);
        assert_eq!(run_mono(&mut r, &x).0, x);
    }

    #[test]
    fn stable() {
        assert_stable_at_extremes(|| Box::new(Reverb::default()), 30.0);
    }
}
