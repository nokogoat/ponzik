//! Mallet instruments by modal synthesis: marimba, vibraphone, xylophone,
//! glockenspiel, celesta, kalimba, tubular bells and steel drum.
//!
//! Each note is the set of vibration modes of a bar, tine, tube or pan
//! note, at the frequency ratios of the real instrument: the marimba and
//! vibraphone bars are undercut so their second mode lands two octaves up
//! (4:1) and the third near 10:1, the xylophone's a twelfth up (3:1), the
//! glockenspiel's steel bars keep the untuned free-bar ratios (2.76, 5.40…),
//! the kalimba's tines a cantilever's (≈ 6), chimes the (2n+1)² series whose
//! upper modes make the strike note an octave below the fourth, and steel
//! pan notes are tuned to harmonics. Upper modes ring shorter than the
//! fundamental.
//!
//! The mallet is a contact pulse: soft yarn stays on the bar for
//! milliseconds and only rings the low modes, a hard mallet (or a hard
//! stroke) for a fraction of one and brings out the upper ones. Hitting the
//! middle of a bar leaves out the modes with a node there. Tuned tubes under
//! the marimba, vibes and xylophone resonate with the fundamental; the
//! vibraphone's motor turns discs in those tubes (the tremolo) and its
//! pedal lifts the damper bar (CC 64). Celesta and chimes have dampers too;
//! the others ring out.

use ponzik_dsp::filter::OnePole;
use ponzik_dsp::lfo::{Lfo, LfoShape};
use ponzik_dsp::modal::{FREE_FREE, Mode, Strike};
use ponzik_dsp::util::{db_to_gain, midi_to_hz};

use super::common::{pan_gains, pick_voice, soft_clip};
use crate::param::{ParamInfo, Params, Unit};
use crate::{Controller, Instrument};

const VOICES: usize = 32;
pub const MODES: usize = 6;
const SILENCE: f32 = 2e-5;

pub const MODELS: &[&str] =
    &["Marimba", "Vibraphone", "Xylophone", "Glockenspiel", "Celesta", "Kalimba", "Tubular Bells", "Steel Drum"];

/// Physical description of one instrument.
#[derive(Clone, Copy, Debug)]
pub struct Model {
    /// Mode frequencies relative to the played pitch.
    pub ratios: [f32; MODES],
    /// Mode amplitudes for a hit at the reference point.
    pub gains: [f32; MODES],
    /// Fundamental decay to −60 dB at C4, s, and how many semitones up it halves.
    pub t60: f32,
    pub halving: f32,
    /// Upper modes decay as ratio^−alpha faster.
    pub alpha: f32,
    /// Contact time of the softest and hardest mallet, s.
    pub contact: (f32, f32),
    /// Tuned resonator (tube, box) under the bar.
    pub resonator: bool,
    /// Note-off damps (unless the pedal is down).
    pub damper: bool,
    /// A bar: strike position matters.
    pub bar: bool,
}

const fn model(
    ratios: [f32; MODES],
    gains: [f32; MODES],
    t60: f32,
    halving: f32,
    alpha: f32,
    contact: (f32, f32),
    flags: (bool, bool, bool),
) -> Model {
    Model { ratios, gains, t60, halving, alpha, contact, resonator: flags.0, damper: flags.1, bar: flags.2 }
}

/// Chimes: modes ∝ (2n + 1)², n = 1…6, with the fourth an octave above the
/// strike note: f = note × 2 (2n + 1)² / 81.
const fn chime(n: f32) -> f32 {
    2.0 * (2.0 * n + 1.0) * (2.0 * n + 1.0) / 81.0
}

pub static MODEL_DATA: [Model; 8] = [
    // Marimba: 1 : 4 : 10 tuned rosewood bar, resonator tubes.
    model([1.0, 3.99, 10.06, 18.4, 0.0, 0.0], [1.0, 0.32, 0.1, 0.03, 0.0, 0.0], 1.6, 16.0, 0.9, (0.007, 0.0006), (true, false, true)),
    // Vibraphone: aluminium 1 : 4 : 10, long ring, tubes with motor discs, damper.
    model([1.0, 3.98, 9.85, 17.6, 0.0, 0.0], [1.0, 0.3, 0.08, 0.025, 0.0, 0.0], 7.0, 22.0, 0.6, (0.005, 0.0004), (true, true, true)),
    // Xylophone: 1 : 3 tuned bar, short and bright, tubes.
    model([1.0, 3.0, 6.1, 9.9, 0.0, 0.0], [1.0, 0.45, 0.2, 0.08, 0.0, 0.0], 1.1, 16.0, 0.8, (0.0025, 0.0002), (true, false, true)),
    // Glockenspiel: untuned steel bar (free-free beam ratios), rings long.
    model(
        [FREE_FREE[0], FREE_FREE[1], FREE_FREE[2], FREE_FREE[3], 0.0, 0.0],
        [1.0, 0.45, 0.22, 0.08, 0.0, 0.0],
        4.5,
        30.0,
        0.5,
        (0.0015, 0.000_15),
        (false, false, true),
    ),
    // Celesta: steel bars over wooden boxes, felt hammers, damper.
    model(
        [FREE_FREE[0], FREE_FREE[1], FREE_FREE[2], 0.0, 0.0, 0.0],
        [1.0, 0.12, 0.04, 0.0, 0.0, 0.0],
        2.8,
        24.0,
        1.0,
        (0.003, 0.0008),
        (true, true, true),
    ),
    // Kalimba: plucked steel tines (cantilever), a small box.
    model([1.0, 5.9, 17.3, 0.0, 0.0, 0.0], [1.0, 0.25, 0.06, 0.0, 0.0, 0.0], 2.4, 24.0, 0.8, (0.0012, 0.0002), (true, false, false)),
    // Tubular bells: (2n+1)² modes, the strike note an octave below the 4th.
    model(
        [chime(1.0), chime(2.0), chime(3.0), chime(4.0), chime(5.0), chime(6.0)],
        [0.04, 0.3, 0.7, 1.0, 0.8, 0.5],
        12.0,
        24.0,
        0.4,
        (0.003, 0.0004),
        (false, true, false),
    ),
    // Steel drum: pan notes tuned to the harmonics.
    model([1.0, 2.0, 3.0, 4.01, 5.02, 0.0], [1.0, 0.6, 0.35, 0.15, 0.07, 0.0], 1.8, 24.0, 0.7, (0.0025, 0.0006), (false, false, false)),
];

const P_MODEL: u32 = 0;
const P_HARDNESS: u32 = 1;
const P_POSITION: u32 = 2;
const P_DECAY: u32 = 3;
const P_RESONATOR: u32 = 4;
const P_DYNAMICS: u32 = 5;
const P_MOTOR_RATE: u32 = 6;
const P_MOTOR_DEPTH: u32 = 7;
const P_DAMPER: u32 = 8;
const P_WIDTH: u32 = 9;
const P_TONE: u32 = 10;
const P_VOLUME: u32 = 11;

static PARAMS: [ParamInfo; 12] = [
    ParamInfo::choice(P_MODEL, "Model", MODELS, 0).group("Instrument"),
    ParamInfo::new(P_HARDNESS, "Hardness", 0.0, 1.0, 0.5, Unit::Percent).group("Mallet"),
    ParamInfo::new(P_POSITION, "Strike Position", 0.0, 1.0, 0.25, Unit::Percent).group("Mallet"),
    ParamInfo::new(P_DECAY, "Decay", 0.25, 4.0, 1.0, Unit::Ratio).log().group("Instrument"),
    ParamInfo::new(P_RESONATOR, "Resonator", 0.0, 1.0, 0.7, Unit::Percent).group("Instrument"),
    ParamInfo::new(P_DYNAMICS, "Dynamics", 0.0, 1.0, 0.75, Unit::Percent).group("Mallet"),
    ParamInfo::new(P_MOTOR_RATE, "Motor Rate", 0.5, 10.0, 4.5, Unit::Hz).log().group("Motor"),
    ParamInfo::new(P_MOTOR_DEPTH, "Motor Depth", 0.0, 1.0, 0.0, Unit::Percent).group("Motor"),
    ParamInfo::new(P_DAMPER, "Damper", 0.03, 1.5, 0.15, Unit::Seconds).log().group("Motor"),
    ParamInfo::new(P_WIDTH, "Stereo Width", 0.0, 1.0, 0.5, Unit::Percent).group("Output"),
    ParamInfo::new(P_TONE, "Tone", -1.0, 1.0, 0.0, Unit::Percent).group("Output"),
    ParamInfo::new(P_VOLUME, "Volume", -40.0, 6.0, -6.0, Unit::Db).group("Output"),
];

/// Decay of the resonator tube, s.
const TUBE_T60: f32 = 0.3;

/// Fundamental decay of a model at a key, s (before the Decay setting).
pub fn fundamental_t60(m: &Model, note: u8) -> f32 {
    (m.t60 * (-(note as f32 - 60.0) / m.halving).exp2()).clamp(0.15, 40.0)
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
    key: bool,
    damped: bool,
    modes: [Mode; MODES],
    gain: [f32; MODES],
    t60: [f32; MODES],
    count: usize,
    strike: Strike,
    tube: Mode,
    tube_in: f32,
    pan: (f32, f32),
}

/// Mallet instruments.
pub struct Mallets {
    values: [f32; 12],
    sample_rate: f32,
    voices: [Voice; VOICES],
    note_counter: u64,
    pedal: bool,
    mod_wheel: f32,
    motor: Lfo,
    tone_lp: [OnePole; 2],
}

impl Default for Mallets {
    fn default() -> Self {
        Self::new()
    }
}

impl Mallets {
    pub fn new() -> Self {
        let mut m = Self {
            values: std::array::from_fn(|i| PARAMS[i].default),
            sample_rate: 48_000.0,
            voices: [Voice::default(); VOICES],
            note_counter: 0,
            pedal: false,
            mod_wheel: 0.0,
            motor: Lfo::new(LfoShape::Sine),
            tone_lp: [OnePole::new(1_500.0, 48_000.0), OnePole::new(1_500.0, 48_000.0)],
        };
        m.set_sample_rate(48_000.0);
        m
    }

    fn v(&self, id: u32) -> f32 {
        self.values[id as usize]
    }

    pub fn model(&self) -> &'static Model {
        &MODEL_DATA[(self.v(P_MODEL).round() as usize).min(MODEL_DATA.len() - 1)]
    }

    fn damp(&mut self, i: usize) {
        let t = self.v(P_DAMPER);
        let sr = self.sample_rate;
        let v = &mut self.voices[i];
        if v.damped {
            return;
        }
        v.damped = true;
        for k in 0..v.count {
            v.modes[k].set_decay((if k == 0 { t } else { t * 0.5 }).min(v.t60[k]), sr);
        }
        v.tube.set_decay(t.min(TUBE_T60), sr);
    }
}

impl Params for Mallets {
    fn params(&self) -> &'static [ParamInfo] {
        &PARAMS
    }

    fn get_param(&self, id: u32) -> f32 {
        self.values.get(id as usize).copied().unwrap_or(0.0)
    }

    fn set_param(&mut self, id: u32, value: f32) {
        let Some(info) = PARAMS.get(id as usize) else { return };
        self.values[id as usize] = info.clamp(value);
        if id == P_MOTOR_RATE {
            self.motor.set_rate(self.v(P_MOTOR_RATE), self.sample_rate);
        }
    }
}

impl Instrument for Mallets {
    fn kind(&self) -> &'static str {
        "mallets"
    }

    fn is_melodic(&self) -> bool {
        true
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.motor.set_rate(self.v(P_MOTOR_RATE), sample_rate);
        self.tone_lp = [OnePole::new(1_500.0, sample_rate), OnePole::new(1_500.0, sample_rate)];
        self.all_notes_off();
    }

    fn note_on(&mut self, note: u8, velocity: f32) {
        let note = note.min(127);
        let sr = self.sample_rate;
        let m = *self.model();
        let dyn_ = self.v(P_DYNAMICS);
        let vel = 1.0 - dyn_ + dyn_ * velocity.clamp(0.0, 1.0);
        let hard = self.v(P_HARDNESS);
        // Softest to hardest mallet; a hard stroke flattens the mallet less.
        let contact = (m.contact.0 * (m.contact.1 / m.contact.0).powf(hard)) * (1.8 - 1.3 * vel);
        let pos = self.v(P_POSITION);
        let decay = self.v(P_DECAY);
        let hz = midi_to_hz(note as f32);
        let t60 = fundamental_t60(&m, note) * decay;
        let width = self.v(P_WIDTH);
        let pan = (width * (note as f32 - 66.0) / 30.0).clamp(-1.0, 1.0);
        let resonator = if m.resonator { self.v(P_RESONATOR) } else { 0.0 };
        // The same bar struck again keeps ringing under the new hit.
        let same = (0..VOICES).find(|&i| self.voices[i].active && self.voices[i].note == note);
        let idx = same.unwrap_or_else(|| pick_voice(VOICES, |i| self.voices[i].active, |i| self.voices[i].started_at));
        self.note_counter += 1;
        let v = &mut self.voices[idx];
        if same.is_none() {
            *v = Voice::default();
        }
        v.note = note;
        v.started_at = self.note_counter;
        v.active = true;
        v.key = true;
        v.damped = false;
        v.pan = pan_gains(pan);
        // Reference mode for the fundamental's level: the strongest.
        let reference = (0..MODES).max_by(|&a, &b| m.gains[a].total_cmp(&m.gains[b])).unwrap_or(0);
        let comp = 1.0 / pulse_gain(hz * m.ratios[reference], contact).max(0.2);
        let mut count = 0;
        for k in 0..MODES {
            if m.gains[k] <= 0.0 {
                continue;
            }
            count = k + 1;
            let ratio = m.ratios[k] / m.ratios[reference];
            let mut g = m.gains[k] * vel * comp;
            if m.bar {
                // Middle hit: no antisymmetric modes (2nd, 4th…); edge: less fundamental.
                if k % 2 == 1 {
                    g *= 0.12 + 0.88 * pos;
                } else if k == 0 {
                    g *= 1.0 - 0.4 * pos;
                }
            }
            // Bars with a tube give part of their fundamental to it.
            if k == 0 && resonator > 0.0 {
                g *= 1.0 - 0.3 * resonator;
            }
            v.gain[k] = g;
            v.t60[k] = (t60 * ratio.max(0.25).powf(-m.alpha)).max(0.04);
            v.modes[k].tune(hz * m.ratios[k], v.t60[k], sr);
        }
        v.count = count;
        v.tube.tune(hz * m.ratios[0], TUBE_T60, sr);
        // A two-pole resonance peaks at 1 / (2 (1 − r)) = T60 · sr / 13.8.
        v.tube_in = resonator * 1.6 * 13.8 / (TUBE_T60 * sr);
        v.strike.hit(1.0, contact, sr);
    }

    fn note_off(&mut self, note: u8) {
        let damper = self.model().damper;
        for i in 0..VOICES {
            if self.voices[i].active && self.voices[i].key && self.voices[i].note == note {
                self.voices[i].key = false;
                if damper && !self.pedal {
                    self.damp(i);
                }
            }
        }
    }

    fn controller(&mut self, ctl: Controller) {
        match ctl {
            // Mod wheel: more motor tremolo.
            Controller::Cc(1, v) if v.is_finite() => self.mod_wheel = v.clamp(0.0, 1.0),
            Controller::Cc(64, v) => {
                let down = v >= 0.5;
                if self.pedal && !down && self.model().damper {
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
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        if !self.voices.iter().any(|v| v.active) {
            // Keep the motor turning.
            for _ in 0..left.len() {
                self.motor.advance();
            }
            return;
        }
        let depth = (self.v(P_MOTOR_DEPTH) + self.mod_wheel).min(1.0);
        let tilt = (self.v(P_TONE) * 1.5).exp2();
        let gain = db_to_gain(self.v(P_VOLUME)) * 0.5;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let motor = 1.0 - depth * (0.5 + 0.5 * self.motor.next_sample());
            let (mut ml, mut mr) = (0.0, 0.0);
            for v in self.voices.iter_mut().filter(|v| v.active) {
                let f = v.strike.next_sample();
                let mut x = 0.0;
                let mut fund = 0.0;
                for k in 0..v.count {
                    let y = v.modes[k].process(f * v.gain[k]);
                    if k == 0 {
                        fund = y;
                    }
                    x += y;
                }
                if v.tube_in > 0.0 {
                    x += v.tube.process(fund * v.tube_in) * motor;
                }
                ml += x * v.pan.0;
                mr += x * v.pan.1;
            }
            for (c, s) in [(0, ml), (1, mr)] {
                let lp = self.tone_lp[c].lowpass(s);
                let y = soft_clip((lp + (s - lp) * tilt) * gain);
                if c == 0 {
                    *l += y;
                } else {
                    *r += y;
                }
            }
        }
        for v in self.voices.iter_mut().filter(|v| v.active) {
            let e: f32 = v.modes[..v.count].iter().map(Mode::energy).sum::<f32>() + v.tube.energy();
            if !v.strike.is_active() && e < SILENCE {
                v.active = false;
            }
            v.modes.iter_mut().for_each(Mode::flush);
            v.tube.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::common::test_util::{render, stress};
    use super::*;

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

    fn play(model: usize, note: u8, vel: f32, hard: f32, frames: usize) -> Vec<f32> {
        let mut m = Mallets::new();
        m.set_param(P_MODEL, model as f32);
        m.set_param(P_HARDNESS, hard);
        m.set_param(P_POSITION, 1.0);
        m.set_param(P_RESONATOR, 0.0);
        m.set_param(P_WIDTH, 0.0);
        m.note_on(note, vel);
        render(&mut m, frames).0
    }

    /// Strongest frequency between `lo` and `hi` Hz (1 Hz steps around peaks).
    fn peak_freq(x: &[f32], lo: f32, hi: f32) -> f32 {
        let mut best = (0.0, lo);
        let mut f = lo;
        while f < hi {
            let a = mag(x, f);
            if a > best.0 {
                best = (a, f);
            }
            f *= 1.002;
        }
        best.1
    }

    #[test]
    fn mode_ratios_are_the_instruments() {
        // Measured on the output: the first partials of each model.
        let note = 48;
        let f0 = midi_to_hz(note as f32);
        for (model, expected) in [
            (0usize, &[1.0, 3.99, 10.06][..]),
            (1, &[1.0, 3.98, 9.85]),
            (2, &[1.0, 3.0, 6.1]),
            (3, &[1.0, 2.756, 5.404]),
            (5, &[1.0, 5.9]),
            (7, &[1.0, 2.0, 3.0]),
        ] {
            let x = play(model, note, 1.0, 1.0, 24_000);
            let x = &x[240..12_240];
            for &r in expected {
                let f = peak_freq(x, f0 * r * 0.97, f0 * r * 1.03);
                assert!((f / (f0 * r) - 1.0).abs() < 0.006, "{}: {r} → {}", MODELS[model], f / f0);
                assert!(mag(x, f) > 0.002, "{} mode {r} missing", MODELS[model]);
            }
        }
        // Chimes: the 4th, 5th and 6th modes near 2 : 3 : 4 of the note.
        let c = &MODEL_DATA[6].ratios;
        assert!((c[3] - 2.0).abs() < 1e-4 && (c[4] / c[3] - 1.494).abs() < 0.01 && (c[5] / c[3] - 2.086).abs() < 0.01);
    }

    #[test]
    fn decays_follow_the_register_and_the_instrument() {
        let t60 = |model: usize, note: u8| {
            let x = play(model, note, 0.7, 0.3, 72_000);
            let a = rms(&x[4_800..9_600]);
            let b = rms(&x[28_800..33_600]);
            -60.0 / (20.0 * (b / a).log10() / 0.5)
        };
        let (marimba_low, marimba_high) = (t60(0, 45), t60(0, 81));
        assert!(marimba_low > marimba_high, "{marimba_low} {marimba_high}");
        assert!((0.8..5.0).contains(&marimba_low), "marimba {marimba_low}");
        let vibes = t60(1, 60);
        assert!((4.0..12.0).contains(&vibes), "vibes {vibes}");
        let xylo = t60(2, 72);
        assert!((0.3..1.5).contains(&xylo), "xylophone {xylo}");
        assert!(vibes > 3.0 * t60(0, 60));
    }

    #[test]
    fn hardness_and_velocity_brighten() {
        let bright = |vel: f32, hard: f32| {
            let x = play(0, 60, vel, hard, 9_600);
            let d: Vec<f32> = x[..4_800].windows(2).map(|w| w[1] - w[0]).collect();
            rms(&d) / rms(&x[..4_800])
        };
        assert!(bright(0.8, 1.0) > 1.5 * bright(0.8, 0.0));
        assert!(bright(1.0, 0.5) > 1.15 * bright(0.2, 0.5));
        let loud = rms(&play(0, 60, 1.0, 0.5, 9_600));
        let soft = rms(&play(0, 60, 0.2, 0.5, 9_600));
        assert!(loud > 2.0 * soft);
    }

    #[test]
    fn middle_hit_loses_the_second_mode() {
        let second = |pos: f32| {
            let mut m = Mallets::new();
            m.set_param(P_HARDNESS, 1.0);
            m.set_param(P_POSITION, pos);
            m.set_param(P_RESONATOR, 0.0);
            m.note_on(48, 1.0);
            let x = render(&mut m, 9_600).0;
            let f0 = midi_to_hz(48.0);
            mag(&x[240..], f0 * 3.99) / mag(&x[240..], f0)
        };
        assert!(second(1.0) > 4.0 * second(0.0));
    }

    #[test]
    fn vibraphone_motor_and_damper() {
        let mut m = Mallets::new();
        m.set_param(P_MODEL, 1.0);
        m.set_param(P_RESONATOR, 1.0);
        m.set_param(P_MOTOR_DEPTH, 1.0);
        m.set_param(P_MOTOR_RATE, 5.0);
        m.controller(Controller::Cc(64, 1.0));
        m.note_on(65, 0.8);
        m.note_off(65);
        let x = render(&mut m, 48_000).0;
        // Tremolo at the motor rate: envelope swings.
        let env: Vec<f32> = x[9_600..].chunks(480).map(rms).collect();
        let (lo, hi) = env.iter().fold((f32::MAX, 0.0f32), |(a, b), &v| (a.min(v), b.max(v)));
        assert!(hi > 2.0 * lo, "{lo} {hi}");
        // Pedal held: still ringing; pedal up: damped.
        assert!(rms(&x[43_200..]) > 0.01 * rms(&x[..4_800]));
        m.controller(Controller::Cc(64, 0.0));
        let y = render(&mut m, 24_000).0;
        assert!(rms(&y[14_400..19_200]) < 0.01 * rms(&x[43_200..]));
        // Marimba ignores note-off: it rings out.
        let mut m = Mallets::new();
        m.note_on(60, 0.8);
        render(&mut m, 2_400);
        m.note_off(60);
        let x = render(&mut m, 9_600).0;
        assert!(rms(&x[4_800..]) > 0.2 * rms(&x[..4_800]));
    }

    #[test]
    fn resonator_reinforces_the_fundamental() {
        let fund = |res: f32| {
            let mut m = Mallets::new();
            m.set_param(P_RESONATOR, res);
            m.note_on(57, 0.8);
            let x = render(&mut m, 24_000).0;
            mag(&x[4_800..], midi_to_hz(57.0))
        };
        assert!(fund(1.0) > 1.4 * fund(0.0));
    }

    #[test]
    fn every_model_plays_and_survives_extremes() {
        for (model, name) in MODELS.iter().enumerate() {
            let x = play(model, 72, 0.8, 0.5, 4_800);
            assert!(rms(&x) > 1e-3, "{}", name);
            assert!(x.iter().all(|s| s.is_finite()));
        }
        let mut m = Mallets::new();
        for n in 0..40u8 {
            m.note_on(40 + n, 1.0);
        }
        assert_eq!(m.voices.iter().filter(|v| v.active).count(), VOICES);
        stress(&mut m);
        m.set_param(P_MODEL, 6.0);
        stress(&mut m);
    }
}
