//! Pitch correction ("Auto-Tune"): from a transparent touch-up to the robotic
//! hard tune of modern rap and pop.
//!
//! [`detect`] finds the voice's pitch every 2 ms, [`correct`] decides the
//! pitch it should have (key and scale, retune speed, humanize, Flex-Tune,
//! vibrato, MIDI target notes), and a shifter moves it there: [`psola`]
//! (Modern: pitch-synchronous overlap-add, formants preserved or moved on
//! purpose) or [`splice`] (Classic: the Auto-Tune 5 era sound, lowest
//! latency). What it hears and does goes to the interface through
//! [`AutoTuneShared`].
//!
//! Everything is allocated in `set_sample_rate`; `process` never allocates.

pub mod correct;
pub mod detect;
mod frames;
mod line;
mod psola;
pub mod shared;
mod splice;
pub mod voice;

#[cfg(test)]
mod tests;

use std::any::Any;
use std::sync::Arc;

use ponzik_dsp::smooth::Smoother;
use ponzik_dsp::util::db_to_gain;

use super::{DEFAULT_SR, OFF_ON, Values};
use crate::effect::{ChannelNotes, Effect};
use crate::param::{ParamInfo, Params, Unit};

pub use correct::{Corrected, Corrector, MidiTarget, Settings, hz_to_midi, nearest};
pub use detect::{Detector, Frame, RANGE_NOTES, RANGES};
pub use shared::{AutoTuneShared, HISTORY, PitchPoint};

pub const P_RETUNE: u32 = 0;
pub const P_HUMANIZE: u32 = 1;
pub const P_FLEX: u32 = 2;
pub const P_NATURAL_VIBRATO: u32 = 3;
pub const P_TRANSITION: u32 = 4;
pub const P_KEY: u32 = 5;
pub const P_SCALE: u32 = 6;
pub const P_FOLLOW: u32 = 7;
pub const P_RANGE: u32 = 8;
pub const P_TRACKING: u32 = 9;
pub const P_MODE: u32 = 10;
pub const P_LATENCY: u32 = 11;
pub const P_TRANSPOSE: u32 = 12;
pub const P_REFERENCE: u32 = 13;
pub const P_FORMANT: u32 = 14;
pub const P_FORMANT_SHIFT: u32 = 15;
pub const P_THROAT: u32 = 16;
pub const P_VIB_DEPTH: u32 = 17;
pub const P_VIB_RATE: u32 = 18;
pub const P_VIB_DELAY: u32 = 19;
pub const P_VIB_ONSET: u32 = 20;
pub const P_WIDTH: u32 = 21;
pub const P_MIX: u32 = 22;
pub const P_MIDI_MODE: u32 = 23;
pub const P_MIDI_CHANNEL: u32 = 24;
/// First of the twelve note states (C … B), see [`NOTE_STATES`].
pub const P_NOTE: u32 = 25;
pub const PARAM_COUNT: usize = 37;

/// Keys, as the project's.
pub const KEYS: &[&str] = &["C", "C♯", "D", "E♭", "E", "F", "F♯", "G", "A♭", "A", "B♭", "B"];
/// Scales, in the order of the project's (`ScaleKind::ALL`).
pub const SCALES: &[&str] = &[
    "Major",
    "Minor",
    "Dorian",
    "Phrygian",
    "Lydian",
    "Mixolydian",
    "Locrian",
    "Harmonic minor",
    "Melodic minor",
    "Harmonic major",
    "Major pentatonic",
    "Minor pentatonic",
    "Blues",
    "Major blues",
    "Phrygian dominant",
    "Double harmonic",
    "Hungarian minor",
    "Hirajoshi",
    "In sen",
    "Whole tone",
    "Diminished",
    "Bebop",
    "Chromatic",
];
/// Intervals of each scale of [`SCALES`].
pub const SCALE_STEPS: [&[u8]; 23] = [
    &[0, 2, 4, 5, 7, 9, 11],
    &[0, 2, 3, 5, 7, 8, 10],
    &[0, 2, 3, 5, 7, 9, 10],
    &[0, 1, 3, 5, 7, 8, 10],
    &[0, 2, 4, 6, 7, 9, 11],
    &[0, 2, 4, 5, 7, 9, 10],
    &[0, 1, 3, 5, 6, 8, 10],
    &[0, 2, 3, 5, 7, 8, 11],
    &[0, 2, 3, 5, 7, 9, 11],
    &[0, 2, 4, 5, 7, 8, 11],
    &[0, 2, 4, 7, 9],
    &[0, 3, 5, 7, 10],
    &[0, 3, 5, 6, 7, 10],
    &[0, 2, 3, 4, 7, 9],
    &[0, 1, 4, 5, 7, 8, 10],
    &[0, 1, 4, 5, 7, 8, 11],
    &[0, 2, 3, 6, 7, 8, 11],
    &[0, 2, 3, 7, 8],
    &[0, 1, 5, 7, 10],
    &[0, 2, 4, 6, 8, 10],
    &[0, 1, 3, 4, 6, 7, 9, 10],
    &[0, 2, 4, 5, 7, 9, 10, 11],
    &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
];
pub const CHROMATIC: usize = 22;
/// State of a note: follows the scale, forced in, removed, left alone.
pub const NOTE_STATES: &[&str] = &["Auto", "On", "Off", "Bypass"];
pub const NOTE_AUTO: usize = 0;
pub const NOTE_ON: usize = 1;
pub const NOTE_OFF: usize = 2;
pub const NOTE_BYPASS: usize = 3;
pub const MODES: &[&str] = &["Modern", "Classic"];
pub const LATENCIES: &[&str] = &["Low", "Quality"];
pub const MIDI_MODES: &[&str] = &["Off", "Notes", "Melody"];

const G_CORRECT: &str = "Correction";
const G_SCALE: &str = "Scale";
const G_VOICE: &str = "Voice";
const G_FORMANT: &str = "Formant";
const G_VIBRATO: &str = "Vibrato";
const G_OUT: &str = "Output";
const G_MIDI: &str = "MIDI";

const fn note(i: u32, name: &'static str) -> ParamInfo {
    ParamInfo::choice(P_NOTE + i, name, NOTE_STATES, 0).group(G_SCALE)
}

static PARAMS: [ParamInfo; PARAM_COUNT] = [
    ParamInfo::new(P_RETUNE, "Retune speed", 0.0, 0.4, 0.02, Unit::Seconds).group(G_CORRECT),
    ParamInfo::new(P_HUMANIZE, "Humanize", 0.0, 1.0, 0.0, Unit::Percent).group(G_CORRECT),
    ParamInfo::new(P_FLEX, "Flex-Tune", 0.0, 1.0, 0.0, Unit::Percent).group(G_CORRECT),
    ParamInfo::new(P_NATURAL_VIBRATO, "Natural vibrato", -12.0, 12.0, 0.0, Unit::Db).group(G_CORRECT),
    ParamInfo::new(P_TRANSITION, "Transition", 0.0, 0.25, 0.0, Unit::Seconds).group(G_CORRECT),
    ParamInfo::choice(P_KEY, "Key", KEYS, 0).group(G_SCALE),
    ParamInfo::choice(P_SCALE, "Scale", SCALES, CHROMATIC).group(G_SCALE),
    ParamInfo::choice(P_FOLLOW, "Follow project", OFF_ON, 1).group(G_SCALE),
    ParamInfo::choice(P_RANGE, "Input type", RANGES, 2).group(G_VOICE),
    ParamInfo::new(P_TRACKING, "Tracking", 0.0, 1.0, 0.5, Unit::Percent).group(G_VOICE),
    ParamInfo::choice(P_MODE, "Mode", MODES, 0).group(G_VOICE),
    ParamInfo::choice(P_LATENCY, "Latency", LATENCIES, 1).group(G_VOICE),
    ParamInfo::new(P_TRANSPOSE, "Transpose", -12.0, 12.0, 0.0, Unit::Semitones).group(G_OUT),
    ParamInfo::new(P_REFERENCE, "Reference A4", 430.0, 450.0, 440.0, Unit::Hz).group(G_OUT),
    ParamInfo::choice(P_FORMANT, "Preserve formants", OFF_ON, 1).group(G_FORMANT),
    ParamInfo::new(P_FORMANT_SHIFT, "Formant shift", -12.0, 12.0, 0.0, Unit::Semitones).group(G_FORMANT),
    ParamInfo::new(P_THROAT, "Throat", 0.5, 1.8, 1.0, Unit::Percent).group(G_FORMANT),
    ParamInfo::new(P_VIB_DEPTH, "Vibrato depth", 0.0, 100.0, 0.0, Unit::Cents).group(G_VIBRATO),
    ParamInfo::new(P_VIB_RATE, "Vibrato rate", 1.0, 10.0, 5.5, Unit::Hz).log().group(G_VIBRATO),
    ParamInfo::new(P_VIB_DELAY, "Vibrato delay", 0.0, 1.0, 0.15, Unit::Seconds).group(G_VIBRATO),
    ParamInfo::new(P_VIB_ONSET, "Vibrato onset", 0.0, 1.0, 0.3, Unit::Seconds).group(G_VIBRATO),
    ParamInfo::new(P_WIDTH, "Width", 0.0, 30.0, 0.0, Unit::Cents).group(G_OUT),
    ParamInfo::new(P_MIX, "Mix", 0.0, 1.0, 1.0, Unit::Percent).group(G_OUT),
    ParamInfo::choice(P_MIDI_MODE, "MIDI target", MIDI_MODES, 0).group(G_MIDI),
    ParamInfo::new(P_MIDI_CHANNEL, "MIDI channel", 0.0, 128.0, 0.0, Unit::None).group(G_MIDI),
    note(0, "Note C"),
    note(1, "Note C♯"),
    note(2, "Note D"),
    note(3, "Note E♭"),
    note(4, "Note E"),
    note(5, "Note F"),
    note(6, "Note F♯"),
    note(7, "Note G"),
    note(8, "Note A♭"),
    note(9, "Note A"),
    note(10, "Note B♭"),
    note(11, "Note B"),
];

/// Fiches des paramètres.
pub fn params() -> &'static [ParamInfo; PARAM_COUNT] {
    &PARAMS
}

/// Pitch classes of a scale (bit 0: C … bit 11: B).
pub fn scale_mask(key: usize, scale: usize) -> u16 {
    SCALE_STEPS[scale.min(SCALE_STEPS.len() - 1)].iter().fold(0, |m, &s| m | 1 << ((key + usize::from(s)) % 12))
}

/// Target notes and notes left alone, from the scale and the note states.
pub fn note_masks(get: &dyn Fn(u32) -> f32) -> (u16, u16) {
    let scale = scale_mask(get(P_KEY).round() as usize % 12, get(P_SCALE).round() as usize);
    let (mut allowed, mut bypass) = (0u16, 0u16);
    for pc in 0..12u32 {
        let bit = 1u16 << pc;
        match get(P_NOTE + pc).round() as usize {
            NOTE_ON => allowed |= bit,
            NOTE_OFF => {}
            NOTE_BYPASS => bypass |= bit,
            _ => allowed |= scale & bit,
        }
    }
    (allowed, bypass)
}

/// Quick modes: name, settings they write (the rest goes back to defaults).
pub const QUICK_MODES: [(&str, &[(u32, f32)]); 6] = [
    ("Default", &[]),
    ("Hard Tune", &[(P_RETUNE, 0.0)]),
    ("Natural", &[(P_RETUNE, 0.09), (P_HUMANIZE, 0.45), (P_FLEX, 0.3), (P_TRANSITION, 0.04)]),
    ("Pop", &[(P_RETUNE, 0.012), (P_HUMANIZE, 0.2), (P_FLEX, 0.08), (P_TRANSITION, 0.015)]),
    ("Rap / Trap", &[(P_RETUNE, 0.0), (P_MODE, 1.0), (P_TRANSITION, 0.008), (P_NATURAL_VIBRATO, -12.0)]),
    (
        "Choir / Wide",
        &[(P_RETUNE, 0.03), (P_HUMANIZE, 0.3), (P_WIDTH, 14.0), (P_VIB_DEPTH, 18.0), (P_VIB_RATE, 5.0), (P_NATURAL_VIBRATO, 3.0)],
    ),
];

/// Settings a quick mode leaves alone: scale, notes, input, MIDI routing.
pub fn quick_mode_keeps(id: u32) -> bool {
    matches!(id, P_KEY | P_SCALE | P_FOLLOW | P_RANGE | P_TRACKING | P_LATENCY | P_REFERENCE | P_MIDI_MODE | P_MIDI_CHANNEL) || id >= P_NOTE
}

/// Points published to the interface: one every this many frames.
const PUBLISH_EVERY: u32 = 2;

pub struct AutoTune {
    values: Values<PARAM_COUNT>,
    sample_rate: f32,
    pub(super) detector: Detector,
    corrector: Corrector,
    settings: Settings,
    frames: frames::Frames,
    line: line::Line,
    psola: psola::Psola,
    splice: splice::Splice,
    now: u64,
    latency: usize,
    /// Mode, latency mode and range the shifters are set for.
    layout: (usize, usize, usize),
    classic: bool,
    formant: Option<f64>,
    /// Ratio on each channel's pitch (stereo width).
    spread: [f64; 2],
    mix: Smoother,
    notes: ChannelNotes,
    shared: Arc<AutoTuneShared>,
    publish: u32,
    level: f32,
    level_dry: f32,
    level_wet: f32,
    level_k: f32,
    gain: f32,
}

impl Default for AutoTune {
    fn default() -> Self {
        let mut a = Self {
            values: Values::new(&PARAMS),
            sample_rate: DEFAULT_SR,
            detector: Detector::new(8_000.0),
            corrector: Corrector::new(0.002),
            settings: Settings::default(),
            frames: frames::Frames::new(),
            line: line::Line::new(64),
            psola: psola::Psola::new(8_000.0, 64),
            splice: splice::Splice::new(8_000.0),
            now: 0,
            latency: 0,
            layout: (usize::MAX, 0, 0),
            classic: false,
            formant: Some(1.0),
            spread: [1.0; 2],
            mix: Smoother::new(1.0),
            notes: ChannelNotes::EMPTY,
            shared: Arc::new(AutoTuneShared::new(0.004)),
            publish: 0,
            level: -120.0,
            level_dry: 0.0,
            level_wet: 0.0,
            level_k: 0.001,
            gain: 1.0,
        };
        a.set_sample_rate(DEFAULT_SR);
        a
    }
}

impl AutoTune {
    /// State shared with the interface.
    pub fn shared_state(&self) -> Arc<AutoTuneShared> {
        self.shared.clone()
    }

    fn update(&mut self) {
        let v = &self.values;
        let get = |id: u32| v.get(id);
        let (allowed, bypass) = note_masks(&get);
        self.classic = v.index(P_MODE) == 1;
        self.settings = Settings {
            retune: v.get(P_RETUNE),
            humanize: v.get(P_HUMANIZE),
            flex: v.get(P_FLEX),
            natural_vibrato: db_to_gain(v.get(P_NATURAL_VIBRATO)),
            transition: v.get(P_TRANSITION),
            allowed,
            bypass,
            midi: match v.index(P_MIDI_MODE) {
                1 => MidiTarget::Notes,
                2 => MidiTarget::Melody,
                _ => MidiTarget::Off,
            },
            transpose: v.get(P_TRANSPOSE).round(),
            reference_hz: v.get(P_REFERENCE),
            vib_depth: v.get(P_VIB_DEPTH) / 100.0,
            vib_rate: v.get(P_VIB_RATE),
            vib_delay: v.get(P_VIB_DELAY),
            vib_onset: v.get(P_VIB_ONSET),
            classic: self.classic,
        };
        let formant = 2f64.powf(f64::from(v.get(P_FORMANT_SHIFT)) / 12.0) / f64::from(v.get(P_THROAT));
        self.formant = v.on(P_FORMANT).then_some(formant);
        let spread = f64::from(v.get(P_WIDTH)) / 2_400.0;
        self.spread = [2f64.powf(-spread), 2f64.powf(spread)];
        self.mix.set_target(v.get(P_MIX));
        self.detector.configure(v.index(P_RANGE), v.get(P_TRACKING), v.index(P_LATENCY) == 1);
        let layout = (v.index(P_MODE), v.index(P_LATENCY), v.index(P_RANGE));
        if layout != self.layout {
            self.layout = layout;
            self.relayout();
        }
    }

    /// Latency and shifters for the mode, latency mode and range.
    fn relayout(&mut self) {
        let sr = self.sample_rate;
        let p_max = self.detector.longest_period();
        let hop = self.detector.hop_samples() as f32;
        let quality = self.layout.1 == 1;
        self.latency = if self.classic {
            let low = splice::Splice::floor(sr) + 0.5 * f64::from(p_max);
            let q = if quality { (self.detector.span() + hop + 8.0) as f64 } else { 0.0 };
            low.max(q).ceil() as usize
        } else if quality {
            (2.0 * p_max + self.detector.span() - 0.5 * p_max + hop + 16.0).ceil() as usize
        } else {
            (2.0 * p_max).ceil() as usize + 8
        };
        let ahead = if quality { f64::from(hop) } else { f64::INFINITY };
        self.psola.configure(self.latency, p_max, ahead);
        self.splice.configure(self.latency, p_max);
        self.shared.set_latency(self.latency);
    }

    /// Note held on the source channel, from the notes of all the channels.
    fn source_notes(&self, notes: &[ChannelNotes]) -> ChannelNotes {
        let ch = self.values.get(P_MIDI_CHANNEL).round() as usize;
        if ch == 0 { ChannelNotes::EMPTY } else { notes.get(ch - 1).copied().unwrap_or_default() }
    }

    #[inline]
    fn frame(&mut self, f: Frame) {
        let c = self.corrector.process(&self.settings, f.hz, f.voiced, &self.notes);
        self.frames.push(frames::Point { time: f.time, hz: f.hz, voiced: f.voiced, shift: if f.voiced { c.shift } else { 0.0 } });
        self.level = f.level_db;
        self.publish += 1;
        if self.publish >= PUBLISH_EVERY {
            self.publish = 0;
            self.shared.push(PitchPoint { input: c.input, output: c.output, note: c.note, bypassed: c.bypassed, level_db: self.level });
        }
    }
}

impl Params for AutoTune {
    fn params(&self) -> &'static [ParamInfo] {
        &PARAMS
    }

    fn get_param(&self, id: u32) -> f32 {
        self.values.get(id)
    }

    fn set_param(&mut self, id: u32, value: f32) {
        if self.values.set(&PARAMS, id, value) {
            self.update();
        }
    }
}

impl Effect for AutoTune {
    fn kind(&self) -> &'static str {
        "autotune"
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate.max(8_000.0);
        let sr = self.sample_rate;
        self.detector = Detector::new(sr);
        self.corrector = Corrector::new(self.detector.hop_samples() as f32 / sr);
        // A quarter second of input covers the longest latency and grains.
        let len = (0.25 * sr) as usize;
        self.line = line::Line::new(len);
        self.psola = psola::Psola::new(sr, len);
        self.splice = splice::Splice::new(sr);
        self.frames = frames::Frames::new();
        self.mix.set_time(0.02, sr);
        self.level_k = 1.0 - (-1.0 / (0.02 * sr)).exp();
        self.shared.set_point_secs(PUBLISH_EVERY as f32 * self.corrector.frame_secs());
        self.layout = (usize::MAX, 0, 0);
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        self.detector.reset();
        self.corrector.reset();
        self.frames.clear();
        self.line.clear();
        self.psola.reset();
        self.splice.reset();
        self.now = 0;
        self.mix.snap();
        (self.level_dry, self.level_wet, self.gain) = (0.0, 0.0, 1.0);
    }

    fn latency(&self) -> usize {
        self.latency
    }

    fn channel_notes(&mut self, notes: &[ChannelNotes]) {
        self.notes = self.source_notes(notes);
        self.shared.set_midi(&self.notes);
    }

    fn shared(&self) -> Option<Arc<dyn Any + Send + Sync>> {
        Some(self.shared.clone())
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let lat = self.latency as i64;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let (xl, xr) = (if l.is_finite() { *l } else { 0.0 }, if r.is_finite() { *r } else { 0.0 });
            self.line.write(self.now, xl, xr);
            self.now += 1;
            if let Some(f) = self.detector.push(0.5 * (xl + xr)) {
                self.frame(f);
            }
            let now = self.now;
            let (wl, wr) = if self.classic {
                let [sl, sr] = self.spread;
                (self.splice.step(0, &self.line, &self.frames, now, sl), self.splice.step(1, &self.line, &self.frames, now, sr))
            } else {
                self.psola.find_marks(&self.line, &self.frames, now);
                let [sl, sr] = self.spread;
                self.psola.synth(0, &self.line, &self.frames, now, sl, self.formant);
                self.psola.synth(1, &self.line, &self.frames, now, sr, self.formant);
                (self.psola.output(0, &self.line, now), self.psola.output(1, &self.line, now))
            };
            let o = now as i64 - 1 - lat;
            let (dl, dr) = (self.line.get(0, o), self.line.get(1, o));
            // The shifted voice keeps the level of the voice (a grain's
            // energy depends on how the spectrum falls off).
            self.level_dry += (dl * dl + dr * dr - self.level_dry) * self.level_k;
            self.level_wet += (wl * wl + wr * wr - self.level_wet) * self.level_k;
            let want = if self.level_wet > 1e-12 { (self.level_dry / self.level_wet).sqrt().clamp(0.5, 2.0) } else { 1.0 };
            self.gain += (want - self.gain) * self.level_k;
            let (wl, wr) = if self.classic { (wl, wr) } else { (wl * self.gain, wr * self.gain) };
            let mix = self.mix.next_value();
            *l = dl + (wl - dl) * mix;
            *r = dr + (wr - dr) * mix;
        }
        if self.level_dry < 1e-20 {
            self.level_dry = 0.0;
        }
        if self.level_wet < 1e-20 {
            self.level_wet = 0.0;
        }
    }
}
