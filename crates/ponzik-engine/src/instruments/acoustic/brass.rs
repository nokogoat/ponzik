//! Brass and Winds: the blown models ([`Blown`]) played by sections, with
//! the instruments' radiation colors, brass mutes, growl, falls and scoops.

use ponzik_dsp::biquad::{Biquad, BiquadCoefs};
use ponzik_dsp::util::{db_to_gain, flush_denormal};

use super::blown::{BlowCfg, Blown, Mechanism, Spec};
use super::section::{Articulation, BUSES, CHUNK, Section};
use super::strings::{PLAY_MODES, SECTION_PLAYERS, SECTION_SIZES};
use crate::instruments::common::soft_clip;
use crate::param::{ParamInfo, Params, Unit};
use crate::{Controller, Instrument, NoteExpression};

/// Radiation color of an instrument: peaking filters `(hz, q, dB)`.
type Color = [(f32, f32, f32); 3];

struct Voicing {
    spec: Spec,
    color: Color,
    /// Playable range (Hz).
    range: (f32, f32),
}

const BRASS: [Voicing; 5] = [
    // Trumpet: bright, a bell resonance around 1.2 kHz.
    Voicing {
        spec: Spec { mechanism: Mechanism::Lips, loss: (0.95, 0.0), pressure: (0.72, 0.94), shape: 0.2, slope: 0.3, gain: 0.5, cents: 0.0 },
        color: [(1_200.0, 1.0, 3.0), (3_000.0, 1.5, 2.0), (250.0, 0.7, -2.0)],
        range: (150.0, 1_400.0),
    },
    // Trombone: rounder.
    Voicing {
        spec: Spec { mechanism: Mechanism::Lips, loss: (0.95, 0.1), pressure: (0.72, 0.94), shape: 0.2, slope: 0.3, gain: 0.55, cents: 0.0 },
        color: [(600.0, 1.0, 3.0), (1_800.0, 1.3, 1.0), (4_000.0, 0.8, -3.0)],
        range: (70.0, 700.0),
    },
    // French horn: dark, the hand in the bell.
    Voicing {
        spec: Spec { mechanism: Mechanism::Lips, loss: (0.95, 0.25), pressure: (0.72, 0.94), shape: 0.2, slope: 0.3, gain: 0.7, cents: 0.0 },
        color: [(450.0, 0.9, 3.0), (1_500.0, 1.0, -3.0), (3_500.0, 0.7, -8.0)],
        range: (60.0, 900.0),
    },
    // Tuba.
    Voicing {
        spec: Spec { mechanism: Mechanism::Lips, loss: (0.95, 0.3), pressure: (0.86, 0.98), shape: 0.2, slope: 0.3, gain: 0.7, cents: 0.0 },
        color: [(250.0, 0.9, 3.0), (900.0, 1.0, 0.0), (2_500.0, 0.7, -8.0)],
        range: (30.0, 360.0),
    },
    // Saxophone: conical reed.
    Voicing {
        spec: Spec { mechanism: Mechanism::Conical, loss: (0.95, 0.0), pressure: (0.64, 0.84), shape: 0.35, slope: 0.32, gain: 1.6, cents: 0.0 },
        color: [(700.0, 1.2, 3.0), (2_200.0, 1.5, 3.0), (5_000.0, 0.8, -4.0)],
        range: (100.0, 1_000.0),
    },
];

const WINDS: [Voicing; 3] = [
    // Flute: jet drive.
    Voicing {
        spec: Spec { mechanism: Mechanism::Jet, loss: (1.0, 0.65), pressure: (0.9, 1.15), shape: 0.32, slope: 0.0, gain: 0.5, cents: -23.0 },
        color: [(900.0, 1.0, 1.0), (2_500.0, 1.0, 1.0), (6_000.0, 0.7, -2.0)],
        range: (240.0, 2_400.0),
    },
    // Clarinet: cylindrical bore, odd harmonics.
    Voicing {
        spec: Spec { mechanism: Mechanism::Clarinet, loss: (0.95, 0.0), pressure: (0.6, 0.85), shape: 0.0, slope: -0.3, gain: 1.2, cents: 0.0 },
        color: [(1_500.0, 1.2, 2.0), (3_000.0, 1.0, 1.0), (300.0, 0.7, -1.0)],
        range: (140.0, 1_600.0),
    },
    // Oboe: conical double reed, nasal formants.
    Voicing {
        spec: Spec { mechanism: Mechanism::Conical, loss: (0.95, 0.0), pressure: (0.52, 0.74), shape: 0.35, slope: 0.35, gain: 0.5, cents: 0.0 },
        color: [(1_100.0, 3.0, 6.0), (2_900.0, 2.5, 5.0), (400.0, 0.8, -4.0)],
        range: (230.0, 1_600.0),
    },
];

/// Brass mutes: filters `(kind, hz, q, dB)`; kind 0 peak, 1 highpass, 2 lowpass.
const MUTES: [[(u8, f32, f32, f32); 3]; 4] = [
    [(0, 1_000.0, 1.0, 0.0), (0, 1_000.0, 1.0, 0.0), (0, 1_000.0, 1.0, 0.0)],
    // Straight: thin, buzzy.
    [(1, 650.0, 0.8, 0.0), (0, 1_800.0, 2.0, 7.0), (2, 6_500.0, 0.7, 0.0)],
    // Cup: round and soft.
    [(2, 1_600.0, 0.9, 0.0), (0, 850.0, 1.5, 5.0), (1, 180.0, 0.7, 0.0)],
    // Harmon: a nasal, singing bandpass.
    [(1, 950.0, 1.0, 0.0), (0, 1_700.0, 3.0, 10.0), (2, 4_200.0, 1.5, 0.0)],
];

/// The section, its model settings and the radiation filters.
struct Core {
    section: Section<Blown>,
    blow: BlowCfg,
    color: [[Biquad; 3]; 2],
    mute: [[Biquad; 3]; 2],
    mute_on: bool,
    dc: [f32; 2],
    expression: f32,
    tail: u32,
    sample_rate: f32,
}

impl Core {
    fn new(seed: u32, voicing: &Voicing) -> Self {
        Self {
            section: Section::new(seed),
            blow: BlowCfg { spec: voicing.spec, brassiness: 0.5, brightness: 0.5, noise: 0.2 },
            color: Default::default(),
            mute: Default::default(),
            mute_on: false,
            dc: [0.0; 2],
            expression: 1.0,
            tail: 0,
            sample_rate: 0.0,
        }
    }

    fn set_voicing(&mut self, v: &Voicing, mute: usize, sr: f32) {
        self.blow.spec = v.spec;
        self.section.cfg.min_hz = 25.0;
        self.section.cfg.max_hz = v.range.1 * 1.6;
        for side in &mut self.color {
            for (f, &(hz, q, db)) in side.iter_mut().zip(&v.color) {
                f.coefs = BiquadCoefs::peaking(hz.min(sr * 0.45), q, db, sr);
            }
        }
        self.mute_on = mute > 0;
        for side in &mut self.mute {
            for (f, &(kind, hz, q, db)) in side.iter_mut().zip(&MUTES[mute.min(3)]) {
                f.coefs = match kind {
                    1 => BiquadCoefs::highpass(hz, q, sr),
                    2 => BiquadCoefs::lowpass(hz.min(sr * 0.45), q, sr),
                    _ => BiquadCoefs::peaking(hz.min(sr * 0.45), q, db, sr),
                };
            }
        }
    }

    fn set_sample_rate(&mut self, sr: f32) {
        self.sample_rate = sr;
        self.section.set_sample_rate(sr);
        self.reset();
    }

    fn reset(&mut self) {
        self.section.all_notes_off();
        self.color.iter_mut().flatten().for_each(Biquad::reset);
        self.mute.iter_mut().flatten().for_each(Biquad::reset);
        self.dc = [0.0; 2];
        self.tail = 0;
    }

    fn note_on(&mut self, note: u8, velocity: f32) {
        self.section.note_on(note, velocity);
        self.tail = (0.5 * self.sample_rate) as u32;
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32], gain: f32) {
        if self.tail == 0 && !self.section.any_active() {
            return;
        }
        let sr = self.sample_rate;
        let dc_coef = 1.0 - (-std::f32::consts::TAU * 12.0 / sr).exp();
        let expr_coef = 1.0 - (-1.0 / (0.02 * sr)).exp();
        let mut done = 0;
        while done < left.len() {
            let n = CHUNK.min(left.len() - done);
            let mut buses = [[[0.0f32; CHUNK]; 2]; BUSES];
            self.section.render_chunk(&self.blow, n, &mut buses);
            for i in 0..n {
                self.expression += (self.section.ctl.expression - self.expression) * expr_coef;
                let mut out = [0.0f32; 2];
                for (ch, o) in out.iter_mut().enumerate() {
                    let mut y = buses[0][ch][i];
                    for f in &mut self.color[ch] {
                        y = f.process(y);
                    }
                    if self.mute_on {
                        for f in &mut self.mute[ch] {
                            y = f.process(y);
                        }
                    }
                    self.dc[ch] += (y - self.dc[ch]) * dc_coef;
                    *o = soft_clip(flush_denormal(y - self.dc[ch]) * gain * self.expression);
                }
                left[done + i] += out[0];
                right[done + i] += out[1];
            }
            done += n;
            if self.section.any_active() {
                self.tail = (0.5 * sr) as u32;
            } else {
                self.tail = self.tail.saturating_sub(n as u32);
            }
        }
        if self.tail == 0 {
            self.color.iter_mut().flatten().for_each(Biquad::reset);
            self.mute.iter_mut().flatten().for_each(Biquad::reset);
        }
    }
}

const BRASS_NAMES: &[&str] = &["Trumpet", "Trombone", "French Horn", "Tuba", "Sax"];
const MUTE_NAMES: &[&str] = &["Open", "Straight", "Cup", "Harmon"];

const B_INSTRUMENT: u32 = 0;
const B_MUTE: u32 = 1;
const B_BRASSINESS: u32 = 2;
const B_GROWL: u32 = 3;
const B_FALL: u32 = 4;
const B_SCOOP: u32 = 5;
const B_ATTACK: u32 = 6;
const B_RELEASE: u32 = 7;
const B_VIB_DEPTH: u32 = 8;
const B_VIB_RATE: u32 = 9;
const B_VIB_DELAY: u32 = 10;
const B_MODE: u32 = 11;
const B_PORTAMENTO: u32 = 12;
const B_SIZE: u32 = 13;
const B_DETUNE: u32 = 14;
const B_TIMING: u32 = 15;
const B_WIDTH: u32 = 16;
const B_BRIGHTNESS: u32 = 17;
const B_BREATH: u32 = 18;
const B_VOLUME: u32 = 19;
const B_PARAMS: usize = 20;

static BRASS_PARAMS: [ParamInfo; B_PARAMS] = [
    ParamInfo::choice(B_INSTRUMENT, "Instrument", BRASS_NAMES, 0).group("Brass"),
    ParamInfo::choice(B_MUTE, "Mute", MUTE_NAMES, 0).group("Brass"),
    ParamInfo::new(B_BRASSINESS, "Brassiness", 0.0, 1.0, 0.5, Unit::Percent).group("Brass"),
    ParamInfo::new(B_GROWL, "Growl", 0.0, 1.0, 0.0, Unit::Percent).group("Brass"),
    ParamInfo::new(B_FALL, "Fall", 0.0, 12.0, 0.0, Unit::Semitones).group("Phrasing"),
    ParamInfo::new(B_SCOOP, "Scoop", 0.0, 5.0, 0.0, Unit::Semitones).group("Phrasing"),
    ParamInfo::new(B_ATTACK, "Attack", 0.005, 1.0, 0.04, Unit::Seconds).log().group("Phrasing"),
    ParamInfo::new(B_RELEASE, "Release", 0.02, 2.0, 0.15, Unit::Seconds).log().group("Phrasing"),
    ParamInfo::new(B_VIB_DEPTH, "Vibrato Depth", 0.0, 60.0, 10.0, Unit::Cents).group("Vibrato"),
    ParamInfo::new(B_VIB_RATE, "Vibrato Rate", 3.0, 8.0, 5.5, Unit::Hz).group("Vibrato"),
    ParamInfo::new(B_VIB_DELAY, "Vibrato Delay", 0.0, 1.5, 0.35, Unit::Seconds).group("Vibrato"),
    ParamInfo::choice(B_MODE, "Play Mode", PLAY_MODES, 0).group("Ensemble"),
    ParamInfo::new(B_PORTAMENTO, "Portamento", 0.0, 0.5, 0.06, Unit::Seconds).group("Ensemble"),
    ParamInfo::choice(B_SIZE, "Section Size", SECTION_SIZES, 0).group("Ensemble"),
    ParamInfo::new(B_DETUNE, "Detune", 0.0, 25.0, 6.0, Unit::Cents).group("Ensemble"),
    ParamInfo::new(B_TIMING, "Timing", 0.0, 0.06, 0.015, Unit::Seconds).group("Ensemble"),
    ParamInfo::new(B_WIDTH, "Stereo Width", 0.0, 1.0, 0.6, Unit::Percent).group("Ensemble"),
    ParamInfo::new(B_BRIGHTNESS, "Brightness", 0.0, 1.0, 0.5, Unit::Percent).group("Output"),
    ParamInfo::new(B_BREATH, "Breath", 0.0, 1.0, 0.2, Unit::Percent).group("Output"),
    ParamInfo::new(B_VOLUME, "Volume", -40.0, 6.0, -10.0, Unit::Db).group("Output"),
];

/// Modeled brass: trumpet, trombone, horn, tuba and saxophone.
pub struct Brass {
    values: [f32; B_PARAMS],
    core: Core,
}

impl Default for Brass {
    fn default() -> Self {
        Self::new()
    }
}

impl Brass {
    pub fn new() -> Self {
        let mut b = Self { values: BRASS_PARAMS.map(|p| p.default), core: Core::new(0xB2A5_5001, &BRASS[0]) };
        b.set_sample_rate(48_000.0);
        b
    }

    fn v(&self, id: u32) -> f32 {
        self.values[id as usize]
    }

    fn update(&mut self) {
        let sr = self.core.sample_rate;
        let inst = (self.v(B_INSTRUMENT).round() as usize).min(BRASS.len() - 1);
        self.core.set_voicing(&BRASS[inst], self.v(B_MUTE).round() as usize, sr);
        self.core.blow.brassiness = self.v(B_BRASSINESS);
        self.core.blow.brightness = self.v(B_BRIGHTNESS);
        self.core.blow.noise = self.v(B_BREATH);
        let c = &mut self.core.section.cfg;
        c.articulation = Articulation::Sustain;
        c.attack = self.values[B_ATTACK as usize];
        c.release = self.values[B_RELEASE as usize];
        c.vib_depth = self.values[B_VIB_DEPTH as usize];
        c.vib_rate = self.values[B_VIB_RATE as usize];
        c.vib_delay = self.values[B_VIB_DELAY as usize];
        c.legato = self.values[B_MODE as usize] >= 0.5;
        c.portamento = self.values[B_PORTAMENTO as usize];
        c.players = SECTION_PLAYERS[(self.values[B_SIZE as usize].round() as usize).min(4)];
        c.detune = self.values[B_DETUNE as usize];
        c.timing = self.values[B_TIMING as usize];
        c.width = self.values[B_WIDTH as usize];
        c.fall = self.values[B_FALL as usize];
        c.scoop = self.values[B_SCOOP as usize];
        c.growl = self.values[B_GROWL as usize];
        c.split = None;
        c.bus = 0;
    }
}

impl Params for Brass {
    fn params(&self) -> &'static [ParamInfo] {
        &BRASS_PARAMS
    }

    fn get_param(&self, id: u32) -> f32 {
        self.values.get(id as usize).copied().unwrap_or(0.0)
    }

    fn set_param(&mut self, id: u32, value: f32) {
        let Some(info) = BRASS_PARAMS.get(id as usize) else { return };
        let v = info.clamp(value);
        if self.values[id as usize] != v {
            self.values[id as usize] = v;
            self.update();
        }
    }
}

impl Instrument for Brass {
    fn kind(&self) -> &'static str {
        "brass"
    }

    fn is_melodic(&self) -> bool {
        true
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.core.sample_rate = sample_rate;
        self.update();
        self.core.set_sample_rate(sample_rate);
    }

    fn note_on(&mut self, note: u8, velocity: f32) {
        self.core.note_on(note, velocity);
    }

    fn note_off(&mut self, note: u8) {
        self.core.section.note_off(note);
    }

    fn supports_expression(&self) -> bool {
        true
    }

    fn note_expression(&mut self, key: u8, expr: NoteExpression) {
        self.core.section.note_expression(key, expr);
    }

    fn controller(&mut self, ctl: Controller) {
        self.core.section.controller(ctl);
    }

    fn all_notes_off(&mut self) {
        self.core.reset();
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let gain = db_to_gain(self.v(B_VOLUME));
        self.core.render(left, right, gain);
    }
}

const WIND_NAMES: &[&str] = &["Flute", "Clarinet", "Oboe"];

const W_INSTRUMENT: u32 = 0;
const W_BREATH: u32 = 1;
const W_BRIGHTNESS: u32 = 2;
const W_ATTACK: u32 = 3;
const W_RELEASE: u32 = 4;
const W_VIB_DEPTH: u32 = 5;
const W_VIB_RATE: u32 = 6;
const W_VIB_DELAY: u32 = 7;
const W_MODE: u32 = 8;
const W_PORTAMENTO: u32 = 9;
const W_SIZE: u32 = 10;
const W_DETUNE: u32 = 11;
const W_WIDTH: u32 = 12;
const W_VOLUME: u32 = 13;
const W_PARAMS: usize = 14;

static WIND_PARAMS: [ParamInfo; W_PARAMS] = [
    ParamInfo::choice(W_INSTRUMENT, "Instrument", WIND_NAMES, 0).group("Winds"),
    ParamInfo::new(W_BREATH, "Breath", 0.0, 1.0, 0.35, Unit::Percent).group("Winds"),
    ParamInfo::new(W_BRIGHTNESS, "Brightness", 0.0, 1.0, 0.5, Unit::Percent).group("Winds"),
    ParamInfo::new(W_ATTACK, "Attack", 0.01, 1.0, 0.06, Unit::Seconds).log().group("Phrasing"),
    ParamInfo::new(W_RELEASE, "Release", 0.02, 2.0, 0.12, Unit::Seconds).log().group("Phrasing"),
    ParamInfo::new(W_VIB_DEPTH, "Vibrato Depth", 0.0, 60.0, 16.0, Unit::Cents).group("Vibrato"),
    ParamInfo::new(W_VIB_RATE, "Vibrato Rate", 3.0, 8.0, 5.0, Unit::Hz).group("Vibrato"),
    ParamInfo::new(W_VIB_DELAY, "Vibrato Delay", 0.0, 1.5, 0.25, Unit::Seconds).group("Vibrato"),
    ParamInfo::choice(W_MODE, "Play Mode", PLAY_MODES, 1).group("Ensemble"),
    ParamInfo::new(W_PORTAMENTO, "Portamento", 0.0, 0.5, 0.05, Unit::Seconds).group("Ensemble"),
    ParamInfo::choice(W_SIZE, "Section Size", SECTION_SIZES, 0).group("Ensemble"),
    ParamInfo::new(W_DETUNE, "Detune", 0.0, 25.0, 6.0, Unit::Cents).group("Ensemble"),
    ParamInfo::new(W_WIDTH, "Stereo Width", 0.0, 1.0, 0.4, Unit::Percent).group("Ensemble"),
    ParamInfo::new(W_VOLUME, "Volume", -40.0, 6.0, -4.0, Unit::Db).group("Output"),
];

/// Modeled woodwinds: flute, clarinet and oboe.
pub struct Winds {
    values: [f32; W_PARAMS],
    core: Core,
}

impl Default for Winds {
    fn default() -> Self {
        Self::new()
    }
}

impl Winds {
    pub fn new() -> Self {
        let mut w = Self { values: WIND_PARAMS.map(|p| p.default), core: Core::new(0x3141_5926, &WINDS[0]) };
        w.set_sample_rate(48_000.0);
        w
    }

    fn v(&self, id: u32) -> f32 {
        self.values[id as usize]
    }

    fn update(&mut self) {
        let sr = self.core.sample_rate;
        let inst = (self.v(W_INSTRUMENT).round() as usize).min(WINDS.len() - 1);
        self.core.set_voicing(&WINDS[inst], 0, sr);
        self.core.blow.brassiness = 0.0;
        self.core.blow.brightness = self.v(W_BRIGHTNESS);
        self.core.blow.noise = self.v(W_BREATH) * 2.0;
        let c = &mut self.core.section.cfg;
        c.articulation = Articulation::Sustain;
        c.attack = self.values[W_ATTACK as usize];
        c.release = self.values[W_RELEASE as usize];
        c.vib_depth = self.values[W_VIB_DEPTH as usize];
        c.vib_rate = self.values[W_VIB_RATE as usize];
        c.vib_delay = self.values[W_VIB_DELAY as usize];
        c.legato = self.values[W_MODE as usize] >= 0.5;
        c.portamento = self.values[W_PORTAMENTO as usize];
        c.players = SECTION_PLAYERS[(self.values[W_SIZE as usize].round() as usize).min(4)];
        c.detune = self.values[W_DETUNE as usize];
        c.timing = 0.015;
        c.width = self.values[W_WIDTH as usize];
        c.fall = 0.0;
        c.scoop = 0.0;
        c.growl = 0.0;
        c.split = None;
        c.bus = 0;
    }
}

impl Params for Winds {
    fn params(&self) -> &'static [ParamInfo] {
        &WIND_PARAMS
    }

    fn get_param(&self, id: u32) -> f32 {
        self.values.get(id as usize).copied().unwrap_or(0.0)
    }

    fn set_param(&mut self, id: u32, value: f32) {
        let Some(info) = WIND_PARAMS.get(id as usize) else { return };
        let v = info.clamp(value);
        if self.values[id as usize] != v {
            self.values[id as usize] = v;
            self.update();
        }
    }
}

impl Instrument for Winds {
    fn kind(&self) -> &'static str {
        "winds"
    }

    fn is_melodic(&self) -> bool {
        true
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.core.sample_rate = sample_rate;
        self.update();
        self.core.set_sample_rate(sample_rate);
    }

    fn note_on(&mut self, note: u8, velocity: f32) {
        self.core.note_on(note, velocity);
    }

    fn note_off(&mut self, note: u8) {
        self.core.section.note_off(note);
    }

    fn supports_expression(&self) -> bool {
        true
    }

    fn note_expression(&mut self, key: u8, expr: NoteExpression) {
        self.core.section.note_expression(key, expr);
    }

    fn controller(&mut self, ctl: Controller) {
        self.core.section.controller(ctl);
    }

    fn all_notes_off(&mut self) {
        self.core.reset();
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let gain = db_to_gain(self.v(W_VOLUME));
        self.core.render(left, right, gain);
    }
}

#[cfg(test)]
mod tests {
    use super::super::wg::test_util::*;
    use super::*;

    const SR: f32 = 48_000.0;

    fn brass(inst: usize) -> Brass {
        let mut b = Brass::new();
        b.set_param(B_INSTRUMENT, inst as f32);
        b.set_param(B_VIB_DEPTH, 0.0);
        b
    }

    fn winds(inst: usize) -> Winds {
        let mut w = Winds::new();
        w.set_param(W_INSTRUMENT, inst as f32);
        w.set_param(W_VIB_DEPTH, 0.0);
        w
    }

    fn blow(x: &mut dyn Instrument, note: u8, wheel: f32, secs: f32) -> Vec<f32> {
        x.controller(Controller::Cc(1, wheel));
        x.note_on(note, 0.8);
        let (l, r) = render(x, (secs * SR) as usize);
        mono(&l, &r)
    }

    /// Core range of each instrument, played mezzo.
    const BRASS_NOTES: [&[u8]; 5] = [&[55, 60, 64, 67, 72], &[40, 46, 52, 58, 62], &[41, 48, 53, 60, 65], &[29, 34, 41, 46], &[49, 56, 61, 66, 73]];
    const WIND_NOTES: [&[u8]; 3] = [&[60, 67, 72, 79, 86, 91], &[50, 57, 64, 71, 79, 86], &[58, 65, 72, 79, 86]];

    #[test]
    fn brass_is_in_tune() {
        for (inst, notes) in BRASS_NOTES.iter().enumerate() {
            for &n in *notes {
                let x = blow(&mut brass(inst), n, 0.5, 1.0);
                let hz = pitch(&x[24_000..], SR).unwrap_or_else(|| panic!("{} {n}: silent", BRASS_NAMES[inst]));
                let off = cents_off(hz, n as f32);
                assert!(off.abs() < 10.0, "{} note {n}: {off:.0} cents", BRASS_NAMES[inst]);
            }
        }
    }

    #[test]
    fn winds_are_in_tune() {
        for (inst, notes) in WIND_NOTES.iter().enumerate() {
            for &n in *notes {
                for wheel in [0.2, 0.9] {
                    let x = blow(&mut winds(inst), n, wheel, 1.0);
                    let hz = pitch(&x[24_000..], SR).unwrap_or_else(|| panic!("{} {n}: silent", WIND_NAMES[inst]));
                    let off = cents_off(hz, n as f32);
                    assert!(off.abs() < 15.0, "{} note {n} wheel {wheel}: {off:.0} cents", WIND_NAMES[inst]);
                }
            }
        }
    }

    #[test]
    fn stable_everywhere() {
        for inst in 0..5 {
            let make = move || -> Box<dyn Instrument> {
                let mut b = brass(inst);
                b.set_param(B_GROWL, (inst % 2) as f32);
                b.set_param(B_MUTE, (inst % 4) as f32);
                b.set_param(B_SIZE, 4.0);
                Box::new(b)
            };
            sweep_stable(&make, 21..=108, 13);
        }
        for inst in 0..3 {
            let make = move || -> Box<dyn Instrument> { Box::new(winds(inst)) };
            sweep_stable(&make, 21..=108, 13);
        }
    }

    #[test]
    fn breath_changes_the_timbre() {
        for inst in 0..5 {
            let c = |wheel| {
                let x = blow(&mut brass(inst), BRASS_NOTES[inst][1], wheel, 1.0);
                (rms(&x[24_000..]), centroid(&x[30_000..34_096], SR))
            };
            let (pl, pc) = c(0.1);
            let (fl, fc) = c(1.0);
            assert!(fl > pl && fc > pc * 1.05, "{}: {pl} {pc} → {fl} {fc}", BRASS_NAMES[inst]);
        }
    }

    #[test]
    fn attacks_are_realistic() {
        for inst in 0..5 {
            let x = blow(&mut brass(inst), BRASS_NOTES[inst][2], 0.6, 1.0);
            let a = attack_time(&x, SR, 0.8);
            assert!(a > 0.005 && a < 0.8, "{}: {a}", BRASS_NAMES[inst]);
        }
        for inst in 0..3 {
            let x = blow(&mut winds(inst), WIND_NOTES[inst][2], 0.6, 1.0);
            let a = attack_time(&x, SR, 0.8);
            assert!(a > 0.01 && a < 0.5, "{}: {a}", WIND_NAMES[inst]);
        }
    }

    #[test]
    fn legato_glides_and_falls_drop() {
        let mut w = winds(0);
        w.note_on(72, 0.8);
        render(&mut w, 24_000);
        w.note_on(76, 0.8);
        render(&mut w, 1_200);
        let mid = w.core.section.pitch_of(76).unwrap();
        assert!(mid > 72.2 && mid < 75.8, "{mid}");
        render(&mut w, 12_000);
        let (l, r) = render(&mut w, 19_200);
        let hz = pitch(&mono(&l, &r), SR).unwrap();
        assert!(cents_off(hz, 76.0).abs() < 15.0, "{hz}");
        // A fall-off after the note-off: the pitch heads down.
        let mut b = brass(0);
        b.set_param(B_FALL, 7.0);
        b.set_param(B_RELEASE, 1.0);
        b.note_on(67, 0.9);
        render(&mut b, 24_000);
        b.note_off(67);
        let (l, r) = render(&mut b, 19_200);
        let late = pitch(&mono(&l, &r)[9_600..], SR).unwrap_or(0.0);
        assert!(late < midi_hz(66.0), "{late}");
    }

    fn midi_hz(n: f32) -> f32 {
        ponzik_dsp::util::midi_to_hz(n)
    }

    #[test]
    fn mutes_darken_or_thin_the_trumpet() {
        let tone = |mute: f32| {
            let mut b = brass(0);
            b.set_param(B_MUTE, mute);
            let x = blow(&mut b, 62, 0.6, 1.0);
            centroid(&x[30_000..34_096], SR)
        };
        assert!(tone(2.0) < tone(0.0) * 0.8, "cup {} open {}", tone(2.0), tone(0.0));
    }
}
