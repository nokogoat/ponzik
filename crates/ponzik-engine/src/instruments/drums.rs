use std::f32::consts::TAU;

use ponzik_dsp::filter::{FilterMode, Svf};
use ponzik_dsp::noise::WhiteNoise;
use ponzik_dsp::util::decay_coef;

use crate::Instrument;
use crate::param::{ParamInfo, Params, Unit};

const SILENCE: f32 = 1e-3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrumKind {
    Kick,
    Snare,
    Clap,
    ClosedHat,
    OpenHat,
}

const KINDS: [DrumKind; 5] = [DrumKind::Kick, DrumKind::Snare, DrumKind::Clap, DrumKind::ClosedHat, DrumKind::OpenHat];
const P_KIND: u32 = 0;
const P_TUNE: u32 = 1;
const P_DECAY: u32 = 2;
const P_TONE: u32 = 3;
const P_VOLUME: u32 = 4;

static PARAMS: [ParamInfo; 5] = [
    ParamInfo::choice(P_KIND, "Type", &["Kick", "Caisse claire", "Clap", "Charleston fermée", "Charleston ouverte"], 0),
    ParamInfo::new(P_TUNE, "Accord", -24.0, 24.0, 0.0, Unit::Semitones),
    ParamInfo::new(P_DECAY, "Longueur", 0.25, 4.0, 1.0, Unit::None).log(),
    ParamInfo::new(P_TONE, "Brillance", 0.0, 1.0, 0.5, Unit::Percent),
    ParamInfo::new(P_VOLUME, "Volume", -40.0, 6.0, 0.0, Unit::Db),
];

/// Percussion synthétisée façon 808/909, monophonique.
/// La note règle l'accord : 60 = hauteur d'origine.
pub struct DrumSynth {
    kind: DrumKind,
    sample_rate: f32,
    noise: WhiteNoise,
    filter: Svf,
    active: bool,
    velocity: f32,
    tune: f32,
    elapsed: u32,
    phase: f32,
    amp: f32,
    amp_coef: f32,
    tone_amp: f32,
    tone_coef: f32,
    pitch_env: f32,
    pitch_coef: f32,
    tune_st: f32,
    decay_scale: f32,
    tone: f32,
    level: f32,
}

impl DrumSynth {
    pub fn new(kind: DrumKind) -> Self {
        let mut d = Self {
            kind,
            sample_rate: 48_000.0,
            noise: WhiteNoise::default(),
            filter: Svf::new(FilterMode::LowPass, 1_000.0, 0.7, 48_000.0),
            active: false,
            velocity: 0.0,
            tune: 1.0,
            elapsed: 0,
            phase: 0.0,
            amp: 0.0,
            amp_coef: 0.0,
            tone_amp: 0.0,
            tone_coef: 0.0,
            pitch_env: 0.0,
            pitch_coef: 0.0,
            tune_st: 0.0,
            decay_scale: 1.0,
            tone: 0.5,
            level: 1.0,
        };
        d.set_sample_rate(48_000.0);
        d
    }

    pub fn drum_kind(&self) -> DrumKind {
        self.kind
    }

    /// Constantes de temps (s) : amplitude, ton, enveloppe de hauteur.
    fn taus(&self) -> (f32, f32, f32) {
        let (a, t, p) = self.base_taus();
        (a * self.decay_scale, t * self.decay_scale, p)
    }

    fn base_taus(&self) -> (f32, f32, f32) {
        match self.kind {
            DrumKind::Kick => (0.35, 0.0, 0.03),
            DrumKind::Snare => (0.12, 0.06, 0.01),
            DrumKind::Clap => (0.12, 0.0, 0.0),
            DrumKind::ClosedHat => (0.035, 0.0, 0.0),
            DrumKind::OpenHat => (0.3, 0.0, 0.0),
        }
    }
}

impl Params for DrumSynth {
    fn params(&self) -> &'static [ParamInfo] {
        &PARAMS
    }

    fn get_param(&self, id: u32) -> f32 {
        match id {
            P_KIND => KINDS.iter().position(|k| *k == self.kind).unwrap_or(0) as f32,
            P_TUNE => self.tune_st,
            P_DECAY => self.decay_scale,
            P_TONE => self.tone,
            P_VOLUME => ponzik_dsp::util::gain_to_db(self.level),
            _ => 0.0,
        }
    }

    fn set_param(&mut self, id: u32, value: f32) {
        let Some(info) = PARAMS.iter().find(|i| i.id == id) else { return };
        let v = info.clamp(value);
        match id {
            P_KIND => self.kind = KINDS[v as usize],
            P_TUNE => self.tune_st = v,
            P_DECAY => self.decay_scale = v,
            P_TONE => self.tone = v,
            P_VOLUME => self.level = ponzik_dsp::util::db_to_gain(v),
            _ => return,
        }
        if id != P_TUNE && id != P_VOLUME {
            self.set_sample_rate(self.sample_rate);
        }
    }
}

impl Instrument for DrumSynth {
    fn kind(&self) -> &'static str {
        "drumsynth"
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        let (amp, tone, pitch) = self.taus();
        self.amp_coef = decay_coef(amp, sample_rate);
        self.tone_coef = decay_coef(tone, sample_rate);
        self.pitch_coef = decay_coef(pitch, sample_rate);
        let (mode, cutoff, q) = match self.kind {
            DrumKind::Kick => (FilterMode::LowPass, 8_000.0, 0.7),
            DrumKind::Snare => (FilterMode::BandPass, 2_500.0, 0.8),
            DrumKind::Clap => (FilterMode::BandPass, 1_200.0, 1.5),
            DrumKind::ClosedHat | DrumKind::OpenHat => (FilterMode::HighPass, 7_000.0, 0.9),
        };
        let bright = 2f32.powf((self.tone - 0.5) * 2.0);
        self.filter = Svf::new(mode, cutoff * bright, q, sample_rate);
    }

    fn note_on(&mut self, note: u8, velocity: f32) {
        self.active = true;
        self.velocity = velocity.clamp(0.0, 1.0);
        self.tune = 2f32.powf((note as f32 - 60.0 + self.tune_st) / 12.0);
        self.elapsed = 0;
        self.phase = 0.0;
        self.amp = 1.0;
        self.tone_amp = 1.0;
        self.pitch_env = 1.0;
    }

    fn note_off(&mut self, _note: u8) {
        // Les percussions jouent toujours leur décroissance complète.
    }

    fn all_notes_off(&mut self) {
        self.active = false;
        self.filter.reset();
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        if !self.active {
            return;
        }
        let sr = self.sample_rate;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let s = match self.kind {
                DrumKind::Kick => {
                    let hz = (45.0 + 110.0 * self.pitch_env) * self.tune;
                    self.pitch_env *= self.pitch_coef;
                    self.phase = (self.phase + hz / sr).fract();
                    let body = (self.phase * TAU).sin() * self.amp;
                    (2.0 * body).tanh() * 0.9
                }
                DrumKind::Snare => {
                    self.phase = (self.phase + 190.0 * self.tune / sr).fract();
                    let tone = (self.phase * TAU).sin() * self.tone_amp * 0.5;
                    tone + self.filter.process(self.noise.next_sample()) * self.amp * 1.4
                }
                DrumKind::Clap => {
                    // Trois rafales rapprochées puis une queue.
                    let t = self.elapsed as f32 / sr;
                    let env = if t < 0.03 { (-(t % 0.01) / 0.002).exp() } else { self.amp };
                    self.filter.process(self.noise.next_sample()) * env * 2.0
                }
                DrumKind::ClosedHat | DrumKind::OpenHat => {
                    self.filter.process(self.noise.next_sample()) * self.amp * 0.5
                }
            } * self.velocity
                * self.level;

            let in_burst = self.kind == DrumKind::Clap && (self.elapsed as f32) < 0.03 * sr;
            if !in_burst {
                self.amp *= self.amp_coef;
            }
            self.tone_amp *= self.tone_coef;
            self.elapsed = self.elapsed.saturating_add(1);
            *l += s;
            *r += s;

            // Sous -60 dB, la frappe est finie.
            if !in_burst && self.amp < SILENCE && self.tone_amp < SILENCE {
                self.active = false;
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_drum_sounds_then_stops() {
        for kind in [DrumKind::Kick, DrumKind::Snare, DrumKind::Clap, DrumKind::ClosedHat, DrumKind::OpenHat] {
            let mut d = DrumSynth::new(kind);
            d.note_on(60, 1.0);
            let mut l = vec![0.0; 192_000];
            let mut r = vec![0.0; 192_000];
            d.render(&mut l, &mut r);
            let peak = l.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            assert!(peak > 0.05 && peak < 2.0, "{kind:?}: {peak}");
            assert!(!d.active, "{kind:?} ne s'arrête pas");
        }
    }
}
