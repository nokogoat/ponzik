//! Grand Piano: a physically modeled acoustic piano, no samples.
//!
//! Each key is a set of stiff strings modeled as damped modes ([`voice`]),
//! struck by a felt hammer whose nonlinear stiffness makes the velocity shape
//! the spectrum. The strings of all keys also exist as waveguides that ring in
//! sympathy when their dampers are lifted ([`resonance`]). The bridge drives a
//! soundboard model, the lid and a small room ([`body`]); the action adds its
//! own noises ([`noise`]). The sustain (CC64, half-pedaling included),
//! sostenuto (CC66) and soft (CC67, una corda) pedals act on the model itself.

mod body;
pub mod keys;
mod noise;
mod resonance;
mod voice;

#[cfg(test)]
mod tests;

use std::sync::OnceLock;

use ponzik_dsp::util::{db_to_gain, gain_to_db};

use self::body::{Body, BodySettings};
use self::keys::{FIRST_UNDAMPED, HIGHEST, KEYS, LOWEST};
use self::noise::{Burst, Noises};
use self::resonance::Resonance;
use self::voice::{BLOCK, Material, Setup, Voice};
use super::common::{pan_gains, soft_clip};
use crate::param::{ParamInfo, Params, Unit};
use crate::{Controller, Instrument};

/// Voices at most; a key keeps its voice when struck again.
pub const VOICES: usize = 64;
/// Hammer velocity at full key velocity, m/s.
const MAX_HAMMER_SPEED: f32 = 5.0;
/// Key velocity of the calibration of the keys.
const REFERENCE_VELOCITY: f32 = 0.7;
/// RMS level of a key struck at the reference velocity, before the body.
const REFERENCE_LEVEL: f32 = 0.04;
/// Strings' share of the bridge motion.
const COUPLING: f32 = 0.0025;
/// The case keeps processing this long after the last sound, s.
const TAIL: f32 = 3.0;

pub const P_HARDNESS: u32 = 0;
pub const P_DYNAMICS: u32 = 1;
pub const P_CURVE: u32 = 2;
pub const P_LID: u32 = 3;
pub const P_DECAY: u32 = 4;
pub const P_RESONANCE: u32 = 5;
pub const P_UNISON: u32 = 6;
pub const P_STRETCH: u32 = 7;
pub const P_TUNING: u32 = 8;
pub const P_HAMMER_NOISE: u32 = 9;
pub const P_RELEASE_NOISE: u32 = 10;
pub const P_PEDAL_NOISE: u32 = 11;
pub const P_BOARD: u32 = 12;
pub const P_ROOM: u32 = 13;
pub const P_WIDTH: u32 = 14;
pub const P_PERSPECTIVE: u32 = 15;
pub const P_SIZE: u32 = 16;
pub const P_FELT: u32 = 17;
pub const P_MATERIAL: u32 = 18;
pub const P_VOLUME: u32 = 19;

static PARAMS: [ParamInfo; 20] = [
    ParamInfo::new(P_HARDNESS, "Hardness", 0.0, 1.0, 0.5, Unit::Percent).group("Hammer"),
    ParamInfo::new(P_DYNAMICS, "Dynamics", 10.0, 40.0, 26.0, Unit::Db).group("Hammer"),
    ParamInfo::new(P_CURVE, "Velocity curve", 0.4, 2.5, 1.0, Unit::None).log().group("Hammer"),
    ParamInfo::new(P_FELT, "Felt", 0.0, 1.0, 0.0, Unit::Percent).group("Hammer"),
    ParamInfo::new(P_DECAY, "Decay", 0.25, 4.0, 1.0, Unit::Percent).log().group("Strings"),
    ParamInfo::new(P_RESONANCE, "Resonance", 0.0, 1.0, 0.5, Unit::Percent).group("Strings"),
    ParamInfo::new(P_UNISON, "Unison detune", 0.0, 30.0, 1.2, Unit::Cents).group("Strings"),
    ParamInfo::new(P_STRETCH, "Stretch", 0.0, 2.0, 1.0, Unit::Percent).group("Strings"),
    ParamInfo::new(P_TUNING, "Tuning", 415.0, 466.0, 440.0, Unit::Hz).group("Strings"),
    ParamInfo::choice(P_MATERIAL, "Material", &["Strings", "Toy rods", "Celesta bars"], 0).group("Strings"),
    ParamInfo::new(P_LID, "Lid", 0.0, 1.0, 0.8, Unit::Percent).group("Body"),
    ParamInfo::new(P_BOARD, "Soundboard", 0.0, 1.0, 0.5, Unit::Percent).group("Body"),
    ParamInfo::new(P_SIZE, "Size", 0.0, 1.0, 1.0, Unit::Percent).group("Body"),
    ParamInfo::new(P_ROOM, "Room", 0.0, 1.0, 0.2, Unit::Percent).group("Body"),
    ParamInfo::new(P_HAMMER_NOISE, "Hammer noise", 0.0, 1.0, 0.4, Unit::Percent).group("Noises"),
    ParamInfo::new(P_RELEASE_NOISE, "Release noise", 0.0, 1.0, 0.4, Unit::Percent).group("Noises"),
    ParamInfo::new(P_PEDAL_NOISE, "Pedal noise", 0.0, 1.0, 0.3, Unit::Percent).group("Noises"),
    ParamInfo::new(P_WIDTH, "Width", 0.0, 1.0, 0.7, Unit::Percent).group("Output"),
    ParamInfo::choice(P_PERSPECTIVE, "Perspective", &["Player", "Audience"], 0).group("Output"),
    ParamInfo::new(P_VOLUME, "Volume", -40.0, 6.0, -4.0, Unit::Db).group("Output"),
];

/// Settings of the model, plain values.
#[derive(Clone, Copy, Debug)]
struct Settings {
    hardness: f32,
    dynamics: f32,
    curve: f32,
    felt: f32,
    decay: f32,
    resonance: f32,
    unison: f32,
    stretch: f32,
    tuning: f32,
    material: Material,
    lid: f32,
    board: f32,
    size: f32,
    room: f32,
    hammer_noise: f32,
    release_noise: f32,
    pedal_noise: f32,
    width: f32,
    audience: bool,
    gain: f32,
}

impl Default for Settings {
    fn default() -> Self {
        let d = |id: u32| PARAMS.iter().find(|p| p.id == id).map_or(0.0, |p| p.default);
        Self {
            hardness: d(P_HARDNESS),
            dynamics: d(P_DYNAMICS),
            curve: d(P_CURVE),
            felt: d(P_FELT),
            decay: d(P_DECAY),
            resonance: d(P_RESONANCE),
            unison: d(P_UNISON),
            stretch: d(P_STRETCH),
            tuning: d(P_TUNING),
            material: Material::Strings,
            lid: d(P_LID),
            board: d(P_BOARD),
            size: d(P_SIZE),
            room: d(P_ROOM),
            hammer_noise: d(P_HAMMER_NOISE),
            release_noise: d(P_RELEASE_NOISE),
            pedal_noise: d(P_PEDAL_NOISE),
            width: d(P_WIDTH),
            audience: false,
            gain: db_to_gain(d(P_VOLUME)),
        }
    }
}

fn material_index(m: Material) -> usize {
    match m {
        Material::Strings => 0,
        Material::Rods => 1,
        Material::Bars => 2,
    }
}

/// Key used for the tables: notes beyond the keyboard take its ends.
fn table_key(note: u8) -> u8 {
    note.clamp(LOWEST, HIGHEST)
}

/// Tuning of every note: first partial in Hz (stretched), index: note.
fn tuning_table(s: &Settings) -> [f32; 128] {
    let curve = keys::stretch_curve(s.size);
    let mut out = [0.0f32; 128];
    for (note, hz) in out.iter_mut().enumerate() {
        let k = table_key(note as u8);
        let cents = if s.material == Material::Strings { curve[(k - LOWEST) as usize] * s.stretch } else { 0.0 };
        *hz = s.tuning * ((note as f32 - 69.0) / 12.0 + cents / 1200.0).exp2();
    }
    out
}

/// What a strike of `note` at key velocity `vel` and soft pedal `soft` sets
/// in motion.
fn setup(s: &Settings, note: u8, f1: f32, vel: f32, soft: f32, sample_rate: f32) -> Setup {
    let k = table_key(note);
    let strings_mat = s.material == Material::Strings;
    let strings = if strings_mat { keys::strings(k) } else { 1 };
    let size = s.size;
    let len = keys::length(k, size);
    let tension = keys::tension(k);
    let mu = tension / (2.0 * len * f1.max(1.0)).powi(2);
    let felt = s.felt;
    let soft = soft.clamp(0.0, 1.0);
    // Unison: each string's own mistuning, fixed per key; single strings get
    // their slightly split polarizations.
    let spread = |salt: u32, sign: f32| sign * (0.5 + 0.5 * keys::hash(note, salt).abs());
    let detune = if strings == 1 {
        let pol = 0.06 + 0.04 * keys::hash(note, 3).abs();
        [pol.copysign(keys::hash(note, 4)) + 0.15 * s.unison * keys::hash(note, 5), 0.0, 0.0]
    } else {
        [s.unison * spread(1, 1.0), s.unison * spread(2, -1.0), 0.0]
    };
    let (mass, k_felt, p) = keys::hammer(k);
    let hard = s.hardness - 0.5;
    // Stiffness as the force at 1 mm of compression, so that changing the
    // exponent keeps the felt's strength. The soft pedal shifts the hammers
    // onto less worn, softer felt; the moderator puts a felt strip between
    // hammers and strings.
    const REF: f32 = 1e-3;
    let mut at_ref = k_felt * REF.powf(p) * 10f32.powf(1.5 * hard) * (1.0 - 0.8 * soft) * (1.0 - 0.8 * felt);
    let mut felt_p = p + 0.6 * hard - 0.6 * soft - 0.4 * felt;
    if s.material == Material::Rods {
        // A toy piano's hammers are hard.
        at_ref *= 4.0;
        felt_p += 0.2;
    }
    let felt_p = felt_p.max(1.5);
    let felt_k = at_ref / REF.powf(felt_p);
    let velocity = hammer_speed(s, vel);
    let after = keys::aftersound_t60(k) * s.decay * (1.0 - 0.6 * felt);
    let t60 = match s.material {
        Material::Strings => after,
        Material::Rods => after * 0.35,
        Material::Bars => after * 0.3,
    };
    // Una corda: the hammer misses a string, which only rings through the
    // bridge (more aftersound, less prompt sound).
    let (sym, asym) = if strings > 1 { (1.0 - 0.15 * soft, 0.28 + 0.08 * soft) } else { (1.0, 0.22) };
    Setup {
        sample_rate,
        material: s.material,
        f1,
        b: if strings_mat { keys::inharmonicity(k, size) } else { 0.0 },
        strings,
        detune,
        t60,
        prompt_ratio: keys::prompt_ratio(k),
        hf_loss: 6.9 / 2.2 * (1.0 + 8.0 * felt) / s.decay.sqrt(),
        damped_t60: keys::damped_t60(k),
        strike: keys::strike_point(k),
        asym,
        slow_out: 0.45,
        sym,
        modal_mass: strings as f32 * mu * len * 0.5 * if strings_mat { 1.0 } else { 0.5 },
        hammer_mass: mass,
        felt_k,
        felt_p,
        velocity,
    }
}

/// Level calibration of the keys, per material (default settings).
struct Calibration {
    /// Output gain so that a strike at the reference velocity has the same
    /// level across the keyboard.
    gain: [[f32; KEYS]; 3],
    /// Correction (dB) of the dynamic range from a soft to a full strike, to
    /// match the middle of the keyboard: the few modes of the top treble
    /// otherwise make soft notes vanish.
    range: [[f32; KEYS]; 3],
    /// Hammer speeds of the soft, reference and full strikes.
    speeds: [f32; 3],
}

/// Key velocities of the soft and full calibration strikes.
const SOFT_VELOCITY: f32 = 0.25;

fn hammer_speed(s: &Settings, vel: f32) -> f32 {
    let curve = vel.clamp(0.0, 1.0).powf(s.curve);
    MAX_HAMMER_SPEED * 10f32.powf(-s.dynamics / 20.0 * (1.0 - curve))
}

fn calibration() -> &'static Calibration {
    static TABLE: OnceLock<Calibration> = OnceLock::new();
    TABLE.get_or_init(|| {
        let sr = 48_000.0;
        let mut voice = Box::<Voice>::default();
        let mut scratch = [0.0f32; BLOCK];
        let mut power_at = |s: &Settings, k: u8, f1: f32, vel: f32| {
            voice.kill();
            voice.strike(k, &setup(s, k, f1, vel, 0.0, sr), 0, 1.0);
            for _ in 0..64 {
                if !voice.is_striking() {
                    break;
                }
                voice.render(&mut scratch);
            }
            voice.power().max(1e-30)
        };
        let base = Settings::default();
        let mut cal = Calibration {
            gain: [[1.0; KEYS]; 3],
            range: [[0.0; KEYS]; 3],
            speeds: [SOFT_VELOCITY, REFERENCE_VELOCITY, 1.0].map(|v| hammer_speed(&base, v)),
        };
        for (m, material) in [Material::Strings, Material::Rods, Material::Bars].into_iter().enumerate() {
            let s = Settings { material, ..base };
            let tuning = tuning_table(&s);
            let mut ranges = [0.0f32; KEYS];
            for k in LOWEST..=HIGHEST {
                let i = (k - LOWEST) as usize;
                let f1 = tuning[k as usize];
                let reference = power_at(&s, k, f1, REFERENCE_VELOCITY);
                let soft = power_at(&s, k, f1, SOFT_VELOCITY);
                let full = power_at(&s, k, f1, 1.0);
                ranges[i] = 10.0 * (full / soft).log10();
                // A little more in the bass, where the ear is less sensitive.
                let tilt = 1.0 + 0.4 * (1.0 - i as f32 / (KEYS - 1) as f32);
                cal.gain[m][i] = REFERENCE_LEVEL * tilt / reference.sqrt();
            }
            // Smoothed over a few keys: a strike's spectrum can fall into a
            // gap of a single key's few modes.
            let smooth: [f32; KEYS] = std::array::from_fn(|i| {
                let (a, b) = (i.saturating_sub(3), (i + 4).min(KEYS));
                ranges[a..b].iter().sum::<f32>() / (b - a) as f32
            });
            let middle = smooth[(60 - LOWEST) as usize];
            for (r, s) in cal.range[m].iter_mut().zip(&smooth) {
                *r = (middle - s).clamp(-12.0, 12.0);
            }
        }
        cal
    })
}

/// Smoothstep of x over [a, b].
fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Physically modeled grand piano.
pub struct Piano {
    s: Settings,
    sample_rate: f32,
    voices: Box<[Voice]>,
    counter: u64,
    /// First partial of every note, Hz.
    f1: [f32; 128],
    pan: [(f32, f32); 128],
    /// Keys held down, keys held up by the sostenuto.
    keys_down: u128,
    sostenuto: u128,
    /// Pedal positions (0 to 1) as received, and the damper lift that follows
    /// the sustain pedal.
    sustain: f32,
    sostenuto_pedal: f32,
    soft: f32,
    lift: f32,
    body: Body,
    resonance: Resonance,
    noises: Noises,
    vbuf: [f32; BLOCK],
    bus_l: [f32; BLOCK],
    bus_r: [f32; BLOCK],
    feed: [f32; BLOCK],
    /// Samples since anything last sounded.
    idle: u32,
    calibration: &'static Calibration,
}

impl Default for Piano {
    fn default() -> Self {
        Self::new()
    }
}

impl Piano {
    pub fn new() -> Self {
        let sr = 48_000.0;
        let mut p = Self {
            s: Settings::default(),
            sample_rate: sr,
            voices: (0..VOICES).map(|_| Voice::default()).collect(),
            counter: 0,
            f1: [440.0; 128],
            pan: [(1.0, 1.0); 128],
            keys_down: 0,
            sostenuto: 0,
            sustain: 0.0,
            sostenuto_pedal: 0.0,
            soft: 0.0,
            lift: 0.0,
            body: Body::new(sr),
            resonance: Resonance::new(sr),
            noises: Noises::new(sr),
            vbuf: [0.0; BLOCK],
            bus_l: [0.0; BLOCK],
            bus_r: [0.0; BLOCK],
            feed: [0.0; BLOCK],
            idle: u32::MAX,
            calibration: calibration(),
        };
        p.retune();
        p.update_body();
        p
    }

    /// Damper lift of the sustain pedal: dampers leave the strings between
    /// about 25 % and 70 % of the pedal's travel (half-pedaling in between).
    fn lift_target(&self) -> f32 {
        smoothstep(0.22, 0.7, self.sustain)
    }

    /// Tuning, pan and sympathetic strings after a change of tuning, stretch,
    /// size, width or perspective.
    fn retune(&mut self) {
        self.f1 = tuning_table(&self.s);
        let dir = if self.s.audience { -1.0 } else { 1.0 };
        for (note, p) in self.pan.iter_mut().enumerate() {
            let pos = ((note as f32 - 64.5) / 43.5).clamp(-1.0, 1.0);
            *p = pan_gains(pos * dir * self.s.width * 0.8);
        }
        for k in LOWEST..=HIGHEST {
            let idx = (k - LOWEST) as usize;
            let t60 = (keys::aftersound_t60(k) * 0.5 * self.s.decay).min(8.0);
            self.resonance.tune(idx, self.f1[k as usize], t60, keys::damped_t60(k) * 0.6, self.pan[k as usize]);
        }
    }

    fn update_body(&mut self) {
        self.body.set(&BodySettings {
            board: self.s.board,
            lid: self.s.lid,
            felt: self.s.felt,
            size: self.s.size,
            room: self.s.room,
        });
    }

    /// Output gain of a strike of `note` at hammer speed `speed`.
    fn gain_of(&self, note: u8, speed: f32) -> f32 {
        let (m, i) = (material_index(self.s.material), (table_key(note) - LOWEST) as usize);
        let c = self.calibration;
        let u = (speed / c.speeds[1]).ln() / (c.speeds[2] / c.speeds[0]).ln();
        c.gain[m][i] * 10f32.powf(c.range[m][i] * u / 20.0)
    }

    fn has_damper(&self, note: u8) -> bool {
        note < FIRST_UNDAMPED
    }

    /// Damper position wanted for `note`: 0 lifted, 1 resting.
    fn damper_target(&self, note: u8) -> f32 {
        let bit = 1u128 << (note & 127);
        if !self.has_damper(note) || (self.keys_down | self.sostenuto) & bit != 0 { 0.0 } else { 1.0 - self.lift }
    }

    /// Moves the pedal and the dampers over `n` samples.
    fn update_dampers(&mut self, n: usize) {
        let sr = self.sample_rate;
        let a = 1.0 - (-(n as f32) / (0.015 * sr)).exp();
        let target = self.lift_target();
        self.lift += (target - self.lift) * a;
        if (target - self.lift).abs() < 1e-3 {
            self.lift = target;
        }
        // Dampers fall onto the strings in about 10 ms.
        let d = 1.0 - (-(n as f32) / (0.01 * sr)).exp();
        let release_noise = self.s.release_noise;
        for i in 0..self.voices.len() {
            if !self.voices[i].active {
                continue;
            }
            let key = self.voices[i].key;
            let goal = self.damper_target(key);
            let v = &mut self.voices[i];
            let before = v.damper;
            let mut e = before + (goal - before) * d;
            if (goal - e).abs() < 2e-3 {
                e = goal;
            }
            if before < 0.5 && e >= 0.5 && release_noise > 0.0 {
                // The felt lands on a ringing string.
                let amp = (v.level.sqrt() * v.gain).min(1.0) * 0.12 * release_noise;
                let hz = (self.f1[key as usize] * 2.0).clamp(300.0, 4_000.0);
                let pan = self.pan[key as usize];
                self.noises.trigger(Burst { hz, q: 1.5, lowpass: false, amp, attack: 0.002, decay: 0.05, pan, to_strings: 0.0 });
            }
            self.voices[i].set_damper(e);
        }
        for k in LOWEST..=HIGHEST {
            self.resonance.set_damper((k - LOWEST) as usize, self.damper_target(k));
        }
    }

    /// One block of at most [`BLOCK`] samples.
    fn block(&mut self, left: &mut [f32], right: &mut [f32]) {
        let n = left.len().min(right.len()).min(BLOCK);
        let sounding = self.voices.iter().any(|v| v.active) || self.noises.is_active() || !self.resonance.is_quiet();
        if sounding {
            self.idle = 0;
        } else if self.idle >= (TAIL * self.sample_rate) as u32 {
            // Silent: only the pedal keeps moving.
            let target = self.lift_target();
            self.lift = target;
            return;
        } else {
            self.idle = self.idle.saturating_add(n as u32);
        }
        self.update_dampers(n);
        self.bus_l[..n].fill(0.0);
        self.bus_r[..n].fill(0.0);
        self.feed[..n].fill(0.0);
        for v in self.voices.iter_mut().filter(|v| v.active) {
            v.render(&mut self.vbuf[..n]);
            let g = v.gain;
            let (pl, pr) = self.pan[v.key as usize];
            for i in 0..n {
                let x = self.vbuf[i] * g;
                self.bus_l[i] += x * pl;
                self.bus_r[i] += x * pr;
                self.feed[i] += x;
            }
        }
        self.noises.render(&mut self.bus_l[..n], &mut self.bus_r[..n], &mut self.feed[..n]);
        let amount = self.s.resonance * 1.5;
        if amount > 0.0 {
            self.resonance.render(&self.feed[..n], COUPLING, amount, &mut self.bus_l[..n], &mut self.bus_r[..n]);
        }
        self.body.process(&mut self.bus_l[..n], &mut self.bus_r[..n]);
        let gain = self.s.gain;
        for i in 0..n {
            left[i] += soft_clip(self.bus_l[i] * gain);
            right[i] += soft_clip(self.bus_r[i] * gain);
        }
    }

    /// Voice for a strike of `note`: its own if it still sounds, else a free
    /// one, else the quietest released one (or the quietest of all).
    fn voice_for(&self, note: u8) -> usize {
        if let Some(i) = self.voices.iter().position(|v| v.active && v.key == note) {
            return i;
        }
        if let Some(i) = self.voices.iter().position(|v| !v.active) {
            return i;
        }
        let quietest = |held: bool| {
            (0..self.voices.len())
                .filter(|&i| self.voices[i].held == held && !self.voices[i].is_striking())
                .min_by(|&a, &b| self.voices[a].level.total_cmp(&self.voices[b].level))
        };
        quietest(false).or_else(|| quietest(true)).unwrap_or(0)
    }

    fn pedal_noise(&mut self, down: bool) {
        let amp = self.s.pedal_noise;
        if amp <= 0.0 {
            return;
        }
        let center = (1.0, 1.0);
        // The pedal's thump through the case…
        let thump = if down { 0.05 } else { 0.03 };
        self.noises.trigger(Burst { hz: 110.0, q: 1.2, lowpass: true, amp: thump * amp, attack: 0.003, decay: 0.045, pan: center, to_strings: 0.2 });
        // …and the damper rail leaving or touching every string: a soft
        // swish that sets the lifted strings ringing.
        let swish = if down { 0.012 } else { 0.008 };
        self.noises.trigger(Burst { hz: 1_800.0, q: 0.7, lowpass: false, amp: swish * amp, attack: 0.01, decay: 0.08, pan: center, to_strings: 3.0 });
    }

    /// Keys held, sustain lift, sostenuto and soft pedals (for tests).
    #[cfg(test)]
    fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active).count()
    }
}

impl Params for Piano {
    fn params(&self) -> &'static [ParamInfo] {
        &PARAMS
    }

    fn get_param(&self, id: u32) -> f32 {
        let s = &self.s;
        match id {
            P_HARDNESS => s.hardness,
            P_DYNAMICS => s.dynamics,
            P_CURVE => s.curve,
            P_FELT => s.felt,
            P_DECAY => s.decay,
            P_RESONANCE => s.resonance,
            P_UNISON => s.unison,
            P_STRETCH => s.stretch,
            P_TUNING => s.tuning,
            P_MATERIAL => material_index(s.material) as f32,
            P_LID => s.lid,
            P_BOARD => s.board,
            P_SIZE => s.size,
            P_ROOM => s.room,
            P_HAMMER_NOISE => s.hammer_noise,
            P_RELEASE_NOISE => s.release_noise,
            P_PEDAL_NOISE => s.pedal_noise,
            P_WIDTH => s.width,
            P_PERSPECTIVE => s.audience as u8 as f32,
            P_VOLUME => gain_to_db(s.gain),
            _ => 0.0,
        }
    }

    fn set_param(&mut self, id: u32, value: f32) {
        let Some(info) = PARAMS.iter().find(|i| i.id == id) else { return };
        let v = info.clamp(value);
        let s = &mut self.s;
        match id {
            P_HARDNESS => s.hardness = v,
            P_DYNAMICS => s.dynamics = v,
            P_CURVE => s.curve = v,
            P_FELT => s.felt = v,
            P_DECAY => s.decay = v,
            P_RESONANCE => s.resonance = v,
            P_UNISON => s.unison = v,
            P_STRETCH => s.stretch = v,
            P_TUNING => s.tuning = v,
            P_MATERIAL => s.material = [Material::Strings, Material::Rods, Material::Bars][v as usize % 3],
            P_LID => s.lid = v,
            P_BOARD => s.board = v,
            P_SIZE => s.size = v,
            P_ROOM => s.room = v,
            P_HAMMER_NOISE => s.hammer_noise = v,
            P_RELEASE_NOISE => s.release_noise = v,
            P_PEDAL_NOISE => s.pedal_noise = v,
            P_WIDTH => s.width = v,
            P_PERSPECTIVE => s.audience = v >= 0.5,
            P_VOLUME => s.gain = db_to_gain(v),
            _ => {}
        }
        if matches!(id, P_TUNING | P_STRETCH | P_SIZE | P_WIDTH | P_PERSPECTIVE | P_DECAY | P_MATERIAL) {
            self.retune();
        }
        if matches!(id, P_LID | P_BOARD | P_SIZE | P_ROOM | P_FELT) {
            self.update_body();
        }
    }
}

impl Instrument for Piano {
    fn kind(&self) -> &'static str {
        "piano"
    }

    fn is_melodic(&self) -> bool {
        true
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate.max(8_000.0);
        self.body = Body::new(self.sample_rate);
        self.resonance = Resonance::new(self.sample_rate);
        self.noises = Noises::new(self.sample_rate);
        self.retune();
        self.update_body();
        self.all_notes_off();
    }

    fn note_on(&mut self, note: u8, velocity: f32) {
        let note = note & 127;
        if velocity.is_nan() || velocity <= 0.0 {
            self.note_off(note);
            return;
        }
        let vel = velocity.min(1.0);
        self.keys_down |= 1u128 << note;
        let idx = self.voice_for(note);
        if self.voices[idx].active && self.voices[idx].key != note {
            self.voices[idx].kill();
        }
        self.counter += 1;
        let su = setup(&self.s, note, self.f1[note as usize], vel, self.soft, self.sample_rate);
        // Una corda: fewer strings struck, less sound.
        let gain = self.gain_of(note, su.velocity) * (1.0 - 0.25 * self.soft * (su.strings > 1) as u8 as f32);
        self.voices[idx].strike(note, &su, self.counter, gain);
        self.idle = 0;
        // The hammer's knock on the string and the key landing on the key bed.
        let hn = self.s.hammer_noise;
        if hn > 0.0 {
            let pan = self.pan[note as usize];
            let force = (su.velocity / MAX_HAMMER_SPEED).powf(1.5);
            let hz = (1_200.0 + 12.0 * (note as f32 - 21.0).max(0.0) * 2.0).min(3_500.0);
            self.noises.trigger(Burst { hz, q: 0.9, lowpass: false, amp: 0.05 * force * hn, attack: 0.0003, decay: 0.006, pan, to_strings: 0.0 });
            self.noises.trigger(Burst {
                hz: 180.0,
                q: 1.1,
                lowpass: true,
                amp: 0.06 * vel * vel * hn,
                attack: 0.002,
                decay: 0.03,
                pan: (1.0, 1.0),
                to_strings: 0.3,
            });
        }
    }

    fn note_off(&mut self, note: u8) {
        let note = note & 127;
        let bit = 1u128 << note;
        if self.keys_down & bit == 0 {
            return;
        }
        self.keys_down &= !bit;
        for v in self.voices.iter_mut().filter(|v| v.active && v.key == note) {
            v.held = false;
        }
        let rn = self.s.release_noise;
        if rn > 0.0 {
            // The key coming back up.
            let pan = self.pan[note as usize];
            self.noises.trigger(Burst { hz: 420.0, q: 1.4, lowpass: false, amp: 0.012 * rn, attack: 0.002, decay: 0.012, pan, to_strings: 0.0 });
        }
    }

    fn controller(&mut self, ctl: Controller) {
        let Controller::Cc(cc, value) = ctl else { return };
        let v = if value.is_finite() { value.clamp(0.0, 1.0) } else { 0.0 };
        match cc {
            64 => {
                let (was, is) = (self.sustain >= 0.5, v >= 0.5);
                self.sustain = v;
                if was != is {
                    self.pedal_noise(is);
                }
            }
            66 => {
                let (was, is) = (self.sostenuto_pedal >= 0.5, v >= 0.5);
                self.sostenuto_pedal = v;
                if is && !was {
                    // Holds up the dampers of the keys down right now.
                    self.sostenuto = self.keys_down;
                } else if was && !is {
                    self.sostenuto = 0;
                }
            }
            67 => self.soft = v,
            120 => self.all_notes_off(),
            123 => {
                for note in 0..128u8 {
                    self.note_off(note);
                }
            }
            _ => {}
        }
    }

    fn all_notes_off(&mut self) {
        for v in self.voices.iter_mut() {
            v.kill();
        }
        self.keys_down = 0;
        self.sostenuto = 0;
        self.resonance.clear();
        self.noises.clear();
        self.body.clear();
        self.idle = u32::MAX;
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let total = left.len().min(right.len());
        let mut off = 0;
        while off < total {
            let n = (total - off).min(BLOCK);
            self.block(&mut left[off..off + n], &mut right[off..off + n]);
            off += n;
        }
    }
}
