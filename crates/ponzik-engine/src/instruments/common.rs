//! Briques partagées par les instruments natifs : sinus rapide, portamento,
//! choix de voix, écrêtage de sécurité.

use ponzik_dsp::util::{decay_coef, midi_to_hz};

/// Sinus rapide, `phase` en tours (n'importe quel réel). Erreur < 0,1 %.
#[inline]
pub(crate) fn fast_sin(phase: f32) -> f32 {
    let x = 2.0 * (phase - phase.floor()) - 1.0;
    let y = -4.0 * x * (1.0 - x.abs());
    y + 0.225 * (y * y.abs() - y)
}

/// Écrêtage doux de sortie : linéaire jusqu'à ±1, jamais au-delà de ±2.
#[inline]
pub(crate) fn soft_clip(x: f32) -> f32 {
    let a = x.abs();
    if a <= 1.0 { x } else { (1.0 + (a - 1.0).tanh()).copysign(x) }
}

/// Une voix libre, sinon la plus ancienne (vol de voix).
pub(crate) fn pick_voice(n: usize, active: impl Fn(usize) -> bool, age: impl Fn(usize) -> u64) -> usize {
    (0..n).find(|&i| !active(i)).unwrap_or_else(|| (0..n).min_by_key(|&i| age(i)).unwrap_or(0))
}

/// Coefficient de portamento pour un temps de glissé en secondes (0 = instantané).
pub(crate) fn glide_coef(time: f32, sample_rate: f32) -> f32 {
    if time <= 0.0 { 0.0 } else { decay_coef(time / 3.0, sample_rate) }
}

/// Hauteur glissante, en notes MIDI fractionnaires.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Glide {
    note: f32,
    target: f32,
    hz: f32,
}

impl Default for Glide {
    fn default() -> Self {
        Self { note: 60.0, target: 60.0, hz: midi_to_hz(60.0) }
    }
}

impl Glide {
    pub fn jump(&mut self, note: f32) {
        self.note = note;
        self.target = note;
        self.hz = midi_to_hz(note);
    }

    /// Glisse depuis la hauteur courante.
    pub fn go(&mut self, note: f32) {
        self.target = note;
    }

    pub fn note(&self) -> f32 {
        self.note
    }

    #[cfg(test)]
    pub fn target(&self) -> f32 {
        self.target
    }

    /// Avance d'un échantillon, renvoie la fréquence en Hz.
    #[inline]
    pub fn next(&mut self, coef: f32) -> f32 {
        if self.note != self.target {
            let d = (self.note - self.target) * coef;
            self.note = if d.abs() < 1e-3 { self.target } else { self.target + d };
            self.hz = midi_to_hz(self.note);
        }
        self.hz
    }
}

#[cfg(test)]
pub(crate) mod test_util {
    use crate::Instrument;

    pub fn render(inst: &mut dyn Instrument, frames: usize) -> (Vec<f32>, Vec<f32>) {
        let mut l = vec![0.0; frames];
        let mut r = vec![0.0; frames];
        inst.render(&mut l, &mut r);
        (l, r)
    }

    pub fn peak(inst: &mut dyn Instrument, frames: usize) -> f32 {
        let (l, r) = render(inst, frames);
        l.iter().chain(&r).fold(0.0f32, |m, s| m.max(s.abs()))
    }

    /// Fronts montants : mesure de hauteur.
    pub fn rising_crossings(s: &[f32]) -> usize {
        s.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count()
    }

    /// Paramètres aux extrêmes : la sortie reste finie et bornée.
    pub fn stress(inst: &mut dyn Instrument) {
        inst.set_sample_rate(44_100.0);
        let params = inst.params();
        for pass in 0..4 {
            for (i, p) in params.iter().enumerate() {
                let v = match pass {
                    0 => 1e9,
                    1 => -1e9,
                    _ if (i + pass) % 2 == 0 => p.max,
                    _ => p.min,
                };
                inst.set_param(p.id, v);
                let got = inst.get_param(p.id);
                assert!(got >= p.min - 1e-3 && got <= p.max + 1e-3, "{} : {got} hors bornes", p.name);
            }
            for n in [0u8, 36, 60, 96, 127, 60] {
                inst.note_on(n, 1.0);
            }
            let check = |inst: &mut dyn Instrument, frames| {
                let (l, r) = render(inst, frames);
                for s in l.iter().chain(&r) {
                    assert!(s.is_finite() && s.abs() < 4.0, "passe {pass} : {s}");
                }
            };
            check(inst, 8_192);
            for n in [0u8, 36, 60, 96, 127] {
                inst.note_off(n);
            }
            check(inst, 4_096);
            inst.all_notes_off();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_sin_is_close() {
        for i in -100..100 {
            let p = i as f32 * 0.0137;
            assert!((fast_sin(p) - (p * std::f32::consts::TAU).sin()).abs() < 2e-3, "{p}");
        }
    }

    #[test]
    fn soft_clip_is_bounded_and_transparent() {
        assert_eq!(soft_clip(0.5), 0.5);
        assert_eq!(soft_clip(-1.0), -1.0);
        assert!(soft_clip(1e6) <= 2.0 && soft_clip(-1e6) >= -2.0);
    }

    #[test]
    fn glide_reaches_target() {
        let mut g = Glide::default();
        g.jump(48.0);
        g.go(60.0);
        let coef = glide_coef(0.1, 1_000.0);
        let mid = (0..10).map(|_| g.next(coef)).last().unwrap();
        assert!(mid > midi_to_hz(48.0) && mid < midi_to_hz(60.0));
        for _ in 0..1_000 {
            g.next(coef);
        }
        assert_eq!(g.note(), 60.0);
        assert!((g.next(0.0) - midi_to_hz(60.0)).abs() < 1e-3);
    }
}
