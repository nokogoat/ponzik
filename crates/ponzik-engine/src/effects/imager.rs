use ponzik_dsp::biquad::Biquad;
use ponzik_dsp::smooth::Smoother;
use ponzik_dsp::util::db_to_gain;

use super::{DEFAULT_SR, Lr4, OFF_ON, ThreeBand, Values, lr4_allpass};
use crate::Effect;
use crate::param::{ParamInfo, Params, Unit};

const P_LOW_WIDTH: u32 = 0;
const P_MID_WIDTH: u32 = 1;
const P_HIGH_WIDTH: u32 = 2;
const P_LOW_CROSS: u32 = 3;
const P_HIGH_CROSS: u32 = 4;
const P_MONO: u32 = 5;
const P_MONO_FREQ: u32 = 6;
const P_OUTPUT: u32 = 7;

static PARAMS: [ParamInfo; 8] = [
    ParamInfo::new(P_LOW_WIDTH, "Largeur graves", 0.0, 2.0, 1.0, Unit::Percent).group("Largeur"),
    ParamInfo::new(P_MID_WIDTH, "Largeur médiums", 0.0, 2.0, 1.0, Unit::Percent).group("Largeur"),
    ParamInfo::new(P_HIGH_WIDTH, "Largeur aigus", 0.0, 2.0, 1.0, Unit::Percent).group("Largeur"),
    ParamInfo::new(P_LOW_CROSS, "Séparation basse", 50.0, 1_000.0, 250.0, Unit::Hz).log().group("Bandes"),
    ParamInfo::new(P_HIGH_CROSS, "Séparation haute", 1_000.0, 15_000.0, 4_000.0, Unit::Hz).log().group("Bandes"),
    ParamInfo::choice(P_MONO, "Graves mono", OFF_ON, 0).group("Mono"),
    ParamInfo::new(P_MONO_FREQ, "Mono sous", 20.0, 500.0, 120.0, Unit::Hz).log().group("Mono"),
    ParamInfo::new(P_OUTPUT, "Sortie", -24.0, 24.0, 0.0, Unit::Db).group("Sortie"),
];

/// Largeur stéréo sur trois bandes, en milieu/côtés : seul le signal latéral est
/// séparé (Linkwitz-Riley 4) et dosé par bande ; le milieu traverse les passe-tout
/// équivalents pour rester en phase. À 100 % partout, l'effet est un simple passe-tout.
pub struct Imager {
    values: Values<8>,
    sample_rate: f32,
    mono_split: Lr4,
    bands: ThreeBand,
    /// Passe-tout du milieu : séparation mono, puis les deux séparations de bandes.
    mid_phase: [Biquad; 3],
    widths: [Smoother; 3],
    mono: Smoother,
    output: Smoother,
}

impl Default for Imager {
    fn default() -> Self {
        let mut i = Self {
            values: Values::new(&PARAMS),
            sample_rate: DEFAULT_SR,
            mono_split: Lr4::default(),
            bands: ThreeBand::default(),
            mid_phase: Default::default(),
            widths: std::array::from_fn(|_| Smoother::new(1.0)),
            mono: Smoother::new(0.0),
            output: Smoother::new(1.0),
        };
        i.set_sample_rate(DEFAULT_SR);
        i
    }
}

impl Imager {
    fn update(&mut self) {
        let (v, sr) = (&self.values, self.sample_rate);
        for (s, id) in self.widths.iter_mut().zip([P_LOW_WIDTH, P_MID_WIDTH, P_HIGH_WIDTH]) {
            s.set_target(v.get(id));
        }
        self.mono.set_target(if v.on(P_MONO) { 1.0 } else { 0.0 });
        self.output.set_target(db_to_gain(v.get(P_OUTPUT)));
        let (mono, low, high) = (v.get(P_MONO_FREQ), v.get(P_LOW_CROSS), v.get(P_HIGH_CROSS));
        self.mono_split.set(mono, sr);
        self.bands.set(low, high, sr);
        for (f, hz) in self.mid_phase.iter_mut().zip([mono, low, high]) {
            f.coefs = lr4_allpass(hz, sr);
        }
    }

    fn smoothers(&mut self) -> impl Iterator<Item = &mut Smoother> {
        [&mut self.mono, &mut self.output].into_iter().chain(&mut self.widths)
    }
}

impl Params for Imager {
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

impl Effect for Imager {
    fn kind(&self) -> &'static str {
        "imager"
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.smoothers().for_each(|s| s.set_time(0.02, sample_rate));
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        self.mono_split.reset();
        self.bands.reset();
        self.mid_phase.iter_mut().for_each(Biquad::reset);
        self.smoothers().for_each(Smoother::snap);
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let mid = self.mid_phase.iter_mut().fold(0.5 * (*l + *r), |x, f| f.process(x));
            let (below, above) = self.mono_split.split(0.5 * (*l - *r));
            let side = above + below * (1.0 - self.mono.next_value());
            let bands = self.bands.split(side);
            let side = bands.iter().zip(&mut self.widths).map(|(b, w)| b * w.next_value()).sum::<f32>();
            let out = self.output.next_value();
            *l = (mid + side) * out;
            *r = (mid - side) * out;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::testing::*;

    fn imager(width: f32) -> Imager {
        let mut i = Imager::default();
        for id in [P_LOW_WIDTH, P_MID_WIDTH, P_HIGH_WIDTH] {
            i.set_param(id, width);
        }
        i.reset();
        i
    }

    #[test]
    fn zero_width_is_mono() {
        let mut i = imager(0.0);
        let (mut l, mut r) = (noise(0.5, 4_800, 1), noise(0.5, 4_800, 2));
        run(&mut i, &mut l, &mut r);
        assert!(l.iter().zip(&r).all(|(a, b)| (a - b).abs() < 1e-6));
    }

    #[test]
    fn full_width_is_transparent() {
        let mut i = imager(1.0);
        // Impulsion à gauche seulement : énergie conservée, aucune fuite à droite.
        let (mut l, mut r) = (impulse(48_000), vec![0.0; 48_000]);
        run(&mut i, &mut l, &mut r);
        let energy: f32 = l.iter().map(|s| s * s).sum();
        assert!((energy - 1.0).abs() < 1e-3, "{energy}");
        assert!(peak(&r) < 1e-4, "{}", peak(&r));
        for hz in [60.0, 1_000.0, 10_000.0] {
            i.reset();
            let (mut l, mut r) = (sine(hz, 0.5, 24_000), vec![0.0; 24_000]);
            run(&mut i, &mut l, &mut r);
            assert!((rms(&l[12_000..]) / rms(&sine(hz, 0.5, 12_000)) - 1.0).abs() < 0.01, "{hz} Hz");
            assert!(rms(&r[12_000..]) < 1e-3, "{hz} Hz");
        }
    }

    #[test]
    fn per_band_width_and_bass_mono() {
        let side_level = |i: &mut Imager, hz: f32| {
            i.reset();
            let mut l = sine(hz, 0.5, 24_000);
            let mut r: Vec<f32> = l.iter().map(|s| -s).collect();
            run(i, &mut l, &mut r);
            rms(&l[12_000..])
        };
        let mut i = imager(1.0);
        i.set_param(P_HIGH_WIDTH, 0.0);
        assert!(side_level(&mut i, 12_000.0) < 0.02);
        assert!(side_level(&mut i, 1_000.0) > 0.34);
        i.set_param(P_HIGH_WIDTH, 2.0);
        assert!(side_level(&mut i, 12_000.0) > 0.68);
        let mut i = imager(1.0);
        i.set_param(P_MONO, 1.0);
        i.set_param(P_MONO_FREQ, 200.0);
        assert!(side_level(&mut i, 40.0) < 0.02);
        assert!(side_level(&mut i, 3_000.0) > 0.34);
    }

    #[test]
    fn stable() {
        assert_stable_at_extremes(|| Box::new(Imager::default()), 100.0);
    }
}
