//! What bowed and blown instruments share: voices made of one or several
//! players (a section), each running its own physical model; the bow or
//! breath envelope and its articulations; dynamics from the velocity or the
//! mod wheel / breath controller; delayed vibrato; legato glides; scoops
//! and falls; growl. The instruments (Strings, Brass, Winds) provide the
//! model and the post-processing (bodies, mutes).

use ponzik_dsp::util::{decay_coef, midi_to_hz};

use super::wg::{Controls, Player, Rng, spread_players, vibrato_depth};
use crate::instruments::common::{Expr, fast_sin, pan_gains, pick_voice};
use crate::{Controller, NoteExpression, Ramp};

pub(crate) const VOICES: usize = 8;
pub(crate) const MAX_PLAYERS: usize = 6;
/// Control rate: pitch and filters are updated every `CHUNK` samples.
pub(crate) const CHUNK: usize = 16;
/// Output buses (one per instrument body).
pub(crate) const BUSES: usize = 4;
const STACK: usize = 16;
const SILENCE: f32 = 1e-4;

/// A physical model played by one player.
pub(crate) trait Model: Send {
    /// Instrument-wide settings the model reads.
    type Cfg;
    /// Lines long enough for `min_hz`.
    fn new(sample_rate: f32, min_hz: f32, seed: u32) -> Self;
    fn reset(&mut self);
    /// The note begins for this player (`pluck`: the articulation excites
    /// the model once instead of driving it).
    fn attack(&mut self, cfg: &Self::Cfg, velocity: f32, pluck: bool);
    /// Pitch and dynamics for the next `n` samples (`fresh`: jump there).
    fn tune(&mut self, cfg: &Self::Cfg, hz: f32, dynamics: f32, released: bool, n: usize, fresh: bool);
    /// One sample; `drive` is the bow velocity or breath pressure in
    /// [−1, 1] (negative: the bow goes up), `noise` white in [−1, 1].
    fn tick(&mut self, cfg: &Self::Cfg, drive: f32, noise: f32) -> f32;
}

/// How the envelope behaves.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Articulation {
    Sustain,
    /// Short, accented, ends by itself.
    Staccato,
    /// No drive: the model is excited once and rings.
    Pluck,
    /// The drive flips sign at this rate (bow strokes per second).
    Tremolo(f32),
}

/// Settings of the section, set by the instrument from its parameters.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SectionCfg {
    pub articulation: Articulation,
    pub attack: f32,
    pub release: f32,
    /// Vibrato depth (cents), rate (Hz), delay (s).
    pub vib_depth: f32,
    pub vib_rate: f32,
    pub vib_delay: f32,
    pub legato: bool,
    pub portamento: f32,
    pub players: usize,
    pub detune: f32,
    pub timing: f32,
    pub width: f32,
    /// Semitones dropped by a fall-off after the note-off.
    pub fall: f32,
    /// Semitones a note scoops up from at its start.
    pub scoop: f32,
    /// Growl (flutter and roughness of the drive), [0, 1].
    pub growl: f32,
    /// Notes below each of these go to buses 3, 2, 1; `None`: all on `bus`.
    pub split: Option<[u8; 3]>,
    pub bus: usize,
    pub min_hz: f32,
    pub max_hz: f32,
}

impl Default for SectionCfg {
    fn default() -> Self {
        Self {
            articulation: Articulation::Sustain,
            attack: 0.1,
            release: 0.3,
            vib_depth: 15.0,
            vib_rate: 5.5,
            vib_delay: 0.3,
            legato: false,
            portamento: 0.08,
            players: 1,
            detune: 8.0,
            timing: 0.02,
            width: 0.6,
            fall: 0.0,
            scoop: 0.0,
            growl: 0.0,
            split: None,
            bus: 0,
            min_hz: 25.0,
            max_hz: 4_000.0,
        }
    }
}

struct Seat<M> {
    model: M,
    spread: Player,
    /// Samples before this player joins.
    wait: u32,
    started: bool,
    rng: Rng,
    pan: (f32, f32),
}

struct Voice<M> {
    key: u8,
    started_at: u64,
    active: bool,
    released: bool,
    pedal: bool,
    pitch: Ramp,
    expr: Expr,
    velocity: f32,
    dynamics: f32,
    /// Envelope level and its target and step per sample.
    env: f32,
    env_target: f32,
    env_step: f32,
    /// Seconds since the note started and since its release.
    age: f32,
    since_release: f32,
    /// Bow direction phase (tremolo), turns.
    stroke: f32,
    /// Growl oscillator phase, turns.
    growl: f32,
    level: f32,
    fresh: bool,
    bus: usize,
    seats: Vec<Seat<M>>,
}

/// A polyphonic set of voices, each played by a section of players.
pub(crate) struct Section<M: Model> {
    pub cfg: SectionCfg,
    sample_rate: f32,
    voices: Vec<Voice<M>>,
    note_counter: u64,
    stack: [u8; STACK],
    n_stack: usize,
    pub ctl: Controls,
    rng: Rng,
}

impl<M: Model> Section<M> {
    pub fn new(seed: u32) -> Self {
        Self {
            cfg: SectionCfg::default(),
            sample_rate: 0.0,
            voices: Vec::new(),
            note_counter: 0,
            stack: [0; STACK],
            n_stack: 0,
            ctl: Controls::default(),
            rng: Rng::new(seed),
        }
    }

    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        if sample_rate != self.sample_rate {
            let min_hz = self.cfg.min_hz;
            self.voices = (0..VOICES)
                .map(|v| Voice {
                    key: 0,
                    started_at: 0,
                    active: false,
                    released: false,
                    pedal: false,
                    pitch: Ramp::new(60.0),
                    expr: Expr::default(),
                    velocity: 0.0,
                    dynamics: 0.0,
                    env: 0.0,
                    env_target: 0.0,
                    env_step: 0.0,
                    age: 0.0,
                    since_release: 0.0,
                    stroke: 0.0,
                    growl: 0.0,
                    level: 0.0,
                    fresh: true,
                    bus: 0,
                    seats: (0..MAX_PLAYERS)
                        .map(|p| {
                            let seed = 0x2545_F491 ^ ((v * MAX_PLAYERS + p) as u32).wrapping_mul(2_654_435_761);
                            Seat {
                                model: M::new(sample_rate, min_hz, seed),
                                spread: Player::default(),
                                wait: 0,
                                started: false,
                                rng: Rng::new(seed),
                                pan: (1.0, 1.0),
                            }
                        })
                        .collect(),
                })
                .collect();
        }
        self.sample_rate = sample_rate;
        self.all_notes_off();
    }

    pub fn any_active(&self) -> bool {
        self.voices.iter().any(|v| v.active)
    }

    /// Active voices: (key, envelope, dynamics, released), for the UI and tests.
    #[cfg(test)]
    pub fn active(&self) -> impl Iterator<Item = (u8, f32, f32, bool)> + '_ {
        self.voices.iter().filter(|v| v.active).map(|v| (v.key, v.env, v.dynamics, v.released))
    }

    #[cfg(test)]
    pub fn pitch_of(&self, key: u8) -> Option<f32> {
        self.voices.iter().find(|v| v.active && v.key == key).map(|v| v.pitch.value())
    }

    pub fn all_notes_off(&mut self) {
        for v in &mut self.voices {
            v.active = false;
            v.seats.iter_mut().for_each(|s| s.model.reset());
        }
        self.n_stack = 0;
    }

    fn bus_of(&self, note: u8) -> usize {
        match self.cfg.split {
            Some([a, b, c]) => {
                if note < a {
                    3
                } else if note < b {
                    2
                } else if note < c {
                    1
                } else {
                    0
                }
            }
            None => self.cfg.bus.min(BUSES - 1),
        }
    }

    fn dynamics_of(&self, velocity: f32) -> f32 {
        self.ctl.wheel_dynamics().unwrap_or(velocity)
    }

    fn start(&mut self, note: u8, velocity: f32) {
        let same = self.voices.iter().position(|v| v.active && v.key == note);
        let idx = same.unwrap_or_else(|| {
            pick_voice(VOICES, |i| self.voices[i].active, |i| {
                let v = &self.voices[i];
                v.started_at + if v.released { 0 } else { 1 << 40 }
            })
        });
        self.note_counter += 1;
        let cfg = self.cfg;
        let sr = self.sample_rate;
        let mut spread = [Player::default(); MAX_PLAYERS];
        spread_players(&mut spread, cfg.players, cfg.detune, cfg.timing, cfg.width, &mut self.rng);
        let players = cfg.players.clamp(1, MAX_PLAYERS);
        let dynamics = self.dynamics_of(velocity);
        let bus = self.bus_of(note);
        // A lone player sits by register, a section spreads out.
        let solo_pan = if players == 1 { ((note as f32 - 64.0) / 40.0 * cfg.width).clamp(-0.6, 0.6) } else { 0.0 };
        let v = &mut self.voices[idx];
        let keep = same.is_some() && v.env > 0.05;
        v.key = note;
        v.started_at = self.note_counter;
        v.active = true;
        v.released = false;
        v.pedal = false;
        v.pitch = Ramp::new(note as f32);
        v.expr.reset();
        v.velocity = velocity;
        v.dynamics = dynamics;
        v.age = 0.0;
        v.since_release = 0.0;
        v.stroke = 0.0;
        v.bus = bus;
        if !keep {
            v.env = 0.0;
            v.fresh = true;
            v.level = 1.0;
        }
        for (k, seat) in v.seats.iter_mut().enumerate() {
            if k >= players {
                seat.started = false;
                seat.wait = u32::MAX;
                continue;
            }
            seat.spread = spread[k];
            seat.wait = (seat.spread.delay * sr) as u32;
            seat.started = false;
            if !keep {
                seat.model.reset();
            }
            let pan = if players == 1 { solo_pan } else { seat.spread.pan };
            let (l, r) = pan_gains(pan);
            // Equal loudness whatever the section size.
            let g = seat.spread.gain.max(0.5) / (players as f32).sqrt();
            seat.pan = (l * g, r * g);
        }
    }

    pub fn note_on(&mut self, note: u8, velocity: f32) {
        let velocity = if velocity.is_finite() { velocity.clamp(0.0, 1.0) } else { 0.0 };
        if self.cfg.legato && self.cfg.articulation != Articulation::Pluck {
            let held = self.n_stack > 0;
            if self.n_stack < STACK {
                self.stack[self.n_stack] = note;
                self.n_stack += 1;
            }
            if held && let Some(i) = self.newest_held() {
                self.glide(i, note);
                return;
            }
            for v in self.voices.iter_mut().filter(|v| v.active) {
                v.released = true;
            }
        }
        self.start(note, velocity);
    }

    fn newest_held(&self) -> Option<usize> {
        (0..self.voices.len()).filter(|&i| self.voices[i].active && !self.voices[i].released).max_by_key(|&i| self.voices[i].started_at)
    }

    fn glide(&mut self, i: usize, note: u8) {
        let t = self.cfg.portamento.max(0.03);
        let sr = self.sample_rate;
        let v = &mut self.voices[i];
        v.key = note;
        v.pitch.go(note as f32, t, sr);
    }

    fn release(&mut self, key: u8) {
        let sustain = self.ctl.sustain;
        for v in self.voices.iter_mut().filter(|v| v.active && !v.released && v.key == key) {
            if sustain {
                v.pedal = true;
            } else {
                v.released = true;
            }
        }
    }

    pub fn note_off(&mut self, note: u8) {
        if self.cfg.legato && self.n_stack > 0 {
            let Some(k) = self.stack[..self.n_stack].iter().position(|&n| n == note) else {
                return self.release(note);
            };
            let top = k + 1 == self.n_stack;
            self.stack.copy_within(k + 1..self.n_stack, k);
            self.n_stack -= 1;
            if self.n_stack == 0 {
                return self.release(note);
            }
            if top && let Some(i) = self.newest_held() {
                let back = self.stack[self.n_stack - 1];
                self.glide(i, back);
            }
            return;
        }
        self.release(note);
    }

    pub fn note_expression(&mut self, key: u8, expr: NoteExpression) {
        let sr = self.sample_rate;
        if let Some(v) = self.voices.iter_mut().filter(|v| v.active && v.key == key).max_by_key(|v| v.started_at) {
            v.expr.apply(expr, sr);
        }
    }

    pub fn controller(&mut self, ctl: Controller) {
        if self.ctl.apply(ctl) {
            for v in self.voices.iter_mut().filter(|v| v.pedal) {
                v.pedal = false;
                v.released = true;
            }
        }
    }

    /// Renders `n` ≤ [`CHUNK`] samples, added to the buses.
    pub fn render_chunk(&mut self, cfg_model: &M::Cfg, n: usize, buses: &mut [[[f32; CHUNK]; 2]; BUSES]) {
        let sr = self.sample_rate;
        let cfg = self.cfg;
        let secs = n as f32 / sr;
        let wheel = self.ctl.wheel_dynamics();
        let dyn_coef = 1.0 - decay_coef(0.03, sr / n as f32);
        let level_coef = decay_coef(0.05, sr);
        let bend = self.ctl.bend;
        for v in self.voices.iter_mut().filter(|v| v.active) {
            // Envelope stage for this chunk.
            let (target, time) = if v.released {
                (0.0, cfg.release)
            } else {
                match cfg.articulation {
                    Articulation::Staccato if v.age > 0.07 => (0.0, 0.07),
                    Articulation::Staccato => (1.0, 0.012),
                    Articulation::Pluck => (0.0, 0.01),
                    _ => (1.0, cfg.attack * (1.3 - 0.6 * v.velocity)),
                }
            };
            v.env_target = target;
            if (target - v.env).abs() > 1e-6 {
                let full = 1.0 / (time.max(0.003) * sr);
                v.env_step = if target > v.env { full } else { -full };
            } else {
                v.env_step = 0.0;
            }
            if let Some(w) = wheel
                && cfg.articulation != Articulation::Pluck
            {
                v.dynamics += (w - v.dynamics) * dyn_coef;
            }
            v.pitch.skip(n as u32);
            let tuning = v.expr.tuning.skip(n as u32);
            let fall = if v.released && cfg.fall > 0.0 { -cfg.fall * (v.since_release / 0.35).min(1.5).powi(2) } else { 0.0 };
            let scoop = -cfg.scoop * (-v.age / 0.09).exp();
            let base = v.pitch.value() + tuning + bend + fall + scoop;
            let vib = cfg.vib_depth / 100.0 * vibrato_depth(v.age, cfg.vib_delay, 0.5);
            let fresh = std::mem::take(&mut v.fresh);
            for seat in v.seats.iter_mut().filter(|s| s.wait != u32::MAX) {
                if !seat.started {
                    if seat.wait as usize > n {
                        seat.wait -= n as u32;
                        continue;
                    }
                    seat.wait = 0;
                    seat.started = true;
                    seat.model.attack(cfg_model, v.velocity, cfg.articulation == Articulation::Pluck);
                }
                let p = &seat.spread;
                let t = v.age;
                let vibrato = vib * fast_sin(cfg.vib_rate * p.vib_rate * t + p.vib_phase);
                let drift = 0.03 * fast_sin(p.drift_rate * t + p.drift_phase) * (cfg.players > 1) as u8 as f32;
                let semis = base + p.cents / 100.0 + vibrato + drift;
                let hz = midi_to_hz(semis).clamp(cfg.min_hz, cfg.max_hz);
                seat.model.tune(cfg_model, hz, v.dynamics, v.released, n, fresh);
            }
            v.age += secs;
            if v.released {
                v.since_release += secs;
            }
            let [bus_l, bus_r] = &mut buses[v.bus];
            let stroke_step = match cfg.articulation {
                Articulation::Tremolo(rate) => rate * 0.5 / sr,
                _ => 0.0,
            };
            let growl_step = 27.0 / sr;
            for (out_l, out_r) in bus_l[..n].iter_mut().zip(bus_r[..n].iter_mut()) {
                v.env = if v.env_step > 0.0 {
                    (v.env + v.env_step).min(v.env_target)
                } else if v.env_step < 0.0 {
                    (v.env + v.env_step).max(v.env_target)
                } else {
                    v.env
                };
                let mut drive = v.env * v.dynamics.max(0.02);
                if stroke_step > 0.0 {
                    // Bow strokes: the direction flips with rounded edges.
                    v.stroke += stroke_step;
                    let s = fast_sin(v.stroke);
                    drive *= (3.0 * s).clamp(-1.0, 1.0);
                }
                if cfg.growl > 0.0 {
                    v.growl += growl_step;
                    drive *= 1.0 + cfg.growl * 0.45 * fast_sin(v.growl);
                }
                let (mut l, mut r) = (0.0, 0.0);
                let mut lvl = 0.0f32;
                for seat in v.seats.iter_mut().filter(|s| s.started) {
                    let noise = seat.rng.next();
                    let noise = if cfg.growl > 0.0 { noise * (1.0 + cfg.growl) } else { noise };
                    let y = seat.model.tick(cfg_model, drive, noise);
                    lvl = lvl.max(y.abs());
                    l += y * seat.pan.0;
                    r += y * seat.pan.1;
                }
                let (el, er) = pan_gains(v.expr.pan);
                *out_l += l * el;
                *out_r += r * er;
                v.level = (v.level * level_coef).max(lvl);
            }
            let started = v.seats.iter().any(|s| s.started);
            let waiting = v.seats.iter().any(|s| s.wait != u32::MAX && !s.started);
            let quiet = v.level < SILENCE && started && !waiting;
            let over = match cfg.articulation {
                Articulation::Pluck => quiet && v.age > 0.05,
                Articulation::Staccato => v.age > 0.07 && v.env <= 0.0 && quiet,
                _ => v.released && v.env <= 0.0 && quiet,
            };
            if over || (v.released && v.since_release > 30.0) {
                v.active = false;
            }
        }
    }
}
