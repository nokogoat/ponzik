//! Modulators (Bitwig style): an LFO, an envelope follower, a step sequencer
//! or a random source, living on a track (targets: its inserts, rack macros
//! and rack devices included) or on a channel (targets: its instrument).
//!
//! Each modulator drives any number of parameters, each with its own bipolar
//! depth: the parameter moves around its *base* value (the knob, the
//! automation) by `depth × output`, in positions of its range. Modulation is
//! evaluated every [`MOD_CHUNK`] samples, with a short slew so stepped sources
//! do not click, and never allocates: the interface builds the whole
//! [`ModSet`] and the engine swaps it in.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use ponzik_dsp::noise::WhiteNoise;
use ponzik_dsp::util::{db_to_gain, decay_coef};

use crate::effects::{DIVISIONS, division_beats};
use crate::param::{ParamInfo, Params, Unit};

/// Samples between two modulation updates of a track.
pub const MOD_CHUNK: usize = 64;
/// Modulators on one track or channel.
pub const MAX_MODULATORS: usize = 8;
/// Steps of the step sequencer.
pub const MAX_STEPS: usize = 16;
/// Slew of the modulation offset, in seconds.
const SLEW_SECONDS: f32 = 0.004;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ModKind {
    Lfo,
    Follower,
    Steps,
    Random,
}

pub const LFO_SHAPES: &[&str] = &["Sine", "Triangle", "Saw Up", "Saw Down", "Square", "Sample & Hold"];
/// Step lengths of the step sequencer.
pub const STEP_RATES: &[&str] = &["1/32", "1/16", "1/8", "1/4", "1/2", "1 bar"];
const STEP_BEATS: [f64; 6] = [0.125, 0.25, 0.5, 1.0, 2.0, 4.0];
pub const STEP_COUNTS: &[&str] = &["4", "8", "12", "16"];
const STEP_COUNT: [usize; 4] = [4, 8, 12, 16];
pub const RANDOM_MODES: &[&str] = &["Smooth", "Stepped"];

// LFO.
pub const LFO_SHAPE: u32 = 0;
pub const LFO_RATE: u32 = 1;
pub const LFO_SYNC: u32 = 2;
pub const LFO_PHASE: u32 = 3;
pub const LFO_DEPTH: u32 = 4;
// Envelope follower.
pub const FOL_ATTACK: u32 = 0;
pub const FOL_RELEASE: u32 = 1;
pub const FOL_GAIN: u32 = 2;
pub const FOL_DEPTH: u32 = 3;
// Step sequencer; steps from `STEP_FIRST`.
pub const STEP_COUNT_P: u32 = 0;
pub const STEP_RATE: u32 = 1;
pub const STEP_GLIDE: u32 = 2;
pub const STEP_DEPTH: u32 = 3;
pub const STEP_FIRST: u32 = 4;
// Random.
pub const RND_RATE: u32 = 0;
pub const RND_SYNC: u32 = 1;
pub const RND_MODE: u32 = 2;
pub const RND_DEPTH: u32 = 3;

static LFO_PARAMS: [ParamInfo; 5] = [
    ParamInfo::choice(LFO_SHAPE, "Shape", LFO_SHAPES, 0),
    ParamInfo::new(LFO_RATE, "Rate", 0.02, 40.0, 1.0, Unit::Hz).log(),
    ParamInfo::choice(LFO_SYNC, "Sync", DIVISIONS, 0),
    ParamInfo::new(LFO_PHASE, "Phase", 0.0, 1.0, 0.0, Unit::Percent),
    ParamInfo::new(LFO_DEPTH, "Depth", 0.0, 1.0, 1.0, Unit::Percent),
];

static FOLLOWER_PARAMS: [ParamInfo; 4] = [
    ParamInfo::new(FOL_ATTACK, "Attack", 0.001, 1.0, 0.01, Unit::Seconds).log(),
    ParamInfo::new(FOL_RELEASE, "Release", 0.01, 5.0, 0.25, Unit::Seconds).log(),
    ParamInfo::new(FOL_GAIN, "Gain", -24.0, 24.0, 0.0, Unit::Db),
    ParamInfo::new(FOL_DEPTH, "Depth", 0.0, 1.0, 1.0, Unit::Percent),
];

const fn step(i: u32, name: &'static str, default: f32) -> ParamInfo {
    ParamInfo::new(STEP_FIRST + i, name, -1.0, 1.0, default, Unit::Percent).group("Steps")
}

static STEP_PARAMS: [ParamInfo; 4 + MAX_STEPS] = [
    ParamInfo::choice(STEP_COUNT_P, "Steps", STEP_COUNTS, 1),
    ParamInfo::choice(STEP_RATE, "Rate", STEP_RATES, 1),
    ParamInfo::new(STEP_GLIDE, "Glide", 0.0, 1.0, 0.0, Unit::Percent),
    ParamInfo::new(STEP_DEPTH, "Depth", 0.0, 1.0, 1.0, Unit::Percent),
    step(0, "Step 1", 1.0),
    step(1, "Step 2", -0.5),
    step(2, "Step 3", 0.5),
    step(3, "Step 4", -1.0),
    step(4, "Step 5", 0.75),
    step(5, "Step 6", 0.0),
    step(6, "Step 7", 0.25),
    step(7, "Step 8", -0.25),
    step(8, "Step 9", 1.0),
    step(9, "Step 10", -0.5),
    step(10, "Step 11", 0.5),
    step(11, "Step 12", -1.0),
    step(12, "Step 13", 0.75),
    step(13, "Step 14", 0.0),
    step(14, "Step 15", 0.25),
    step(15, "Step 16", -0.25),
];

static RANDOM_PARAMS: [ParamInfo; 4] = [
    ParamInfo::new(RND_RATE, "Rate", 0.05, 40.0, 2.0, Unit::Hz).log(),
    ParamInfo::choice(RND_SYNC, "Sync", DIVISIONS, 0),
    ParamInfo::choice(RND_MODE, "Mode", RANDOM_MODES, 0),
    ParamInfo::new(RND_DEPTH, "Depth", 0.0, 1.0, 1.0, Unit::Percent),
];

/// Largest parameter list of a modulator.
const MAX_PARAMS: usize = STEP_PARAMS.len();

impl ModKind {
    pub const ALL: [ModKind; 4] = [ModKind::Lfo, ModKind::Follower, ModKind::Steps, ModKind::Random];

    /// Stable id (project files).
    pub fn id(self) -> &'static str {
        match self {
            ModKind::Lfo => "lfo",
            ModKind::Follower => "follower",
            ModKind::Steps => "steps",
            ModKind::Random => "random",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.id() == id)
    }

    /// English name, translated where shown.
    pub fn name(self) -> &'static str {
        match self {
            ModKind::Lfo => "LFO",
            ModKind::Follower => "Envelope Follower",
            ModKind::Steps => "Step Sequencer",
            ModKind::Random => "Random",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            ModKind::Lfo => "Repeating wave, free or synced to the tempo",
            ModKind::Follower => "Follows the loudness of the track",
            ModKind::Steps => "A synced pattern of up to 16 values",
            ModKind::Random => "Random values, smooth or stepped",
        }
    }

    pub fn params(self) -> &'static [ParamInfo] {
        match self {
            ModKind::Lfo => &LFO_PARAMS,
            ModKind::Follower => &FOLLOWER_PARAMS,
            ModKind::Steps => &STEP_PARAMS,
            ModKind::Random => &RANDOM_PARAMS,
        }
    }

    /// Output in [-1, 1] (or [0, 1] for the follower).
    pub fn bipolar(self) -> bool {
        self != ModKind::Follower
    }
}

/// Where the transport is, for the synced modulators.
#[derive(Clone, Copy, Debug)]
pub struct ModClock {
    pub sample_rate: f32,
    pub bpm: f64,
    /// Song position at the start of the block, in quarter notes.
    pub beats: f64,
    pub playing: bool,
}

impl Default for ModClock {
    fn default() -> Self {
        Self { sample_rate: 48_000.0, bpm: 120.0, beats: 0.0, playing: false }
    }
}

impl ModClock {
    /// The same clock `frames` samples later.
    pub fn after(&self, frames: usize) -> Self {
        let beats = if self.playing { self.beats + self.beats_per_sample() * frames as f64 } else { self.beats };
        Self { beats, ..*self }
    }

    fn beats_per_sample(&self) -> f64 {
        self.bpm / 60.0 / self.sample_rate.max(1.0) as f64
    }
}

/// What the envelope follower listens to.
pub enum ModInput<'a> {
    Audio(&'a [f32], &'a [f32]),
    /// A level held over the chunk (a channel's last block).
    Level(f32),
}

pub struct Modulator {
    pub kind: ModKind,
    values: [f32; MAX_PARAMS],
    /// Running phase, in cycles (steps for the step sequencer).
    phase: f64,
    env: f32,
    noise: WhiteNoise,
    /// Random: last cycle drawn, previous and next value.
    cycle: i64,
    from: f32,
    to: f32,
    /// Output of the last chunk.
    value: f32,
}

impl Modulator {
    pub fn new(kind: ModKind) -> Self {
        let mut values = [0.0; MAX_PARAMS];
        for p in kind.params() {
            values[p.id as usize] = p.default;
        }
        Self { kind, values, phase: 0.0, env: 0.0, noise: WhiteNoise::new(0x5eed), cycle: i64::MIN, from: 0.0, to: 0.0, value: 0.0 }
    }

    pub fn with_seed(mut self, seed: u32) -> Self {
        self.noise = WhiteNoise::new(seed.wrapping_mul(2_654_435_761).max(1));
        self
    }

    /// Output of the last update.
    pub fn value(&self) -> f32 {
        self.value
    }

    fn v(&self, id: u32) -> f32 {
        self.values[id as usize]
    }

    /// Carries the running state of `old` over (same kind).
    fn inherit(&mut self, old: &Modulator) {
        if old.kind == self.kind {
            self.phase = old.phase;
            self.env = old.env;
            self.noise = old.noise.clone();
            self.cycle = old.cycle;
            self.from = old.from;
            self.to = old.to;
            self.value = old.value;
        }
    }

    /// Advances by `frames` samples starting at `clock`; returns the output.
    pub fn tick(&mut self, clock: &ModClock, frames: usize, input: &ModInput) -> f32 {
        let sr = clock.sample_rate.max(1.0) as f64;
        let raw = match self.kind {
            ModKind::Lfo => {
                let pos = self.cycles(clock, frames, self.v(LFO_RATE) as f64, division_beats(self.v(LFO_SYNC)));
                let t = (pos + self.v(LFO_PHASE) as f64).rem_euclid(1.0) as f32;
                match self.v(LFO_SHAPE).round() as usize {
                    0 => (t * std::f32::consts::TAU).sin(),
                    1 => 4.0 * ((t - 0.25).rem_euclid(1.0) - 0.5).abs() - 1.0,
                    2 => 2.0 * t - 1.0,
                    3 => 1.0 - 2.0 * t,
                    4 => {
                        if t < 0.5 {
                            1.0
                        } else {
                            -1.0
                        }
                    }
                    _ => self.held(pos.floor() as i64),
                }
            }
            ModKind::Follower => {
                let attack = decay_coef(self.v(FOL_ATTACK), sr as f32);
                let release = decay_coef(self.v(FOL_RELEASE), sr as f32);
                let gain = db_to_gain(self.v(FOL_GAIN));
                let mut env = self.env;
                let mut follow = |x: f32| {
                    let x = x * gain;
                    let c = if x > env { attack } else { release };
                    env = x + (env - x) * c;
                };
                match input {
                    ModInput::Audio(l, r) => l.iter().zip(r.iter()).for_each(|(a, b)| follow(a.abs().max(b.abs()))),
                    ModInput::Level(x) => {
                        // Closed form of `frames` identical samples.
                        let x = x * gain;
                        let c = if x > env { attack } else { release };
                        env = x + (env - x) * c.powi(frames as i32);
                    }
                }
                self.env = ponzik_dsp::util::flush_denormal(env);
                self.env.min(1.0)
            }
            ModKind::Steps => {
                let count = STEP_COUNT[(self.v(STEP_COUNT_P).round() as usize).min(STEP_COUNT.len() - 1)];
                let beats = STEP_BEATS[(self.v(STEP_RATE).round() as usize).min(STEP_BEATS.len() - 1)];
                let pos = self.cycles(clock, frames, 0.0, Some(beats as f32));
                let idx = pos.floor().rem_euclid(count as f64) as usize;
                let frac = pos.rem_euclid(1.0) as f32;
                let cur = self.v(STEP_FIRST + idx as u32);
                let next = self.v(STEP_FIRST + ((idx + 1) % count) as u32);
                let glide = self.v(STEP_GLIDE);
                let k = if glide <= 0.0 { 0.0 } else { ((frac - (1.0 - glide)) / glide).clamp(0.0, 1.0) };
                cur + (next - cur) * k
            }
            ModKind::Random => {
                let pos = self.cycles(clock, frames, self.v(RND_RATE) as f64, division_beats(self.v(RND_SYNC)));
                let cycle = pos.floor() as i64;
                if cycle != self.cycle {
                    // A jump (locate) draws afresh; one cycle ahead glides on.
                    self.from = if cycle == self.cycle.wrapping_add(1) { self.to } else { self.noise.next_sample() };
                    self.to = self.noise.next_sample();
                    self.cycle = cycle;
                }
                if self.v(RND_MODE) >= 0.5 {
                    self.from
                } else {
                    let t = pos.rem_euclid(1.0) as f32;
                    let k = 0.5 - 0.5 * (t * std::f32::consts::PI).cos();
                    self.from + (self.to - self.from) * k
                }
            }
        };
        let depth = match self.kind {
            ModKind::Lfo => self.v(LFO_DEPTH),
            ModKind::Follower => self.v(FOL_DEPTH),
            ModKind::Steps => self.v(STEP_DEPTH),
            ModKind::Random => self.v(RND_DEPTH),
        };
        self.value = (raw * depth).clamp(-1.0, 1.0);
        self.value
    }

    /// Position in cycles at the end of the chunk: from the song position when
    /// synced and playing, otherwise free running at `hz` (or the synced rate).
    fn cycles(&mut self, clock: &ModClock, frames: usize, hz: f64, sync: Option<f32>) -> f64 {
        let sr = clock.sample_rate.max(1.0) as f64;
        match sync {
            Some(beats) if clock.playing => {
                self.phase = clock.after(frames).beats / beats.max(1e-3) as f64;
            }
            Some(beats) => self.phase += frames as f64 * clock.bpm / 60.0 / sr / beats.max(1e-3) as f64,
            None => self.phase += frames as f64 * hz / sr,
        }
        // Free running: keep the phase small; a synced phase follows the song.
        if !(sync.is_some() && clock.playing) && self.phase > 1e6 {
            self.phase = self.phase.rem_euclid(1.0);
        }
        self.phase
    }

    /// Sample & hold value of cycle `cycle`.
    fn held(&mut self, cycle: i64) -> f32 {
        if cycle != self.cycle {
            self.cycle = cycle;
            self.from = self.noise.next_sample();
        }
        self.from
    }
}

impl Params for Modulator {
    fn params(&self) -> &'static [ParamInfo] {
        self.kind.params()
    }

    fn get_param(&self, id: u32) -> f32 {
        self.values.get(id as usize).copied().unwrap_or(0.0)
    }

    fn set_param(&mut self, id: u32, value: f32) {
        if let Some(info) = self.param_info(id) {
            self.values[id as usize] = info.clamp(value);
        }
    }
}

/// A modulated parameter: where it is and what it would be without modulation.
#[derive(Clone, Copy, Debug)]
pub struct ModParam {
    /// Insert slot (track modulators); 0 for a channel's instrument.
    pub slot: usize,
    pub id: u32,
    pub info: ParamInfo,
    /// Value set by the knob or the automation.
    pub base: f32,
    offset: f32,
    smooth: f32,
    last: f32,
}

impl ModParam {
    pub fn new(slot: usize, id: u32, info: ParamInfo, base: f32) -> Self {
        Self { slot, id, info, base, offset: 0.0, smooth: f32::NAN, last: f32::NAN }
    }
}

/// One modulator → one parameter, with a bipolar depth (-1..1, in positions
/// of the parameter's range).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModLink {
    pub modulator: usize,
    pub param: usize,
    pub depth: f32,
}

/// Outputs of the modulators, read by the interface (moving rings, live trace).
#[derive(Debug, Default)]
pub struct ModMeters {
    values: [AtomicU32; MAX_MODULATORS],
}

impl ModMeters {
    pub fn get(&self, modulator: usize) -> f32 {
        self.values.get(modulator).map_or(0.0, |v| f32::from_bits(v.load(Ordering::Relaxed)))
    }

    fn set(&self, modulator: usize, v: f32) {
        if let Some(cell) = self.values.get(modulator) {
            cell.store(v.to_bits(), Ordering::Relaxed);
        }
    }
}

/// The modulators of a track or a channel, their targets and links.
pub struct ModSet {
    pub mods: Vec<Modulator>,
    pub params: Vec<ModParam>,
    pub links: Vec<ModLink>,
    pub meters: Arc<ModMeters>,
}

/// Value of a parameter at `base` moved by `offset` positions of its range.
pub fn modulated(info: &ParamInfo, base: f32, offset: f32) -> f32 {
    info.from_norm((info.to_norm(base) + offset).clamp(0.0, 1.0))
}

impl ModSet {
    pub fn new(mods: Vec<Modulator>, params: Vec<ModParam>, links: Vec<ModLink>, meters: Arc<ModMeters>) -> Self {
        let mut mods = mods;
        mods.truncate(MAX_MODULATORS);
        let (m, p) = (mods.len(), params.len());
        let links = links.into_iter().filter(|l| l.modulator < m && l.param < p).collect();
        Self { mods, params, links, meters }
    }

    /// Carries the running state (phases, envelopes, smoothing) of `old` over.
    pub fn inherit(&mut self, old: &ModSet) {
        for (new, old) in self.mods.iter_mut().zip(&old.mods) {
            new.inherit(old);
        }
        for p in &mut self.params {
            if let Some(o) = old.params.iter().find(|o| o.slot == p.slot && o.id == p.id) {
                p.smooth = o.smooth;
            }
        }
    }

    /// The knob or the automation sets a parameter: if it is modulated, its
    /// base moves (applied at the next update) and `true` is returned.
    pub fn set_base(&mut self, slot: usize, id: u32, value: f32) -> bool {
        let mut hit = false;
        for p in self.params.iter_mut().filter(|p| p.slot == slot && p.id == id) {
            p.base = p.info.clamp(value);
            hit = true;
        }
        hit
    }

    pub fn base(&self, slot: usize, id: u32) -> Option<f32> {
        self.params.iter().find(|p| p.slot == slot && p.id == id).map(|p| p.base)
    }

    pub fn is_empty(&self) -> bool {
        self.mods.is_empty()
    }

    /// Advances the modulators by `frames` samples and sets every modulated
    /// parameter whose value changed through `set(slot, id, value)`.
    pub fn run(&mut self, clock: &ModClock, frames: usize, input: &ModInput, mut set: impl FnMut(usize, u32, f32)) {
        for (i, m) in self.mods.iter_mut().enumerate() {
            let v = m.tick(clock, frames, input);
            self.meters.set(i, v);
        }
        for p in &mut self.params {
            p.offset = 0.0;
        }
        for l in &self.links {
            self.params[l.param].offset += l.depth * self.mods[l.modulator].value;
        }
        let slew = 1.0 - decay_coef(SLEW_SECONDS, clock.sample_rate.max(1.0) / frames.max(1) as f32);
        for p in &mut self.params {
            p.smooth = if p.smooth.is_finite() { p.smooth + (p.offset - p.smooth) * slew } else { p.offset };
            let v = modulated(&p.info, p.base, p.smooth);
            if v != p.last {
                p.last = v;
                set(p.slot, p.id, v);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock(playing: bool) -> ModClock {
        ModClock { sample_rate: 48_000.0, bpm: 120.0, beats: 0.0, playing }
    }

    #[test]
    fn depth_is_bipolar_around_the_base() {
        let lin = ParamInfo::new(0, "Mix", 0.0, 1.0, 0.5, Unit::Percent);
        assert!((modulated(&lin, 0.5, 0.25) - 0.75).abs() < 1e-6);
        assert!((modulated(&lin, 0.5, -0.25) - 0.25).abs() < 1e-6);
        // Clamped to the range.
        assert_eq!(modulated(&lin, 0.9, 0.5), 1.0);
        assert_eq!(modulated(&lin, 0.1, -0.5), 0.0);
        // Logarithmic parameters move in knob positions, not in hertz.
        let hz = ParamInfo::new(0, "Cutoff", 20.0, 20_000.0, 1_000.0, Unit::Hz).log();
        let mid = hz.from_norm(0.5);
        assert!((hz.to_norm(modulated(&hz, mid, 0.25)) - 0.75).abs() < 1e-4);

        // Two modulators on one parameter add up; a negative depth inverts.
        let meters = Arc::new(ModMeters::default());
        let mut a = Modulator::new(ModKind::Steps);
        a.set_param(STEP_GLIDE, 0.0);
        for s in 0..MAX_STEPS as u32 {
            a.set_param(STEP_FIRST + s, 1.0);
        }
        let b = {
            let mut b = Modulator::new(ModKind::Steps);
            for s in 0..MAX_STEPS as u32 {
                b.set_param(STEP_FIRST + s, 0.5);
            }
            b
        };
        let params = vec![ModParam::new(2, 7, lin, 0.5)];
        let links = vec![
            ModLink { modulator: 0, param: 0, depth: 0.25 },
            ModLink { modulator: 1, param: 0, depth: -0.2 },
        ];
        let mut set = ModSet::new(vec![a, b], params, links, meters.clone());
        let mut got = Vec::new();
        set.run(&clock(false), 64, &ModInput::Level(0.0), |slot, id, v| got.push((slot, id, v)));
        assert_eq!(got.len(), 1);
        let (slot, id, v) = got[0];
        assert_eq!((slot, id), (2, 7));
        // 0.5 + 0.25 × 1 − 0.2 × 0.5 = 0.65.
        assert!((v - 0.65).abs() < 1e-5, "{v}");
        assert!((meters.get(0) - 1.0).abs() < 1e-6 && (meters.get(1) - 0.5).abs() < 1e-6);
        // Unchanged: nothing is sent again.
        got.clear();
        set.run(&clock(false), 64, &ModInput::Level(0.0), |slot, id, v| got.push((slot, id, v)));
        assert!(got.is_empty());
        // The knob moves the base.
        assert!(set.set_base(2, 7, 0.2));
        assert!(!set.set_base(2, 8, 0.2));
        set.run(&clock(false), 64, &ModInput::Level(0.0), |_, _, v| got.push((0, 0, v)));
        assert!((got[0].2 - 0.35).abs() < 1e-5);
    }

    #[test]
    fn lfo_follows_the_song_when_synced() {
        let mut lfo = Modulator::new(ModKind::Lfo);
        lfo.set_param(LFO_SYNC, 11.0); // 1/4
        lfo.set_param(LFO_SHAPE, 2.0); // saw up
        let c = ModClock { beats: 2.25, ..clock(true) };
        // End of the chunk lands a quarter into the beat.
        let v = lfo.tick(&c, 0, &ModInput::Level(0.0));
        assert!((v - (-0.5)).abs() < 1e-5, "{v}");
        // Free: one hertz advances a quarter cycle in 12 000 samples.
        let mut free = Modulator::new(ModKind::Lfo);
        free.set_param(LFO_RATE, 1.0);
        let v = free.tick(&clock(false), 12_000, &ModInput::Level(0.0));
        assert!((v - 1.0).abs() < 1e-5, "{v}");
        // Depth scales the output.
        free.set_param(LFO_DEPTH, 0.5);
        let v = free.tick(&clock(false), 24_000, &ModInput::Level(0.0));
        assert!((v + 0.5).abs() < 1e-4, "{v}");
    }

    #[test]
    fn steps_and_random_stay_in_range() {
        let mut steps = Modulator::new(ModKind::Steps);
        steps.set_param(STEP_COUNT_P, 0.0); // 4 steps
        steps.set_param(STEP_RATE, 3.0); // quarter notes
        let at = |beats: f64| ModClock { beats, ..clock(true) };
        assert_eq!(steps.tick(&at(0.5), 0, &ModInput::Level(0.0)), 1.0);
        assert_eq!(steps.tick(&at(1.5), 0, &ModInput::Level(0.0)), -0.5);
        assert_eq!(steps.tick(&at(4.5), 0, &ModInput::Level(0.0)), 1.0);
        let mut rnd = Modulator::new(ModKind::Random).with_seed(3);
        for mode in [0.0, 1.0] {
            rnd.set_param(RND_MODE, mode);
            for _ in 0..1_000 {
                assert!(rnd.tick(&clock(false), 64, &ModInput::Level(0.0)).abs() <= 1.0);
            }
        }
    }

    #[test]
    fn follower_tracks_the_level() {
        let mut f = Modulator::new(ModKind::Follower);
        let loud = vec![0.8f32; 64];
        let mut v = 0.0;
        for _ in 0..100 {
            v = f.tick(&clock(false), 64, &ModInput::Audio(&loud, &loud));
        }
        assert!((v - 0.8).abs() < 0.01, "{v}");
        for _ in 0..2_000 {
            v = f.tick(&clock(false), 64, &ModInput::Level(0.0));
        }
        assert!(v < 0.01);
    }
}
