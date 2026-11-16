//! Pré-écoute d'un échantillon : une voix hors du mixer, ajoutée directement à
//! la sortie (comme l'écoute du navigateur de FL Studio). Elle sert à
//! l'Atelier pour jouer une tranche, balayer la forme d'onde ou boucler une
//! zone, sans toucher au projet.

use std::sync::Arc;

use crate::instruments::common::read_cubic;
use crate::sample::Sample;

/// Fondu d'entrée et de sortie, en secondes.
const FADE: f32 = 0.004;

/// Lecture d'une zone d'un échantillon, en boucle ou non.
pub struct Audition {
    sample: Arc<Sample>,
    end: usize,
    /// Zone rebouclée (début, fin) une fois atteinte.
    looped: Option<(usize, usize)>,
    gain: f32,
    pos: f64,
    /// Niveau du fondu (0 à 1) et sens : vrai pendant l'arrêt.
    fade: f32,
    stopping: bool,
}

impl Audition {
    /// Joue `sample` de `start` à `end` (images) ; `looped` reboucle une zone.
    pub fn new(sample: Arc<Sample>, start: usize, end: usize, looped: Option<(usize, usize)>, gain: f32) -> Self {
        let len = sample.len();
        let end = end.min(len);
        let looped = looped.map(|(a, b)| (a.min(len), b.min(len))).filter(|(a, b)| b > a);
        Self { sample, end, looped, gain, pos: start.min(end) as f64, fade: 0.0, stopping: false }
    }

    /// Position de lecture en images de l'échantillon.
    pub fn position(&self) -> f64 {
        self.pos
    }

    /// Demande l'arrêt : court fondu de sortie, puis la voix se termine.
    pub fn stop(&mut self) {
        self.stopping = true;
    }

    /// Ajoute la voix à `out` (entrelacé, `channels` canaux) ; renvoie faux une fois terminée.
    pub fn render_add(&mut self, out: &mut [f32], channels: usize, sample_rate: f32) -> bool {
        let s = &*self.sample;
        if s.is_empty() || channels == 0 {
            return false;
        }
        let rate = s.sample_rate as f64 / sample_rate.max(1.0) as f64;
        let step = 1.0 / (FADE * sample_rate.max(1.0));
        for frame in out.chunks_mut(channels) {
            let end = self.looped.map_or(self.end, |(_, b)| b);
            if self.pos >= end as f64 - 1.0 {
                match self.looped {
                    Some((a, _)) => self.pos = a as f64,
                    None => return false,
                }
            }
            // Fondu de sortie aussi juste avant la fin : pas de clic.
            let left = (self.end as f64 - self.pos) / rate;
            let closing = self.stopping || (self.looped.is_none() && left < (FADE * sample_rate) as f64);
            self.fade = if closing { (self.fade - step).max(0.0) } else { (self.fade + step).min(1.0) };
            if self.stopping && self.fade <= 0.0 {
                return false;
            }
            let g = self.gain * self.fade;
            let l = read_cubic(&s.left, self.pos) * g;
            let r = read_cubic(&s.right, self.pos) * g;
            if channels == 1 {
                frame[0] += 0.5 * (l + r);
            } else {
                frame[0] += l;
                frame[1] += r;
            }
            self.pos += rate;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp() -> Arc<Sample> {
        let left: Vec<f32> = (0..1_000).map(|i| i as f32 / 1_000.0).collect();
        Arc::new(Sample { name: "rampe".into(), sample_rate: 48_000.0, right: left.clone(), left, markers: Vec::new() })
    }

    #[test]
    fn plays_the_range_then_ends() {
        let mut a = Audition::new(ramp(), 200, 900, None, 1.0);
        let mut out = vec![0.0f32; 2 * 300];
        assert!(a.render_add(&mut out, 2, 48_000.0));
        // Après le fondu d'entrée, on lit la rampe à partir de 200.
        assert!((out[2 * 250] - 0.45).abs() < 1e-3, "{}", out[500]);
        assert_eq!(out[2 * 250], out[2 * 250 + 1]);
        let mut out = vec![0.0f32; 2 * 500];
        assert!(!a.render_add(&mut out, 2, 48_000.0), "la zone est finie");
    }

    #[test]
    fn loops_and_stops_with_a_fade() {
        let mut a = Audition::new(ramp(), 0, 1_000, Some((100, 200)), 1.0);
        let mut out = vec![0.0f32; 4_000];
        assert!(a.render_add(&mut out, 1, 48_000.0));
        assert!((100.0..200.0).contains(&a.position()), "{}", a.position());
        a.stop();
        let mut out = vec![0.0f32; 4_000];
        assert!(!a.render_add(&mut out, 1, 48_000.0));
    }
}
