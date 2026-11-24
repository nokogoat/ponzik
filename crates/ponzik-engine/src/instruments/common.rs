//! Briques partagées par les instruments natifs : sinus rapide, portamento,
//! choix de voix, écrêtage de sécurité.

use ponzik_dsp::env::{Adsr, AdsrParams, EnvStage};
use ponzik_dsp::util::{decay_coef, midi_to_hz};

use crate::{EnvProbe, NoteExpression, Ramp};

/// What an envelope's extra parameter (after ADSR) sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EnvField {
    Delay,
    Hold,
    AttackCurve,
    DecayCurve,
    ReleaseCurve,
}

impl EnvField {
    pub fn get(self, p: &AdsrParams) -> f32 {
        match self {
            Self::Delay => p.delay,
            Self::Hold => p.hold,
            Self::AttackCurve => p.attack_curve,
            Self::DecayCurve => p.decay_curve,
            Self::ReleaseCurve => p.release_curve,
        }
    }

    pub fn set(self, p: &mut AdsrParams, v: f32) {
        match self {
            Self::Delay => p.delay = v,
            Self::Hold => p.hold = v,
            Self::AttackCurve => p.attack_curve = v,
            Self::DecayCurve => p.decay_curve = v,
            Self::ReleaseCurve => p.release_curve = v,
        }
    }
}

/// Publishes the envelope of the most recently started voice still sounding
/// (`(started_at, envelope)` pairs), `Idle` when none. Audio thread.
pub(crate) fn publish_env<'a>(probe: &EnvProbe, voices: impl Iterator<Item = (u64, &'a Adsr)>) {
    match voices.filter(|(_, e)| e.is_active()).max_by_key(|(t, _)| *t) {
        Some((_, e)) => probe.publish(e.stage(), e.progress(), e.level()),
        None => probe.publish(EnvStage::Idle, 0.0, 0.0),
    }
}

/// Expression of a voice: tuning (semitones, ramped) and pan.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Expr {
    pub tuning: Ramp,
    pub pan: f32,
}

impl Expr {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn apply(&mut self, expr: NoteExpression, sample_rate: f32) {
        match expr {
            NoteExpression::Tuning { semitones, secs } if semitones.is_finite() => {
                self.tuning.go(semitones.clamp(-96.0, 96.0), secs, sample_rate)
            }
            NoteExpression::Tuning { .. } => {}
            NoteExpression::Pan(p) => self.pan = if p.is_finite() { p.clamp(-1.0, 1.0) } else { 0.0 },
        }
    }
}

/// Frequency ratio of a pitch offset in semitones, computed again only when
/// the offset changes.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Tune {
    off: f32,
    ratio: f32,
}

impl Default for Tune {
    fn default() -> Self {
        Self { off: 0.0, ratio: 1.0 }
    }
}

impl Tune {
    #[inline]
    pub fn ratio(&mut self, off: f32) -> f32 {
        if off != self.off {
            self.off = off;
            self.ratio = (off / 12.0).exp2();
        }
        self.ratio
    }
}

/// Balance gains (left, right) of a pan in [−1, 1]: unity at the center.
#[inline]
pub(crate) fn pan_gains(pan: f32) -> (f32, f32) {
    ((1.0 - pan).min(1.0), (1.0 + pan).min(1.0))
}

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

/// Saturation douce réglable, `amount` dans [0, 1] : transparente à 0, gain
/// petit signal ×2 à 1, sortie toujours dans [−1, 1] au-delà de 0.
#[inline]
pub(crate) fn drive(x: f32, amount: f32) -> f32 {
    if amount <= 0.0 {
        return x;
    }
    let k = 1.0 + 9.0 * amount;
    (k * x).tanh() / k.powf(0.7)
}

/// Interpolation cubique d'Hermite entre `x0` et `x1`, `t` dans [0, 1].
#[inline]
fn hermite(xm1: f32, x0: f32, x1: f32, x2: f32, t: f32) -> f32 {
    let c = (x1 - xm1) * 0.5;
    let v = x0 - x1;
    let w = c + v;
    let a = w + v + (x2 - x0) * 0.5;
    let b = w + a;
    ((a * t - b) * t + c) * t + x0
}

/// Lecture cubique à une position fractionnaire ; les bords sont répétés.
#[inline]
pub(crate) fn read_cubic(buf: &[f32], pos: f64) -> f32 {
    let last = buf.len() as isize - 1;
    let i = pos.floor() as isize;
    let at = |k: isize| buf[k.clamp(0, last) as usize];
    hermite(at(i - 1), at(i), at(i + 1), at(i + 2), (pos - i as f64) as f32)
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

    /// Sets the parameter `name` of `group`.
    pub fn set_named(inst: &mut dyn Instrument, group: &str, name: &str, value: f32) {
        let id = inst.params().iter().find(|p| p.group == group && p.name == name).map(|p| p.id);
        let id = id.unwrap_or_else(|| panic!("no {name} in {group}"));
        inst.set_param(id, value);
        assert_eq!(inst.get_param(id), value, "{group} / {name}");
    }

    /// The envelope of `group`: a 0.1 s delay keeps the note silent that long,
    /// a 0.2 s hold keeps it at full level past a short decay to silence, the
    /// probe sees the note and the curves leave it bounded. 48 kHz.
    pub fn envelope_delay_and_hold(inst: &mut dyn Instrument, group: &str) {
        inst.set_sample_rate(48_000.0);
        let probe = inst.env_probe().expect("an envelope probe");
        set_named(inst, group, "Delay", 0.1);
        inst.note_on(60, 1.0);
        assert_eq!(probe.notes(), 1);
        assert_eq!(peak(inst, 4_700), 0.0, "{group}: the delay lets sound through");
        assert_eq!(probe.read().0, ponzik_dsp::env::EnvStage::Delay);
        assert!(peak(inst, 4_800) > 0.01, "{group}: no sound after the delay");
        inst.all_notes_off();
        set_named(inst, group, "Delay", 0.0);
        set_named(inst, group, "Attack", 0.005);
        set_named(inst, group, "Decay", 0.01);
        set_named(inst, group, "Sustain", 0.0);
        inst.note_on(60, 1.0);
        render(inst, 4_800);
        assert!(peak(inst, 256) < 1e-3, "{group}: still sounding without hold");
        inst.all_notes_off();
        set_named(inst, group, "Hold", 0.2);
        for (name, c) in [("Attack curve", 1.0), ("Decay curve", -1.0), ("Release curve", 1.0)] {
            set_named(inst, group, name, c);
        }
        inst.note_on(60, 1.0);
        render(inst, 4_800);
        assert!(peak(inst, 256) > 0.01, "{group}: the hold did not hold");
        let (l, r) = render(inst, 48_000);
        assert!(l.iter().chain(&r).all(|s| s.is_finite() && s.abs() < 4.0));
        inst.note_off(60);
        render(inst, 48_000);
        assert!(peak(inst, 256) < 1e-3, "{group}: no release");
        inst.all_notes_off();
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
