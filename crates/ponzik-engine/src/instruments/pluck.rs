//! Cordes pincées : guide d'onde de Karplus-Strong étendu.
//!
//! Chaque voix fait sonner deux cordes très légèrement désaccordées (largeur
//! stéréo). L'excitation est une rafale de bruit d'une période, filtrée selon
//! la brillance et la vélocité, puis passée dans un filtre en peigne qui
//! imite la position du médiator. Dans la boucle : retard fractionnaire à
//! interpolation linéaire et moyenne à deux points (amortissement) ; ces deux
//! filtres ne dépassent jamais un gain de 1, la boucle est donc toujours
//! stable. Une caisse de résonance (trois passe-bandes) colore la sortie.

use ponzik_dsp::biquad::{Biquad, BiquadCoefs};
use ponzik_dsp::filter::OnePole;
use ponzik_dsp::noise::WhiteNoise;
use ponzik_dsp::util::{db_to_gain, flush_denormal, gain_to_db, midi_to_hz};

use super::common::{pick_voice, soft_clip};
use crate::Instrument;
use crate::param::{ParamInfo, Params, Unit};

const VOICES: usize = 8;
/// Fondamentale la plus grave servie par les lignes de retard.
const MIN_HZ: f32 = 8.0;
/// Sous −80 dB, une voix est éteinte.
const SILENCE: f32 = 1e-4;
/// Désaccord maximal de la seconde corde, en cents.
const MAX_DETUNE: f32 = 12.0;
/// Résonances de caisse : (fréquence, Q, gain).
const BODY: [(f32, f32, f32); 3] = [(100.0, 2.5, 1.0), (210.0, 3.0, 0.8), (480.0, 2.0, 0.5)];

const P_BRIGHTNESS: u32 = 0;
const P_DECAY: u32 = 1;
const P_DAMPING: u32 = 2;
const P_PICK: u32 = 3;
const P_BODY: u32 = 4;
const P_WIDTH: u32 = 5;
const P_RELEASE: u32 = 6;
const P_VOLUME: u32 = 7;

static PARAMS: [ParamInfo; 8] = [
    ParamInfo::new(P_BRIGHTNESS, "Brightness", 0.0, 1.0, 0.6, Unit::Percent).group("String"),
    ParamInfo::new(P_DECAY, "Decay", 0.1, 20.0, 3.0, Unit::Seconds).log().group("String"),
    ParamInfo::new(P_DAMPING, "Damping", 0.0, 1.0, 0.4, Unit::Percent).group("String"),
    ParamInfo::new(P_PICK, "Pick Position", 0.02, 0.5, 0.15, Unit::Percent).group("String"),
    ParamInfo::new(P_RELEASE, "Choke", 0.01, 4.0, 0.3, Unit::Seconds).log().group("String"),
    ParamInfo::new(P_BODY, "Body Resonance", 0.0, 1.0, 0.3, Unit::Percent).group("Output"),
    ParamInfo::new(P_WIDTH, "Stereo Width", 0.0, 1.0, 0.3, Unit::Percent).group("Output"),
    ParamInfo::new(P_VOLUME, "Volume", -40.0, 6.0, -6.0, Unit::Db).group("Output"),
];

/// Ligne de retard circulaire à lecture linéaire (gain ≤ 1 à toute fréquence).
struct Line {
    buf: Vec<f32>,
    mask: usize,
    pos: usize,
}

impl Line {
    fn new(max: usize) -> Self {
        let len = (max + 4).next_power_of_two();
        Self { buf: vec![0.0; len], mask: len - 1, pos: 0 }
    }

    #[inline]
    fn write(&mut self, x: f32) {
        self.buf[self.pos] = x;
        self.pos = (self.pos + 1) & self.mask;
    }

    /// Échantillon écrit il y a `d` pas, `d` dans [1, longueur − 2].
    #[inline]
    fn read(&self, d: f32) -> f32 {
        let d = d.clamp(1.0, (self.mask - 1) as f32);
        let i = d as usize;
        let t = d - i as f32;
        let at = |k: usize| self.buf[(self.pos + self.mask + 1 - k) & self.mask];
        at(i) + (at(i + 1) - at(i)) * t
    }
}

struct Strand {
    line: Line,
    /// Retard de boucle, compensé du retard du filtre d'amortissement.
    delay: f32,
    /// Gain par aller-retour (déclin).
    feedback: f32,
    prev: f32,
}

impl Strand {
    fn new(max: usize) -> Self {
        Self { line: Line::new(max), delay: 2.0, feedback: 0.0, prev: 0.0 }
    }

    #[inline]
    fn tick(&mut self, damping: f32) -> f32 {
        let v = self.line.read(self.delay);
        let y = v + (self.prev - v) * damping;
        self.prev = v;
        self.line.write(flush_denormal(y * self.feedback));
        v
    }
}

struct Voice {
    note: u8,
    started_at: u64,
    active: bool,
    released: bool,
    strings: [Strand; 2],
    /// Fondamentales des deux cordes, en Hz.
    hz: [f32; 2],
    /// Suivi de crête pour détecter l'extinction.
    level: f32,
    /// Images avant la première mesure d'extinction (l'excitation doit passer).
    hold: u32,
}

/// Cordes pincées polyphoniques huit voix.
pub struct Pluck {
    brightness: f32,
    decay: f32,
    damping: f32,
    pick: f32,
    body: f32,
    width: f32,
    release: f32,
    gain: f32,
    sample_rate: f32,
    noise: WhiteNoise,
    /// Brouillon d'excitation, alloué avec les lignes.
    scratch: Vec<f32>,
    body_filters: [[Biquad; 3]; 2],
    voices: [Voice; VOICES],
    note_counter: u64,
}

impl Default for Pluck {
    fn default() -> Self {
        Self::new()
    }
}

/// Gain de boucle pour un temps de déclin à −60 dB.
fn loop_gain(t60: f32, hz: f32) -> f32 {
    10f32.powf(-3.0 / (t60.max(1e-3) * hz)).min(0.9999)
}

impl Pluck {
    pub fn new() -> Self {
        let mut p = Self {
            brightness: 0.6,
            decay: 3.0,
            damping: 0.4,
            pick: 0.15,
            body: 0.3,
            width: 0.3,
            release: 0.3,
            gain: 1.0,
            sample_rate: 0.0,
            noise: WhiteNoise::new(0x51_7CC1),
            scratch: Vec::new(),
            body_filters: std::array::from_fn(|_| std::array::from_fn(|_| Biquad::default())),
            voices: std::array::from_fn(|_| Voice {
                note: 0,
                started_at: 0,
                active: false,
                released: false,
                strings: std::array::from_fn(|_| Strand::new(4)),
                hz: [0.0; 2],
                level: 0.0,
                hold: 0,
            }),
            note_counter: 0,
        };
        for info in &PARAMS {
            p.set_param(info.id, info.default);
        }
        p.set_sample_rate(48_000.0);
        p
    }

    /// Amortissement : poids de l'échantillon précédent dans la moyenne, [0, 0,5].
    fn damping_weight(&self) -> f32 {
        0.5 * self.damping
    }

    fn update_feedback(&mut self) {
        let (decay, release) = (self.decay, self.release.min(self.decay));
        for v in &mut self.voices {
            let t60 = if v.released { release } else { decay };
            for (s, &hz) in v.strings.iter_mut().zip(&v.hz) {
                s.feedback = loop_gain(t60, hz);
            }
        }
    }
}

impl Params for Pluck {
    fn params(&self) -> &'static [ParamInfo] {
        &PARAMS
    }

    fn get_param(&self, id: u32) -> f32 {
        match id {
            P_BRIGHTNESS => self.brightness,
            P_DECAY => self.decay,
            P_DAMPING => self.damping,
            P_PICK => self.pick,
            P_BODY => self.body,
            P_WIDTH => self.width,
            P_RELEASE => self.release,
            P_VOLUME => gain_to_db(self.gain),
            _ => 0.0,
        }
    }

    fn set_param(&mut self, id: u32, value: f32) {
        let Some(info) = PARAMS.iter().find(|i| i.id == id) else { return };
        let v = info.clamp(value);
        match id {
            P_BRIGHTNESS => self.brightness = v,
            P_DECAY => self.decay = v,
            P_DAMPING => self.damping = v,
            P_PICK => self.pick = v,
            P_BODY => self.body = v,
            P_WIDTH => self.width = v,
            P_RELEASE => self.release = v,
            P_VOLUME => self.gain = db_to_gain(v),
            _ => {}
        }
        if matches!(id, P_DECAY | P_RELEASE) {
            self.update_feedback();
        }
    }
}

impl Instrument for Pluck {
    fn kind(&self) -> &'static str {
        "pluck"
    }

    fn is_melodic(&self) -> bool {
        true
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        if sample_rate != self.sample_rate {
            let max = (sample_rate / MIN_HZ) as usize + 4;
            for v in &mut self.voices {
                v.active = false;
                v.strings = std::array::from_fn(|_| Strand::new(max));
            }
            self.scratch = vec![0.0; max];
        }
        self.sample_rate = sample_rate;
        for side in &mut self.body_filters {
            for (f, &(hz, q, _)) in side.iter_mut().zip(&BODY) {
                f.coefs = BiquadCoefs::bandpass(hz, q, sample_rate);
                f.reset();
            }
        }
    }

    fn note_on(&mut self, note: u8, velocity: f32) {
        let idx = pick_voice(VOICES, |i| self.voices[i].active, |i| self.voices[i].started_at);
        self.note_counter += 1;
        let sr = self.sample_rate;
        let velocity = velocity.clamp(0.0, 1.0);
        let damping = self.damping_weight();
        let base = midi_to_hz(note as f32).clamp(MIN_HZ, sr * 0.3);
        let detune = (self.width * MAX_DETUNE / 1_200.0).exp2();
        let hz = [base, base * detune];

        // Excitation : bruit filtré (brillance × vélocité) sur une période.
        let period = ((sr / base) as usize).clamp(2, self.scratch.len());
        let cutoff = 200.0 * 100f32.powf(self.brightness * (0.4 + 0.6 * velocity));
        let mut lp = OnePole::new(cutoff, sr);
        for x in &mut self.scratch[..period] {
            *x = lp.lowpass(self.noise.next_sample());
        }
        // Peigne du médiator : supprime les harmoniques ayant un nœud au point d'attaque.
        let pick = ((self.pick * period as f32) as usize).max(1);
        let amp = 0.5 * (0.3 + 0.7 * velocity);

        let v = &mut self.voices[idx];
        v.note = note;
        v.started_at = self.note_counter;
        v.active = true;
        v.released = false;
        v.hz = hz;
        v.level = 1.0;
        v.hold = period as u32 * 2;
        for (s, &f) in v.strings.iter_mut().zip(&hz) {
            s.line.buf.fill(0.0);
            s.prev = 0.0;
            s.delay = sr / f - damping;
            s.feedback = loop_gain(self.decay, f);
            for i in 0..period {
                let back = if i >= pick { self.scratch[i - pick] } else { 0.0 };
                s.line.write((self.scratch[i] - back) * amp);
            }
        }
    }

    fn note_off(&mut self, note: u8) {
        let release = self.release.min(self.decay);
        for v in self.voices.iter_mut().filter(|v| v.active && !v.released && v.note == note) {
            v.released = true;
            for (s, &hz) in v.strings.iter_mut().zip(&v.hz) {
                s.feedback = loop_gain(release, hz);
            }
        }
    }

    fn all_notes_off(&mut self) {
        for v in &mut self.voices {
            v.active = false;
        }
        for f in self.body_filters.iter_mut().flatten() {
            f.reset();
        }
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        if !self.voices.iter().any(|v| v.active) {
            for f in self.body_filters.iter_mut().flatten() {
                f.reset();
            }
            return;
        }
        let damping = self.damping_weight();
        // Corde 1 au centre-gauche, corde 2 au centre-droit, selon la largeur.
        let near = 0.5 + 0.5 * self.width;
        let far = 1.0 - near;
        let level_coef = ponzik_dsp::util::decay_coef(0.05, self.sample_rate);
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let (mut ml, mut mr) = (0.0, 0.0);
            for v in self.voices.iter_mut().filter(|v| v.active) {
                let a = v.strings[0].tick(damping);
                let b = v.strings[1].tick(damping);
                ml += a * near + b * far;
                mr += a * far + b * near;
                v.level = (v.level * level_coef).max(a.abs().max(b.abs()));
                if v.hold > 0 {
                    v.hold -= 1;
                } else if v.level < SILENCE {
                    v.active = false;
                }
            }
            let mut body = [0.0f32; 2];
            for (out, (side, x)) in body.iter_mut().zip(self.body_filters.iter_mut().zip([ml, mr])) {
                for (f, &(_, _, g)) in side.iter_mut().zip(&BODY) {
                    *out += f.process(x) * g;
                }
            }
            *l += soft_clip((ml + body[0] * self.body * 2.0) * self.gain);
            *r += soft_clip((mr + body[1] * self.body * 2.0) * self.gain);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::common::test_util::{peak, render, rising_crossings, stress};
    use super::*;

    #[test]
    fn plays_and_releases() {
        let mut p = Pluck::new();
        assert_eq!(peak(&mut p, 256), 0.0);
        p.note_on(57, 1.0);
        assert!(peak(&mut p, 4_800) > 0.05);
        p.note_off(57);
        render(&mut p, 2 * 48_000);
        assert!(p.voices.iter().all(|v| !v.active));
        // Les résonances de caisse finissent de sonner, puis plus rien.
        render(&mut p, 512);
        assert_eq!(peak(&mut p, 512), 0.0);
    }

    #[test]
    fn a4_is_in_tune() {
        let mut p = Pluck::new();
        p.set_param(P_WIDTH, 0.0);
        p.set_param(P_BODY, 0.0);
        p.set_param(P_DAMPING, 1.0);
        p.set_param(P_BRIGHTNESS, 0.0);
        // Médiator au milieu : harmoniques paires supprimées, la fondamentale domine.
        p.set_param(P_PICK, 0.5);
        p.set_param(P_DECAY, 10.0);
        p.note_on(69, 1.0);
        render(&mut p, 9_600);
        let (l, _) = render(&mut p, 48_000);
        // Passe-bas de mesure : les petites ondulations aiguës ne comptent pas comme des périodes.
        let mut lp = OnePole::new(800.0, 48_000.0);
        let smooth: Vec<f32> = l.iter().map(|&x| lp.lowpass(x)).collect();
        let n = rising_crossings(&smooth) as f32;
        assert!((n - 440.0).abs() <= 440.0 * 0.03, "{n}");
    }

    #[test]
    fn decay_param_lengthens_the_note() {
        let ring = |decay: f32| {
            let mut p = Pluck::new();
            p.set_param(P_DECAY, decay);
            p.note_on(57, 1.0);
            render(&mut p, 48_000);
            peak(&mut p, 4_800)
        };
        assert!(ring(8.0) > 2.0 * ring(0.5));
    }

    #[test]
    fn survives_extremes() {
        stress(&mut Pluck::new());
    }
}
