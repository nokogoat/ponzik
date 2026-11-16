use ponzik_dsp::biquad::{Biquad, BiquadCoefs};
use ponzik_dsp::filter::DcBlocker;
use ponzik_dsp::oversample::{LATENCY, Oversampler2x};
use ponzik_dsp::smooth::Smoother;
use ponzik_dsp::util::db_to_gain;

use super::{DEFAULT_SR, Values};
use crate::Effect;
use crate::param::{ParamInfo, Params, Unit};

const P_TYPE: u32 = 0;
const P_DRIVE: u32 = 1;
const P_TONE: u32 = 2;
const P_OUTPUT: u32 = 3;
const P_MIX: u32 = 4;

static PARAMS: [ParamInfo; 5] = [
    ParamInfo::choice(P_TYPE, "Type", &["Soft", "Hard", "Tube", "Wavefold", "Rectifier"], 0).group("Saturation"),
    ParamInfo::new(P_DRIVE, "Drive", 0.0, 36.0, 6.0, Unit::Db).group("Saturation"),
    ParamInfo::new(P_TONE, "Tone", -12.0, 12.0, 0.0, Unit::Db).group("Color"),
    ParamInfo::new(P_OUTPUT, "Output", -24.0, 12.0, 0.0, Unit::Db).group("Output"),
    ParamInfo::new(P_MIX, "Mix", 0.0, 1.0, 1.0, Unit::Percent).group("Output"),
];

/// Pivot de la bascule grave/aigu du réglage « Ton ».
const TILT_HZ: f32 = 1_000.0;
const TUBE_BIAS: f32 = 0.25;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Soft,
    Hard,
    Tube,
    Fold,
    Rectify,
}

const SHAPES: [Shape; 5] = [Shape::Soft, Shape::Hard, Shape::Tube, Shape::Fold, Shape::Rectify];

#[inline]
fn shape(kind: Shape, x: f32) -> f32 {
    match kind {
        Shape::Soft => x.tanh(),
        Shape::Hard => x.clamp(-1.0, 1.0),
        // Asymétrique : harmoniques paires ; la composante continue est retirée ensuite.
        Shape::Tube => ((x + TUBE_BIAS).tanh() - TUBE_BIAS.tanh()) / (1.0 + TUBE_BIAS.tanh()),
        // Repli triangulaire : identité sur [-1, 1], puis rebonds.
        Shape::Fold => 1.0 - ((x + 1.0).rem_euclid(4.0) - 2.0).abs(),
        Shape::Rectify => x.tanh().abs(),
    }
}

struct Channel {
    os: Oversampler2x,
    dc: DcBlocker,
    tilt: [Biquad; 2],
    dry: [f32; LATENCY],
}

/// Saturation suréchantillonnée 2× ; le signal direct est retardé d'autant
/// pour que le mix parallèle reste en phase.
pub struct Saturator {
    values: Values<5>,
    sample_rate: f32,
    channels: [Channel; 2],
    dry_pos: usize,
    drive: Smoother,
    output: Smoother,
    mix: Smoother,
}

impl Default for Saturator {
    fn default() -> Self {
        let channel = || Channel {
            os: Oversampler2x::default(),
            dc: DcBlocker::new(DEFAULT_SR),
            tilt: Default::default(),
            dry: [0.0; LATENCY],
        };
        let mut s = Self {
            values: Values::new(&PARAMS),
            sample_rate: DEFAULT_SR,
            channels: [channel(), channel()],
            dry_pos: 0,
            drive: Smoother::new(1.0),
            output: Smoother::new(1.0),
            mix: Smoother::new(1.0),
        };
        s.set_sample_rate(DEFAULT_SR);
        s
    }
}

impl Saturator {
    fn update(&mut self) {
        let v = &self.values;
        let tone = v.get(P_TONE);
        let low = BiquadCoefs::low_shelf(TILT_HZ, 0.5, -tone / 2.0, self.sample_rate);
        let high = BiquadCoefs::high_shelf(TILT_HZ, 0.5, tone / 2.0, self.sample_rate);
        for ch in &mut self.channels {
            ch.tilt[0].coefs = low;
            ch.tilt[1].coefs = high;
        }
        self.drive.set_target(db_to_gain(v.get(P_DRIVE)));
        self.output.set_target(db_to_gain(v.get(P_OUTPUT)));
        self.mix.set_target(v.get(P_MIX));
    }
}

impl Params for Saturator {
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

impl Effect for Saturator {
    fn kind(&self) -> &'static str {
        "saturator"
    }

    /// Aller-retour du suréchantillonnage 2× (le signal sec est retardé d'autant).
    fn latency(&self) -> usize {
        LATENCY
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        for ch in &mut self.channels {
            ch.dc = DcBlocker::new(sample_rate);
        }
        for s in [&mut self.drive, &mut self.output, &mut self.mix] {
            s.set_time(0.02, sample_rate);
        }
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        for ch in &mut self.channels {
            ch.os.reset();
            ch.dc.reset();
            ch.tilt.iter_mut().for_each(Biquad::reset);
            ch.dry = [0.0; LATENCY];
        }
        for s in [&mut self.drive, &mut self.output, &mut self.mix] {
            s.snap();
        }
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let kind = SHAPES[self.values.index(P_TYPE).min(SHAPES.len() - 1)];
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let drive = self.drive.next_value();
            let out = self.output.next_value();
            let mix = self.mix.next_value();
            let pos = self.dry_pos;
            for (x, ch) in [l, r].into_iter().zip(&mut self.channels) {
                let wet = ch.os.process(*x * drive, |v| shape(kind, v));
                let [low, high] = &mut ch.tilt;
                let wet = high.process(low.process(ch.dc.process(wet)));
                let dry = std::mem::replace(&mut ch.dry[pos], *x);
                *x = dry * (1.0 - mix) + wet * out * mix;
            }
            self.dry_pos = (pos + 1) % LATENCY;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::testing::*;

    #[test]
    fn shapes_are_bounded() {
        for kind in SHAPES {
            for i in -1_000..=1_000 {
                let y = shape(kind, i as f32 * 0.05);
                assert!(y.abs() <= 1.0 + 1e-6, "{y}");
            }
        }
        assert!((shape(Shape::Fold, 0.5) - 0.5).abs() < 1e-6);
        assert!((shape(Shape::Fold, 1.5) - 0.5).abs() < 1e-6);
        assert!(shape(Shape::Tube, 0.0).abs() < 1e-6);
    }

    #[test]
    fn dry_path_is_latency_compensated() {
        let mut s = Saturator::default();
        s.set_param(P_MIX, 0.0);
        s.reset();
        let x = noise(0.5, 2_000, 9);
        let (l, _) = run_mono(&mut s, &x);
        for i in LATENCY..x.len() {
            assert!((l[i] - x[i - LATENCY]).abs() < 1e-6);
        }
    }

    #[test]
    fn drive_compresses_peaks() {
        let mut s = Saturator::default();
        s.set_param(P_DRIVE, 30.0);
        s.reset();
        let (l, _) = run_mono(&mut s, &sine(200.0, 0.8, 9_600));
        let tail = &l[4_800..];
        // Un sinus saturé s'approche du carré : crête ≈ RMS.
        assert!(peak(tail) < 1.2 && peak(tail) / rms(tail) < 1.15, "{} {}", peak(tail), rms(tail));
    }

    #[test]
    fn oversampling_reduces_aliasing() {
        // Écrêtage dur d'un sinus à 7 kHz : l'harmonique 7 (49 kHz) se replie à 1 kHz
        // sans suréchantillonnage ; à 96 kHz elle reste au-dessus de la bande utile.
        let mut s = Saturator::default();
        s.set_param(P_TYPE, 1.0);
        s.set_param(P_DRIVE, 24.0);
        s.reset();
        let x = sine(7_000.0, 0.5, 24_000);
        let (l, _) = run_mono(&mut s, &x);
        let naive: Vec<f32> = x.iter().map(|v| shape(Shape::Hard, v * db_to_gain(24.0))).collect();
        let at = |y: &[f32], hz: f32| {
            let w = std::f32::consts::TAU * hz / SR;
            let (c, s) = y
                .iter()
                .enumerate()
                .fold((0.0, 0.0), |(c, s), (n, v)| (c + v * (w * n as f32).cos(), s + v * (w * n as f32).sin()));
            (c * c + s * s).sqrt() / y.len() as f32
        };
        let (os, raw) = (at(&l[4_800..], 1_000.0), at(&naive[4_800..], 1_000.0));
        assert!(os < 0.3 * raw, "{os} {raw}");
    }

    #[test]
    fn stable() {
        assert_stable_at_extremes(|| Box::new(Saturator::default()), 30.0);
    }
}
