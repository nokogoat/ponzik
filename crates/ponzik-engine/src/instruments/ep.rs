//! Electric pianos modelled from their mechanics: the tine piano (Rhodes
//! style) and the reed piano (Wurlitzer style).
//!
//! Each key is a set of modal resonators struck by a hammer pulse — a
//! longer, softer contact for a gentle touch, a short hard one for a forte,
//! so velocity changes the spectrum and not only the level.
//!
//! - Tine piano: the tine (a cantilever: its second mode near 6.3 × the
//!   fundamental is the bell of the attack) is coupled to a tone bar tuned to
//!   the same pitch, which gives the long two-stage decay. Its motion passes
//!   in front of an electromagnetic pickup whose field falls off on both
//!   sides of the pole: the output follows the time derivative of that field
//!   at the tine's position. Centred, the tine gives mostly the octave; offset
//!   (the voicing screw), the fundamental comes back, and the harder the
//!   strike or the closer the pickup, the more of the curve is swept — the
//!   bark.
//! - Reed piano: a steel reed between the plates of an electrostatic pickup:
//!   the capacitance goes as 1 / gap, so the reed moving towards the plate
//!   counts more than away from it — an asymmetric, gritty growl when hit
//!   hard — then a transistor preamp that clips unevenly.
//!
//! Dampers are felt: a released key rings down in a fraction of a second,
//! upper modes first, with a soft thump. The sustain pedal (CC 64) holds the
//! dampers off. After the keys come the amp's tremolo (or the Suitcase's
//! stereo auto-pan), optional phaser and chorus pedals, a tube drive and a
//! tone tilt.

use ponzik_dsp::filter::{DcBlocker, OnePole};
use ponzik_dsp::lfo::{Lfo, LfoShape};
use ponzik_dsp::modal::{Mode, Strike};
use ponzik_dsp::noise::WhiteNoise;
use ponzik_dsp::util::{db_to_gain, flush_denormal, midi_to_hz};

use super::common::{Expr, Tune, pick_voice, soft_clip};
use crate::effects::rotary::TubeDrive;
use crate::effects::{Chorus, Phaser};
use crate::param::{ParamInfo, Params, Unit};
use crate::{Controller, Effect, Instrument, NoteExpression};

const VOICES: usize = 24;
const MODES: usize = 4;
/// Samples between two retunings of a voice (pitch bend, slides).
const SUB: usize = 32;
/// Longest block rendered at once (longer ones are split).
const BLOCK: usize = 1_024;
const SILENCE: f32 = 3e-5;

/// Which instrument the model plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EpModel {
    /// Tine and tone bar, electromagnetic pickup.
    Tine,
    /// Steel reed, electrostatic pickup.
    Reed,
}

const P_BELL: u32 = 0;
const P_HAMMER: u32 = 1;
const P_DECAY: u32 = 2;
const P_RELEASE: u32 = 3;
const P_PICKUP_A: u32 = 4;
const P_PICKUP_B: u32 = 5;
const P_DYNAMICS: u32 = 6;
const P_TREM_RATE: u32 = 7;
const P_TREM_DEPTH: u32 = 8;
const P_AUTOPAN: u32 = 9;
const P_CHORUS: u32 = 10;
const P_PHASER: u32 = 11;
const P_DRIVE: u32 = 12;
const P_TONE: u32 = 13;
const P_VOLUME: u32 = 14;
const N_PARAMS: usize = 15;

static TINE_PARAMS: [ParamInfo; N_PARAMS] = [
    ParamInfo::new(P_BELL, "Bell", 0.0, 1.0, 0.4, Unit::Percent).group("Tine"),
    ParamInfo::new(P_HAMMER, "Hammer", 0.0, 1.0, 0.5, Unit::Percent).group("Tine"),
    ParamInfo::new(P_DECAY, "Decay", 0.25, 4.0, 1.0, Unit::Ratio).log().group("Tine"),
    ParamInfo::new(P_RELEASE, "Damper", 0.03, 1.5, 0.18, Unit::Seconds).log().group("Tine"),
    ParamInfo::new(P_PICKUP_A, "Pickup Offset", -1.0, 1.0, 0.35, Unit::Percent).group("Pickup"),
    ParamInfo::new(P_PICKUP_B, "Pickup Distance", 0.0, 1.0, 0.5, Unit::Percent).group("Pickup"),
    ParamInfo::new(P_DYNAMICS, "Dynamics", 0.0, 1.0, 0.75, Unit::Percent).group("Pickup"),
    ParamInfo::new(P_TREM_RATE, "Rate", 0.5, 12.0, 4.5, Unit::Hz).log().group("Tremolo"),
    ParamInfo::new(P_TREM_DEPTH, "Depth", 0.0, 1.0, 0.3, Unit::Percent).group("Tremolo"),
    ParamInfo::new(P_AUTOPAN, "Auto Pan", 0.0, 1.0, 1.0, Unit::Percent).group("Tremolo"),
    ParamInfo::new(P_CHORUS, "Chorus", 0.0, 1.0, 0.0, Unit::Percent).group("Effects"),
    ParamInfo::new(P_PHASER, "Phaser", 0.0, 1.0, 0.0, Unit::Percent).group("Effects"),
    ParamInfo::new(P_DRIVE, "Drive", 0.0, 1.0, 0.15, Unit::Percent).group("Amp"),
    ParamInfo::new(P_TONE, "Tone", -1.0, 1.0, 0.0, Unit::Percent).group("Amp"),
    ParamInfo::new(P_VOLUME, "Volume", -40.0, 6.0, -6.0, Unit::Db).group("Amp"),
];

static REED_PARAMS: [ParamInfo; N_PARAMS] = [
    ParamInfo::new(P_BELL, "Bell", 0.0, 1.0, 0.4, Unit::Percent).group("Reed"),
    ParamInfo::new(P_HAMMER, "Hammer", 0.0, 1.0, 0.5, Unit::Percent).group("Reed"),
    ParamInfo::new(P_DECAY, "Decay", 0.25, 4.0, 1.0, Unit::Ratio).log().group("Reed"),
    ParamInfo::new(P_RELEASE, "Damper", 0.03, 1.5, 0.12, Unit::Seconds).log().group("Reed"),
    ParamInfo::new(P_PICKUP_A, "Pickup Gap", 0.0, 1.0, 0.5, Unit::Percent).group("Pickup"),
    ParamInfo::new(P_PICKUP_B, "Grit", 0.0, 1.0, 0.35, Unit::Percent).group("Pickup"),
    ParamInfo::new(P_DYNAMICS, "Dynamics", 0.0, 1.0, 0.75, Unit::Percent).group("Pickup"),
    ParamInfo::new(P_TREM_RATE, "Rate", 0.5, 12.0, 5.6, Unit::Hz).log().group("Tremolo"),
    ParamInfo::new(P_TREM_DEPTH, "Depth", 0.0, 1.0, 0.25, Unit::Percent).group("Tremolo"),
    ParamInfo::new(P_AUTOPAN, "Auto Pan", 0.0, 1.0, 0.0, Unit::Percent).group("Tremolo"),
    ParamInfo::new(P_CHORUS, "Chorus", 0.0, 1.0, 0.0, Unit::Percent).group("Effects"),
    ParamInfo::new(P_PHASER, "Phaser", 0.0, 1.0, 0.0, Unit::Percent).group("Effects"),
    ParamInfo::new(P_DRIVE, "Drive", 0.0, 1.0, 0.2, Unit::Percent).group("Amp"),
    ParamInfo::new(P_TONE, "Tone", -1.0, 1.0, 0.0, Unit::Percent).group("Amp"),
    ParamInfo::new(P_VOLUME, "Volume", -40.0, 6.0, -6.0, Unit::Db).group("Amp"),
];

/// Magnetic field of the tine pickup across the pole, at a normalized
/// position: maximal over the pole, falling off on both sides.
#[inline]
pub fn tine_field(u: f32) -> f32 {
    1.0 / (1.0 + u * u)
}

/// Steepest slope of [`tine_field`] (at u = ±1/√3): normalizes the pickup.
const FIELD_SLOPE: f32 = 0.649_519;

/// Voltage of the reed pickup for a reed displacement `x` towards the plate,
/// gap factor `g` in [0, 0.9]: unit small-signal gain, x / (1 − g·x).
#[inline]
pub fn reed_pickup(x: f32, g: f32) -> f32 {
    x / (1.0 - (g * x).min(0.92))
}

/// Tine pickup sensitivity: how far across the field a full-velocity tine
/// swings, from the pickup distance setting (0: close).
fn tine_depth(distance: f32) -> f32 {
    0.3 + 1.9 * (1.0 - distance)
}

/// Gap factor of the reed pickup from its setting (0: narrow, growly).
fn reed_gap(gap: f32) -> f32 {
    0.15 + 0.7 * (1.0 - gap)
}

/// One cycle (`out.len()` points) of the pickup output for a pure
/// fundamental at `velocity` in [0, 1], for drawing the pickup's character.
pub fn pickup_cycle(model: EpModel, a: f32, b: f32, velocity: f32, out: &mut [f32]) {
    let n = out.len().max(2);
    let field = |p: f32| match model {
        EpModel::Tine => tine_field(tine_depth(b) * velocity * (std::f32::consts::TAU * p).sin() + a),
        EpModel::Reed => reed_pickup(velocity * (std::f32::consts::TAU * p).sin(), reed_gap(a)),
    };
    for (i, o) in out.iter_mut().enumerate() {
        let p = i as f32 / n as f32;
        *o = match model {
            EpModel::Tine => {
                let d = 1.0 / n as f32;
                (field(p + d) - field(p - d)) / (2.0 * d) / (std::f32::consts::TAU * tine_depth(b) * FIELD_SLOPE)
            }
            EpModel::Reed => field(p),
        };
    }
}

/// Decay of the fundamental to −60 dB, s, by key: long in the bass, a couple
/// of seconds at the top.
pub fn fundamental_t60(model: EpModel, note: u8) -> f32 {
    let n = note as f32 - 60.0;
    match model {
        EpModel::Tine => (9.0 * (-n / 19.0).exp2()).clamp(1.2, 25.0),
        EpModel::Reed => (3.6 * (-n / 22.0).exp2()).clamp(0.6, 10.0),
    }
}

/// Ratio of the second (bell) mode: a cantilever's 6.27, lowered in the
/// bass by the tine's tuning spring, raised on the reed by its solder tip.
pub fn bell_ratio(model: EpModel, note: u8) -> f32 {
    let n = (note as f32 - 60.0).clamp(-36.0, 36.0);
    match model {
        EpModel::Tine => 6.27 + 0.012 * n,
        EpModel::Reed => 6.9 + 0.008 * n,
    }
}

/// Spectrum of a raised-cosine contact of length `t` at `hz`, relative to DC.
fn pulse_gain(hz: f32, t: f32) -> f32 {
    let x = hz * t;
    if x < 1e-4 {
        return 1.0;
    }
    if (x - 1.0).abs() < 1e-3 {
        return 0.5;
    }
    let s = (std::f32::consts::PI * x).sin() / (std::f32::consts::PI * x);
    (s / (1.0 - x * x)).abs()
}

#[derive(Clone, Copy, Debug, Default)]
struct Voice {
    note: u8,
    started_at: u64,
    active: bool,
    /// Key held down.
    key: bool,
    /// Dampers on the tine/reed.
    damped: bool,
    hz: f32,
    modes: [Mode; MODES],
    ratio: [f32; MODES],
    gain: [f32; MODES],
    t60: [f32; MODES],
    strike: Strike,
    /// Displacement scale into the pickup (velocity, register).
    swing: f32,
    /// Derivative normalization of the tine pickup (1 / (2π f / sr)).
    deriv: f32,
    /// Displacement at the previous sample (the pickup differentiates).
    last_x: f32,
    /// Damper time while damped.
    release: f32,
    /// Hammer and damper noise.
    thump: f32,
    thump_coef: f32,
    thump_lp: f32,
    /// Retuning: the pitch ratio last applied to the modes.
    applied: f32,
    expr: Expr,
    tune: Tune,
    quiet: u32,
}

/// Electric piano, tine or reed.
pub struct ElectricPiano {
    model: EpModel,
    values: [f32; N_PARAMS],
    sample_rate: f32,
    voices: [Voice; VOICES],
    note_counter: u64,
    pedal: bool,
    mod_wheel: f32,
    bend: f32,
    noise: WhiteNoise,
    trem: Lfo,
    dc: [DcBlocker; 2],
    tone_lp: [OnePole; 2],
    drive: [TubeDrive; 2],
    chorus: Chorus,
    phaser: Phaser,
    buf_l: Vec<f32>,
    buf_r: Vec<f32>,
}

impl ElectricPiano {
    pub fn new(model: EpModel) -> Self {
        let params = Self::table(model);
        let mut e = Self {
            model,
            values: std::array::from_fn(|i| params[i].default),
            sample_rate: 48_000.0,
            voices: [Voice::default(); VOICES],
            note_counter: 0,
            pedal: false,
            mod_wheel: 0.0,
            bend: 0.0,
            noise: WhiteNoise::new(0x7e5),
            trem: Lfo::new(LfoShape::Sine),
            dc: [DcBlocker::new(48_000.0), DcBlocker::new(48_000.0)],
            tone_lp: [OnePole::new(900.0, 48_000.0), OnePole::new(900.0, 48_000.0)],
            drive: Default::default(),
            chorus: Chorus::default(),
            phaser: Phaser::default(),
            buf_l: Vec::new(),
            buf_r: Vec::new(),
        };
        e.set_sample_rate(48_000.0);
        e
    }

    /// Tine piano (Rhodes style).
    pub fn tine() -> Self {
        Self::new(EpModel::Tine)
    }

    /// Reed piano (Wurlitzer style).
    pub fn reed() -> Self {
        Self::new(EpModel::Reed)
    }

    pub fn model(&self) -> EpModel {
        self.model
    }

    fn table(model: EpModel) -> &'static [ParamInfo; N_PARAMS] {
        match model {
            EpModel::Tine => &TINE_PARAMS,
            EpModel::Reed => &REED_PARAMS,
        }
    }

    fn v(&self, id: u32) -> f32 {
        self.values[id as usize]
    }

    fn update_effects(&mut self) {
        let sr = self.sample_rate;
        self.trem.set_rate(self.v(P_TREM_RATE), sr);
        // Chorus pedal: slow and fairly deep, wide; phaser: a slow
        // four-stage sweep with some resonance (the classic small pedal).
        let c = &mut self.chorus;
        c.set_param(0, 0.0);
        c.set_param(1, 0.7);
        c.set_param(2, 0.45);
        c.set_param(3, 0.0);
        c.set_param(4, 0.8);
        c.set_param(5, 0.5 * self.values[P_CHORUS as usize]);
        let p = &mut self.phaser;
        p.set_param(0, 0.0);
        p.set_param(1, 600.0);
        p.set_param(2, 0.35);
        p.set_param(3, 0.8);
        p.set_param(4, 0.45);
        p.set_param(5, 0.3);
        p.set_param(6, 0.5 * self.values[P_PHASER as usize]);
    }

    /// Strikes (or strikes again) a voice.
    fn strike(&mut self, idx: usize, note: u8, velocity: f32, fresh: bool) {
        let sr = self.sample_rate;
        let model = self.model;
        let dyn_ = self.v(P_DYNAMICS);
        let vel = 1.0 - dyn_ + dyn_ * velocity.clamp(0.0, 1.0);
        let hz = midi_to_hz(note as f32);
        // Contact: 3 ms (soft hammer, soft touch) down to a fraction of a ms.
        let hard = self.v(P_HAMMER);
        let contact = (0.0032 - 0.0024 * hard) * (1.5 - 0.9 * vel);
        let bell = self.v(P_BELL) * vel.powf(1.5);
        let decay = self.v(P_DECAY);
        let key = (note as f32 - 60.0).clamp(-36.0, 36.0);
        let t60 = fundamental_t60(model, note) * decay;
        let br = bell_ratio(model, note);
        let (ratio, gain, t60s) = match model {
            EpModel::Tine => (
                // Tine, tone bar (a hair sharp: the slow bloom), bell, tink.
                [1.0, 1.000_6, br, 17.4],
                [0.6, 0.4, 1.4 * bell / br, 0.6 * bell / 17.4],
                [0.45 * t60, t60, (0.9 * (-key / 24.0).exp2() * decay.sqrt()).min(t60), 0.12],
            ),
            EpModel::Reed => (
                [1.0, br, 17.5, 2.0],
                [1.0, 1.2 * bell, 0.15 * bell, 0.0],
                [t60, (0.45 * (-key / 24.0).exp2() * decay.sqrt()).min(t60), 0.08, 1.0],
            ),
        };
        // Treble tines and reeds swing less: less bark up there.
        let swing = vel * (1.0 - 0.45 * ((note as f32 - 48.0) / 48.0).clamp(0.0, 1.0));
        self.note_counter += 1;
        let counter = self.note_counter;
        let bend = self.bend;
        let v = &mut self.voices[idx];
        if fresh {
            let keep = v.active.then_some(v.modes);
            *v = Voice { note, started_at: 0, active: true, ..Voice::default() };
            if let Some(m) = keep {
                // A stolen voice keeps ringing into the new note (no click).
                v.modes = m;
            }
            v.applied = 0.0;
            v.expr.reset();
        }
        v.started_at = counter;
        v.active = true;
        v.key = true;
        v.damped = false;
        v.hz = hz;
        v.ratio = ratio;
        v.t60 = t60s;
        v.swing = swing;
        v.deriv = sr / (std::f32::consts::TAU * hz);
        // Fundamental level set by the velocity alone, partials filtered by
        // the contact's spectrum.
        let comp = 1.0 / pulse_gain(hz, contact).max(0.25);
        v.gain = gain.map(|g| g * comp);
        v.strike.hit(1.0, contact, sr);
        v.thump = 0.012 * vel * vel * (0.5 + hard);
        v.thump_coef = ponzik_dsp::util::decay_coef(0.004, sr);
        v.quiet = 0;
        let ratio = v.tune.ratio(v.expr.tuning.value() + bend);
        Self::retune(v, ratio, sr);
    }

    /// Tunes the modes for a pitch ratio (bend, slides), with the decays of
    /// the voice's state (free or damped).
    fn retune(v: &mut Voice, pitch: f32, sr: f32) {
        v.applied = pitch;
        for m in 0..MODES {
            v.modes[m].tune(v.hz * v.ratio[m] * pitch, v.t60[m], sr);
        }
        if v.damped {
            // Retuning restores the free decays: damp again.
            let t = v.release;
            Self::damp_modes(v, t, sr);
        }
    }

    fn damp_modes(v: &mut Voice, release: f32, sr: f32) {
        v.release = release;
        for m in 0..MODES {
            // Felt grabs the upper modes faster.
            let t = if m == 0 || v.ratio[m] < 1.01 { release } else { release * 0.4 };
            v.modes[m].set_decay(t.min(v.t60[m]), sr);
        }
    }

    fn damp(&mut self, idx: usize) {
        let release = self.v(P_RELEASE);
        let sr = self.sample_rate;
        let v = &mut self.voices[idx];
        if v.damped {
            return;
        }
        v.damped = true;
        Self::damp_modes(v, release, sr);
        // The felt lands with a soft thump.
        v.thump = v.thump.max(0.003 * v.swing.min(1.0));
    }

    fn render_block(&mut self, left: &mut [f32], right: &mut [f32]) {
        let sr = self.sample_rate;
        let model = self.model;
        let n = left.len();
        let vals = self.values;
        let (bl, br) = (&mut self.buf_l[..n], &mut self.buf_r[..n]);
        bl.fill(0.0);
        br.fill(0.0);
        let any = self.voices.iter().any(|v| v.active);
        if any {
            let (a, b) = (vals[P_PICKUP_A as usize], vals[P_PICKUP_B as usize]);
            let depth = tine_depth(b);
            let norm = 1.0 / (depth * FIELD_SLOPE);
            let g = reed_gap(a);
            let bend = self.bend;
            for v in self.voices.iter_mut().filter(|v| v.active) {
                let mut start = 0;
                while start < n {
                    let end = (start + SUB).min(n);
                    // Pitch bend and slides: retune between sub-blocks.
                    let pitch = v.tune.ratio(v.expr.tuning.skip((end - start) as u32) + bend);
                    if pitch != v.applied {
                        Self::retune(v, pitch, sr);
                    }
                    let (pl, pr) = super::common::pan_gains(v.expr.pan);
                    for i in start..end {
                        let f = v.strike.next_sample();
                        let mut x = 0.0;
                        for m in 0..MODES {
                            x += v.modes[m].process(f * v.gain[m]);
                        }
                        let x = x * v.swing;
                        let y = match model {
                            EpModel::Tine => {
                                // Both ends with the current settings: a
                                // knob turn does not click.
                                let d = tine_field(depth * x + a) - tine_field(depth * v.last_x + a);
                                v.last_x = x;
                                d * v.deriv * norm / v.applied.max(0.1)
                            }
                            EpModel::Reed => reed_pickup(x, g),
                        };
                        // Mechanical noise: hammer and felt.
                        let y = if v.thump > 1e-6 {
                            v.thump_lp += (self.noise.next_sample() - v.thump_lp) * 0.08;
                            let t = v.thump * v.thump_lp;
                            v.thump *= v.thump_coef;
                            y + t
                        } else {
                            y
                        };
                        bl[i] += y * pl;
                        br[i] += y * pr;
                    }
                    start = end;
                }
                let energy: f32 = v.modes.iter().map(Mode::energy).sum();
                if !v.strike.is_active() && energy * v.swing < SILENCE && v.thump < 1e-6 {
                    v.quiet += 1;
                    if v.quiet > 2 {
                        v.active = false;
                    }
                } else {
                    v.quiet = 0;
                }
                v.modes.iter_mut().for_each(Mode::flush);
            }
        }
        // Reed preamp: an unevenly biased transistor stage, clean at 0.
        if model == EpModel::Reed {
            let grit = vals[P_PICKUP_B as usize];
            if grit > 0.0 {
                let k = 1.0 + 8.0 * grit * grit;
                let bias = 0.3 * grit;
                let shape = |u: f32| if u > 0.0 { u / (1.0 + u) } else { (1.5 * u).tanh() / 1.5 };
                let rest = shape(bias);
                for s in bl.iter_mut().chain(br.iter_mut()) {
                    let y = (shape(k * *s + bias) - rest) / k.powf(0.6);
                    *s = *s + (y - *s) * grit.sqrt();
                }
            }
        }
        // Amp tremolo: depth from the knob and the mod wheel; auto-pan moves
        // the two sides in opposition.
        let depth = (vals[P_TREM_DEPTH as usize] + self.mod_wheel).min(1.0);
        let pan = vals[P_AUTOPAN as usize];
        for i in 0..n {
            let lfo = self.trem.next_sample();
            let gl = 1.0 - depth * (0.5 + 0.5 * lfo);
            let gr = 1.0 - depth * (0.5 + 0.5 * lfo * (1.0 - 2.0 * pan));
            let (l, r) = (self.dc[0].process(bl[i]) * gl, self.dc[1].process(br[i]) * gr);
            bl[i] = flush_denormal(l);
            br[i] = flush_denormal(r);
        }
        if vals[P_PHASER as usize] > 0.0 {
            self.phaser.process(bl, br);
        }
        if vals[P_CHORUS as usize] > 0.0 {
            self.chorus.process(bl, br);
        }
        let drive = vals[P_DRIVE as usize];
        let tilt = (vals[P_TONE as usize] * 1.5).exp2();
        let gain = db_to_gain(vals[P_VOLUME as usize]);
        for i in 0..n {
            for (c, s) in [&mut bl[i], &mut br[i]].into_iter().enumerate() {
                let d = self.drive[c].process(*s, drive);
                let lp = self.tone_lp[c].lowpass(d);
                let t = lp + (d - lp) * tilt;
                *s = soft_clip(t * gain);
            }
            left[i] += bl[i];
            right[i] += br[i];
        }
    }
}

impl Params for ElectricPiano {
    fn params(&self) -> &'static [ParamInfo] {
        Self::table(self.model)
    }

    fn get_param(&self, id: u32) -> f32 {
        self.values.get(id as usize).copied().unwrap_or(0.0)
    }

    fn set_param(&mut self, id: u32, value: f32) {
        let Some(info) = Self::table(self.model).get(id as usize) else { return };
        self.values[id as usize] = info.clamp(value);
        match id {
            P_TREM_RATE | P_CHORUS | P_PHASER => self.update_effects(),
            P_RELEASE => {
                let release = self.v(P_RELEASE);
                let sr = self.sample_rate;
                for v in self.voices.iter_mut().filter(|v| v.active && v.damped) {
                    Self::damp_modes(v, release, sr);
                }
            }
            _ => {}
        }
    }
}

impl Instrument for ElectricPiano {
    fn kind(&self) -> &'static str {
        match self.model {
            EpModel::Tine => "rhodes",
            EpModel::Reed => "wurli",
        }
    }

    fn is_melodic(&self) -> bool {
        true
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.dc = [DcBlocker::new(sample_rate), DcBlocker::new(sample_rate)];
        self.tone_lp = [OnePole::new(900.0, sample_rate), OnePole::new(900.0, sample_rate)];
        self.drive.iter_mut().for_each(|d| d.set_sample_rate(sample_rate));
        self.chorus.set_sample_rate(sample_rate);
        self.phaser.set_sample_rate(sample_rate);
        self.buf_l = vec![0.0; BLOCK];
        self.buf_r = vec![0.0; BLOCK];
        self.update_effects();
        self.all_notes_off();
    }

    fn note_on(&mut self, note: u8, velocity: f32) {
        let note = note.min(127);
        // The same key strikes its own tine again.
        let same = (0..VOICES).find(|&i| self.voices[i].active && self.voices[i].note == note);
        let (idx, fresh) = match same {
            Some(i) => (i, false),
            None => (pick_voice(VOICES, |i| self.voices[i].active, |i| self.voices[i].started_at), true),
        };
        self.strike(idx, note, velocity, fresh);
    }

    fn note_off(&mut self, note: u8) {
        for i in 0..VOICES {
            let v = self.voices[i];
            if v.active && v.key && v.note == note {
                self.voices[i].key = false;
                if !self.pedal {
                    self.damp(i);
                }
            }
        }
    }

    fn supports_expression(&self) -> bool {
        true
    }

    fn note_expression(&mut self, key: u8, expr: NoteExpression) {
        let sr = self.sample_rate;
        if let Some(v) = self.voices.iter_mut().filter(|v| v.active && v.note == key).max_by_key(|v| v.started_at) {
            v.expr.apply(expr, sr);
        }
    }

    fn controller(&mut self, ctl: Controller) {
        match ctl {
            Controller::PitchBend(b) if b.is_finite() => self.bend = b.clamp(-1.0, 1.0) * crate::PITCH_BEND_RANGE,
            Controller::Cc(1, v) if v.is_finite() => self.mod_wheel = v.clamp(0.0, 1.0),
            Controller::Cc(64, v) => {
                let down = v >= 0.5;
                if self.pedal && !down {
                    for i in 0..VOICES {
                        if self.voices[i].active && !self.voices[i].key {
                            self.damp(i);
                        }
                    }
                }
                self.pedal = down;
            }
            _ => {}
        }
    }

    fn all_notes_off(&mut self) {
        for v in &mut self.voices {
            v.active = false;
        }
        self.pedal = false;
        self.chorus.reset();
        self.phaser.reset();
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let n = left.len().min(right.len());
        let mut start = 0;
        while start < n {
            let end = (start + BLOCK).min(n);
            self.render_block(&mut left[start..end], &mut right[start..end]);
            start = end;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::common::test_util::{render, stress};
    use super::*;
    use ponzik_dsp::util::gain_to_db;

    const SR: f32 = 48_000.0;

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|s| s * s).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    fn mag(x: &[f32], hz: f32) -> f32 {
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (n, &s) in x.iter().enumerate() {
            let ph = std::f64::consts::TAU * hz as f64 * n as f64 / SR as f64;
            re += s as f64 * ph.cos();
            im += s as f64 * ph.sin();
        }
        ((re * re + im * im).sqrt() / x.len() as f64 * 2.0) as f32
    }

    /// Bare instrument: no tremolo, effects or drive.
    fn bare(model: EpModel) -> ElectricPiano {
        let mut e = ElectricPiano::new(model);
        e.set_param(P_TREM_DEPTH, 0.0);
        e.set_param(P_DRIVE, 0.0);
        if model == EpModel::Reed {
            e.set_param(P_PICKUP_B, 0.0);
        }
        e
    }

    fn brightness(x: &[f32]) -> f32 {
        let d: Vec<f32> = x.windows(2).map(|w| w[1] - w[0]).collect();
        rms(&d) / rms(x)
    }

    #[test]
    fn plays_releases_and_damps() {
        for model in [EpModel::Tine, EpModel::Reed] {
            let mut e = bare(model);
            e.note_on(60, 0.8);
            let (l, _) = render(&mut e, 4_800);
            assert!(rms(&l) > 0.01, "{model:?}");
            e.note_off(60);
            render(&mut e, 48_000);
            assert!(e.voices.iter().all(|v| !v.active), "{model:?}");
        }
    }

    #[test]
    fn velocity_brightens_and_barks() {
        for model in [EpModel::Tine, EpModel::Reed] {
            let take = |vel: f32| {
                let mut e = bare(model);
                e.note_on(48, vel);
                let (l, _) = render(&mut e, 9_600);
                (rms(&l[480..]), brightness(&l[480..4_800]))
            };
            let (soft_e, soft_b) = take(0.25);
            let (hard_e, hard_b) = take(1.0);
            assert!(hard_e > 2.0 * soft_e, "{model:?} energy {soft_e} → {hard_e}");
            assert!(hard_b > 1.3 * soft_b, "{model:?} brightness {soft_b} → {hard_b}");
        }
    }

    #[test]
    fn tine_pickup_offset_sets_the_octave_and_bark() {
        // Centred over the pole the tine gives mostly its octave; offset,
        // the fundamental dominates; closer pickup, more harmonics.
        let take = |offset: f32, distance: f32, vel: f32| {
            let mut e = bare(EpModel::Tine);
            e.set_param(P_BELL, 0.0);
            e.set_param(P_PICKUP_A, offset);
            e.set_param(P_PICKUP_B, distance);
            e.note_on(45, vel);
            let (l, _) = render(&mut e, 48_000);
            let x = &l[24_000..];
            let f = midi_to_hz(45.0);
            (mag(x, f), mag(x, 2.0 * f), mag(x, 3.0 * f))
        };
        let (f1, f2, _) = take(0.0, 0.45, 0.8);
        assert!(f2 > 3.0 * f1, "centred: {f1} {f2}");
        let (f1, f2, _) = take(0.5, 0.45, 0.8);
        assert!(f1 > f2, "offset: {f1} {f2}");
        let (c1, c2, c3) = take(0.4, 0.0, 1.0);
        let (d1, d2, d3) = take(0.4, 1.0, 1.0);
        assert!((c2 + c3) / c1 > 2.0 * (d2 + d3) / d1, "bark close {} far {}", (c2 + c3) / c1, (d2 + d3) / d1);
    }

    #[test]
    fn reed_pickup_is_asymmetric() {
        // Hard: strong even harmonics from the 1 / gap law.
        let take = |vel: f32| {
            let mut e = bare(EpModel::Reed);
            e.set_param(P_BELL, 0.0);
            e.set_param(P_PICKUP_A, 0.0);
            e.set_param(P_DYNAMICS, 1.0);
            e.note_on(57, vel);
            let (l, _) = render(&mut e, 24_000);
            let x = &l[9_600..];
            let f = midi_to_hz(57.0);
            mag(x, 2.0 * f) / mag(x, f)
        };
        let (soft, hard) = (take(0.15), take(1.0));
        assert!(hard > 0.2, "{hard}");
        assert!(hard > 3.0 * soft, "{soft} {hard}");
        assert!((reed_pickup(0.01, 0.8) - 0.01).abs() < 1e-3);
        assert!(reed_pickup(0.9, 0.8) > 2.0 * -reed_pickup(-0.9, 0.8) * 0.9);
    }

    #[test]
    fn bell_mode_is_the_cantilever_s() {
        // The bell partial sits near 6.27 × the fundamental, and dies fast.
        let mut e = bare(EpModel::Tine);
        e.set_param(P_BELL, 1.0);
        e.set_param(P_HAMMER, 1.0);
        e.set_param(P_PICKUP_A, 0.6);
        e.set_param(P_PICKUP_B, 1.0);
        e.note_on(48, 1.0);
        let (l, _) = render(&mut e, 96_000);
        let f = midi_to_hz(48.0);
        let bell = f * bell_ratio(EpModel::Tine, 48);
        assert!((bell / f - 6.27).abs() < 0.3);
        let early = mag(&l[480..5_280], bell);
        let late = mag(&l[48_000..52_800], bell);
        let fund_early = mag(&l[480..5_280], f);
        let fund_late = mag(&l[48_000..52_800], f);
        assert!(early > 0.05 * fund_early, "{early} {fund_early}");
        assert!(late / early < 0.1 * fund_late / fund_early, "bell decays faster");
    }

    #[test]
    fn decays_are_longer_in_the_bass() {
        // Fundamental decay measured on the output, against the model.
        for model in [EpModel::Tine, EpModel::Reed] {
            let measure = |note: u8| {
                let mut e = bare(model);
                e.set_param(P_BELL, 0.0);
                e.note_on(note, 0.5);
                let (l, _) = render(&mut e, 96_000);
                let a = rms(&l[9_600..14_400]);
                let b = rms(&l[33_600..38_400]);
                // dB per second → T60.
                let rate = gain_to_db(b / a) / 0.5;
                -60.0 / rate
            };
            let (low, mid, high) = (measure(36), measure(60), measure(84));
            assert!(low > mid && mid > high, "{model:?}: {low} {mid} {high}");
            match model {
                EpModel::Tine => {
                    assert!((5.0..30.0).contains(&low), "tine low {low}");
                    assert!((3.0..15.0).contains(&mid), "tine mid {mid}");
                    assert!((1.0..6.0).contains(&high), "tine high {high}");
                }
                EpModel::Reed => {
                    assert!((3.0..12.0).contains(&low), "reed low {low}");
                    assert!((1.5..6.0).contains(&mid), "reed mid {mid}");
                    assert!((0.5..3.0).contains(&high), "reed high {high}");
                }
            }
        }
    }

    #[test]
    fn dampers_and_sustain_pedal() {
        let mut e = bare(EpModel::Tine);
        e.controller(Controller::Cc(64, 1.0));
        e.note_on(60, 0.8);
        render(&mut e, 4_800);
        e.note_off(60);
        let (l, _) = render(&mut e, 24_000);
        assert!(rms(&l[19_200..]) > 0.3 * rms(&l[..4_800]), "pedal holds");
        e.controller(Controller::Cc(64, 0.0));
        let (l, _) = render(&mut e, 24_000);
        // Damper: down by more than 40 dB within ~0.2 s.
        assert!(rms(&l[14_400..19_200]) < 0.01 * rms(&l[..480]).max(1e-6), "{}", rms(&l[14_400..19_200]));
        assert!(!e.voices[0].active || e.voices[0].damped);
    }

    #[test]
    fn restriking_reuses_the_tine() {
        let mut e = bare(EpModel::Tine);
        e.note_on(60, 0.6);
        render(&mut e, 2_400);
        e.note_on(60, 0.6);
        assert_eq!(e.voices.iter().filter(|v| v.active).count(), 1);
    }

    #[test]
    fn tremolo_and_autopan() {
        let mut e = ElectricPiano::tine();
        e.set_param(P_TREM_DEPTH, 1.0);
        e.set_param(P_TREM_RATE, 4.0);
        e.set_param(P_AUTOPAN, 1.0);
        e.set_param(P_DECAY, 4.0);
        e.note_on(60, 0.8);
        let (l, r) = render(&mut e, 48_000);
        let env = |x: &[f32]| x.chunks(1_200).map(rms).collect::<Vec<_>>();
        let (el, er) = (env(&l[4_800..]), env(&r[4_800..]));
        // Opposite phases: when one side is loud, the other is quiet.
        let mean_l = el.iter().sum::<f32>() / el.len() as f32;
        let mean_r = er.iter().sum::<f32>() / er.len() as f32;
        let cov: f32 = el.iter().zip(&er).map(|(a, b)| (a - mean_l) * (b - mean_r)).sum();
        assert!(cov < 0.0, "{cov}");
    }

    #[test]
    fn pitch_bend_retunes() {
        let mut e = bare(EpModel::Reed);
        e.set_param(P_BELL, 0.0);
        e.controller(Controller::PitchBend(1.0));
        e.note_on(57, 0.3);
        let (l, _) = render(&mut e, 48_000);
        let x = &l[4_800..];
        assert!(mag(x, midi_to_hz(59.0)) > 5.0 * mag(x, midi_to_hz(57.0)));
    }

    #[test]
    fn pickup_cycles_are_bounded() {
        let mut out = [0.0; 64];
        for model in [EpModel::Tine, EpModel::Reed] {
            for v in [0.0, 0.5, 1.0] {
                pickup_cycle(model, 0.3, 0.2, v, &mut out);
                assert!(out.iter().all(|s| s.is_finite() && s.abs() < 20.0));
            }
        }
    }

    #[test]
    fn steals_voices_and_survives_extremes() {
        for model in [EpModel::Tine, EpModel::Reed] {
            let mut e = ElectricPiano::new(model);
            for n in 0..(VOICES as u8 + 8) {
                e.note_on(30 + n, 0.9);
            }
            assert_eq!(e.voices.iter().filter(|v| v.active).count(), VOICES);
            stress(&mut e);
            e.controller(Controller::Cc(64, 1.0));
            stress(&mut e);
        }
    }
}
