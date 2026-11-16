//! Drum Pads: sixteen sample pads in a 4×4 grid, the FPC / Drum Rack
//! equivalent.
//!
//! Pad `p` answers note `36 + p` (General MIDI layout: 36 kick, 38 snare,
//! 42 closed hat, 46 open hat…). Each pad holds up to four velocity layers,
//! picked by how hard the pad is hit, and has its own volume, pan, tuning,
//! start offset, attack/decay envelope, reverse, filter, choke group and
//! velocity sensitivity. Hitting a pad in a choke group cuts every voice of
//! that group (the classic closed hat cutting the open hat) with a short fade.
//!
//! Samples are set from the UI thread ([`Instrument::set_pad_sample`]) before
//! the instrument goes to the engine; the audio thread never allocates.
//!
//! Every pad plays on the channel's main output: the engine has one stereo
//! output per channel, so per-pad mixer tracks are not available.

use std::sync::Arc;

use ponzik_dsp::filter::{FilterMode, Svf};
use ponzik_dsp::util::{balance_gains, db_to_gain, decay_coef, gain_to_db};

use super::common::{pick_voice, read_cubic, soft_clip};
use crate::Instrument;
use crate::param::{ParamInfo, Params, Unit};
use crate::sample::Sample;

/// Number of pads.
pub const PADS: usize = 16;
/// Velocity layers per pad.
pub const LAYERS: usize = 4;
/// Note of the first pad.
pub const FIRST_NOTE: u8 = 36;
/// Shared voice pool.
const VOICES: usize = 32;
/// Below −80 dB, a voice is done.
const SILENCE: f32 = 1e-4;
/// Fade of a choked voice.
const CHOKE_TAU: f32 = 0.003;
/// Release of a gated voice after its note-off.
const GATE_TAU: f32 = 0.02;
/// Decay at or above this value means "play the whole sample".
pub const DECAY_FULL: f32 = 9.99;

/// General MIDI names of the sixteen pads (notes 36 to 51).
pub const PAD_NAMES: [&str; PADS] = [
    "Kick",
    "Rim",
    "Snare",
    "Clap",
    "Snare 2",
    "Floor Tom",
    "Closed Hat",
    "Floor Tom 2",
    "Pedal Hat",
    "Low Tom",
    "Open Hat",
    "Mid Tom",
    "Mid Tom 2",
    "Crash",
    "High Tom",
    "Ride",
];

const OFF_ON: &[&str] = &["Off", "On"];
const FILTERS: &[&str] = &["Off", "LP", "HP"];
const CHOKES: &[&str] = &["None", "1", "2", "3", "4"];
/// Shot plays the whole hit, Gate stops at the note-off.
const MODES: &[&str] = &["Shot", "Gate"];

/// Parameter slots of a pad: the id of slot `s` of pad `p` is `16p + s`.
pub const S_VOLUME: u32 = 0;
pub const S_PAN: u32 = 1;
pub const S_TUNE: u32 = 2;
pub const S_FINE: u32 = 3;
pub const S_START: u32 = 4;
pub const S_ATTACK: u32 = 5;
pub const S_DECAY: u32 = 6;
pub const S_REVERSE: u32 = 7;
pub const S_FILTER: u32 = 8;
pub const S_CUTOFF: u32 = 9;
pub const S_RESO: u32 = 10;
pub const S_CHOKE: u32 = 11;
pub const S_VELOCITY: u32 = 12;
pub const S_MODE: u32 = 13;
const SLOTS: usize = 14;
const STRIDE: u32 = 16;
/// Master volume of the instrument.
pub const P_VOLUME: u32 = PADS as u32 * STRIDE;
const N_PARAMS: usize = PADS * SLOTS + 1;

/// Parameter id of `slot` for `pad`.
pub const fn pad_param(pad: usize, slot: u32) -> u32 {
    pad as u32 * STRIDE + slot
}

/// Default choke group of a pad: the three hi-hats share group 1.
const fn default_choke(pad: usize) -> usize {
    match pad {
        6 | 8 | 10 => 1,
        _ => 0,
    }
}

const fn build_params() -> [ParamInfo; N_PARAMS] {
    let mut out = [ParamInfo::new(0, "", 0.0, 0.0, 0.0, Unit::None); N_PARAMS];
    let mut p = 0;
    while p < PADS {
        let g = PAD_NAMES[p];
        let b = p * SLOTS;
        out[b] = ParamInfo::new(pad_param(p, S_VOLUME), "Volume", -40.0, 6.0, 0.0, Unit::Db).group(g);
        out[b + 1] = ParamInfo::new(pad_param(p, S_PAN), "Pan", -1.0, 1.0, 0.0, Unit::Pan).group(g);
        out[b + 2] = ParamInfo::new(pad_param(p, S_TUNE), "Tune", -24.0, 24.0, 0.0, Unit::Semitones).group(g);
        out[b + 3] = ParamInfo::new(pad_param(p, S_FINE), "Fine", -100.0, 100.0, 0.0, Unit::Cents).group(g);
        out[b + 4] = ParamInfo::new(pad_param(p, S_START), "Start", 0.0, 0.95, 0.0, Unit::Percent).group(g);
        out[b + 5] = ParamInfo::new(pad_param(p, S_ATTACK), "Attack", 0.000_5, 2.0, 0.000_5, Unit::Seconds).log().group(g);
        out[b + 6] = ParamInfo::new(pad_param(p, S_DECAY), "Decay", 0.01, 10.0, 10.0, Unit::Seconds).log().group(g);
        out[b + 7] = ParamInfo::choice(pad_param(p, S_REVERSE), "Reverse", OFF_ON, 0).group(g);
        out[b + 8] = ParamInfo::choice(pad_param(p, S_FILTER), "Filter", FILTERS, 0).group(g);
        out[b + 9] = ParamInfo::new(pad_param(p, S_CUTOFF), "Cutoff", 20.0, 20_000.0, 20_000.0, Unit::Hz).log().group(g);
        out[b + 10] = ParamInfo::new(pad_param(p, S_RESO), "Resonance", 0.0, 1.0, 0.0, Unit::Percent).group(g);
        out[b + 11] = ParamInfo::choice(pad_param(p, S_CHOKE), "Choke", CHOKES, default_choke(p)).group(g);
        out[b + 12] = ParamInfo::new(pad_param(p, S_VELOCITY), "Velocity", 0.0, 1.0, 1.0, Unit::Percent).group(g);
        out[b + 13] = ParamInfo::choice(pad_param(p, S_MODE), "Mode", MODES, 0).group(g);
        p += 1;
    }
    out[PADS * SLOTS] = ParamInfo::new(P_VOLUME, "Volume", -40.0, 6.0, -3.0, Unit::Db).group("Master");
    out
}

static PARAMS: [ParamInfo; N_PARAMS] = build_params();

/// Settings of one pad, in engine units.
#[derive(Clone, Copy, Debug)]
struct PadSettings {
    gain: f32,
    pan: f32,
    tune: f32,
    fine: f32,
    start: f32,
    attack: f32,
    decay: f32,
    reverse: bool,
    filter: u8,
    cutoff: f32,
    reso: f32,
    choke: u8,
    velocity: f32,
    gate: bool,
}

struct Pad {
    set: PadSettings,
    layers: [Option<Arc<Sample>>; LAYERS],
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
    Attack,
    Decay,
    /// Note-off in gate mode, or choked: fast fade to silence.
    Fade,
}

struct Voice {
    active: bool,
    pad: usize,
    layer: usize,
    note: u8,
    started_at: u64,
    /// Position in playback direction, in frames from the start offset.
    pos: f64,
    amp: f32,
    env: f32,
    stage: Stage,
    fade_coef: f32,
    filter: [Svf; 2],
}

/// Sixteen sample pads with choke groups and velocity layers.
pub struct DrumPads {
    pads: [Pad; PADS],
    gain: f32,
    sample_rate: f32,
    voices: [Voice; VOICES],
    counter: u64,
}

impl Default for DrumPads {
    fn default() -> Self {
        Self::new()
    }
}

/// Amplitude for a hit: `sensitivity` 0 plays every hit at full level, 1
/// follows the velocity linearly.
pub fn velocity_gain(sensitivity: f32, velocity: f32) -> f32 {
    let (s, v) = (sensitivity.clamp(0.0, 1.0), velocity.clamp(0.0, 1.0));
    1.0 - s * (1.0 - v)
}

/// Playback rate for a tuning in semitones and cents.
pub fn tune_ratio(semitones: f32, cents: f32) -> f64 {
    2f64.powf((semitones as f64 + cents as f64 / 100.0) / 12.0)
}

impl DrumPads {
    pub fn new() -> Self {
        let sr = 48_000.0;
        let set = PadSettings {
            gain: 1.0,
            pan: 0.0,
            tune: 0.0,
            fine: 0.0,
            start: 0.0,
            attack: 0.000_5,
            decay: 10.0,
            reverse: false,
            filter: 0,
            cutoff: 20_000.0,
            reso: 0.0,
            choke: 0,
            velocity: 1.0,
            gate: false,
        };
        let mut s = Self {
            pads: std::array::from_fn(|_| Pad { set, layers: Default::default() }),
            gain: 1.0,
            sample_rate: sr,
            voices: std::array::from_fn(|_| Voice {
                active: false,
                pad: 0,
                layer: 0,
                note: 0,
                started_at: 0,
                pos: 0.0,
                amp: 0.0,
                env: 0.0,
                stage: Stage::Attack,
                fade_coef: 0.0,
                filter: std::array::from_fn(|_| Svf::new(FilterMode::LowPass, 20_000.0, 0.707, sr)),
            }),
            counter: 0,
        };
        for p in &PARAMS {
            s.set_param(p.id, p.default);
        }
        s
    }

    /// Sample of a layer of a pad.
    pub fn pad_sample(&self, pad: usize, layer: usize) -> Option<&Arc<Sample>> {
        self.pads.get(pad)?.layers.get(layer)?.as_ref()
    }

    /// Layer slot played by a hit of `velocity` on `pad`: the loaded layers
    /// split the velocity range evenly, softest first.
    pub fn layer_for(&self, pad: usize, velocity: f32) -> Option<usize> {
        let layers = &self.pads.get(pad)?.layers;
        let n = layers.iter().filter(|l| l.as_ref().is_some_and(|s| !s.is_empty())).count();
        if n == 0 {
            return None;
        }
        let k = ((velocity.clamp(0.0, 1.0) * n as f32) as usize).min(n - 1);
        layers.iter().enumerate().filter(|(_, l)| l.as_ref().is_some_and(|s| !s.is_empty())).nth(k).map(|(i, _)| i)
    }

    /// Number of voices playing.
    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active).count()
    }

    fn fade(&mut self, idx: usize, tau: f32) {
        let coef = decay_coef(tau, self.sample_rate);
        let v = &mut self.voices[idx];
        v.stage = Stage::Fade;
        v.fade_coef = coef;
    }
}

impl Params for DrumPads {
    fn params(&self) -> &'static [ParamInfo] {
        &PARAMS
    }

    fn get_param(&self, id: u32) -> f32 {
        if id == P_VOLUME {
            return gain_to_db(self.gain);
        }
        let (pad, slot) = ((id / STRIDE) as usize, id % STRIDE);
        let Some(p) = self.pads.get(pad) else { return 0.0 };
        let s = &p.set;
        let b = |x: bool| f32::from(u8::from(x));
        match slot {
            S_VOLUME => gain_to_db(s.gain),
            S_PAN => s.pan,
            S_TUNE => s.tune,
            S_FINE => s.fine,
            S_START => s.start,
            S_ATTACK => s.attack,
            S_DECAY => s.decay,
            S_REVERSE => b(s.reverse),
            S_FILTER => f32::from(s.filter),
            S_CUTOFF => s.cutoff,
            S_RESO => s.reso,
            S_CHOKE => f32::from(s.choke),
            S_VELOCITY => s.velocity,
            S_MODE => b(s.gate),
            _ => 0.0,
        }
    }

    fn set_param(&mut self, id: u32, value: f32) {
        // Ids are dense: pad-major, then the master volume.
        let at = if id == P_VOLUME {
            PADS * SLOTS
        } else {
            let (pad, slot) = ((id / STRIDE) as usize, (id % STRIDE) as usize);
            if pad >= PADS || slot >= SLOTS {
                return;
            }
            pad * SLOTS + slot
        };
        let v = PARAMS[at].clamp(value);
        if id == P_VOLUME {
            self.gain = db_to_gain(v);
            return;
        }
        let s = &mut self.pads[(id / STRIDE) as usize].set;
        match id % STRIDE {
            S_VOLUME => s.gain = db_to_gain(v),
            S_PAN => s.pan = v,
            S_TUNE => s.tune = v,
            S_FINE => s.fine = v,
            S_START => s.start = v,
            S_ATTACK => s.attack = v,
            S_DECAY => s.decay = v,
            S_REVERSE => s.reverse = v > 0.5,
            S_FILTER => s.filter = v as u8,
            S_CUTOFF => s.cutoff = v,
            S_RESO => s.reso = v,
            S_CHOKE => s.choke = v as u8,
            S_VELOCITY => s.velocity = v,
            S_MODE => s.gate = v > 0.5,
            _ => {}
        }
    }
}

impl Instrument for DrumPads {
    fn kind(&self) -> &'static str {
        "drum_pads"
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
    }

    fn set_pad_sample(&mut self, pad: usize, layer: usize, sample: Option<Arc<Sample>>) {
        if pad >= PADS || layer >= LAYERS {
            return;
        }
        for v in self.voices.iter_mut().filter(|v| v.pad == pad) {
            v.active = false;
        }
        self.pads[pad].layers[layer] = sample;
    }

    fn note_on(&mut self, note: u8, velocity: f32) {
        let Some(pad) = note.checked_sub(FIRST_NOTE).map(usize::from).filter(|&p| p < PADS) else { return };
        let Some(layer) = self.layer_for(pad, velocity) else { return };
        let set = self.pads[pad].set;
        // Choke: everything else in the group fades out, this pad included.
        if set.choke > 0 {
            for i in 0..VOICES {
                let v = &self.voices[i];
                if v.active && self.pads[v.pad].set.choke == set.choke {
                    self.fade(i, CHOKE_TAU);
                }
            }
        }
        let idx = pick_voice(VOICES, |i| self.voices[i].active, |i| self.voices[i].started_at);
        self.counter += 1;
        let v = &mut self.voices[idx];
        v.active = true;
        v.pad = pad;
        v.layer = layer;
        v.note = note;
        v.started_at = self.counter;
        v.pos = 0.0;
        v.amp = velocity_gain(set.velocity, velocity);
        v.env = 0.0;
        v.stage = Stage::Attack;
        let mode = if set.filter == 2 { FilterMode::HighPass } else { FilterMode::LowPass };
        for f in &mut v.filter {
            f.mode = mode;
            f.reset();
        }
    }

    fn note_off(&mut self, note: u8) {
        for i in 0..VOICES {
            let v = &self.voices[i];
            if v.active && v.note == note && v.stage != Stage::Fade && self.pads[v.pad].set.gate {
                self.fade(i, GATE_TAU);
            }
        }
    }

    fn all_notes_off(&mut self) {
        for v in &mut self.voices {
            v.active = false;
        }
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let sr = self.sample_rate;
        let Self { pads, voices, gain, .. } = self;
        // Per-voice block state, computed once.
        let mut blocks: [Option<Block>; VOICES] = [None; VOICES];
        let mut any = false;
        for (v, block) in voices.iter_mut().zip(&mut blocks).filter(|(v, _)| v.active) {
            let pad = &pads[v.pad];
            let set = pad.set;
            let Some(sample) = pad.layers[v.layer].as_deref().filter(|s| !s.is_empty()) else {
                v.active = false;
                continue;
            };
            let last = (sample.len() - 1) as f64;
            let first = set.start as f64 * last;
            if set.filter > 0 {
                let q = 0.707 * (1.0 + set.reso * 13.0);
                for f in &mut v.filter {
                    f.set_params(set.cutoff, q, sr);
                }
            }
            let (bl, br) = balance_gains(set.pan);
            let amp = v.amp * set.gain;
            *block = Some(Block {
                last,
                first,
                region: last - first,
                rate: sample.sample_rate as f64 / sr as f64 * tune_ratio(set.tune, set.fine),
                gl: amp * bl,
                gr: amp * br,
                filtered: set.filter > 0,
                reverse: set.reverse,
                attack_step: 1.0 / (set.attack * sr).max(1.0),
                decay: if set.decay >= DECAY_FULL { 1.0 } else { decay_coef(set.decay / 6.9, sr) },
            });
            any = true;
        }
        if !any {
            return;
        }
        let master = *gain;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let (mut ml, mut mr) = (0.0, 0.0);
            for (v, b) in voices.iter_mut().zip(&blocks) {
                let Some(b) = b.as_ref().filter(|_| v.active) else { continue };
                let Some(sample) = pads[v.pad].layers[v.layer].as_deref() else { continue };
                match v.stage {
                    Stage::Attack => {
                        v.env += b.attack_step;
                        if v.env >= 1.0 {
                            v.env = 1.0;
                            v.stage = Stage::Decay;
                        }
                    }
                    Stage::Decay => v.env *= b.decay,
                    Stage::Fade => v.env *= v.fade_coef,
                }
                let at = if b.reverse { b.last - v.pos } else { b.first + v.pos };
                let (mut sl, mut sr_) = (read_cubic(&sample.left, at), read_cubic(&sample.right, at));
                if b.filtered {
                    sl = v.filter[0].process(sl);
                    sr_ = v.filter[1].process(sr_);
                }
                ml += sl * v.env * b.gl;
                mr += sr_ * v.env * b.gr;
                v.pos += b.rate;
                if v.pos >= b.region || (v.stage != Stage::Attack && v.env < SILENCE) {
                    v.active = false;
                }
            }
            *l += soft_clip(ml * master);
            *r += soft_clip(mr * master);
        }
    }
}

/// What a voice needs for one block.
#[derive(Clone, Copy)]
struct Block {
    last: f64,
    first: f64,
    region: f64,
    rate: f64,
    gl: f32,
    gr: f32,
    filtered: bool,
    reverse: bool,
    attack_step: f32,
    decay: f32,
}

#[cfg(test)]
mod tests {
    use super::super::common::test_util::{peak, render, rising_crossings, stress};
    use super::*;

    fn sine(hz: f32, sample_rate: f32, seconds: f32) -> Arc<Sample> {
        let n = (sample_rate * seconds) as usize;
        let left: Vec<f32> = (0..n).map(|i| (std::f32::consts::TAU * hz * i as f32 / sample_rate).sin() * 0.5).collect();
        Arc::new(Sample { name: "sine".into(), sample_rate, right: left.clone(), left, markers: Vec::new() })
    }

    /// A ramp from 0 to 1: tells which end is played first.
    fn ramp(n: usize) -> Arc<Sample> {
        let left: Vec<f32> = (0..n).map(|i| i as f32 / n as f32 * 0.8).collect();
        Arc::new(Sample { name: "ramp".into(), sample_rate: 48_000.0, right: left.clone(), left, markers: Vec::new() })
    }

    fn pads() -> DrumPads {
        let mut d = DrumPads::new();
        d.set_sample_rate(48_000.0);
        d.set_param(P_VOLUME, 0.0);
        d
    }

    #[test]
    fn silent_without_samples_and_outside_the_grid() {
        let mut d = pads();
        d.note_on(36, 1.0);
        assert_eq!(peak(&mut d, 256), 0.0);
        d.set_pad_sample(0, 0, Some(sine(440.0, 48_000.0, 0.5)));
        d.note_on(35, 1.0);
        d.note_on(52, 1.0);
        assert_eq!(d.active_voices(), 0);
        d.note_on(36, 1.0);
        assert!(peak(&mut d, 2_400) > 0.1);
    }

    #[test]
    fn choke_group_cuts_the_other_pad() {
        let mut d = pads();
        // Closed hat (pad 6, note 42) and open hat (pad 10, note 46) share group 1.
        d.set_pad_sample(6, 0, Some(sine(440.0, 48_000.0, 1.0)));
        d.set_pad_sample(10, 0, Some(sine(660.0, 48_000.0, 1.0)));
        d.set_pad_sample(0, 0, Some(sine(110.0, 48_000.0, 1.0)));
        d.note_on(46, 1.0);
        d.note_on(36, 1.0);
        render(&mut d, 1_000);
        assert_eq!(d.active_voices(), 2);
        d.note_on(42, 1.0);
        render(&mut d, 2_400);
        // Open hat gone, kick (no group) and closed hat still ringing.
        assert_eq!(d.active_voices(), 2);
        assert!(d.voices.iter().filter(|v| v.active).all(|v| v.pad != 10));
        // Without a group, nothing is cut.
        d.set_param(pad_param(10, S_CHOKE), 0.0);
        d.note_on(46, 1.0);
        d.note_on(42, 1.0);
        render(&mut d, 2_400);
        assert!(d.voices.iter().any(|v| v.active && v.pad == 10));
    }

    #[test]
    fn velocity_mapping() {
        assert_eq!(velocity_gain(1.0, 0.5), 0.5);
        assert_eq!(velocity_gain(0.0, 0.1), 1.0);
        assert!((velocity_gain(0.5, 0.0) - 0.5).abs() < 1e-6);
        let mut d = pads();
        d.set_pad_sample(2, 0, Some(sine(440.0, 48_000.0, 0.5)));
        d.note_on(38, 1.0);
        let loud = peak(&mut d, 2_400);
        d.all_notes_off();
        d.note_on(38, 0.25);
        let soft = peak(&mut d, 2_400);
        assert!((soft / loud - 0.25).abs() < 0.02, "{soft} / {loud}");
        d.set_param(pad_param(2, S_VELOCITY), 0.0);
        d.all_notes_off();
        d.note_on(38, 0.25);
        assert!((peak(&mut d, 2_400) - loud).abs() < 0.01);
    }

    #[test]
    fn velocity_layers_split_the_range() {
        let mut d = pads();
        assert_eq!(d.layer_for(0, 1.0), None);
        d.set_pad_sample(0, 0, Some(sine(100.0, 48_000.0, 0.1)));
        assert_eq!(d.layer_for(0, 0.0), Some(0));
        assert_eq!(d.layer_for(0, 1.0), Some(0));
        // Layers 0 and 3 loaded: soft half → 0, loud half → 3.
        d.set_pad_sample(0, 3, Some(sine(200.0, 48_000.0, 0.1)));
        assert_eq!(d.layer_for(0, 0.3), Some(0));
        assert_eq!(d.layer_for(0, 0.7), Some(3));
        assert_eq!(d.layer_for(0, 1.0), Some(3));
        d.set_pad_sample(0, 1, Some(sine(300.0, 48_000.0, 0.1)));
        d.set_pad_sample(0, 2, Some(sine(400.0, 48_000.0, 0.1)));
        assert_eq!(d.layer_for(0, 0.2), Some(0));
        assert_eq!(d.layer_for(0, 0.3), Some(1));
        assert_eq!(d.layer_for(0, 0.6), Some(2));
        assert_eq!(d.layer_for(0, 0.9), Some(3));
    }

    #[test]
    fn tuning_ratio() {
        assert!((tune_ratio(12.0, 0.0) - 2.0).abs() < 1e-9);
        assert!((tune_ratio(-12.0, 0.0) - 0.5).abs() < 1e-9);
        assert!((tune_ratio(0.0, 100.0) - tune_ratio(1.0, 0.0)).abs() < 1e-9);
        // 441 Hz at 44.1 kHz played at 48 kHz, an octave up: 882 crossings a second.
        let mut d = pads();
        d.set_pad_sample(0, 0, Some(sine(441.0, 44_100.0, 3.0)));
        d.note_on(36, 1.0);
        let (l, _) = render(&mut d, 48_000);
        assert!((440..=442).contains(&rising_crossings(&l)), "{}", rising_crossings(&l));
        d.set_param(pad_param(0, S_TUNE), 12.0);
        d.note_on(36, 1.0);
        let (l, _) = render(&mut d, 24_000);
        assert!((440..=442).contains(&rising_crossings(&l)), "{}", rising_crossings(&l));
    }

    #[test]
    fn reverse_plays_from_the_end() {
        let mut d = pads();
        d.set_pad_sample(0, 0, Some(ramp(4_800)));
        d.note_on(36, 1.0);
        let (fwd, _) = render(&mut d, 4_000);
        d.set_param(pad_param(0, S_REVERSE), 1.0);
        d.note_on(36, 1.0);
        let (rev, _) = render(&mut d, 4_000);
        // Forward rises, reverse falls (after the half-millisecond attack).
        assert!(fwd[100] < fwd[3_000]);
        assert!(rev[100] > rev[3_000]);
        assert!(rev[100] > 0.7);
    }

    #[test]
    fn start_offset_skips_the_head() {
        let mut d = pads();
        d.set_pad_sample(0, 0, Some(ramp(4_800)));
        d.set_param(pad_param(0, S_START), 0.5);
        d.note_on(36, 1.0);
        let (l, _) = render(&mut d, 3_000);
        assert!(l[50] > 0.38, "{}", l[50]);
        // Half the sample is left: done after 2 400 frames.
        assert_eq!(d.active_voices(), 0);
        assert_eq!(l[2_500], 0.0);
    }

    #[test]
    fn decay_and_gate_shorten_the_hit() {
        let mut d = pads();
        d.set_pad_sample(0, 0, Some(sine(200.0, 48_000.0, 2.0)));
        d.set_param(pad_param(0, S_DECAY), 0.05);
        d.note_on(36, 1.0);
        render(&mut d, 9_600);
        assert_eq!(d.active_voices(), 0);
        d.set_param(pad_param(0, S_DECAY), 10.0);
        d.set_param(pad_param(0, S_MODE), 1.0);
        d.note_on(36, 1.0);
        render(&mut d, 2_400);
        d.note_off(36);
        render(&mut d, 9_600);
        assert_eq!(d.active_voices(), 0);
    }

    #[test]
    fn filter_darkens() {
        let mut d = pads();
        d.set_pad_sample(0, 0, Some(sine(5_000.0, 48_000.0, 0.5)));
        d.note_on(36, 1.0);
        let open = peak(&mut d, 4_800);
        d.set_param(pad_param(0, S_FILTER), 1.0);
        d.set_param(pad_param(0, S_CUTOFF), 200.0);
        d.note_on(36, 1.0);
        render(&mut d, 1_000);
        assert!(peak(&mut d, 2_400) < open * 0.1);
    }

    #[test]
    fn pan_moves_the_pad() {
        let mut d = pads();
        d.set_pad_sample(0, 0, Some(sine(200.0, 48_000.0, 0.5)));
        d.set_param(pad_param(0, S_PAN), -1.0);
        d.note_on(36, 1.0);
        let (l, r) = render(&mut d, 2_400);
        assert!(l.iter().any(|s| s.abs() > 0.1));
        assert!(r.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn survives_extremes() {
        let mut d = pads();
        for p in 0..PADS {
            d.set_pad_sample(p, p % LAYERS, Some(sine(300.0 + p as f32 * 50.0, 8_000.0, 0.3)));
        }
        stress(&mut d);
    }
}
