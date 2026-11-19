//! Tonewheel organ (B3 style): 91 spinning tonewheels, nine drawbars, key
//! contacts that click and bounce, single-trigger percussion, the scanner
//! vibrato and chorus, a tube preamp and a rotary speaker.
//!
//! Like the real generator, every wheel spins all the time at the frequency
//! of its gear ratio (slightly off equal temperament) and keys only connect
//! wheels to the nine drawbar busbars: notes share wheels, phases never
//! restart, and each of a key's nine contacts closes at its own instant,
//! bouncing once or twice — the key click. Neighbouring wheels leak a little
//! into each other's pickups. Percussion comes from the 1' contact (which it
//! takes away from its drawbar), fires only when every key was up, and
//! lowers the drawbars a little in its "normal" volume, as on the original.

use ponzik_dsp::biquad::{Biquad, BiquadCoefs};
use ponzik_dsp::delay::DelayLine;
use ponzik_dsp::noise::WhiteNoise;
use ponzik_dsp::smooth::Smoother;
use ponzik_dsp::util::{db_to_gain, flush_denormal};

use super::common::{fast_sin, soft_clip};
use crate::effects::rotary::{RotarySpeaker, SPEEDS, Speed, TubeDrive};
use crate::param::{ParamInfo, Params, Unit};
use crate::{Controller, Instrument};

/// Tonewheels of the generator.
pub const WHEELS: usize = 91;
/// Drawbars, and busbar contacts under each key.
pub const DRAWBARS: usize = 9;

/// Gear ratios of the twelve wheel families (C to B): wheel frequency is
/// 20 Hz (1200 rpm shaft) × ratio × teeth, with 2 teeth on the lowest octave
/// doubling every octave up: A4 comes out at exactly 440 Hz, the others a
/// few cents off equal temperament, like the original.
pub const GEAR_RATIOS: [f32; 12] = [
    85.0 / 104.0,
    71.0 / 82.0,
    67.0 / 73.0,
    105.0 / 108.0,
    103.0 / 100.0,
    84.0 / 77.0,
    74.0 / 64.0,
    98.0 / 80.0,
    96.0 / 74.0,
    88.0 / 64.0,
    67.0 / 46.0,
    108.0 / 70.0,
];

/// Pitch of each drawbar relative to the key, in semitones:
/// 16', 5⅓', 8', 4', 2⅔', 2', 1⅗', 1⅓', 1'.
pub const FOOTAGES: [i32; DRAWBARS] = [-12, 7, 0, 12, 19, 24, 28, 31, 36];
pub const FOOTAGE_NAMES: [&str; DRAWBARS] = ["16'", "5⅓'", "8'", "4'", "2⅔'", "2'", "1⅗'", "1⅓'", "1'"];

/// Frequency of wheel `n` (1 to 91), Hz.
pub fn wheel_hz(n: usize) -> f32 {
    let i = n.clamp(1, WHEELS) - 1;
    20.0 * GEAR_RATIOS[i % 12] * (2u32 << (i / 12)) as f32
}

/// Wheel (1 to 91) a key's drawbar contact taps: the lowest key (C2, MIDI
/// 36) takes wheel 1 on 16'; past the top the drawbars fold back an octave.
pub fn wheel_of(note: u8, drawbar: usize) -> usize {
    let mut w = note as i32 + FOOTAGES[drawbar] - 23;
    while w > WHEELS as i32 {
        w -= 12;
    }
    while w < 1 {
        w += 12;
    }
    w as usize
}

/// Level of a drawbar position (0 to 8): 3 dB a step, off at 0.
pub fn drawbar_gain(v: f32) -> f32 {
    if v <= 0.0 {
        0.0
    } else if v >= 1.0 {
        db_to_gain((v.min(8.0) - 8.0) * 3.0)
    } else {
        v * db_to_gain(-21.0)
    }
}

/// Percussion decay to −60 dB, s: about half a second fast, two and a half slow.
pub const PERC_FAST: f32 = 0.5;
pub const PERC_SLOW: f32 = 2.5;

/// Scanner vibrato: rate (Hz) and peak-to-peak delay swing (s) of V1–V3:
/// pitch swings of about ±0.4 %, ±0.7 % and ±1.1 % (±19 cents).
pub const SCANNER_HZ: f32 = 6.87;
pub const SCANNER_SWING: [f32; 3] = [0.000_29, 0.000_51, 0.000_8];

const P_PERC: u32 = 9;
const P_PERC_DECAY: u32 = 10;
const P_PERC_VOLUME: u32 = 11;
const P_VIBRATO: u32 = 12;
const P_CLICK: u32 = 13;
const P_LEAKAGE: u32 = 14;
const P_DRIVE: u32 = 15;
const P_VOLUME: u32 = 16;
const P_SPEED: u32 = 17;
const P_ROTARY_MIX: u32 = 18;
const P_INERTIA: u32 = 19;
const P_BALANCE: u32 = 20;
const P_MIC_WIDTH: u32 = 21;

const fn drawbar(i: usize, default: f32) -> ParamInfo {
    ParamInfo::new(i as u32, FOOTAGE_NAMES[i], 0.0, 8.0, default, Unit::None).group("Drawbars")
}

static PARAMS: [ParamInfo; 22] = [
    drawbar(0, 8.0),
    drawbar(1, 8.0),
    drawbar(2, 8.0),
    drawbar(3, 0.0),
    drawbar(4, 0.0),
    drawbar(5, 0.0),
    drawbar(6, 0.0),
    drawbar(7, 0.0),
    drawbar(8, 0.0),
    ParamInfo::choice(P_PERC, "Percussion", &["Off", "2nd", "3rd"], 0).group("Percussion"),
    ParamInfo::choice(P_PERC_DECAY, "Perc Decay", &["Fast", "Slow"], 0).group("Percussion"),
    ParamInfo::choice(P_PERC_VOLUME, "Perc Volume", &["Normal", "Soft"], 0).group("Percussion"),
    ParamInfo::choice(P_VIBRATO, "Vibrato", &["Off", "V1", "V2", "V3", "C1", "C2", "C3"], 0).group("Tonewheels"),
    ParamInfo::new(P_CLICK, "Key Click", 0.0, 1.0, 0.5, Unit::Percent).group("Tonewheels"),
    ParamInfo::new(P_LEAKAGE, "Leakage", 0.0, 1.0, 0.3, Unit::Percent).group("Tonewheels"),
    ParamInfo::new(P_DRIVE, "Drive", 0.0, 1.0, 0.15, Unit::Percent).group("Amp"),
    ParamInfo::new(P_VOLUME, "Volume", -40.0, 6.0, -6.0, Unit::Db).group("Amp"),
    ParamInfo::choice(P_SPEED, "Speed", SPEEDS, 1).group("Rotary"),
    ParamInfo::new(P_ROTARY_MIX, "Mix", 0.0, 1.0, 1.0, Unit::Percent).group("Rotary"),
    ParamInfo::new(P_INERTIA, "Inertia", 0.25, 4.0, 1.0, Unit::Ratio).log().group("Rotary"),
    ParamInfo::new(P_BALANCE, "Horn/Drum", 0.0, 1.0, 0.5, Unit::Percent).group("Rotary"),
    ParamInfo::new(P_MIC_WIDTH, "Mic Width", 0.0, 1.0, 0.7, Unit::Percent).group("Rotary"),
];

/// Contacts of one key.
#[derive(Clone, Copy, Debug, Default)]
struct Key {
    down: bool,
    /// Closed busbar contacts (bit per drawbar).
    closed: u16,
    /// State each contact settles in.
    goal: u16,
    /// Samples to the next toggle of each contact (0: none pending).
    wait: [u16; DRAWBARS],
    /// Bounces left before a contact settles.
    bounces: [u8; DRAWBARS],
}

/// Tonewheel organ.
pub struct Organ {
    sample_rate: f32,
    values: [f32; 22],
    inc: [f32; WHEELS],
    phase: [f32; WHEELS],
    /// Third-harmonic content of each wheel (the lowest wheels are not pure).
    h3: [f32; WHEELS],
    /// Contacts closed on each wheel, per drawbar.
    count: [[u8; DRAWBARS]; WHEELS],
    /// Keys (1' contact closed) holding each wheel on the percussion bus.
    perc_count: [u8; WHEELS],
    target: [f32; WHEELS],
    gain: [f32; WHEELS],
    perc_gain: [f32; WHEELS],
    leak: [f32; WHEELS],
    /// Per-sample approach of the wheel gains (1: instant steps, all click).
    slew: f32,
    levels: [f32; DRAWBARS],
    keys: [Key; 128],
    /// Keys with contacts in motion.
    moving: u128,
    keys_down: u32,
    perc_env: f32,
    perc_coef: f32,
    noise: WhiteNoise,
    rng: u32,
    click_env: f32,
    click_coef: f32,
    click_filter: Biquad,
    scanner: DelayLine,
    scanner_phase: f32,
    scanner_lp: Biquad,
    drive: TubeDrive,
    speaker: RotarySpeaker,
    rotary_mix: Smoother,
    expression: Smoother,
    mod_wheel: f32,
    gain_out: f32,
}

impl Default for Organ {
    fn default() -> Self {
        Self::new()
    }
}

impl Organ {
    pub fn new() -> Self {
        let mut o = Self {
            sample_rate: 48_000.0,
            values: std::array::from_fn(|i| PARAMS[i].default),
            inc: [0.0; WHEELS],
            phase: std::array::from_fn(|i| (i as f32 * 0.618_034).fract()),
            h3: std::array::from_fn(|i| if i < 12 { 0.1 } else if i < 24 { 0.04 } else { 0.0 }),
            count: [[0; DRAWBARS]; WHEELS],
            perc_count: [0; WHEELS],
            target: [0.0; WHEELS],
            gain: [0.0; WHEELS],
            perc_gain: [0.0; WHEELS],
            leak: [0.0; WHEELS],
            slew: 1.0,
            levels: [0.0; DRAWBARS],
            keys: [Key::default(); 128],
            moving: 0,
            keys_down: 0,
            perc_env: 0.0,
            perc_coef: 0.0,
            noise: WhiteNoise::new(0x0b3),
            rng: 0x9e37_79b9,
            click_env: 0.0,
            click_coef: 0.0,
            click_filter: Biquad::default(),
            scanner: DelayLine::new(2),
            scanner_phase: 0.0,
            scanner_lp: Biquad::default(),
            drive: TubeDrive::default(),
            speaker: RotarySpeaker::new(48_000.0),
            rotary_mix: Smoother::new(1.0),
            expression: Smoother::new(1.0),
            mod_wheel: 0.0,
            gain_out: 1.0,
        };
        o.set_sample_rate(48_000.0);
        o
    }

    fn v(&self, id: u32) -> f32 {
        self.values[id as usize]
    }

    fn index(&self, id: u32) -> usize {
        self.v(id).round() as usize
    }

    fn perc_on(&self) -> bool {
        self.index(P_PERC) > 0
    }

    /// Drawbar levels, the 1' cancelled by percussion and the rest lowered
    /// by its normal volume, as on the original.
    fn update_levels(&mut self) {
        let perc = self.perc_on();
        let comp = if perc && self.index(P_PERC_VOLUME) == 0 { db_to_gain(-3.0) } else { 1.0 };
        for b in 0..DRAWBARS {
            self.levels[b] = if perc && b == DRAWBARS - 1 { 0.0 } else { drawbar_gain(self.v(b as u32)) * comp };
        }
        for w in 0..WHEELS {
            self.retarget(w);
        }
        self.update_leak();
    }

    fn retarget(&mut self, w: usize) {
        self.target[w] = (0..DRAWBARS).map(|b| self.count[w][b] as f32 * self.levels[b]).sum();
    }

    /// Leakage between wheels sharing a compartment (48 apart) and between
    /// neighbouring pickups, plus a faint floor of the whole generator while
    /// a key is down. Low enough to follow the targets, not the click.
    fn update_leak(&mut self) {
        let k = self.v(P_LEAKAGE);
        let floor = if self.keys_down > 0 { k * 0.000_2 } else { 0.0 };
        for w in 0..WHEELS {
            let t = |i: isize| if (0..WHEELS as isize).contains(&i) { self.target[i as usize] } else { 0.0 };
            let i = w as isize;
            self.leak[w] = k * (0.016 * (t(i - 48) + t(i + 48)) + 0.004 * (t(i - 1) + t(i + 1))) + floor;
        }
    }

    fn update_slew(&mut self) {
        // Click 0: 3 ms ramps (no click), 1: steps.
        let c = self.v(P_CLICK);
        let tau = 0.003 * (1.0 - c).powi(2) + 0.000_02;
        self.slew = 1.0 - (-1.0 / (tau * self.sample_rate)).exp();
    }

    fn update_perc(&mut self) {
        let t60 = if self.index(P_PERC_DECAY) == 0 { PERC_FAST } else { PERC_SLOW };
        self.perc_coef = ponzik_dsp::modal::pole_radius(t60, self.sample_rate);
    }

    fn update_speed(&mut self) {
        let speed = if self.mod_wheel >= 0.5 { Speed::Fast } else { Speed::from_index(self.index(P_SPEED)) };
        self.speaker.set_speed(speed);
    }

    #[inline]
    fn rand(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        (self.rng >> 8) as f32 / (1u32 << 24) as f32
    }

    /// Moves a key's contacts towards `down`: each closes (or opens) at its
    /// own time within a few milliseconds and bounces once or twice.
    fn press(&mut self, note: u8, down: bool, velocity: f32) {
        let sr = self.sample_rate;
        // A quick stroke closes the contacts closer together.
        let spread = (0.0035 - 0.002 * velocity) * sr;
        let key_goal = if down { (1u16 << DRAWBARS) - 1 } else { 0 };
        for b in 0..DRAWBARS {
            let wait = 1.0 + self.rand() * spread;
            let bounces = 2 * (self.rand() * 2.0) as u8;
            let k = &mut self.keys[note as usize];
            k.wait[b] = wait as u16;
            k.bounces[b] = bounces;
        }
        let k = &mut self.keys[note as usize];
        k.goal = key_goal;
        k.down = down;
        self.moving |= 1u128 << note;
    }

    fn set_contact(&mut self, note: u8, b: usize, closed: bool) {
        let k = &mut self.keys[note as usize];
        let bit = 1u16 << b;
        if (k.closed & bit != 0) == closed {
            return;
        }
        if closed {
            k.closed |= bit;
        } else {
            k.closed &= !bit;
        }
        let w = wheel_of(note, b) - 1;
        let c = &mut self.count[w][b];
        *c = if closed { c.saturating_add(1) } else { c.saturating_sub(1) };
        self.retarget(w);
        // Percussion rides the 1' contact.
        if b == DRAWBARS - 1 {
            let harmonic = if self.index(P_PERC) == 2 { 19 } else { 12 };
            let pw = perc_wheel(note, harmonic) - 1;
            let pc = &mut self.perc_count[pw];
            *pc = if closed { pc.saturating_add(1) } else { pc.saturating_sub(1) };
        }
        // Every make and break of a contact snaps.
        let click = self.v(P_CLICK);
        self.click_env = self.click_env.max(0.35 * click * click);
    }

    /// Advances the moving contacts by one sample.
    fn tick_contacts(&mut self) {
        let mut moving = self.moving;
        let mut changed = false;
        while moving != 0 {
            let note = moving.trailing_zeros() as u8;
            moving &= moving - 1;
            let mut busy = false;
            for b in 0..DRAWBARS {
                let k = &mut self.keys[note as usize];
                if k.wait[b] == 0 {
                    continue;
                }
                k.wait[b] -= 1;
                if k.wait[b] > 0 {
                    busy = true;
                    continue;
                }
                let goal = k.goal & (1 << b) != 0;
                if k.bounces[b] > 0 {
                    k.bounces[b] -= 1;
                    let now = k.closed & (1 << b) != 0;
                    // Bounce: 0.1 to 0.5 ms apart.
                    let gap = (0.0001 + 0.0004 * self.rand()) * self.sample_rate;
                    self.keys[note as usize].wait[b] = gap.max(1.0) as u16;
                    busy = true;
                    // The first toggle heads for the goal, bounces alternate.
                    let next = if now == goal { !now } else { goal };
                    self.set_contact(note, b, next);
                } else {
                    self.set_contact(note, b, goal);
                }
                changed = true;
            }
            if !busy {
                self.moving &= !(1u128 << note);
            }
        }
        if changed {
            self.update_leak();
        }
    }
}

/// Wheel of the percussion harmonic (12: second, 19: third) of a key.
fn perc_wheel(note: u8, harmonic: i32) -> usize {
    let mut w = note as i32 + harmonic - 23;
    while w > WHEELS as i32 {
        w -= 12;
    }
    while w < 1 {
        w += 12;
    }
    w as usize
}

impl Params for Organ {
    fn params(&self) -> &'static [ParamInfo] {
        &PARAMS
    }

    fn get_param(&self, id: u32) -> f32 {
        self.values.get(id as usize).copied().unwrap_or(0.0)
    }

    fn set_param(&mut self, id: u32, value: f32) {
        let Some(info) = PARAMS.get(id as usize) else { return };
        let v = info.clamp(value);
        self.values[id as usize] = v;
        match id {
            0..=8 | P_PERC_VOLUME | P_LEAKAGE => self.update_levels(),
            P_PERC => {
                // Keys already down keep their wheel on the old harmonic:
                // rebuild the percussion bus from the closed 1' contacts.
                self.perc_count = [0; WHEELS];
                let harmonic = if self.index(P_PERC) == 2 { 19 } else { 12 };
                for n in 0..128u8 {
                    if self.keys[n as usize].closed & (1 << (DRAWBARS - 1)) != 0 {
                        let pw = perc_wheel(n, harmonic) - 1;
                        self.perc_count[pw] = self.perc_count[pw].saturating_add(1);
                    }
                }
                self.update_levels();
            }
            P_PERC_DECAY => self.update_perc(),
            P_CLICK => self.update_slew(),
            P_VOLUME => self.gain_out = db_to_gain(v),
            P_SPEED => self.update_speed(),
            P_ROTARY_MIX => self.rotary_mix.set_target(v),
            P_INERTIA => self.speaker.set_inertia(v),
            P_BALANCE => self.speaker.set_balance(v),
            P_MIC_WIDTH => self.speaker.set_mics(v, 0.4),
            _ => {}
        }
    }
}

impl Instrument for Organ {
    fn kind(&self) -> &'static str {
        "organ"
    }

    fn is_melodic(&self) -> bool {
        true
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        for w in 0..WHEELS {
            self.inc[w] = (wheel_hz(w + 1) / sample_rate).min(0.49);
        }
        self.scanner = DelayLine::new((0.004 * sample_rate) as usize + 8);
        self.scanner_lp.coefs = BiquadCoefs::lowpass(6_500.0_f32.min(sample_rate * 0.45), 0.707, sample_rate);
        self.click_filter.coefs = BiquadCoefs::bandpass(2_500.0_f32.min(sample_rate * 0.4), 0.6, sample_rate);
        self.click_coef = ponzik_dsp::util::decay_coef(0.0012, sample_rate);
        self.drive.set_sample_rate(sample_rate);
        self.speaker.set_sample_rate(sample_rate);
        self.rotary_mix.set_time(0.05, sample_rate);
        self.expression.set_time(0.02, sample_rate);
        for i in 0..PARAMS.len() {
            self.set_param(i as u32, self.values[i]);
        }
        self.rotary_mix.snap();
        self.all_notes_off();
    }

    fn note_on(&mut self, note: u8, velocity: f32) {
        let n = note.min(127);
        if self.keys[n as usize].down {
            return;
        }
        // Single trigger: percussion fires only from all keys up.
        if self.keys_down == 0 && self.perc_on() {
            self.perc_env = if self.index(P_PERC_VOLUME) == 0 { 1.0 } else { db_to_gain(-8.0) };
        }
        self.keys_down += 1;
        self.press(n, true, velocity.clamp(0.0, 1.0));
    }

    fn note_off(&mut self, note: u8) {
        let n = note.min(127);
        if !self.keys[n as usize].down {
            return;
        }
        self.keys_down = self.keys_down.saturating_sub(1);
        self.press(n, false, 0.7);
    }

    fn controller(&mut self, ctl: Controller) {
        match ctl {
            // Mod wheel: the rotary's slow/fast switch.
            Controller::Cc(1, v) => {
                let v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
                if (v >= 0.5) != (self.mod_wheel >= 0.5) {
                    self.mod_wheel = v;
                    self.update_speed();
                }
                self.mod_wheel = v;
            }
            // Expression pedal, before the preamp (it drives it less).
            Controller::Cc(11, v) => self.expression.set_target(if v.is_finite() { v.clamp(0.0, 1.0) } else { 1.0 }),
            _ => {}
        }
    }

    fn all_notes_off(&mut self) {
        self.keys = [Key::default(); 128];
        self.moving = 0;
        self.keys_down = 0;
        self.count = [[0; DRAWBARS]; WHEELS];
        self.perc_count = [0; WHEELS];
        self.target = [0.0; WHEELS];
        self.gain = [0.0; WHEELS];
        self.perc_gain = [0.0; WHEELS];
        self.leak = [0.0; WHEELS];
        self.perc_env = 0.0;
        self.click_env = 0.0;
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let sr = self.sample_rate;
        let vib = self.index(P_VIBRATO);
        let swing = if vib == 0 { 0.0 } else { SCANNER_SWING[(vib - 1) % 3] * sr };
        let chorus = vib >= 4;
        let scan_inc = SCANNER_HZ / sr;
        let drive = self.v(P_DRIVE);
        let slew = self.slew;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            if self.moving != 0 {
                self.tick_contacts();
            }
            let perc = self.perc_env;
            self.perc_env = flush_denormal(self.perc_env * self.perc_coef);
            let mut acc = 0.0;
            for w in 0..WHEELS {
                let mut p = self.phase[w] + self.inc[w];
                if p >= 1.0 {
                    p -= 1.0;
                }
                self.phase[w] = p;
                let g = self.gain[w] + (self.target[w] - self.gain[w]) * slew;
                self.gain[w] = if g < 1e-7 && self.target[w] == 0.0 { 0.0 } else { g };
                let pt = self.perc_count[w] as f32;
                let pg = self.perc_gain[w] + (pt - self.perc_gain[w]) * slew;
                self.perc_gain[w] = if pg < 1e-7 && pt == 0.0 { 0.0 } else { pg };
                let e = self.gain[w] + self.perc_gain[w] * perc + self.leak[w];
                if e > 1e-7 {
                    let s = fast_sin(p) + self.h3[w] * fast_sin(3.0 * p);
                    acc += e * s;
                }
            }
            // Contact noise.
            if self.click_env > 1e-6 {
                acc += self.click_filter.process(self.noise.next_sample()) * self.click_env;
                self.click_env *= self.click_coef;
            } else {
                self.click_env = 0.0;
            }
            let mut x = acc * 0.2;
            // Scanner: a delay swept by a triangle, alone (vibrato) or with
            // the dry signal (chorus).
            if vib > 0 {
                self.scanner.write(x);
                let tri = 4.0 * (self.scanner_phase - 0.5).abs() - 1.0;
                self.scanner_phase += scan_inc;
                if self.scanner_phase >= 1.0 {
                    self.scanner_phase -= 1.0;
                }
                let wet = self.scanner_lp.process(self.scanner.read_frac(0.0015 * sr + 0.5 * swing * tri));
                x = if chorus { 0.5 * (x + wet) * 1.2 } else { wet };
            }
            x *= self.expression.next_value();
            let x = self.drive.process(x, drive);
            let (rl, rr) = self.speaker.process(x);
            let mix = self.rotary_mix.next_value();
            let g = self.gain_out;
            *l += soft_clip((x * (1.0 - mix) + rl * mix) * g);
            *r += soft_clip((x * (1.0 - mix) + rr * mix) * g);
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

    /// Amplitude of the `hz` component of `x`.
    fn mag(x: &[f32], hz: f32) -> f32 {
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (n, &s) in x.iter().enumerate() {
            let ph = std::f64::consts::TAU * hz as f64 * n as f64 / SR as f64;
            re += s as f64 * ph.cos();
            im += s as f64 * ph.sin();
        }
        ((re * re + im * im).sqrt() / x.len() as f64 * 2.0) as f32
    }

    fn dry() -> Organ {
        let mut o = Organ::new();
        o.set_param(P_ROTARY_MIX, 0.0);
        o.set_param(P_DRIVE, 0.0);
        o.set_param(P_LEAKAGE, 0.0);
        for b in 0..9 {
            o.set_param(b, 0.0);
        }
        // Settles the smoothed settings.
        o.set_sample_rate(SR);
        o
    }

    #[test]
    fn wheels_follow_the_gear_ratios() {
        assert!((wheel_hz(46) - 440.0).abs() < 1e-3, "A4 {}", wheel_hz(46));
        assert!((wheel_hz(1) - 32.69).abs() < 0.01);
        // Within a few cents of equal temperament, never exactly on it.
        for n in 1..=WHEELS {
            let et = 440.0 * 2f32.powf((n as f32 - 46.0) / 12.0);
            let cents = 1200.0 * (wheel_hz(n) / et).log2();
            assert!(cents.abs() < 1.5, "wheel {n}: {cents} cents");
        }
        // Drawbar footages: 8' on C4 is the key, 16' an octave down, 5⅓' a
        // fifth up, 1' three octaves up; the top folds back.
        assert_eq!(wheel_of(60, 2), 37);
        assert_eq!(wheel_of(60, 0), 25);
        assert_eq!(wheel_of(60, 1), 44);
        assert_eq!(wheel_of(60, 8), 73);
        assert_eq!(wheel_of(96, 8), 109 - 12 - 12);
        assert!((drawbar_gain(8.0) - 1.0).abs() < 1e-6);
        assert!((gain_to_db(drawbar_gain(6.0)) + 6.0).abs() < 1e-3);
    }

    #[test]
    fn drawbars_build_the_spectrum() {
        // 16' + 8' + 2⅔' on A4: 220, 440 and 1320 Hz (the 2⅔' wheel's own
        // ratio), nothing else.
        let mut o = dry();
        o.set_param(P_CLICK, 0.0);
        o.set_param(0, 8.0);
        o.set_param(2, 8.0);
        o.set_param(4, 6.0);
        o.note_on(69, 1.0);
        let (l, _) = render(&mut o, 48_000);
        let x = &l[9_600..];
        let (a, b, c) = (mag(x, wheel_hz(34)), mag(x, wheel_hz(46)), mag(x, wheel_hz(65)));
        assert!((a / b - 1.0).abs() < 0.05, "{a} {b}");
        assert!((gain_to_db(c / b) + 6.0).abs() < 0.7, "{}", gain_to_db(c / b));
        assert!(mag(x, 660.0) < 0.01 * b && mag(x, 880.0) < 0.01 * b);
    }

    /// Energy above 5 kHz in the 15 ms after a key goes down, relative to
    /// the note's level.
    fn click_energy(click: f32) -> f32 {
        let mut o = dry();
        o.set_param(P_CLICK, click);
        o.set_param(2, 8.0);
        o.note_on(57, 0.8);
        let (l, _) = render(&mut o, 9_600);
        let mut hp = Biquad::new(BiquadCoefs::highpass(5_000.0, 0.707, SR));
        let mut hp2 = Biquad::new(BiquadCoefs::highpass(5_000.0, 0.707, SR));
        let high: Vec<f32> = l.iter().map(|&s| hp2.process(hp.process(s))).collect();
        rms(&high[..720]) / rms(&l[4_800..])
    }

    #[test]
    fn key_click_is_there_and_can_be_dialed_out() {
        let with = click_energy(1.0);
        let without = click_energy(0.0);
        assert!(with > 0.02, "click {with}");
        assert!(with > 10.0 * without, "{with} vs {without}");
        // After the contacts settle, a pure 8' tone has no highs.
        let mut o = dry();
        o.set_param(2, 8.0);
        o.note_on(57, 0.8);
        let (l, _) = render(&mut o, 9_600);
        let mut hp = Biquad::new(BiquadCoefs::highpass(5_000.0, 0.707, SR));
        let high: Vec<f32> = l.iter().map(|&s| hp.process(s)).collect();
        assert!(rms(&high[4_800..]) < 0.01 * rms(&l[4_800..]));
    }

    #[test]
    fn contacts_close_at_different_times() {
        let mut o = dry();
        o.note_on(60, 0.5);
        let mut first_closed = None;
        let mut all_closed = None;
        for n in 0..480 {
            o.tick_contacts();
            let c = o.keys[60].closed;
            if first_closed.is_none() && c != 0 {
                first_closed = Some(n);
            }
            if all_closed.is_none() && c == 0x1ff && o.moving == 0 {
                all_closed = Some(n);
            }
        }
        let (a, b) = (first_closed.unwrap(), all_closed.unwrap());
        assert!(b > a + 10, "{a} {b}");
        assert!(b < (0.006 * SR) as usize, "{b}");
    }

    #[test]
    fn percussion_is_single_trigger_and_decays() {
        let mut o = dry();
        o.set_param(P_CLICK, 0.0);
        o.set_param(2, 8.0);
        o.set_param(P_PERC, 1.0);
        o.note_on(60, 1.0);
        let (l, _) = render(&mut o, 4_800);
        let early = rms(&l[480..1_440]);
        let (l, _) = render(&mut o, 48_000);
        let late = rms(&l[38_400..]);
        assert!(early > 1.3 * late, "percussion {early} → {late}");
        // Fast decay: −60 dB in about half a second.
        assert!(o.perc_env < 1e-3, "{}", o.perc_env);
        // Legato: a second key while the first is held gets no percussion.
        o.note_on(64, 1.0);
        assert!(o.perc_env < 1e-3);
        o.note_off(60);
        o.note_off(64);
        render(&mut o, 4_800);
        o.note_on(67, 1.0);
        assert!(o.perc_env > 0.5);
        // Percussion takes the 1' drawbar away.
        assert_eq!(o.levels[8], 0.0);
    }

    #[test]
    fn percussion_times_are_real() {
        for (decay, t60) in [(0.0, PERC_FAST), (1.0, PERC_SLOW)] {
            let mut o = dry();
            o.set_param(P_PERC, 1.0);
            o.set_param(P_PERC_DECAY, decay);
            o.note_on(60, 1.0);
            let n = (t60 * SR) as usize;
            render(&mut o, n);
            assert!((gain_to_db(o.perc_env) + 60.0).abs() < 1.0, "{}", gain_to_db(o.perc_env));
        }
        assert!((0.3..=0.8).contains(&PERC_FAST) && (1.5..=3.0).contains(&PERC_SLOW));
    }

    #[test]
    fn vibrato_swings_the_pitch_by_the_scanner_depth() {
        let mut o = dry();
        o.set_param(P_CLICK, 0.0);
        o.set_param(2, 8.0);
        o.set_param(P_VIBRATO, 3.0);
        o.note_on(81, 1.0);
        let (l, _) = render(&mut o, 48_000);
        let f = crate::effects::rotary::tests::inst_freq(&l[4_800..], wheel_hz(58));
        let f = &f[480..];
        let mut s = f.to_vec();
        s.sort_by(f32::total_cmp);
        let (lo, hi) = (s[s.len() / 100], s[s.len() * 99 / 100]);
        let dev = (hi - lo) * 0.5 / wheel_hz(58);
        let expect = SCANNER_SWING[2] * 2.0 * SCANNER_HZ;
        assert!((dev - expect).abs() < 0.3 * expect, "{dev} vs {expect}");
        assert!(dev > 0.007 && dev < 0.016, "{dev}");
    }

    #[test]
    fn mod_wheel_switches_the_rotary() {
        let mut o = Organ::new();
        assert!((o.speaker.horn.speed - crate::effects::rotary::HORN_SLOW).abs() < 1e-3);
        o.controller(Controller::Cc(1, 1.0));
        render(&mut o, 144_000);
        assert!((o.speaker.horn.speed - crate::effects::rotary::HORN_FAST).abs() < 0.05);
        o.controller(Controller::Cc(1, 0.0));
        render(&mut o, 4_800);
        assert!(o.speaker.horn.speed < crate::effects::rotary::HORN_FAST - 0.5);
    }

    #[test]
    fn shared_wheels_add_up_coherently() {
        // C4 and C5 on 8' + 4': the C4 4' and the C5 8' are one wheel,
        // twice as loud (one phase), not two copies.
        let mut o = dry();
        o.set_param(P_CLICK, 0.0);
        o.set_param(2, 8.0);
        o.set_param(3, 8.0);
        o.note_on(60, 1.0);
        o.note_on(72, 1.0);
        let (l, _) = render(&mut o, 48_000);
        let x = &l[9_600..];
        let ratio = mag(x, wheel_hz(49)) / mag(x, wheel_hz(37));
        assert!((ratio - 2.0).abs() < 0.1, "{ratio}");
    }

    #[test]
    fn releases_and_survives_extremes() {
        let mut o = Organ::new();
        o.note_on(60, 1.0);
        render(&mut o, 4_800);
        o.note_off(60);
        render(&mut o, 4_800);
        assert!(o.gain.iter().all(|&g| g == 0.0));
        assert_eq!(o.keys_down, 0);
        stress(&mut o);
    }
}
