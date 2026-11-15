//! Granulaire : nuage de grains prélevés dans un échantillon chargé.
//!
//! Chaque note émet des grains à cadence régulière (densité) autour d'une
//! position, avec dispersion, hauteur et panoramique aléatoires. Les grains
//! viennent d'une réserve fixe de 64 partagée entre les voix ; quand elle est
//! pleine, les nouveaux grains sont abandonnés. Hauteur relative à la note :
//! 60 (Do4) joue l'échantillon à sa vitesse d'origine. Le tirage aléatoire
//! est déterministe : un même jeu donne toujours le même rendu.

use std::sync::Arc;

use ponzik_dsp::env::{Adsr, AdsrParams};
use ponzik_dsp::noise::WhiteNoise;
use ponzik_dsp::util::{balance_gains, db_to_gain, gain_to_db};

use super::common::{fast_sin, pick_voice, read_cubic, soft_clip};
use crate::Instrument;
use crate::param::{ParamInfo, Params, Unit};
use crate::sample::Sample;

const VOICES: usize = 8;
const GRAINS: usize = 64;
const ROOT: f32 = 60.0;
/// Grain le plus court, en images de sortie.
const MIN_GRAIN: f32 = 16.0;

const WINDOWS: &[&str] = &["Hann", "Triangle", "Trapèze"];

const P_POSITION: u32 = 0;
const P_SPRAY: u32 = 1;
const P_SIZE: u32 = 2;
const P_DENSITY: u32 = 3;
const P_PITCH: u32 = 4;
const P_RAND_PITCH: u32 = 5;
const P_SPREAD: u32 = 6;
const P_WINDOW: u32 = 7;
const P_ATTACK: u32 = 8;
const P_DECAY: u32 = 9;
const P_SUSTAIN: u32 = 10;
const P_RELEASE: u32 = 11;
const P_VOLUME: u32 = 12;

static PARAMS: [ParamInfo; 13] = [
    ParamInfo::new(P_POSITION, "Position", 0.0, 1.0, 0.3, Unit::Percent).group("Grains"),
    ParamInfo::new(P_SPRAY, "Dispersion", 0.0, 2.0, 0.05, Unit::Seconds).group("Grains"),
    ParamInfo::new(P_SIZE, "Taille", 0.005, 0.5, 0.08, Unit::Seconds).log().group("Grains"),
    ParamInfo::new(P_DENSITY, "Densité", 1.0, 200.0, 30.0, Unit::Hz).log().group("Grains"),
    ParamInfo::choice(P_WINDOW, "Fenêtre", WINDOWS, 0).group("Grains"),
    ParamInfo::new(P_PITCH, "Transposition", -24.0, 24.0, 0.0, Unit::Semitones).group("Hauteur"),
    ParamInfo::new(P_RAND_PITCH, "Hauteur aléatoire", 0.0, 1_200.0, 0.0, Unit::Cents).group("Hauteur"),
    ParamInfo::new(P_SPREAD, "Largeur stéréo", 0.0, 1.0, 0.5, Unit::Percent).group("Sortie"),
    ParamInfo::new(P_ATTACK, "Attaque", 0.001, 4.0, 0.02, Unit::Seconds).log().group("Amplitude"),
    ParamInfo::new(P_DECAY, "Déclin", 0.005, 8.0, 0.3, Unit::Seconds).log().group("Amplitude"),
    ParamInfo::new(P_SUSTAIN, "Maintien", 0.0, 1.0, 1.0, Unit::Percent).group("Amplitude"),
    ParamInfo::new(P_RELEASE, "Relâche", 0.005, 8.0, 0.3, Unit::Seconds).log().group("Amplitude"),
    ParamInfo::new(P_VOLUME, "Volume", -40.0, 6.0, -3.0, Unit::Db).group("Sortie"),
];

#[derive(Clone, Copy, Debug, Default)]
struct Grain {
    active: bool,
    voice: usize,
    /// Position de lecture dans l'échantillon, en images.
    pos: f64,
    rate: f64,
    age: f32,
    len: f32,
    gain_l: f32,
    gain_r: f32,
}

struct Voice {
    note: u8,
    velocity: f32,
    started_at: u64,
    env: Adsr,
    /// Images de sortie avant le prochain grain.
    countdown: f32,
}

/// Fenêtre de grain, `t` dans [0, 1].
#[inline]
fn window(shape: usize, t: f32) -> f32 {
    match shape {
        0 => {
            let s = fast_sin(0.5 * t);
            s * s
        }
        1 => 1.0 - (2.0 * t - 1.0).abs(),
        _ => (t.min(1.0 - t) * 5.0).min(1.0),
    }
}

/// Synthé granulaire huit voix ; muet tant qu'aucun échantillon n'est chargé.
pub struct Granular {
    sample: Option<Arc<Sample>>,
    position: f32,
    spray: f32,
    size: f32,
    density: f32,
    pitch: f32,
    rand_pitch: f32,
    spread: f32,
    window: usize,
    env: AdsrParams,
    gain: f32,
    sample_rate: f32,
    rng: WhiteNoise,
    voices: [Voice; VOICES],
    grains: [Grain; GRAINS],
    note_counter: u64,
}

impl Default for Granular {
    fn default() -> Self {
        Self::new()
    }
}

impl Granular {
    pub fn new() -> Self {
        let sr = 48_000.0;
        let env = AdsrParams { attack: 0.02, decay: 0.3, sustain: 1.0, release: 0.3 };
        let mut g = Self {
            sample: None,
            position: 0.3,
            spray: 0.05,
            size: 0.08,
            density: 30.0,
            pitch: 0.0,
            rand_pitch: 0.0,
            spread: 0.5,
            window: 0,
            env,
            gain: 1.0,
            sample_rate: sr,
            rng: WhiteNoise::new(0x9E37_79B9),
            voices: std::array::from_fn(|_| Voice {
                note: 0,
                velocity: 0.0,
                started_at: 0,
                env: Adsr::new(env, sr),
                countdown: 0.0,
            }),
            grains: [Grain::default(); GRAINS],
            note_counter: 0,
        };
        for p in &PARAMS {
            g.set_param(p.id, p.default);
        }
        g
    }

    /// Lance un grain pour la voix `vi`, si la réserve le permet.
    fn spawn(&mut self, vi: usize, sample: &Sample) {
        let Some(slot) = self.grains.iter().position(|g| !g.active) else { return };
        let last = (sample.len() - 1) as f64;
        let jitter = self.rng.next_sample() as f64 * self.spray as f64 * sample.sample_rate as f64;
        let pos = (self.position as f64 * last + jitter).clamp(0.0, last);
        let st = self.voices[vi].note as f32 - ROOT + self.pitch + self.rand_pitch / 100.0 * self.rng.next_sample();
        let rate = sample.sample_rate as f64 / self.sample_rate as f64 * (st as f64 / 12.0).exp2();
        let (gain_l, gain_r) = balance_gains(self.spread * self.rng.next_sample());
        let len = (self.size * self.sample_rate).max(MIN_GRAIN);
        self.grains[slot] = Grain { active: true, voice: vi, pos, rate, age: 0.0, len, gain_l, gain_r };
    }
}

impl Params for Granular {
    fn params(&self) -> &'static [ParamInfo] {
        &PARAMS
    }

    fn get_param(&self, id: u32) -> f32 {
        match id {
            P_POSITION => self.position,
            P_SPRAY => self.spray,
            P_SIZE => self.size,
            P_DENSITY => self.density,
            P_PITCH => self.pitch,
            P_RAND_PITCH => self.rand_pitch,
            P_SPREAD => self.spread,
            P_WINDOW => self.window as f32,
            P_ATTACK => self.env.attack,
            P_DECAY => self.env.decay,
            P_SUSTAIN => self.env.sustain,
            P_RELEASE => self.env.release,
            P_VOLUME => gain_to_db(self.gain),
            _ => 0.0,
        }
    }

    fn set_param(&mut self, id: u32, value: f32) {
        let Some(info) = PARAMS.iter().find(|i| i.id == id) else { return };
        let v = info.clamp(value);
        match id {
            P_POSITION => self.position = v,
            P_SPRAY => self.spray = v,
            P_SIZE => self.size = v,
            P_DENSITY => self.density = v,
            P_PITCH => self.pitch = v,
            P_RAND_PITCH => self.rand_pitch = v,
            P_SPREAD => self.spread = v,
            P_WINDOW => self.window = v as usize,
            P_ATTACK => self.env.attack = v,
            P_DECAY => self.env.decay = v,
            P_SUSTAIN => self.env.sustain = v,
            P_RELEASE => self.env.release = v,
            P_VOLUME => self.gain = db_to_gain(v),
            _ => {}
        }
        let env = self.env;
        for voice in &mut self.voices {
            voice.env.params = env;
        }
    }
}

impl Instrument for Granular {
    fn kind(&self) -> &'static str {
        "granular"
    }

    fn is_melodic(&self) -> bool {
        true
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        for v in &mut self.voices {
            v.env.set_sample_rate(sample_rate);
        }
    }

    fn set_sample(&mut self, sample: Arc<Sample>) {
        self.all_notes_off();
        self.sample = Some(sample);
    }

    fn note_on(&mut self, note: u8, velocity: f32) {
        if self.sample.as_ref().is_none_or(|s| s.is_empty()) {
            return;
        }
        let idx = pick_voice(VOICES, |i| self.voices[i].env.is_active(), |i| self.voices[i].started_at);
        self.note_counter += 1;
        let v = &mut self.voices[idx];
        v.note = note;
        v.velocity = velocity.clamp(0.0, 1.0);
        v.started_at = self.note_counter;
        v.countdown = 0.0;
        v.env.gate_on();
    }

    fn note_off(&mut self, note: u8) {
        for v in self.voices.iter_mut().filter(|v| v.note == note && v.env.is_gated()) {
            v.env.gate_off();
        }
    }

    fn all_notes_off(&mut self) {
        for v in &mut self.voices {
            v.env.reset();
        }
        for g in &mut self.grains {
            g.active = false;
        }
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let Some(sample) = self.sample.clone() else { return };
        if sample.is_empty() || !self.voices.iter().any(|v| v.env.is_active()) {
            return;
        }
        let interval = self.sample_rate / self.density;
        // Recouvrement moyen des grains : on normalise en puissance.
        let norm = self.gain / (self.density * self.size).max(1.0).sqrt();
        let end = (sample.len() - 1) as f64;
        let shape = self.window;
        let mut levels = [0.0f32; VOICES];
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            for (vi, level) in levels.iter_mut().enumerate() {
                let v = &mut self.voices[vi];
                if !v.env.is_active() {
                    *level = 0.0;
                    continue;
                }
                *level = v.env.next_sample() * v.velocity;
                v.countdown -= 1.0;
                if v.countdown <= 0.0 {
                    v.countdown += interval;
                    self.spawn(vi, &sample);
                }
            }
            let (mut ml, mut mr) = (0.0, 0.0);
            for g in self.grains.iter_mut().filter(|g| g.active) {
                let level = levels[g.voice];
                if level <= 0.0 && !self.voices[g.voice].env.is_active() {
                    g.active = false;
                    continue;
                }
                if g.pos <= end {
                    let w = window(shape, g.age / g.len) * level;
                    ml += read_cubic(&sample.left, g.pos) * w * g.gain_l;
                    mr += read_cubic(&sample.right, g.pos) * w * g.gain_r;
                }
                g.pos += g.rate;
                g.age += 1.0;
                if g.age >= g.len {
                    g.active = false;
                }
            }
            *l += soft_clip(ml * norm);
            *r += soft_clip(mr * norm);
        }
        if !self.voices.iter().any(|v| v.env.is_active()) {
            self.all_notes_off();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::common::test_util::{peak, render, stress};
    use super::*;

    fn sine(seconds: f32) -> Arc<Sample> {
        let n = (48_000.0 * seconds) as usize;
        let left: Vec<f32> =
            (0..n).map(|i| (std::f32::consts::TAU * 441.0 * i as f32 / 48_000.0).sin() * 0.8).collect();
        Arc::new(Sample { name: "sinus".into(), sample_rate: 48_000.0, right: left.clone(), left })
    }

    #[test]
    fn silent_without_sample() {
        let mut g = Granular::new();
        g.note_on(60, 1.0);
        assert_eq!(peak(&mut g, 4_800), 0.0);
    }

    #[test]
    fn plays_and_releases() {
        let mut g = Granular::new();
        g.set_sample_rate(48_000.0);
        g.set_sample(sine(1.0));
        g.note_on(60, 1.0);
        assert!(peak(&mut g, 9_600) > 0.05);
        assert!(g.grains.iter().any(|g| g.active));
        g.note_off(60);
        render(&mut g, 48_000);
        assert_eq!(peak(&mut g, 512), 0.0);
        assert!(g.grains.iter().all(|g| !g.active));
    }

    #[test]
    fn is_deterministic_and_bounded_by_the_pool() {
        let play = || {
            let mut g = Granular::new();
            g.set_sample(sine(0.5));
            g.set_param(P_DENSITY, 200.0);
            g.set_param(P_SIZE, 0.5);
            g.set_param(P_RAND_PITCH, 700.0);
            for n in [48, 60, 67, 72] {
                g.note_on(n, 0.9);
            }
            let out = render(&mut g, 24_000);
            let busy = g.grains.iter().filter(|g| g.active).count();
            assert!(busy > GRAINS / 2 && busy <= GRAINS, "{busy}");
            out
        };
        assert_eq!(play(), play());
    }

    #[test]
    fn survives_extremes() {
        let mut g = Granular::new();
        g.set_sample(sine(0.5));
        stress(&mut g);
    }
}
