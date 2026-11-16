//! Effets natifs de Ponzik.
//!
//! Chaque effet range ses paramètres bruts dans un [`Values`] indexé par id
//! (les ids valent l'indice dans la table `PARAMS`) et recalcule ses
//! coefficients à chaque `set_param`, sans allouer.

mod autofilter;
mod bitcrusher;
mod bus_comp;
mod char_delay;
mod char_saturation;
mod chorus;
mod compressor;
mod delay;
mod ducker;
mod eq;
mod gate;
mod imager;
mod limiter;
mod lofi;
mod master_limiter;
pub mod mix_assistant;
mod ott;
mod phaser;
mod pitch;
mod pompe;
mod reverb;
mod saturator;
mod shimmer;
mod tranche;
mod transient;
mod utility;
mod vintage_verb;

pub use autofilter::AutoFilter;
pub use bitcrusher::Bitcrusher;
pub use bus_comp::BusComp;
pub use char_delay::CharDelay;
pub use char_saturation::CharSaturation;
pub use chorus::Chorus;
pub use compressor::Compressor;
pub use delay::Delay;
pub use ducker::Ducker;
pub use eq::Eq;
pub use gate::Gate;
pub use imager::Imager;
pub use limiter::Limiter;
pub use lofi::Lofi;
pub use master_limiter::MasterLimiter;
pub use mix_assistant::MixAssistant;
pub use ott::Ott;
pub use phaser::Phaser;
pub use pitch::PitchShifter;
pub use pompe::Pompe;
pub use reverb::Reverb;
pub use saturator::Saturator;
pub use shimmer::Shimmer;
pub use tranche::Tranche;
pub use transient::Transient;
pub use utility::Utility;
pub use vintage_verb::VintageVerb;

use ponzik_dsp::biquad::{Biquad, BiquadCoefs};

use crate::param::ParamInfo;

pub(crate) const OFF_ON: &[&str] = &["Off", "On"];

/// Fréquence d'échantillonnage en attendant `set_sample_rate`.
pub(crate) const DEFAULT_SR: f32 = 48_000.0;

/// Tempo le plus lent accepté par le moteur : dimensionne les lignes synchronisées.
pub(crate) const MIN_BPM: f32 = 20.0;

/// Valeurs de note pour la synchronisation au tempo ; « P » pointée, « T » triolet.
pub(crate) const DIVISIONS: &[&str] = &[
    "Free", "1/32 T", "1/32", "1/32 D", "1/16 T", "1/16", "1/16 D", "1/8 T", "1/8", "1/8 D", "1/4 T", "1/4", "1/4 D",
    "1/2 T", "1/2", "1/2 D", "1 bar",
];

/// Durée d'une division en noires ; `None` pour « Libre ».
pub(crate) fn division_beats(index: f32) -> Option<f32> {
    let i = index.round() as usize;
    match i {
        0 => None,
        16.. => Some(4.0),
        _ => {
            let base = [0.125, 0.25, 0.5, 1.0, 2.0][(i - 1) / 3];
            Some(base * [2.0 / 3.0, 1.0, 1.5][(i - 1) % 3])
        }
    }
}

/// Séparation Linkwitz-Riley d'ordre 4 : graves + aigus = passe-tout d'ordre 2 (Q = 0,707).
#[derive(Clone, Debug, Default)]
pub(crate) struct Lr4 {
    low: [Biquad; 2],
    high: [Biquad; 2],
}

impl Lr4 {
    pub fn set(&mut self, hz: f32, sample_rate: f32) {
        let q = std::f32::consts::FRAC_1_SQRT_2;
        let (lp, hp) = (BiquadCoefs::lowpass(hz, q, sample_rate), BiquadCoefs::highpass(hz, q, sample_rate));
        self.low.iter_mut().for_each(|f| f.coefs = lp);
        self.high.iter_mut().for_each(|f| f.coefs = hp);
    }

    pub fn reset(&mut self) {
        self.low.iter_mut().chain(&mut self.high).for_each(Biquad::reset);
    }

    #[inline]
    pub fn split(&mut self, x: f32) -> (f32, f32) {
        let [l0, l1] = &mut self.low;
        let [h0, h1] = &mut self.high;
        (l1.process(l0.process(x)), h1.process(h0.process(x)))
    }
}

/// Passe-tout équivalent à la somme d'un [`Lr4`] à `hz` : aligne la phase d'un signal non séparé.
pub(crate) fn lr4_allpass(hz: f32, sample_rate: f32) -> BiquadCoefs {
    BiquadCoefs::allpass(hz, std::f32::consts::FRAC_1_SQRT_2, sample_rate)
}

/// Trois bandes Linkwitz-Riley d'ordre 4 ; la somme des bandes est un passe-tout (spectre plat).
#[derive(Clone, Debug, Default)]
pub(crate) struct ThreeBand {
    first: Lr4,
    second: Lr4,
    /// Compense dans les graves la phase de la seconde séparation.
    low_phase: Biquad,
}

impl ThreeBand {
    pub fn set(&mut self, low_hz: f32, high_hz: f32, sample_rate: f32) {
        self.first.set(low_hz, sample_rate);
        self.second.set(high_hz, sample_rate);
        self.low_phase.coefs = lr4_allpass(high_hz, sample_rate);
    }

    pub fn reset(&mut self) {
        self.first.reset();
        self.second.reset();
        self.low_phase.reset();
    }

    /// `[graves, médiums, aigus]`.
    #[inline]
    pub fn split(&mut self, x: f32) -> [f32; 3] {
        let (low, rest) = self.first.split(x);
        let (mid, high) = self.second.split(rest);
        [self.low_phase.process(low), mid, high]
    }
}

/// Plus long bloc de clé gardé ; au-delà, le dernier échantillon est répété.
pub(crate) const MAX_KEY_BLOCK: usize = 4_096;

/// Signal clé reçu par [`crate::Effect::sidechain`], valable pour le seul bloc qui suit.
#[derive(Clone, Debug)]
pub(crate) struct KeyInput {
    l: Vec<f32>,
    r: Vec<f32>,
    len: usize,
    fresh: bool,
}

impl KeyInput {
    pub fn new() -> Self {
        Self { l: vec![0.0; MAX_KEY_BLOCK], r: vec![0.0; MAX_KEY_BLOCK], len: 0, fresh: false }
    }

    /// Copie la clé du bloc (sans allouer).
    pub fn store(&mut self, left: &[f32], right: &[f32]) {
        let n = left.len().min(right.len()).min(MAX_KEY_BLOCK);
        self.l[..n].copy_from_slice(&left[..n]);
        self.r[..n].copy_from_slice(&right[..n]);
        self.len = n;
        self.fresh = true;
    }

    /// Vrai si une clé a été reçue pour ce bloc ; la consomme.
    pub fn take(&mut self) -> bool {
        std::mem::replace(&mut self.fresh, false)
    }

    /// Échantillon `i` de la clé (le dernier au-delà de la fin, silence si vide).
    #[inline]
    pub fn at(&self, i: usize) -> (f32, f32) {
        if self.len == 0 {
            return (0.0, 0.0);
        }
        let i = i.min(self.len - 1);
        (self.l[i], self.r[i])
    }
}

/// Valeurs courantes des paramètres d'un effet.
#[derive(Clone, Debug)]
pub(crate) struct Values<const N: usize>([f32; N]);

impl<const N: usize> Values<N> {
    pub fn new(params: &[ParamInfo; N]) -> Self {
        debug_assert!(params.iter().enumerate().all(|(i, p)| p.id as usize == i));
        Self(std::array::from_fn(|i| params[i].default))
    }

    pub fn get(&self, id: u32) -> f32 {
        self.0.get(id as usize).copied().unwrap_or(0.0)
    }

    /// Range la valeur bornée ; `false` si l'id est inconnu.
    pub fn set(&mut self, params: &[ParamInfo; N], id: u32, value: f32) -> bool {
        let Some(info) = params.get(id as usize) else { return false };
        self.0[id as usize] = info.clamp(value);
        true
    }

    pub fn on(&self, id: u32) -> bool {
        self.get(id) >= 0.5
    }

    pub fn index(&self, id: u32) -> usize {
        self.get(id).round() as usize
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use std::f32::consts::TAU;

    use ponzik_dsp::noise::WhiteNoise;

    use crate::Effect;

    pub const SR: f32 = 48_000.0;

    pub fn sine(hz: f32, amp: f32, frames: usize) -> Vec<f32> {
        (0..frames).map(|n| amp * (TAU * hz * n as f32 / SR).sin()).collect()
    }

    pub fn noise(amp: f32, frames: usize, seed: u32) -> Vec<f32> {
        let mut n = WhiteNoise::new(seed);
        (0..frames).map(|_| amp * n.next_sample()).collect()
    }

    pub fn impulse(frames: usize) -> Vec<f32> {
        let mut v = vec![0.0; frames];
        v[0] = 1.0;
        v
    }

    pub fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|s| s * s).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    pub fn peak(x: &[f32]) -> f32 {
        x.iter().fold(0.0f32, |m, s| m.max(s.abs()))
    }

    /// Traite `l`/`r` par blocs de 256, comme le moteur.
    pub fn run(fx: &mut dyn Effect, l: &mut [f32], r: &mut [f32]) {
        for (cl, cr) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
            fx.process(cl, cr);
        }
    }

    /// Même signal sur les deux canaux ; rend la sortie (gauche, droite).
    pub fn run_mono(fx: &mut dyn Effect, x: &[f32]) -> (Vec<f32>, Vec<f32>) {
        let (mut l, mut r) = (x.to_vec(), x.to_vec());
        run(fx, &mut l, &mut r);
        (l, r)
    }

    /// Toutes les combinaisons extrêmes des paramètres restent finies et bornées.
    pub fn assert_stable_at_extremes(make: fn() -> Box<dyn Effect>, bound: f32) {
        let probe = make();
        let params = probe.params();
        let input = noise(1.0, 9_600, 7);
        for corner in 0..3 {
            let mut fx = make();
            fx.set_sample_rate(SR);
            for (i, p) in params.iter().enumerate() {
                let v = match (corner + i) % 3 {
                    0 => p.min,
                    1 => p.max,
                    _ => p.default,
                };
                fx.set_param(p.id, v);
            }
            fx.set_tempo(240.0);
            let (l, r) = run_mono(fx.as_mut(), &input);
            for s in l.iter().chain(&r) {
                assert!(s.is_finite() && s.abs() < bound, "{} : {s}", fx.kind());
            }
        }
        for p in params {
            let mut fx = make();
            fx.set_sample_rate(SR);
            for v in [p.min, p.max] {
                fx.set_param(p.id, v);
                let (l, r) = run_mono(fx.as_mut(), &input[..2_400]);
                for s in l.iter().chain(&r) {
                    assert!(s.is_finite() && s.abs() < bound, "{} / {} = {v} : {s}", fx.kind(), p.name);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_divisions() {
        assert_eq!(DIVISIONS.len(), 17);
        assert_eq!(division_beats(0.0), None);
        assert_eq!(division_beats(2.0), Some(0.125));
        assert_eq!(division_beats(11.0), Some(1.0));
        assert_eq!(division_beats(12.0), Some(1.5));
        assert!((division_beats(10.0).unwrap() - 2.0 / 3.0).abs() < 1e-6);
        assert_eq!(division_beats(16.0), Some(4.0));
    }

    #[test]
    fn three_bands_sum_to_an_allpass() {
        let mut b = ThreeBand::default();
        b.set(120.0, 2_500.0, testing::SR);
        let y: Vec<f32> = testing::impulse(48_000).iter().map(|&x| b.split(x).iter().sum()).collect();
        let energy: f32 = y.iter().map(|s| s * s).sum();
        assert!((energy - 1.0).abs() < 1e-3, "{energy}");
        for hz in [40.0, 120.0, 800.0, 2_500.0, 9_000.0] {
            b.reset();
            let x = testing::sine(hz, 0.5, 48_000);
            let y: Vec<f32> = x.iter().map(|&x| b.split(x).iter().sum()).collect();
            let ratio = testing::rms(&y[24_000..]) / testing::rms(&x[24_000..]);
            assert!((ratio - 1.0).abs() < 0.01, "{hz} Hz : {ratio}");
        }
    }
}
