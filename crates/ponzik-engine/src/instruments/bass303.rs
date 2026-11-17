//! Acid 303 : basse monophonique façon TB-303.
//!
//! Un oscillateur scie ou carré, un passe-bas 24 dB (deux SVF en cascade,
//! la résonance sur le second), une enveloppe de filtre exponentielle.
//! Une vélocité > 0,8 joue un accent ; une note jouée avant le relâchement
//! de la précédente glisse sans redéclencher les enveloppes.

use ponzik_dsp::env::{Adsr, AdsrParams};
use ponzik_dsp::filter::{FilterMode, Svf};
use ponzik_dsp::osc::{Oscillator, Waveform};
use ponzik_dsp::util::{db_to_gain, decay_coef, gain_to_db};

use super::common::{Expr, Glide, Tune, glide_coef, pan_gains, soft_clip};
use crate::{Controller, Instrument, NoteExpression};
use crate::param::{ParamInfo, Params, Unit};

const ACCENT_VELOCITY: f32 = 0.8;
/// Déclin du filtre sur une note accentuée, comme sur l'original.
const ACCENT_DECAY: f32 = 0.2;
/// Ouverture maximale du filtre par l'enveloppe, en octaves.
const ENV_OCTAVES: f32 = 5.0;
const HELD: usize = 16;
const AMP_ENV: AdsrParams = AdsrParams { attack: 0.003, decay: 0.01, sustain: 1.0, release: 0.012 };

const WAVES: &[&str] = &["Saw", "Square"];

const P_WAVE: u32 = 0;
const P_CUTOFF: u32 = 1;
const P_RESO: u32 = 2;
const P_ENV_MOD: u32 = 3;
const P_DECAY: u32 = 4;
const P_ACCENT: u32 = 5;
const P_SLIDE: u32 = 6;
const P_DIST: u32 = 7;
const P_TUNE: u32 = 8;
const P_VOLUME: u32 = 9;

static PARAMS: [ParamInfo; 10] = [
    ParamInfo::choice(P_WAVE, "Wave", WAVES, 0).group("Oscillator"),
    ParamInfo::new(P_TUNE, "Tune", -12.0, 12.0, 0.0, Unit::Semitones).group("Oscillator"),
    ParamInfo::new(P_CUTOFF, "Cutoff", 40.0, 8_000.0, 400.0, Unit::Hz).log().group("Filter"),
    ParamInfo::new(P_RESO, "Resonance", 0.0, 1.0, 0.7, Unit::Percent).group("Filter"),
    ParamInfo::new(P_ENV_MOD, "Modulation", 0.0, 1.0, 0.5, Unit::Percent).group("Filter"),
    ParamInfo::new(P_DECAY, "Decay", 0.03, 3.0, 0.4, Unit::Seconds).log().group("Filter"),
    ParamInfo::new(P_ACCENT, "Accent", 0.0, 1.0, 0.6, Unit::Percent).group("Performance"),
    ParamInfo::new(P_SLIDE, "Slide", 0.01, 0.5, 0.06, Unit::Seconds).log().group("Performance"),
    ParamInfo::new(P_DIST, "Distortion", 0.0, 1.0, 0.2, Unit::Percent).group("Output"),
    ParamInfo::new(P_VOLUME, "Volume", -40.0, 6.0, -6.0, Unit::Db).group("Output"),
];

/// Acid 303, monophonique.
pub struct Bass303 {
    wave: usize,
    cutoff: f32,
    resonance: f32,
    env_mod: f32,
    decay: f32,
    accent: f32,
    slide: f32,
    dist: f32,
    tune: f32,
    gain: f32,
    sample_rate: f32,
    osc: Oscillator,
    filters: [Svf; 2],
    amp_env: Adsr,
    glide: Glide,
    slide_coef: f32,
    filter_env: f32,
    decay_coef: f32,
    accent_coef: f32,
    accented: bool,
    velocity: f32,
    /// Notes tenues, la plus récente en dernier.
    held: [u8; HELD],
    held_len: usize,
    /// Expression of the note that plays.
    expr: Expr,
    expr_tune: Tune,
    /// Pitch bend in semitones.
    bend: f32,
}

impl Default for Bass303 {
    fn default() -> Self {
        Self::new()
    }
}

impl Bass303 {
    pub fn new() -> Self {
        let sr = 48_000.0;
        let mut s = Self {
            wave: 0,
            cutoff: 400.0,
            resonance: 0.7,
            env_mod: 0.5,
            decay: 0.4,
            accent: 0.6,
            slide: 0.06,
            dist: 0.2,
            tune: 0.0,
            gain: 1.0,
            sample_rate: sr,
            osc: Oscillator::new(Waveform::Saw),
            filters: std::array::from_fn(|_| Svf::new(FilterMode::LowPass, 400.0, 0.707, sr)),
            amp_env: Adsr::new(AMP_ENV, sr),
            glide: Glide::default(),
            slide_coef: 0.0,
            filter_env: 0.0,
            decay_coef: 0.0,
            accent_coef: 0.0,
            accented: false,
            velocity: 0.0,
            held: [0; HELD],
            held_len: 0,
            expr: Expr::default(),
            expr_tune: Tune::default(),
            bend: 0.0,
        };
        for p in &PARAMS {
            s.set_param(p.id, p.default);
        }
        s.set_sample_rate(sr);
        s
    }

    fn update_coefs(&mut self) {
        let sr = self.sample_rate;
        // `decay` est le temps pour tomber à −60 dB.
        self.decay_coef = decay_coef(self.decay / 6.9, sr);
        self.accent_coef = decay_coef(self.decay.min(ACCENT_DECAY) / 6.9, sr);
        self.slide_coef = glide_coef(self.slide, sr);
    }

    fn release_held(&mut self, note: u8) {
        if let Some(i) = self.held[..self.held_len].iter().position(|&n| n == note) {
            self.held.copy_within(i + 1..self.held_len, i);
            self.held_len -= 1;
        }
    }
}

impl Params for Bass303 {
    fn params(&self) -> &'static [ParamInfo] {
        &PARAMS
    }

    fn get_param(&self, id: u32) -> f32 {
        match id {
            P_WAVE => self.wave as f32,
            P_CUTOFF => self.cutoff,
            P_RESO => self.resonance,
            P_ENV_MOD => self.env_mod,
            P_DECAY => self.decay,
            P_ACCENT => self.accent,
            P_SLIDE => self.slide,
            P_DIST => self.dist,
            P_TUNE => self.tune,
            P_VOLUME => gain_to_db(self.gain),
            _ => 0.0,
        }
    }

    fn set_param(&mut self, id: u32, value: f32) {
        let Some(info) = PARAMS.iter().find(|i| i.id == id) else { return };
        let v = info.clamp(value);
        match id {
            P_WAVE => {
                self.wave = v as usize;
                self.osc.waveform = if self.wave == 0 { Waveform::Saw } else { Waveform::Square };
            }
            P_CUTOFF => self.cutoff = v,
            P_RESO => self.resonance = v,
            P_ENV_MOD => self.env_mod = v,
            P_DECAY => self.decay = v,
            P_ACCENT => self.accent = v,
            P_SLIDE => self.slide = v,
            P_DIST => self.dist = v,
            P_TUNE => self.tune = v,
            P_VOLUME => self.gain = db_to_gain(v),
            _ => {}
        }
        if matches!(id, P_DECAY | P_SLIDE) {
            self.update_coefs();
        }
    }
}

impl Instrument for Bass303 {
    fn kind(&self) -> &'static str {
        "bass303"
    }

    fn is_melodic(&self) -> bool {
        true
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.amp_env.set_sample_rate(sample_rate);
        self.update_coefs();
    }

    fn note_on(&mut self, note: u8, velocity: f32) {
        let legato = self.held_len > 0 && self.amp_env.is_gated();
        self.velocity = velocity.clamp(0.0, 1.0);
        self.accented = self.velocity > ACCENT_VELOCITY;
        self.expr.reset();
        if legato {
            self.glide.go(note as f32);
        } else {
            self.glide.jump(note as f32);
            if !self.amp_env.is_active() {
                self.osc.reset(0.0);
                self.filters.iter_mut().for_each(Svf::reset);
            }
            self.filter_env = 1.0;
            self.amp_env.gate_on();
        }
        self.release_held(note);
        if self.held_len == HELD {
            self.held.copy_within(1.., 0);
            self.held_len -= 1;
        }
        self.held[self.held_len] = note;
        self.held_len += 1;
    }

    fn note_off(&mut self, note: u8) {
        let current = self.held_len > 0 && self.held[self.held_len - 1] == note;
        self.release_held(note);
        if self.held_len == 0 {
            if self.amp_env.is_gated() {
                self.amp_env.gate_off();
            }
        } else if current {
            // On revient en glissant vers la note encore tenue.
            self.glide.go(self.held[self.held_len - 1] as f32);
        }
    }

    fn supports_expression(&self) -> bool {
        true
    }

    fn note_expression(&mut self, key: u8, expr: NoteExpression) {
        if self.held_len > 0 && self.held[self.held_len - 1] == key {
            self.expr.apply(expr, self.sample_rate);
        }
    }

    fn controller(&mut self, ctl: Controller) {
        if let Controller::PitchBend(b) = ctl {
            self.bend = b.clamp(-1.0, 1.0) * crate::PITCH_BEND_RANGE;
        }
    }

    fn all_notes_off(&mut self) {
        self.held_len = 0;
        self.amp_env.reset();
        self.filters.iter_mut().for_each(Svf::reset);
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        if !self.amp_env.is_active() {
            return;
        }
        let sr = self.sample_rate;
        let tune = 2f32.powf(self.tune / 12.0);
        let acc = if self.accented { self.accent } else { 0.0 };
        let coef = if self.accented { self.accent_coef } else { self.decay_coef };
        let env_oct = ENV_OCTAVES * (self.env_mod + 0.5 * acc);
        let q = 0.5 + self.resonance * (11.0 + 6.0 * acc);
        // La résonance creuse le bas du spectre : on compense un peu.
        let comp = 1.0 + 0.5 * self.resonance;
        let drive = 1.0 + self.dist * 15.0;
        let makeup = 1.0 / drive.tanh().max(0.5) / (1.0 + self.dist * 1.5);
        let level = self.gain * (0.6 + 0.4 * self.velocity) * (1.0 + 0.6 * acc);
        let (pl, pr) = pan_gains(self.expr.pan);
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let hz = self.glide.next(self.slide_coef) * tune * self.expr_tune.ratio(self.expr.tuning.tick() + self.bend);
            self.osc.set_frequency(hz, sr);
            let fc = self.cutoff * 2f32.powf(env_oct * self.filter_env);
            self.filter_env *= coef;
            self.filters[0].set_params(fc, 0.707, sr);
            self.filters[1].set_params(fc, q, sr);
            let x = self.filters[0].process(self.osc.next_sample());
            let y = self.filters[1].process(x) * comp;
            let s = (y * drive).tanh() * makeup * self.amp_env.next_sample() * level;
            let s = soft_clip(s);
            *l += s * pl;
            *r += s * pr;
            if !self.amp_env.is_active() {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::common::test_util::{peak, stress};
    use super::*;

    #[test]
    fn plays_and_releases() {
        let mut b = Bass303::new();
        assert_eq!(peak(&mut b, 256), 0.0);
        b.note_on(36, 0.7);
        assert!(peak(&mut b, 4_800) > 0.05);
        b.note_off(36);
        peak(&mut b, 4_800);
        assert_eq!(peak(&mut b, 512), 0.0);
    }

    #[test]
    fn overlapping_notes_slide_without_retrigger() {
        let mut b = Bass303::new();
        b.note_on(36, 0.7);
        peak(&mut b, 4_800);
        let env = b.filter_env;
        b.note_on(48, 0.7);
        assert_eq!(b.filter_env, env, "l'enveloppe ne doit pas repartir");
        assert_eq!(b.glide.target(), 48.0);
        peak(&mut b, 48);
        assert!(b.glide.note() > 36.0 && b.glide.note() < 48.0);
        // Relâcher la note glissée revient vers la note encore tenue.
        b.note_off(48);
        assert_eq!(b.glide.target(), 36.0);
        assert!(b.amp_env.is_gated());
        b.note_off(36);
        assert!(!b.amp_env.is_gated());
        // Une note détachée redéclenche tout.
        peak(&mut b, 4_800);
        b.note_on(40, 0.9);
        assert!(b.accented && b.filter_env == 1.0);
        assert_eq!(b.glide.note(), 40.0);
    }

    #[test]
    fn accent_is_louder() {
        let level = |vel: f32| {
            let mut b = Bass303::new();
            b.note_on(36, vel);
            peak(&mut b, 2_400)
        };
        assert!(level(1.0) > level(0.75));
    }

    #[test]
    fn survives_extremes() {
        let mut b = Bass303::new();
        stress(&mut b);
    }
}
