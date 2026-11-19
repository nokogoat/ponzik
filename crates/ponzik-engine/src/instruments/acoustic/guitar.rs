//! Guitar: modeled acoustic, nylon, electric and bass guitars.
//!
//! Every voice is a string made of two waveguide loops, one per
//! polarization (slightly different lengths and decays: the beating and the
//! two-stage decay of a real string). Each loop holds a fractional delay
//! (Lagrange), a loss filter (lowpass with a gain) and two allpass stages
//! for stiffness (inharmonicity); the phase delay of the filters at the
//! fundamental is subtracted from the delay, so the string stays in tune
//! across the neck and during slides.
//!
//! The pluck is a raised-cosine pulse whose width follows the velocity and
//! the technique (pick, finger, slap), passed through the comb of the
//! plucking point; a pick adds its scrape. Hard plucks start a little sharp
//! (tension modulation). Slaps buzz against the frets (a passive limiter
//! in the loop). Palm muting damps the loop, hammer-ons and pull-offs move
//! the "fret" of a sounding string, slides move it continuously and the
//! wound strings zip on the way. Strummed chords are spread in time from
//! one string to the next.
//!
//! Acoustic models ring through a body made of resonators (air cavity, top
//! plate, then denser modes); electric models read the string at the
//! pickup (its comb) through the pickup's resonance, then the built-in amp
//! and cabinet ([`AmpCore`], shared with the Amp & Cab effect).

use ponzik_dsp::biquad::{Biquad, BiquadCoefs};
use ponzik_dsp::filter::{FilterMode, OnePole, Svf};
use ponzik_dsp::smooth::Smoother;
use ponzik_dsp::util::{db_to_gain, flush_denormal, midi_to_hz};

use super::wg::{Allpass1, Body, Controls, Delay, Loss, Rng};
use crate::effects::amp::{AmpCore, AmpSettings, CABS};
use crate::instruments::common::{Expr, pan_gains, pick_voice, soft_clip};
use crate::param::{ParamInfo, Params, Unit};
use crate::{Controller, Instrument, NoteExpression, Ramp};

const VOICES: usize = 12;
/// Lowest fundamental the delay lines serve.
const MIN_HZ: f32 = 24.0;
/// Control rate: delays and filters are updated every `CHUNK` samples.
const CHUNK: usize = 16;
/// Longest excitation, in samples.
const EXC_LEN: usize = 4_096;
/// Under −80 dB a voice is over.
const SILENCE: f32 = 1e-4;
/// Notes waiting to be strummed.
const PENDING: usize = 16;
/// Notes held in legato mode.
const STACK: usize = 16;
/// Post-chain tail after the last voice ends, in seconds.
const TAIL: f32 = 0.5;

pub(crate) const MODELS: &[&str] = &["Steel Acoustic", "Nylon", "Electric", "Bass"];
const TECHNIQUES: &[&str] = &["Pick", "Finger", "Slap"];
const DIRECTIONS: &[&str] = &["Down", "Up", "Alternate"];
const PLAY_MODES: &[&str] = &["Poly", "Legato"];
const HARMONICS: &[&str] = &["Off", "12th Fret", "7th Fret", "5th Fret"];
const AMPS: &[&str] = &["Off", "Clean", "Crunch", "Lead", "Bass"];

const P_MODEL: u32 = 0;
const P_TECHNIQUE: u32 = 1;
const P_POSITION: u32 = 2;
const P_BRIGHTNESS: u32 = 3;
const P_SUSTAIN: u32 = 4;
const P_DAMPING: u32 = 5;
const P_BODY: u32 = 6;
const P_PICKUP: u32 = 7;
const P_MUTE: u32 = 8;
const P_STRUM: u32 = 9;
const P_DIRECTION: u32 = 10;
const P_MODE: u32 = 11;
const P_SLIDE: u32 = 12;
const P_HARMONICS: u32 = 13;
const P_FRET_NOISE: u32 = 14;
const P_HUMANIZE: u32 = 15;
const P_AMP: u32 = 16;
const P_DRIVE: u32 = 17;
const P_BASS: u32 = 18;
const P_MID: u32 = 19;
const P_TREBLE: u32 = 20;
const P_PRESENCE: u32 = 21;
const P_CAB: u32 = 22;
const P_MIC: u32 = 23;
const P_WIDTH: u32 = 24;
const P_VOLUME: u32 = 25;
const N_PARAMS: usize = 26;

static PARAMS: [ParamInfo; N_PARAMS] = [
    ParamInfo::choice(P_MODEL, "Model", MODELS, 0).group("String"),
    ParamInfo::choice(P_TECHNIQUE, "Technique", TECHNIQUES, 0).group("String"),
    ParamInfo::new(P_POSITION, "Pluck Position", 0.03, 0.5, 0.16, Unit::Percent).group("String"),
    ParamInfo::new(P_BRIGHTNESS, "Brightness", 0.0, 1.0, 0.55, Unit::Percent).group("String"),
    ParamInfo::new(P_SUSTAIN, "Sustain", 0.2, 20.0, 5.0, Unit::Seconds).log().group("String"),
    ParamInfo::new(P_DAMPING, "Damping", 0.0, 1.0, 0.35, Unit::Percent).group("String"),
    ParamInfo::new(P_BODY, "Body", 0.0, 1.0, 0.6, Unit::Percent).group("String"),
    ParamInfo::new(P_PICKUP, "Pickup Position", 0.03, 0.5, 0.18, Unit::Percent).group("String"),
    ParamInfo::new(P_MUTE, "Palm Mute", 0.0, 1.0, 0.0, Unit::Percent).group("Playing"),
    ParamInfo::new(P_STRUM, "Strum", 0.0, 0.12, 0.0, Unit::Seconds).group("Playing"),
    ParamInfo::choice(P_DIRECTION, "Strum Direction", DIRECTIONS, 2).group("Playing"),
    ParamInfo::choice(P_MODE, "Play Mode", PLAY_MODES, 0).group("Playing"),
    ParamInfo::new(P_SLIDE, "Slide Time", 0.0, 1.0, 0.0, Unit::Seconds).group("Playing"),
    ParamInfo::choice(P_HARMONICS, "Harmonics", HARMONICS, 0).group("Playing"),
    ParamInfo::new(P_FRET_NOISE, "Fret Noise", 0.0, 1.0, 0.3, Unit::Percent).group("Playing"),
    ParamInfo::new(P_HUMANIZE, "Humanize", 0.0, 1.0, 0.3, Unit::Percent).group("Playing"),
    ParamInfo::choice(P_AMP, "Amp", AMPS, 0).group("Amp"),
    ParamInfo::new(P_DRIVE, "Drive", 0.0, 1.0, 0.4, Unit::Percent).group("Amp"),
    ParamInfo::new(P_BASS, "Bass", 0.0, 10.0, 5.0, Unit::None).group("Amp"),
    ParamInfo::new(P_MID, "Mid", 0.0, 10.0, 5.0, Unit::None).group("Amp"),
    ParamInfo::new(P_TREBLE, "Treble", 0.0, 10.0, 5.0, Unit::None).group("Amp"),
    ParamInfo::new(P_PRESENCE, "Presence", 0.0, 10.0, 5.0, Unit::None).group("Amp"),
    ParamInfo::choice(P_CAB, "Cabinet", CABS, 2).group("Amp"),
    ParamInfo::new(P_MIC, "Mic Position", 0.0, 1.0, 0.3, Unit::Percent).group("Amp"),
    ParamInfo::new(P_WIDTH, "Stereo Width", 0.0, 1.0, 0.4, Unit::Percent).group("Output"),
    ParamInfo::new(P_VOLUME, "Volume", -40.0, 6.0, -4.0, Unit::Db).group("Output"),
];

/// What sets one guitar apart from another.
struct Spec {
    /// Sustain factor on the knob.
    decay: f32,
    /// Loss filter pole at damping 0 and its range.
    pole: (f32, f32),
    /// Allpass coefficient of the two stiffness stages.
    stiffness: f32,
    /// Pitch rise of a hard pluck, in cents.
    tension: f32,
    /// Detune (cents) and decay factor of the second polarization.
    second: (f32, f32),
    /// Body modes `(hz, q, gain)`; empty: a solid body read by a pickup.
    body: &'static [(f32, f32, f32)],
    /// Pickup resonance `(hz, q)`.
    pickup: (f32, f32),
    /// Pick scrape level.
    scrape: f32,
    /// Damping of a released note (s).
    release: f32,
    /// Excitation level.
    level: f32,
    /// Fret buzz threshold for slaps (fraction of the pluck level).
    buzz: f32,
    /// Notes under this one sit on wound strings (zip on slides).
    wound_below: u8,
}

/// Steel-string dreadnought: Helmholtz air mode, top plate, then denser modes.
const STEEL_BODY: [(f32, f32, f32); 14] = [
    (98.0, 6.0, 1.0),
    (188.0, 9.0, 0.9),
    (238.0, 11.0, 0.5),
    (385.0, 10.0, 0.55),
    (505.0, 12.0, 0.4),
    (640.0, 13.0, 0.32),
    (815.0, 12.0, 0.3),
    (1_040.0, 10.0, 0.26),
    (1_390.0, 9.0, 0.22),
    (1_880.0, 7.0, 0.2),
    (2_550.0, 6.0, 0.17),
    (3_350.0, 5.0, 0.14),
    (4_500.0, 4.0, 0.12),
    (6_200.0, 3.0, 0.09),
];

/// Classical guitar: lighter top, warmer and rounder.
const NYLON_BODY: [(f32, f32, f32); 14] = [
    (104.0, 5.0, 1.0),
    (205.0, 8.0, 0.95),
    (280.0, 10.0, 0.5),
    (420.0, 10.0, 0.45),
    (555.0, 12.0, 0.36),
    (740.0, 11.0, 0.3),
    (960.0, 10.0, 0.26),
    (1_230.0, 9.0, 0.2),
    (1_640.0, 7.0, 0.16),
    (2_250.0, 6.0, 0.12),
    (3_000.0, 5.0, 0.09),
    (3_900.0, 4.0, 0.07),
    (5_200.0, 3.0, 0.05),
    (7_000.0, 3.0, 0.03),
];

const SPECS: [Spec; 4] = [
    Spec {
        decay: 1.0,
        pole: (0.03, 0.55),
        stiffness: -0.12,
        tension: 7.0,
        second: (0.7, 0.45),
        body: &STEEL_BODY,
        pickup: (5_000.0, 0.7),
        scrape: 0.35,
        release: 0.12,
        level: 0.55,
        buzz: 0.0,
        wound_below: 55,
    },
    Spec {
        decay: 0.7,
        pole: (0.12, 0.6),
        stiffness: -0.05,
        tension: 12.0,
        second: (0.5, 0.5),
        body: &NYLON_BODY,
        pickup: (5_000.0, 0.7),
        scrape: 0.12,
        release: 0.14,
        level: 0.6,
        buzz: 0.0,
        wound_below: 55,
    },
    Spec {
        decay: 1.3,
        pole: (0.02, 0.5),
        stiffness: -0.1,
        tension: 5.0,
        second: (0.3, 0.6),
        body: &[],
        pickup: (4_200.0, 1.6),
        scrape: 0.2,
        release: 0.09,
        level: 0.75,
        buzz: 0.0,
        wound_below: 55,
    },
    Spec {
        decay: 1.2,
        pole: (0.05, 0.6),
        stiffness: -0.3,
        tension: 9.0,
        second: (0.3, 0.6),
        body: &[],
        pickup: (2_600.0, 1.1),
        scrape: 0.15,
        release: 0.12,
        level: 0.8,
        buzz: 0.35,
        wound_below: 127,
    },
];

/// One polarization of a string.
struct Pol {
    line: Delay,
    loss: Loss,
    ap: [Allpass1; 2],
    /// Current delay and its step per sample within a chunk.
    d: f32,
    step: f32,
}

impl Pol {
    fn new(max: usize) -> Self {
        Self { line: Delay::new(max), loss: Loss::default(), ap: Default::default(), d: 2.0, step: 0.0 }
    }

    fn reset(&mut self) {
        self.line.clear();
        self.loss.reset();
        self.ap.iter_mut().for_each(Allpass1::reset);
    }

    #[inline]
    fn tick(&mut self, input: f32, buzz: f32) -> f32 {
        self.d += self.step;
        let v = self.line.read(self.d);
        let mut y = self.loss.process(v);
        for ap in &mut self.ap {
            y = ap.process(y);
        }
        if buzz > 0.0 {
            // The string hits the frets: anything past the threshold is squashed (passive).
            let a = y.abs();
            if a > buzz {
                y = (buzz + (a - buzz) * 0.25).copysign(y);
            }
        }
        self.line.write(flush_denormal(y + input));
        v
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Excite {
    /// A full pluck.
    Pluck,
    /// A hammer-on or pull-off on a sounding string: soft, no pick.
    Legato(f32),
}

struct Voice {
    /// Key that started the voice (expression, note-off).
    key: u8,
    started_at: u64,
    active: bool,
    released: bool,
    /// Note-off arrived while the sustain pedal was down.
    pedal: bool,
    pol: [Pol; 2],
    /// Fretted pitch (MIDI note), moved by legato slides.
    pitch: Ramp,
    /// Harmonic: semitones above the fretted pitch.
    harmonic: f32,
    expr: Expr,
    /// Tension modulation, semitones, decaying.
    tension: f32,
    /// Humanized tuning, semitones.
    detune: f32,
    velocity: f32,
    exc: Vec<f32>,
    exc_len: usize,
    exc_pos: usize,
    /// Fresh voice: the delays jump to their target.
    fresh: bool,
    /// Pickup distance from the bridge, in samples.
    pickup: f32,
    pan: f32,
    level: f32,
    hold: u32,
    /// Squeaks: slide zip level, burst level, filter, last pitch.
    zip: f32,
    burst: f32,
    burst_hz: f32,
    squeak: Svf,
    last_pitch: f32,
    rng: Rng,
}

impl Voice {
    fn new(max: usize, seed: u32) -> Self {
        Self {
            key: 0,
            started_at: 0,
            active: false,
            released: false,
            pedal: false,
            pol: [Pol::new(max), Pol::new(max)],
            pitch: Ramp::new(60.0),
            harmonic: 0.0,
            expr: Expr::default(),
            tension: 0.0,
            detune: 0.0,
            velocity: 0.0,
            exc: vec![0.0; EXC_LEN],
            exc_len: 0,
            exc_pos: 0,
            fresh: true,
            pickup: 10.0,
            pan: 0.0,
            level: 0.0,
            hold: 0,
            zip: 0.0,
            burst: 0.0,
            burst_hz: 1_500.0,
            squeak: Svf::new(FilterMode::BandPass, 1_500.0, 3.0, 48_000.0),
            last_pitch: 60.0,
            rng: Rng::new(seed),
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Pending {
    note: u8,
    velocity: f32,
    /// Sample clock at which it starts; `u64::MAX`: not scheduled yet.
    due: u64,
}

/// Modeled guitars, twelve voices.
pub struct Guitar {
    values: [f32; N_PARAMS],
    sample_rate: f32,
    voices: Vec<Voice>,
    note_counter: u64,
    /// Samples rendered so far.
    clock: u64,
    pending: [Pending; PENDING],
    n_pending: usize,
    /// The next alternate strum goes up.
    strum_up: bool,
    stack: [u8; STACK],
    n_stack: usize,
    ctl: Controls,
    expression: Smoother,
    bodies: [Body<14>; 2],
    pickup_res: [Biquad; 2],
    dc: [OnePole; 2],
    amp: AmpCore,
    amp_on: bool,
    tail: u32,
    rng: Rng,
}

impl Default for Guitar {
    fn default() -> Self {
        Self::new()
    }
}

impl Guitar {
    pub fn new() -> Self {
        let mut g = Self {
            values: PARAMS.map(|p| p.default),
            sample_rate: 0.0,
            voices: Vec::new(),
            note_counter: 0,
            clock: 0,
            pending: [Pending::default(); PENDING],
            n_pending: 0,
            strum_up: false,
            stack: [0; STACK],
            n_stack: 0,
            ctl: Controls::default(),
            expression: Smoother::new(1.0),
            bodies: Default::default(),
            pickup_res: Default::default(),
            dc: Default::default(),
            amp: AmpCore::new(),
            amp_on: false,
            tail: 0,
            rng: Rng::new(0x6A17),
        };
        g.set_sample_rate(48_000.0);
        g
    }

    fn v(&self, id: u32) -> f32 {
        self.values[id as usize]
    }

    fn index(&self, id: u32) -> usize {
        self.v(id).round() as usize
    }

    fn spec(&self) -> &'static Spec {
        &SPECS[self.index(P_MODEL).min(SPECS.len() - 1)]
    }

    fn legato(&self) -> bool {
        self.index(P_MODE) == 1
    }

    /// Recomputes the post chain (body, pickup, amp).
    fn update_chain(&mut self) {
        let sr = self.sample_rate;
        let spec = self.spec();
        let body = self.v(P_BODY);
        for b in &mut self.bodies {
            b.set(spec.body, 1.0, sr);
            b.direct = 1.0 - 0.35 * body;
            b.amount = if spec.body.is_empty() { 0.0 } else { 1.6 * body };
        }
        let (hz, q) = spec.pickup;
        for f in &mut self.pickup_res {
            f.coefs = BiquadCoefs::lowpass(hz.min(sr * 0.45), q, sr);
        }
        for d in &mut self.dc {
            d.set_cutoff(12.0, sr);
        }
        let amp = self.index(P_AMP);
        self.amp_on = amp > 0;
        if self.amp_on {
            self.amp.set(AmpSettings {
                model: amp - 1,
                gain: self.v(P_DRIVE),
                bass: self.v(P_BASS),
                mid: self.v(P_MID),
                treble: self.v(P_TREBLE),
                presence: self.v(P_PRESENCE),
                sag: [0.3, 0.45, 0.3, 0.5][(amp - 1).min(3)],
                cab: self.index(P_CAB),
                mic: self.v(P_MIC),
            });
        }
    }

    /// Palm mute from the knob and the mod wheel.
    fn mute(&self) -> f32 {
        (self.v(P_MUTE) + self.ctl.modwheel).min(1.0)
    }

    fn harmonic_offset(&self) -> f32 {
        [0.0, 12.0, 19.0, 24.0][self.index(P_HARMONICS).min(3)]
    }

    /// Fills `exc` of voice `i` with a pluck (or a legato tap) for a string at `hz`.
    fn excite(&mut self, i: usize, hz: f32, velocity: f32, kind: Excite) {
        let sr = self.sample_rate;
        let spec = self.spec();
        let technique = self.index(P_TECHNIQUE);
        let mute = self.mute();
        let humanize = self.v(P_HUMANIZE);
        let harmonic = self.index(P_HARMONICS) > 0;
        let period = sr / hz;
        let bright = (self.v(P_BRIGHTNESS) * (0.35 + 0.65 * velocity) * (1.0 - 0.6 * mute)).clamp(0.0, 1.0);
        // Pulse width as a fraction of the period: narrow is bright.
        let (narrow, wide) = match (kind, technique) {
            (Excite::Legato(_), _) => (0.08, 0.3),
            (_, 1) => (0.05, 0.35),
            (_, 2) => (0.008, 0.04),
            _ => (0.012, 0.16),
        };
        let mut frac = wide + (narrow - wide) * bright;
        if harmonic {
            frac = 0.3;
        }
        let width = (frac * period).clamp(2.0, (EXC_LEN / 3) as f32);
        let mut pos = self.v(P_POSITION) * (1.0 + 0.15 * humanize * self.rng.next());
        if harmonic {
            pos = 0.12;
        }
        let comb = (pos.clamp(0.02, 0.5) * period).max(1.0);
        let level = match kind {
            Excite::Pluck => spec.level * (0.25 + 0.75 * velocity) * if technique == 2 { 1.6 } else { 1.0 },
            Excite::Legato(g) => spec.level * g,
        };
        let scrape = match kind {
            Excite::Pluck if technique == 0 => spec.scrape * velocity * (1.0 - 0.5 * mute),
            _ => 0.0,
        };
        let v = &mut self.voices[i];
        let w = width as usize;
        let c = comb as usize;
        let ct = comb - c as f32;
        let len = (w + c + 2).min(EXC_LEN);
        let pulse = |n: isize| -> f32 {
            if n < 0 || n as usize >= w {
                0.0
            } else {
                0.5 - 0.5 * (std::f32::consts::TAU * n as f32 / width).cos()
            }
        };
        for n in 0..len {
            let k = n as isize;
            let back = pulse(k - c as isize) * (1.0 - ct) + pulse(k - c as isize - 1) * ct;
            v.exc[n] = (pulse(k) - back) * level;
        }
        // Pick scrape: a short, bright noise burst.
        let scrape_len = ((0.002 * sr) as usize).min(len);
        let mut prev = 0.0;
        for n in 0..scrape_len {
            let noise = v.rng.next();
            let hp = noise - prev;
            prev = noise;
            let env = 1.0 - n as f32 / scrape_len as f32;
            v.exc[n] += hp * scrape * 0.25 * env * env;
        }
        v.exc_len = len;
        v.exc_pos = 0;
    }

    /// Starts a note now (strum already applied).
    fn start(&mut self, note: u8, velocity: f32) {
        let sr = self.sample_rate;
        let humanize = self.v(P_HUMANIZE);
        let velocity = (velocity * (1.0 + 0.08 * humanize * self.rng.next())).clamp(0.0, 1.0);
        // The same key on the same string: struck again, it keeps a little of its ring.
        let same = self.voices.iter().position(|v| v.active && v.key == note);
        let idx = same.unwrap_or_else(|| pick_voice(VOICES, |i| self.voices[i].active, |i| self.voice_age(i)));
        self.note_counter += 1;
        let harmonic = self.harmonic_offset();
        let spec = self.spec();
        let detune = 0.03 * humanize * self.rng.next();
        let width = self.v(P_WIDTH);
        let pickup = self.v(P_PICKUP);
        let hz = midi_to_hz(note as f32 + harmonic).clamp(MIN_HZ, sr * 0.4);
        let tension = spec.tension / 100.0 * velocity * velocity;
        {
            let v = &mut self.voices[idx];
            if same.is_some() {
                // The pick touches the string before plucking it.
                v.pol.iter_mut().for_each(|p| p.line.scale(0.3));
            } else {
                v.pol.iter_mut().for_each(Pol::reset);
                v.fresh = true;
                v.level = 1.0;
            }
            v.key = note;
            v.started_at = self.note_counter;
            v.active = true;
            v.released = false;
            v.pedal = false;
            v.pitch = Ramp::new(note as f32);
            v.harmonic = harmonic;
            v.expr.reset();
            v.tension = tension;
            v.detune = detune;
            v.velocity = velocity;
            v.pickup = pickup * sr / hz;
            v.pan = ((note as f32 - 62.0) / 24.0 * width).clamp(-1.0, 1.0);
            v.hold = (sr * 0.05) as u32;
            v.zip = 0.0;
            v.burst = 0.0;
            v.last_pitch = note as f32;
        }
        self.excite(idx, hz, velocity, Excite::Pluck);
        self.tail = (TAIL * sr) as u32;
    }

    /// Older notes are stolen first; released ones before held ones.
    fn voice_age(&self, i: usize) -> u64 {
        let v = &self.voices[i];
        v.started_at + if v.released { 0 } else { 1 << 40 }
    }

    fn release_voice(&mut self, key: u8) {
        let sustain = self.ctl.sustain;
        let fret = self.v(P_FRET_NOISE);
        for v in self.voices.iter_mut().filter(|v| v.active && !v.released && v.key == key) {
            if sustain {
                v.pedal = true;
            } else {
                v.released = true;
                // The fretting hand lets go: a soft squeak.
                v.burst = 0.05 * fret;
                v.burst_hz = 2_200.0;
            }
        }
    }

    /// The legato voice (the newest held one).
    fn legato_voice(&self) -> Option<usize> {
        (0..VOICES).filter(|&i| self.voices[i].active && !self.voices[i].released).max_by_key(|&i| self.voices[i].started_at)
    }

    /// Moves the legato voice to `note`: a slide when the slide time is set,
    /// otherwise a hammer-on (up) or pull-off (down).
    fn legato_to(&mut self, i: usize, note: u8, velocity: f32) {
        let sr = self.sample_rate;
        let slide = self.v(P_SLIDE);
        let fret = self.v(P_FRET_NOISE);
        let from = self.voices[i].pitch.target();
        let up = note as f32 > from;
        {
            let v = &mut self.voices[i];
            v.key = note;
            v.pitch.go(note as f32, slide, sr);
            if slide <= 0.0 {
                v.burst = 0.04 * fret;
                v.burst_hz = 900.0;
            }
        }
        if slide <= 0.0 {
            let hz = midi_to_hz(note as f32 + self.voices[i].harmonic).clamp(MIN_HZ, sr * 0.4);
            // A pull-off plucks the string with the fretting finger, a hammer-on just taps it.
            let g = if up { 0.18 } else { 0.32 } * (0.4 + 0.6 * velocity);
            self.excite(i, hz, velocity, Excite::Legato(g));
        }
    }

    /// Schedules the notes that arrived since the last render: a strum.
    fn schedule(&mut self) {
        let fresh = self.n_pending - self.pending[..self.n_pending].iter().filter(|p| p.due != u64::MAX).count();
        if fresh == 0 {
            return;
        }
        let strum = self.v(P_STRUM);
        let humanize = self.v(P_HUMANIZE);
        let up = match self.index(P_DIRECTION) {
            0 => false,
            1 => true,
            _ => {
                if fresh > 1 {
                    self.strum_up = !self.strum_up;
                }
                !self.strum_up
            }
        };
        // Unscheduled notes first, sorted by pitch.
        let slice = &mut self.pending[..self.n_pending];
        slice.sort_by_key(|p| (p.due != u64::MAX, p.note));
        let avg = slice[..fresh].iter().map(|p| p.velocity).sum::<f32>() / fresh as f32;
        // Harder strums are faster.
        let spacing = strum * (1.4 - 0.8 * avg) * self.sample_rate;
        for (k, p) in slice[..fresh].iter_mut().enumerate() {
            let order = if up { fresh - 1 - k } else { k };
            let jitter = 1.0 + 0.25 * humanize * self.rng.next();
            p.due = self.clock + (order as f32 * spacing * jitter).max(0.0) as u64;
            if up {
                p.velocity *= 0.85;
            }
        }
    }

    fn start_due(&mut self, now: u64) {
        let mut k = 0;
        while k < self.n_pending {
            let p = self.pending[k];
            if p.due != u64::MAX && p.due <= now {
                self.n_pending -= 1;
                self.pending[k] = self.pending[self.n_pending];
                self.start(p.note, p.velocity);
            } else {
                k += 1;
            }
        }
    }

    /// Delays and loop filters of every voice for the next chunk.
    fn control(&mut self, n: usize) {
        let sr = self.sample_rate;
        let spec = self.spec();
        let mute = self.mute();
        let sustain = self.v(P_SUSTAIN) * spec.decay;
        let damping = self.v(P_DAMPING);
        let fret = self.v(P_FRET_NOISE);
        let bend = self.ctl.bend;
        let chunk_secs = n as f32 / sr;
        let tension_decay = (-chunk_secs / 0.07).exp();
        let burst_decay = (-chunk_secs / 0.04).exp();
        let pole_base = spec.pole.0 + spec.pole.1 * damping * damping.sqrt();
        for v in self.voices.iter_mut().filter(|v| v.active) {
            v.pitch.skip(n as u32);
            let tuning = v.expr.tuning.skip(n as u32);
            let fretted = v.pitch.value() + tuning;
            let semis = fretted + v.harmonic + bend + v.tension + v.detune;
            v.tension *= tension_decay;
            let hz = midi_to_hz(semis).clamp(MIN_HZ, sr * 0.4);
            // Squeaks: zip of a moving finger on a wound string, bursts of taps and lifts.
            let speed = (fretted - v.last_pitch).abs() / chunk_secs;
            v.last_pitch = fretted;
            let wound = if (fretted as u8) < spec.wound_below { 1.0 } else { 0.35 };
            v.zip += ((fret * wound * (speed / 30.0).min(1.0) * 0.12) - v.zip) * 0.5;
            v.burst *= burst_decay;
            let squeak_hz = if v.zip > 1e-4 { (350.0 + speed * 30.0).min(4_000.0) } else { v.burst_hz };
            v.squeak.set_params(squeak_hz, 2.5, sr);
            // Decay: lower notes ring longer; muted and released strings die fast.
            let mut t60 = (sustain * (110.0 / hz).powf(0.45)).clamp(0.15, 40.0);
            let mut pole = pole_base;
            if v.harmonic > 0.0 {
                t60 *= 1.5;
            }
            if mute > 0.0 {
                t60 += (0.1 + 0.2 * (1.0 - mute) - t60) * mute.sqrt();
                pole += (0.8 - pole) * mute;
            }
            if v.released {
                t60 = t60.min(spec.release);
                pole += (0.6 - pole) * 0.5;
            }
            let fresh = std::mem::take(&mut v.fresh);
            for (k, p) in v.pol.iter_mut().enumerate() {
                let (cents, decay) = if k == 0 { (0.0, 1.0) } else { spec.second };
                let f = hz * (cents / 1_200.0).exp2();
                let gain = 10f32.powf(-3.0 / (t60 * decay * f)).min(0.99995);
                p.loss.set(gain, pole);
                p.ap.iter_mut().for_each(|a| a.c = spec.stiffness);
                let comp = p.loss.delay(f, sr) + p.ap.iter().map(|a| a.delay(f, sr)).sum::<f32>();
                let target = (sr / f - comp).clamp(2.0, p.line.max_delay() * 0.6);
                if fresh {
                    p.d = target;
                    p.step = 0.0;
                } else {
                    p.step = (target - p.d) / n as f32;
                }
            }
            v.pickup = (self.values[P_PICKUP as usize] * sr / hz).min(v.pol[0].line.max_delay() * 0.35);
        }
    }

    #[inline]
    fn tick_voices(&mut self, buzz_on: bool, electric: bool, level_coef: f32) -> (f32, f32) {
        let (mut ml, mut mr) = (0.0, 0.0);
        let buzz_level = self.spec().buzz;
        for v in self.voices.iter_mut().filter(|v| v.active) {
            let e = if v.exc_pos < v.exc_len {
                let e = v.exc[v.exc_pos];
                v.exc_pos += 1;
                e
            } else {
                0.0
            };
            let buzz = if buzz_on { buzz_level * (0.3 + v.velocity) } else { 0.0 };
            let [a, b] = &mut v.pol;
            let ya = a.tick(e * 0.8, buzz);
            let yb = b.tick(e * 0.35, buzz);
            let mut y = ya + yb;
            if electric {
                // The pickup reads the string at a point: its comb.
                let back = a.line.read(a.d + v.pickup) + b.line.read(b.d + v.pickup);
                y = (y - back) * 0.8;
            }
            let squeak_in = v.zip + v.burst;
            if squeak_in > 1e-5 {
                y += v.squeak.process(v.rng.next() * squeak_in);
            }
            v.level = (v.level * level_coef).max(ya.abs().max(yb.abs()));
            if v.hold > 0 {
                v.hold -= 1;
            } else if v.level < SILENCE && v.exc_pos >= v.exc_len {
                v.active = false;
            }
            let (gl, gr) = pan_gains((v.pan + v.expr.pan).clamp(-1.0, 1.0));
            ml += y * gl;
            mr += y * gr;
        }
        (ml, mr)
    }
}

impl Params for Guitar {
    fn params(&self) -> &'static [ParamInfo] {
        &PARAMS
    }

    fn get_param(&self, id: u32) -> f32 {
        self.values.get(id as usize).copied().unwrap_or(0.0)
    }

    fn set_param(&mut self, id: u32, value: f32) {
        let Some(info) = PARAMS.get(id as usize) else { return };
        let v = info.clamp(value);
        if self.values[id as usize] == v {
            return;
        }
        self.values[id as usize] = v;
        match id {
            P_MODEL | P_BODY | P_AMP..=P_MIC => self.update_chain(),
            P_MODE => {
                self.n_stack = 0;
            }
            _ => {}
        }
    }
}

impl Instrument for Guitar {
    fn kind(&self) -> &'static str {
        "guitar"
    }

    fn is_melodic(&self) -> bool {
        true
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        if sample_rate != self.sample_rate {
            let max = (sample_rate / MIN_HZ * 1.7) as usize + 8;
            self.voices = (0..VOICES).map(|i| Voice::new(max, 0x9E37_79B9 ^ (i as u32 * 7_919))).collect();
        }
        self.sample_rate = sample_rate;
        self.expression.set_time(0.02, sample_rate);
        self.amp.set_sample_rate(sample_rate);
        self.update_chain();
        self.all_notes_off();
    }

    fn note_on(&mut self, note: u8, velocity: f32) {
        let velocity = if velocity.is_finite() { velocity.clamp(0.0, 1.0) } else { 0.0 };
        if self.legato() {
            let held = self.n_stack > 0;
            if self.n_stack < STACK {
                self.stack[self.n_stack] = note;
                self.n_stack += 1;
            }
            if held && let Some(i) = self.legato_voice() {
                self.legato_to(i, note, velocity);
                return;
            }
            // A new phrase: the previous string is damped.
            for v in self.voices.iter_mut().filter(|v| v.active) {
                v.released = true;
            }
            self.start(note, velocity);
            return;
        }
        if self.v(P_STRUM) > 0.0 && self.n_pending < PENDING {
            self.pending[self.n_pending] = Pending { note, velocity, due: u64::MAX };
            self.n_pending += 1;
            return;
        }
        self.start(note, velocity);
    }

    fn note_off(&mut self, note: u8) {
        // A note still waiting for its strum is dropped.
        if let Some(k) = self.pending[..self.n_pending].iter().position(|p| p.note == note) {
            self.n_pending -= 1;
            self.pending[k] = self.pending[self.n_pending];
            return;
        }
        if self.legato() {
            let Some(k) = self.stack[..self.n_stack].iter().position(|&n| n == note) else {
                return self.release_voice(note);
            };
            let top = k + 1 == self.n_stack;
            self.stack.copy_within(k + 1..self.n_stack, k);
            self.n_stack -= 1;
            if top && self.n_stack > 0 {
                let back = self.stack[self.n_stack - 1];
                if let Some(i) = self.legato_voice() {
                    let vel = self.voices[i].velocity;
                    self.legato_to(i, back, vel);
                }
                return;
            }
            if self.n_stack == 0 {
                self.release_voice(note);
            }
            return;
        }
        self.release_voice(note);
    }

    fn supports_expression(&self) -> bool {
        true
    }

    fn note_expression(&mut self, key: u8, expr: NoteExpression) {
        let sr = self.sample_rate;
        if let Some(v) = self.voices.iter_mut().filter(|v| v.active && v.key == key).max_by_key(|v| v.started_at) {
            v.expr.apply(expr, sr);
        }
    }

    fn controller(&mut self, ctl: Controller) {
        if self.ctl.apply(ctl) {
            for v in self.voices.iter_mut().filter(|v| v.pedal) {
                v.pedal = false;
                v.released = true;
            }
        }
        self.expression.set_target(self.ctl.expression);
    }

    fn all_notes_off(&mut self) {
        for v in &mut self.voices {
            v.active = false;
            v.pol.iter_mut().for_each(Pol::reset);
            v.squeak.reset();
        }
        self.n_pending = 0;
        self.n_stack = 0;
        self.bodies.iter_mut().for_each(Body::reset);
        self.pickup_res.iter_mut().for_each(Biquad::reset);
        self.dc.iter_mut().for_each(OnePole::reset);
        self.amp.reset();
        self.tail = 0;
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        if self.n_pending == 0 && self.tail == 0 && !self.voices.iter().any(|v| v.active) {
            return;
        }
        self.schedule();
        let sr = self.sample_rate;
        let spec = self.spec();
        let electric = spec.body.is_empty();
        let buzz_on = self.index(P_TECHNIQUE) == 2 && spec.buzz > 0.0;
        let gain = db_to_gain(self.v(P_VOLUME));
        let level_coef = ponzik_dsp::util::decay_coef(0.05, sr);
        let mut done = 0;
        while done < left.len() {
            let n = CHUNK.min(left.len() - done);
            self.start_due(self.clock);
            self.control(n);
            let any = self.voices.iter().any(|v| v.active);
            for i in done..done + n {
                let (ml, mr) = if any { self.tick_voices(buzz_on, electric, level_coef) } else { (0.0, 0.0) };
                let mut out = [ml, mr];
                for (ch, x) in out.iter_mut().enumerate() {
                    let mut y = *x;
                    if electric {
                        y = self.pickup_res[ch].process(y);
                    } else {
                        y = self.bodies[ch].process(y);
                    }
                    y -= self.dc[ch].lowpass(y);
                    if self.amp_on {
                        y = self.amp.process(ch, y * 2.0);
                    }
                    *x = flush_denormal(y);
                }
                let g = gain * self.expression.next_value();
                left[i] += soft_clip(out[0] * g);
                right[i] += soft_clip(out[1] * g);
            }
            self.clock += n as u64;
            done += n;
            if any || self.n_pending > 0 {
                self.tail = (TAIL * sr) as u32;
            } else {
                self.tail = self.tail.saturating_sub(n as u32);
            }
        }
        if self.tail == 0 {
            self.bodies.iter_mut().for_each(Body::reset);
            self.pickup_res.iter_mut().for_each(Biquad::reset);
            self.dc.iter_mut().for_each(OnePole::reset);
            self.amp.reset();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::wg::test_util::*;
    use super::*;

    const SR: f32 = 48_000.0;

    fn guitar(model: usize) -> Guitar {
        let mut g = Guitar::new();
        g.set_param(P_MODEL, model as f32);
        g.set_param(P_HUMANIZE, 0.0);
        g
    }

    fn pluck(g: &mut Guitar, note: u8, vel: f32, secs: f32) -> Vec<f32> {
        g.note_on(note, vel);
        let (l, r) = render(g, (secs * SR) as usize);
        mono(&l, &r)
    }

    #[test]
    fn every_model_is_in_tune_across_the_neck() {
        for model in 0..4 {
            let range: &[u8] = if model == 3 { &[28, 33, 40, 45, 52, 60] } else { &[40, 45, 52, 57, 64, 71, 76, 84] };
            for &note in range {
                let mut g = guitar(model);
                let s = pluck(&mut g, note, 0.7, 1.0);
                // Skip the attack (tension modulation, pluck).
                let hz = pitch(&s[9_600..], SR).unwrap_or_else(|| panic!("model {model} note {note}: no pitch"));
                let off = cents_off(hz, note as f32);
                assert!(off.abs() < 8.0, "model {} note {note}: {off:.1} cents ({hz} Hz)", MODELS[model]);
            }
        }
    }

    #[test]
    fn stable_for_all_notes_and_velocities() {
        for model in 0..4 {
            for technique in 0..3 {
                let make = move || -> Box<dyn Instrument> {
                    let mut g = guitar(model);
                    g.set_param(P_TECHNIQUE, technique as f32);
                    g.set_param(P_AMP, (model + technique) as f32 % 5.0);
                    Box::new(g)
                };
                sweep_stable(&make, 0..=127, 13);
            }
        }
    }

    #[test]
    fn velocity_brightens_the_tone() {
        for model in 0..4 {
            let c = |vel| {
                let mut g = guitar(model);
                let s = pluck(&mut g, 52, vel, 0.3);
                centroid(&s[480..480 + 4_096], SR)
            };
            let (soft, hard) = (c(0.2), c(1.0));
            assert!(hard > soft * 1.2, "model {}: {soft} → {hard}", MODELS[model]);
        }
    }

    #[test]
    fn attack_is_fast_and_decay_realistic() {
        let mut g = guitar(0);
        let s = pluck(&mut g, 45, 0.8, 4.0);
        let attack = attack_time(&s, SR, 0.9);
        assert!(attack < 0.03, "attack {attack}");
        let env = envelope(&s, SR);
        let top = peak(&env);
        // An open A rings for seconds: still above −40 dB after 2 s, below the peak by then.
        let at2 = env[(2.0 * SR) as usize];
        assert!(at2 > top * 0.01 && at2 < top * 0.7, "{top} {at2}");
    }

    #[test]
    fn palm_mute_and_release_shorten_the_note() {
        let tail = |mute: f32| {
            let mut g = guitar(2);
            g.set_param(P_MUTE, mute);
            let s = pluck(&mut g, 45, 0.8, 1.0);
            rms(&s[24_000..])
        };
        assert!(tail(1.0) < tail(0.0) * 0.05);
        let mut g = guitar(0);
        g.note_on(57, 0.8);
        render(&mut g, 4_800);
        g.note_off(57);
        render(&mut g, 24_000);
        assert!(g.voices.iter().all(|v| !v.active));
    }

    #[test]
    fn strum_spreads_the_chord() {
        let mut g = guitar(0);
        g.set_param(P_STRUM, 0.03);
        g.set_param(P_DIRECTION, 0.0);
        for n in [40, 47, 52, 56, 59, 64] {
            g.note_on(n, 0.7);
        }
        render(&mut g, 128);
        assert_eq!(g.voices.iter().filter(|v| v.active).count(), 1);
        render(&mut g, (0.2 * SR) as usize);
        assert_eq!(g.voices.iter().filter(|v| v.active).count(), 6);
        // Down strum: the lowest string first.
        let first = g.voices.iter().filter(|v| v.active).min_by_key(|v| v.started_at).unwrap();
        assert_eq!(first.key, 40);
    }

    #[test]
    fn legato_hammer_on_moves_the_string() {
        let mut g = guitar(2);
        g.set_param(P_MODE, 1.0);
        g.note_on(57, 0.8);
        render(&mut g, 9_600);
        g.note_on(59, 0.8);
        let s = {
            let (l, r) = render(&mut g, 24_000);
            mono(&l, &r)
        };
        assert_eq!(g.voices.iter().filter(|v| v.active && !v.released).count(), 1);
        let hz = pitch(&s[4_800..], SR).unwrap();
        assert!(cents_off(hz, 59.0).abs() < 10.0, "{hz}");
        // Pull-off back to the held note.
        g.note_off(59);
        let (l, r) = render(&mut g, 24_000);
        let hz = pitch(&mono(&l, &r)[4_800..], SR).unwrap();
        assert!(cents_off(hz, 57.0).abs() < 10.0, "{hz}");
    }

    #[test]
    fn slides_glide_through_the_expression_hook() {
        let mut g = guitar(2);
        g.note_on(52, 0.9);
        render(&mut g, 4_800);
        g.note_expression(52, NoteExpression::Tuning { semitones: 5.0, secs: 0.2 });
        render(&mut g, 2_400);
        let mid = g.voices.iter().find(|v| v.active).map(|v| v.expr.tuning.value()).unwrap();
        assert!(mid > 0.5 && mid < 4.5, "{mid}");
        render(&mut g, 9_600);
        let (l, r) = render(&mut g, 19_200);
        let hz = pitch(&mono(&l, &r), SR).unwrap();
        assert!(cents_off(hz, 57.0).abs() < 10.0, "{hz}");
    }

    #[test]
    fn pitch_bend_bends_the_strings() {
        let mut g = guitar(2);
        g.controller(Controller::PitchBend(1.0));
        let s = pluck(&mut g, 57, 0.7, 1.0);
        let hz = pitch(&s[9_600..], SR).unwrap();
        assert!(cents_off(hz, 59.0).abs() < 10.0, "{hz}");
    }

    #[test]
    fn amp_changes_the_tone() {
        let c = |amp: usize, drive: f32| {
            let mut g = guitar(2);
            g.set_param(P_AMP, amp as f32);
            g.set_param(P_DRIVE, drive);
            g.set_param(P_CAB, 0.0);
            let s = pluck(&mut g, 45, 0.8, 0.5);
            centroid(&s[2_400..2_400 + 4_096], SR)
        };
        assert!(c(3, 1.0) > c(0, 0.0) * 1.3, "{} {}", c(3, 1.0), c(0, 0.0));
    }

    #[test]
    fn harmonics_sound_an_octave_up() {
        let mut g = guitar(0);
        g.set_param(P_HARMONICS, 1.0);
        let s = pluck(&mut g, 52, 0.7, 1.0);
        let hz = pitch(&s[9_600..], SR).unwrap();
        assert!(cents_off(hz, 64.0).abs() < 10.0, "{hz}");
    }
}
