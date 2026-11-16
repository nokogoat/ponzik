use ponzik_dsp::smooth::Smoother;

use super::{DEFAULT_SR, Values};
use crate::Effect;
use crate::param::{ParamInfo, Params, Unit};

const P_BITS: u32 = 0;
const P_DOWNSAMPLE: u32 = 1;
const P_MIX: u32 = 2;

static PARAMS: [ParamInfo; 3] = [
    ParamInfo::new(P_BITS, "Bits", 1.0, 24.0, 8.0, Unit::None).group("Reduction"),
    ParamInfo::new(P_DOWNSAMPLE, "Downsample", 1.0, 64.0, 1.0, Unit::Ratio).log().group("Reduction"),
    ParamInfo::new(P_MIX, "Mix", 0.0, 1.0, 1.0, Unit::Percent).group("Output"),
];

/// Réduction de résolution (bits fractionnaires) et de fréquence d'échantillonnage
/// (échantillonneur-bloqueur, sans filtre : le repliement est voulu).
pub struct Bitcrusher {
    values: Values<3>,
    phase: f32,
    held: [f32; 2],
    mix: Smoother,
}

impl Default for Bitcrusher {
    fn default() -> Self {
        let mut b = Self { values: Values::new(&PARAMS), phase: 1.0, held: [0.0; 2], mix: Smoother::new(1.0) };
        b.set_sample_rate(DEFAULT_SR);
        b
    }
}

impl Params for Bitcrusher {
    fn params(&self) -> &'static [ParamInfo] {
        &PARAMS
    }

    fn get_param(&self, id: u32) -> f32 {
        self.values.get(id)
    }

    fn set_param(&mut self, id: u32, value: f32) {
        if self.values.set(&PARAMS, id, value) {
            self.mix.set_target(self.values.get(P_MIX));
        }
    }
}

impl Effect for Bitcrusher {
    fn kind(&self) -> &'static str {
        "bitcrusher"
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.mix.set_time(0.02, sample_rate);
        self.mix.set_target(self.values.get(P_MIX));
        self.reset();
    }

    fn reset(&mut self) {
        self.phase = 1.0;
        self.held = [0.0; 2];
        self.mix.snap();
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let levels = (self.values.get(P_BITS) - 1.0).exp2();
        let step = 1.0 / self.values.get(P_DOWNSAMPLE);
        let quantize = |x: f32| (x * levels).round() / levels;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            if self.phase >= 1.0 {
                self.phase -= 1.0;
                self.held = [quantize(*l), quantize(*r)];
            }
            self.phase += step;
            let mix = self.mix.next_value();
            *l = *l * (1.0 - mix) + self.held[0] * mix;
            *r = *r * (1.0 - mix) + self.held[1] * mix;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::testing::*;

    #[test]
    fn quantizes_to_the_bit_grid() {
        let mut b = Bitcrusher::default();
        b.set_param(P_BITS, 4.0);
        let (l, _) = run_mono(&mut b, &noise(0.9, 4_800, 4));
        assert!(l.iter().all(|s| (s * 8.0 - (s * 8.0).round()).abs() < 1e-5));
        assert!(l.iter().any(|s| s.abs() > 0.5));
    }

    #[test]
    fn holds_samples_when_downsampling() {
        let mut b = Bitcrusher::default();
        b.set_param(P_BITS, 24.0);
        b.set_param(P_DOWNSAMPLE, 4.0);
        let x = noise(0.5, 4_800, 6);
        let (l, _) = run_mono(&mut b, &x);
        for (i, chunk) in l.chunks(4).enumerate() {
            assert!(chunk.iter().all(|s| *s == chunk[0]));
            assert!((chunk[0] - x[4 * i]).abs() < 1e-6);
        }
    }

    #[test]
    fn stable() {
        assert_stable_at_extremes(|| Box::new(Bitcrusher::default()), 2.0);
    }
}
