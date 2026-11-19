//! Strings: bowed violin, viola, cello and double bass, solo or in sections.
//!
//! Each player is a bowed-string waveguide (McIntyre, Schumacher and
//! Woodhouse, after Smith): two delay lines meet at the bow, one toward the
//! nut (inverting reflection), one toward the bridge (inverting, lossy
//! reflection). At the bow the string sticks or slips depending on the
//! difference between bow and string velocities, through a friction curve
//! whose steepness follows the bow pressure; the stick-slip settles into
//! Helmholtz motion by itself. Bow velocity and pressure follow the
//! dynamics (velocity, or the mod wheel / breath controller once moved);
//! harder bowing is louder and brighter. The bridge velocity rings through
//! the instrument's body (resonator bank: air mode, wood modes, bridge
//! hill), scaled from violin to double bass.
//!
//! Articulations (parameter or key switches C1–D#1): sustain, staccato,
//! pizzicato (the string is plucked at the bow point and rings freely) and
//! tremolo (fast alternating bow strokes). Legato mode glides the pitch
//! while the bow keeps moving; sections spread detuned, late and panned
//! players, with a light chorus on top.

use ponzik_dsp::util::{db_to_gain, flush_denormal};

use super::section::{Articulation, BUSES, CHUNK, MAX_PLAYERS, Model, Section};
use super::wg::{Body, Delay, Loss};
use crate::effects::OFF_ON;
use crate::param::{ParamInfo, Params, Unit};
use crate::{Controller, Instrument, NoteExpression};

const INSTRUMENTS: &[&str] = &["Violin", "Viola", "Cello", "Double Bass", "Full Section"];
const ARTICULATIONS: &[&str] = &["Sustain", "Staccato", "Pizzicato", "Tremolo"];
pub(crate) const PLAY_MODES: &[&str] = &["Poly", "Legato"];
pub(crate) const SECTION_SIZES: &[&str] = &["Solo", "Duo", "Trio", "Quartet", "Section"];
pub(crate) const SECTION_PLAYERS: [usize; 5] = [1, 2, 3, 4, MAX_PLAYERS];
/// First key switch (C1); the next three follow.
const KEY_SWITCH: u8 = 24;

const P_INSTRUMENT: u32 = 0;
const P_ARTICULATION: u32 = 1;
const P_KEY_SWITCHES: u32 = 2;
const P_PRESSURE: u32 = 3;
const P_POSITION: u32 = 4;
const P_ATTACK: u32 = 5;
const P_RELEASE: u32 = 6;
const P_BOW_NOISE: u32 = 7;
const P_VIB_DEPTH: u32 = 8;
const P_VIB_RATE: u32 = 9;
const P_VIB_DELAY: u32 = 10;
const P_MODE: u32 = 11;
const P_PORTAMENTO: u32 = 12;
const P_SIZE: u32 = 13;
const P_DETUNE: u32 = 14;
const P_TIMING: u32 = 15;
const P_WIDTH: u32 = 16;
const P_CHORUS: u32 = 17;
const P_BODY: u32 = 18;
const P_BRIGHTNESS: u32 = 19;
const P_VOLUME: u32 = 20;
const N_PARAMS: usize = 21;

static PARAMS: [ParamInfo; N_PARAMS] = [
    ParamInfo::choice(P_INSTRUMENT, "Instrument", INSTRUMENTS, 4).group("Strings"),
    ParamInfo::choice(P_ARTICULATION, "Articulation", ARTICULATIONS, 0).group("Strings"),
    ParamInfo::choice(P_KEY_SWITCHES, "Key Switches", OFF_ON, 1).group("Strings"),
    ParamInfo::new(P_PRESSURE, "Bow Pressure", 0.0, 1.0, 0.5, Unit::Percent).group("Bow"),
    ParamInfo::new(P_POSITION, "Bow Position", 0.08, 0.3, 0.12, Unit::Percent).group("Bow"),
    ParamInfo::new(P_ATTACK, "Attack", 0.01, 1.5, 0.15, Unit::Seconds).log().group("Bow"),
    ParamInfo::new(P_RELEASE, "Release", 0.02, 3.0, 0.35, Unit::Seconds).log().group("Bow"),
    ParamInfo::new(P_BOW_NOISE, "Bow Noise", 0.0, 1.0, 0.3, Unit::Percent).group("Bow"),
    ParamInfo::new(P_VIB_DEPTH, "Vibrato Depth", 0.0, 60.0, 18.0, Unit::Cents).group("Vibrato"),
    ParamInfo::new(P_VIB_RATE, "Vibrato Rate", 3.0, 8.0, 5.5, Unit::Hz).group("Vibrato"),
    ParamInfo::new(P_VIB_DELAY, "Vibrato Delay", 0.0, 1.5, 0.3, Unit::Seconds).group("Vibrato"),
    ParamInfo::choice(P_MODE, "Play Mode", PLAY_MODES, 0).group("Ensemble"),
    ParamInfo::new(P_PORTAMENTO, "Portamento", 0.0, 0.6, 0.1, Unit::Seconds).group("Ensemble"),
    ParamInfo::choice(P_SIZE, "Section Size", SECTION_SIZES, 4).group("Ensemble"),
    ParamInfo::new(P_DETUNE, "Detune", 0.0, 25.0, 7.0, Unit::Cents).group("Ensemble"),
    ParamInfo::new(P_TIMING, "Timing", 0.0, 0.08, 0.025, Unit::Seconds).group("Ensemble"),
    ParamInfo::new(P_WIDTH, "Stereo Width", 0.0, 1.0, 0.7, Unit::Percent).group("Ensemble"),
    ParamInfo::new(P_CHORUS, "Chorus", 0.0, 1.0, 0.25, Unit::Percent).group("Ensemble"),
    ParamInfo::new(P_BODY, "Body", 0.0, 1.0, 0.7, Unit::Percent).group("Output"),
    ParamInfo::new(P_BRIGHTNESS, "Brightness", 0.0, 1.0, 0.5, Unit::Percent).group("Output"),
    ParamInfo::new(P_VOLUME, "Volume", -40.0, 6.0, -3.0, Unit::Db).group("Output"),
];

/// Violin body: air (A0), corpus bending (CBR), main wood modes (B1−, B1+),
/// then the bridge hill around 2.5 kHz.
const VIOLIN_BODY: [(f32, f32, f32); 10] = [
    (275.0, 12.0, 0.9),
    (460.0, 14.0, 0.6),
    (530.0, 16.0, 1.0),
    (595.0, 15.0, 0.9),
    (720.0, 11.0, 0.5),
    (1_000.0, 8.0, 0.4),
    (1_500.0, 6.0, 0.35),
    (2_400.0, 2.5, 0.75),
    (3_300.0, 3.5, 0.4),
    (4_600.0, 3.0, 0.18),
];

/// Body scale per bus: violin, viola, cello, double bass.
const BODY_SCALE: [f32; BUSES] = [1.0, 0.78, 0.43, 0.24];
/// Lowest key per instrument.
const SPLIT: [u8; 3] = [36, 48, 55];

/// What the bowed model reads.
pub(crate) struct BowCfg {
    pressure: f32,
    position: f32,
    noise: f32,
    brightness: f32,
}

/// One bowed string.
pub(crate) struct Bowed {
    sr: f32,
    neck: Delay,
    bridge: Delay,
    loss: Loss,
    d: [f32; 2],
    step: [f32; 2],
    slope: f32,
    contact: f32,
    contact_decay: f32,
    pizz: bool,
    /// Pizzicato pulse: position, width (samples), level.
    pulse: (f32, f32, f32),
    hz: f32,
    /// Output tone: rounder corners of the Helmholtz motion when bowed softly.
    tone: f32,
    tone_y: f32,
}

impl Bowed {
    #[inline]
    fn bow_table(&self, dv: f32) -> f32 {
        let x = (dv * self.slope).abs() + 0.75;
        let x2 = x * x;
        (1.0 / (x2 * x2)).min(1.0)
    }
}

impl Model for Bowed {
    type Cfg = BowCfg;

    fn new(sample_rate: f32, min_hz: f32, _seed: u32) -> Self {
        let max = (sample_rate / min_hz) as usize + 8;
        Self {
            sr: sample_rate,
            neck: Delay::new(max),
            bridge: Delay::new(max),
            loss: Loss::default(),
            d: [2.0; 2],
            step: [0.0; 2],
            slope: 3.0,
            contact: 0.0,
            contact_decay: (-1.0 / (0.03 * sample_rate)).exp(),
            pizz: false,
            pulse: (0.0, 0.0, 0.0),
            hz: 220.0,
            tone: 1.0,
            tone_y: 0.0,
        }
    }

    fn reset(&mut self) {
        self.neck.clear();
        self.bridge.clear();
        self.loss.reset();
        self.contact = 0.0;
        self.pulse.2 = 0.0;
        self.tone_y = 0.0;
    }

    fn attack(&mut self, _cfg: &BowCfg, velocity: f32, pluck: bool) {
        self.pizz = pluck;
        if pluck {
            // A finger plucks at the bow point: one sine cycle, narrower when harder.
            let period = self.sr / self.hz;
            let width = (period * (0.5 - 0.3 * velocity)).clamp(3.0, 600.0);
            self.pulse = (0.0, width, 0.35 * (0.2 + 0.8 * velocity));
        }
    }

    fn tune(&mut self, cfg: &BowCfg, hz: f32, dynamics: f32, released: bool, n: usize, fresh: bool) {
        self.hz = hz;
        let sr = self.sr;
        let (gain, pole) = if self.pizz {
            // Gut and steel ring for about a second, less high up.
            let t60 = (1.6 * (196.0 / hz).powf(0.5)).clamp(0.25, 4.0) * if released { 0.4 } else { 1.0 };
            (10f32.powf(-3.0 / (t60 * hz)).min(0.9995), 0.35 - 0.2 * cfg.brightness)
        } else {
            (0.95, (0.62 - 0.3 * cfg.brightness - 0.2 * dynamics).clamp(0.1, 0.9))
        };
        self.loss.set(gain, pole);
        // Soft bowing rounds the corners of the motion: a darker tone.
        let cutoff = if self.pizz { 6_000.0 } else { 900.0 * (1.0 + 9.0 * dynamics * dynamics) * (0.5 + cfg.brightness) };
        self.tone = 1.0 - (-std::f32::consts::TAU * cutoff.min(sr * 0.45) / sr).exp();
        // Schelleng: a faster bow needs more force to keep the Helmholtz motion.
        let period = sr / hz - self.loss.delay(hz, sr);
        // The knob's range maps to where the Helmholtz motion stays stable.
        let beta = cfg.position.clamp(0.08, 0.25).max(2.5 / period).min(0.5);
        // Closer to the bridge needs more force for the same bow speed.
        let pressure = ((0.3 + 0.35 * dynamics + 0.3 * cfg.pressure) * (0.12 / beta).sqrt()).clamp(0.0, 1.0);
        self.slope = 5.0 - 4.0 * pressure;
        let target = [(period * (1.0 - beta)).max(2.0), (period * beta).max(2.0)];
        for ((d, step), t) in self.d.iter_mut().zip(&mut self.step).zip(target) {
            if fresh {
                *d = t;
                *step = 0.0;
            } else {
                *step = (t - *d) / n as f32;
            }
        }
    }

    #[inline]
    fn tick(&mut self, cfg: &BowCfg, drive: f32, noise: f32) -> f32 {
        self.d[0] += self.step[0];
        self.d[1] += self.step[1];
        let bridge_out = self.bridge.read(self.d[1]);
        let neck_out = self.neck.read(self.d[0]);
        let bridge_refl = -self.loss.process(bridge_out);
        let nut_refl = -neck_out;
        let string_vel = bridge_refl + nut_refl;
        let mut new_vel = 0.0;
        if !self.pizz {
            // The bow touches while it moves, lifts a little after it stops.
            self.contact = (drive.abs() * 25.0).min(1.0).max(self.contact * self.contact_decay);
            if self.contact > 1e-4 {
                let speed = drive.abs();
                let bow = (0.12 * speed + 0.03 * (speed * 20.0).min(1.0)).copysign(drive) * (1.0 + cfg.noise * 0.12 * noise);
                let dv = bow - string_vel;
                new_vel = dv * self.bow_table(dv) * self.contact;
            }
        } else if self.pulse.2 > 0.0 {
            let (pos, width, level) = self.pulse;
            new_vel = level * (std::f32::consts::TAU * pos / width).sin();
            self.pulse.0 += 1.0;
            if self.pulse.0 >= width {
                self.pulse.2 = 0.0;
            }
        }
        self.neck.write(flush_denormal(bridge_refl + new_vel));
        self.bridge.write(flush_denormal(nut_refl + new_vel));
        self.tone_y = flush_denormal(self.tone_y + (bridge_out - self.tone_y) * self.tone);
        self.tone_y
    }
}

/// Bowed strings, eight voices of up to six players each.
pub struct Strings {
    values: [f32; N_PARAMS],
    sample_rate: f32,
    section: Section<Bowed>,
    bow: BowCfg,
    bodies: [[Body<10>; 2]; BUSES],
    dc: [f32; 4],
    /// Leaky integrators per bus and side: the bridge force is a sawtooth.
    force: [[f32; 2]; BUSES],
    /// DC of the bridge velocity (the bow pulls the string aside), removed first.
    offset: [[f32; 2]; BUSES],
    chorus: [Delay; 2],
    chorus_phase: f32,
    expression: f32,
    tail: u32,
    /// Samples each bus keeps ringing after its last input.
    bus_tail: [u32; BUSES],
}

impl Default for Strings {
    fn default() -> Self {
        Self::new()
    }
}

impl Strings {
    pub fn new() -> Self {
        let mut s = Self {
            values: PARAMS.map(|p| p.default),
            sample_rate: 0.0,
            section: Section::new(0x0579_11A1),
            bow: BowCfg { pressure: 0.5, position: 0.12, noise: 0.3, brightness: 0.5 },
            bodies: Default::default(),
            dc: [0.0; 4],
            force: [[0.0; 2]; BUSES],
            offset: [[0.0; 2]; BUSES],
            chorus: [Delay::new(8), Delay::new(8)],
            chorus_phase: 0.0,
            expression: 1.0,
            tail: 0,
            bus_tail: [0; BUSES],
        };
        s.section.cfg.min_hz = 27.0;
        s.set_sample_rate(48_000.0);
        s
    }

    fn v(&self, id: u32) -> f32 {
        self.values[id as usize]
    }

    fn index(&self, id: u32) -> usize {
        self.v(id).round() as usize
    }

    fn update(&mut self) {
        let sr = self.sample_rate;
        let articulation = match self.index(P_ARTICULATION) {
            1 => Articulation::Staccato,
            2 => Articulation::Pluck,
            3 => Articulation::Tremolo(13.0),
            _ => Articulation::Sustain,
        };
        let instrument = self.index(P_INSTRUMENT);
        let c = &mut self.section.cfg;
        c.articulation = articulation;
        c.attack = self.values[P_ATTACK as usize];
        c.release = self.values[P_RELEASE as usize];
        c.vib_depth = self.values[P_VIB_DEPTH as usize];
        c.vib_rate = self.values[P_VIB_RATE as usize];
        c.vib_delay = self.values[P_VIB_DELAY as usize];
        c.legato = self.values[P_MODE as usize] >= 0.5;
        c.portamento = self.values[P_PORTAMENTO as usize];
        c.players = SECTION_PLAYERS[(self.values[P_SIZE as usize].round() as usize).min(4)];
        c.detune = self.values[P_DETUNE as usize];
        c.timing = self.values[P_TIMING as usize];
        c.width = self.values[P_WIDTH as usize];
        c.split = (instrument == 4).then_some(SPLIT);
        c.bus = instrument.min(3);
        c.max_hz = sr * 0.2;
        self.bow = BowCfg {
            pressure: self.v(P_PRESSURE),
            position: self.v(P_POSITION),
            noise: self.v(P_BOW_NOISE),
            brightness: self.v(P_BRIGHTNESS),
        };
        let body = self.v(P_BODY);
        for (bus, pair) in self.bodies.iter_mut().enumerate() {
            for b in pair {
                b.set(&VIOLIN_BODY, BODY_SCALE[bus], sr);
                b.direct = 1.0 - 0.6 * body;
                b.amount = 1.5 * body;
            }
        }
    }
}

impl Params for Strings {
    fn params(&self) -> &'static [ParamInfo] {
        &PARAMS
    }

    fn get_param(&self, id: u32) -> f32 {
        self.values.get(id as usize).copied().unwrap_or(0.0)
    }

    fn set_param(&mut self, id: u32, value: f32) {
        let Some(info) = PARAMS.get(id as usize) else { return };
        let v = info.clamp(value);
        if self.values[id as usize] != v {
            self.values[id as usize] = v;
            self.update();
        }
    }
}

impl Instrument for Strings {
    fn kind(&self) -> &'static str {
        "strings"
    }

    fn is_melodic(&self) -> bool {
        true
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        if sample_rate != self.sample_rate {
            let len = (0.05 * sample_rate) as usize;
            self.chorus = [Delay::new(len), Delay::new(len)];
        }
        self.sample_rate = sample_rate;
        self.update();
        self.section.set_sample_rate(sample_rate);
        self.all_notes_off();
    }

    fn note_on(&mut self, note: u8, velocity: f32) {
        if self.v(P_KEY_SWITCHES) >= 0.5 && (KEY_SWITCH..KEY_SWITCH + 4).contains(&note) {
            self.values[P_ARTICULATION as usize] = (note - KEY_SWITCH) as f32;
            self.update();
            return;
        }
        self.section.note_on(note, velocity);
        self.tail = (0.5 * self.sample_rate) as u32;
    }

    fn note_off(&mut self, note: u8) {
        self.section.note_off(note);
    }

    fn supports_expression(&self) -> bool {
        true
    }

    fn note_expression(&mut self, key: u8, expr: NoteExpression) {
        self.section.note_expression(key, expr);
    }

    fn controller(&mut self, ctl: Controller) {
        self.section.controller(ctl);
    }

    fn all_notes_off(&mut self) {
        self.section.all_notes_off();
        self.bodies.iter_mut().flatten().for_each(Body::reset);
        self.chorus.iter_mut().for_each(Delay::clear);
        self.dc = [0.0; 4];
        self.force = [[0.0; 2]; BUSES];
        self.offset = [[0.0; 2]; BUSES];
        self.tail = 0;
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        if self.tail == 0 && !self.section.any_active() {
            return;
        }
        let sr = self.sample_rate;
        let gain = db_to_gain(self.v(P_VOLUME)) * 3.0;
        let chorus = self.v(P_CHORUS) * if self.section.cfg.players > 1 { 1.0 } else { 0.4 };
        let dc_coef = 1.0 - (-std::f32::consts::TAU * 15.0 / sr).exp();
        let leak = (-std::f32::consts::TAU * 1_500.0 / sr).exp();
        let force_gain = (1.0 - leak) * 0.3;
        let expr_coef = 1.0 - (-1.0 / (0.02 * sr)).exp();
        let mut done = 0;
        while done < left.len() {
            let n = CHUNK.min(left.len() - done);
            let mut buses = [[[0.0f32; CHUNK]; 2]; BUSES];
            self.section.render_chunk(&self.bow, n, &mut buses);
            // Only the bodies that sound are computed.
            for (bus, t) in self.bus_tail.iter_mut().enumerate() {
                if buses[bus].iter().flatten().any(|x| *x != 0.0) {
                    *t = (0.5 * sr) as u32;
                } else if *t > 0 {
                    *t = t.saturating_sub(n as u32);
                    if *t == 0 {
                        self.bodies[bus].iter_mut().for_each(Body::reset);
                        self.force[bus] = [0.0; 2];
                        self.offset[bus] = [0.0; 2];
                    }
                }
            }
            for i in 0..n {
                let mut out = [0.0f32; 2];
                for (bus, pair) in self.bodies.iter_mut().enumerate() {
                    if self.bus_tail[bus] == 0 {
                        continue;
                    }
                    for (ch, y) in out.iter_mut().enumerate() {
                        let x = buses[bus][ch][i];
                        let o = &mut self.offset[bus][ch];
                        *o = flush_denormal(*o + (x - *o) * dc_coef);
                        let f = &mut self.force[bus][ch];
                        *f = flush_denormal(*f * leak + x - *o);
                        *y += pair[ch].process(*f * force_gain);
                    }
                }
                self.expression += (self.section.ctl.expression - self.expression) * expr_coef;
                self.chorus_phase = (self.chorus_phase + 0.31 / sr).fract();
                for (ch, o) in out.iter_mut().enumerate() {
                    // DC blocker.
                    self.dc[ch] += (*o - self.dc[ch]) * dc_coef;
                    let x = *o - self.dc[ch];
                    // Light chorus: a slowly moving copy, opposite phase on each side.
                    let ph = self.chorus_phase + ch as f32 * 0.5;
                    let d = (0.012 + 0.004 * crate::instruments::common::fast_sin(ph)) * sr;
                    let wet = self.chorus[ch].read(d);
                    self.chorus[ch].write(x);
                    let y = x * (1.0 - 0.3 * chorus) + wet * 0.5 * chorus;
                    *o = crate::instruments::common::soft_clip(flush_denormal(y) * gain * self.expression);
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
            self.bodies.iter_mut().flatten().for_each(Body::reset);
            self.chorus.iter_mut().for_each(Delay::clear);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::wg::test_util::*;
    use super::*;

    const SR: f32 = 48_000.0;

    fn solo(instrument: usize) -> Strings {
        let mut s = Strings::new();
        s.set_param(P_INSTRUMENT, instrument as f32);
        s.set_param(P_SIZE, 0.0);
        s.set_param(P_VIB_DEPTH, 0.0);
        s.set_param(P_CHORUS, 0.0);
        s
    }

    fn play(s: &mut Strings, note: u8, vel: f32, secs: f32) -> Vec<f32> {
        s.note_on(note, vel);
        let (l, r) = render(s, (secs * SR) as usize);
        mono(&l, &r)
    }

    #[test]
    fn bowed_notes_are_in_tune() {
        let ranges: [&[u8]; 4] = [&[55, 62, 69, 76, 84, 91], &[48, 55, 62, 69, 76], &[36, 43, 50, 57, 64, 72], &[28, 33, 40, 45, 52]];
        for (inst, notes) in ranges.iter().enumerate() {
            for &note in *notes {
                for vel in [0.4, 0.9] {
                    let mut s = solo(inst);
                    let x = play(&mut s, note, vel, 1.2);
                    let hz = pitch(&x[24_000..], SR).unwrap_or_else(|| panic!("{} {note} {vel}: no pitch", INSTRUMENTS[inst]));
                    let off = cents_off(hz, note as f32);
                    assert!(off.abs() < 12.0, "{} note {note} vel {vel}: {off:.1} cents", INSTRUMENTS[inst]);
                }
            }
        }
    }

    #[test]
    fn stable_everywhere() {
        for art in 0..4 {
            for size in [0, 4] {
                let make = move || -> Box<dyn Instrument> {
                    let mut s = Strings::new();
                    s.set_param(P_ARTICULATION, art as f32);
                    s.set_param(P_SIZE, size as f32);
                    Box::new(s)
                };
                sweep_stable(&make, 21..=108, 11);
            }
        }
    }

    #[test]
    fn dynamics_change_the_timbre() {
        // Through the mod wheel, as orchestral players do.
        let c = |wheel: f32| {
            let mut s = solo(0);
            s.controller(Controller::Cc(1, wheel));
            let x = play(&mut s, 62, 0.8, 1.0);
            (rms(&x[24_000..]), centroid(&x[30_000..34_096], SR))
        };
        let (soft_level, soft) = c(0.15);
        let (loud_level, loud) = c(1.0);
        assert!(loud_level > soft_level * 1.5, "{soft_level} → {loud_level}");
        assert!(loud > soft * 1.1, "{soft} → {loud}");
    }

    #[test]
    fn bow_attack_and_pizzicato_decay_are_realistic() {
        let mut s = solo(2);
        let x = play(&mut s, 48, 0.6, 1.5);
        let a = attack_time(&x, SR, 0.8);
        assert!(a > 0.04 && a < 0.6, "bowed attack {a}");
        let mut s = solo(2);
        s.set_param(P_ARTICULATION, 2.0);
        let x = play(&mut s, 48, 0.8, 3.0);
        let a = attack_time(&x, SR, 0.8);
        assert!(a < 0.02, "pizz attack {a}");
        let env = envelope(&x, SR);
        let top = peak(&env);
        assert!(env[(1.5 * SR) as usize] < top * 0.2, "pizz rings too long");
        assert!(env[(0.3 * SR) as usize] > top * 0.05, "pizz dies too fast");
    }

    #[test]
    fn legato_glides_between_notes() {
        let mut s = solo(2);
        s.set_param(P_MODE, 1.0);
        s.set_param(P_PORTAMENTO, 0.2);
        s.note_on(48, 0.8);
        render(&mut s, 24_000);
        s.note_on(55, 0.8);
        render(&mut s, 4_800);
        let mid = s.section.pitch_of(55).unwrap();
        assert!(mid > 48.5 && mid < 54.5, "{mid}");
        assert_eq!(s.section.active().filter(|v| !v.3).count(), 1);
        render(&mut s, 12_000);
        let (l, r) = render(&mut s, 24_000);
        let hz = pitch(&mono(&l, &r), SR).unwrap();
        assert!(cents_off(hz, 55.0).abs() < 12.0, "{hz}");
    }

    #[test]
    fn key_switches_pick_the_articulation() {
        let mut s = Strings::new();
        s.note_on(KEY_SWITCH + 2, 1.0);
        assert_eq!(s.get_param(P_ARTICULATION), 2.0);
        assert!(!s.section.any_active());
    }

    #[test]
    fn ensemble_is_wide() {
        let mut s = Strings::new();
        s.set_param(P_INSTRUMENT, 0.0);
        s.note_on(67, 0.8);
        let (l, r) = render(&mut s, 48_000);
        let diff: Vec<f32> = l.iter().zip(&r).map(|(a, b)| a - b).collect();
        assert!(rms(&diff[24_000..]) > 0.2 * rms(&l[24_000..]));
    }

    #[test]
    fn bowing_stays_on_pitch_at_extreme_settings() {
        let mut bad = 0;
        for inst in 0..4 {
            let notes: &[u8] = [&[55u8, 91][..], &[48, 67], &[36, 74], &[28, 57]][inst];
            for &note in notes {
                for pressure in [0.0, 1.0] {
                    for wheel in [0.05, 1.0] {
                        for pos in [0.08, 0.3] {
                            let mut s = Strings::new();
                            s.set_param(P_INSTRUMENT, inst as f32);
                            s.set_param(P_SIZE, 0.0);
                            s.set_param(P_VIB_DEPTH, 0.0);
                            s.set_param(P_PRESSURE, pressure);
                            s.set_param(P_POSITION, pos);
                            s.controller(Controller::Cc(1, wheel));
                            s.note_on(note, 0.8);
                            let (l, r) = render(&mut s, 36_000);
                            let x = mono(&l, &r);
                            let hz = pitch(&x[24_000..], 48_000.0);
                            let off = hz.map(|h| cents_off(h, note as f32)).unwrap_or(9999.0);
                            if off.abs() > 15.0 {
                                bad += 1;
                                println!("{} note {note} pressure {pressure} wheel {wheel} pos {pos}: {off:.0} cents", INSTRUMENTS[inst]);
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(bad, 0);
    }
}
