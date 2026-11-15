//! Effets natifs de Ponzik.
//!
//! Chaque effet range ses paramètres bruts dans un [`Values`] indexé par id
//! (les ids valent l'indice dans la table `PARAMS`) et recalcule ses
//! coefficients à chaque `set_param`, sans allouer.

mod autofilter;
mod bitcrusher;
mod chorus;
mod compressor;
mod delay;
mod eq;
mod gate;
mod limiter;
mod phaser;
mod reverb;
mod saturator;
mod utility;

pub use autofilter::AutoFilter;
pub use bitcrusher::Bitcrusher;
pub use chorus::Chorus;
pub use compressor::Compressor;
pub use delay::Delay;
pub use eq::Eq;
pub use gate::Gate;
pub use limiter::Limiter;
pub use phaser::Phaser;
pub use reverb::Reverb;
pub use saturator::Saturator;
pub use utility::Utility;

use crate::param::ParamInfo;

pub(crate) const OFF_ON: &[&str] = &["Off", "On"];

/// Fréquence d'échantillonnage en attendant `set_sample_rate`.
pub(crate) const DEFAULT_SR: f32 = 48_000.0;

/// Tempo le plus lent accepté par le moteur : dimensionne les lignes synchronisées.
pub(crate) const MIN_BPM: f32 = 20.0;

/// Valeurs de note pour la synchronisation au tempo ; « P » pointée, « T » triolet.
pub(crate) const DIVISIONS: &[&str] = &[
    "Libre", "1/32 T", "1/32", "1/32 P", "1/16 T", "1/16", "1/16 P", "1/8 T", "1/8", "1/8 P", "1/4 T", "1/4", "1/4 P",
    "1/2 T", "1/2", "1/2 P", "1 mesure",
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
}
