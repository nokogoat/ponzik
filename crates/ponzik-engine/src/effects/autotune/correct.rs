//! From the detected pitch to the pitch wanted: the note aimed at (scale,
//! per-note overrides, or the notes a channel holds), retune speed with
//! humanize, Flex-Tune, the singer's vibrato scaled, a vibrato created,
//! transposition. Runs once per detection frame (every 2 ms), never allocates.

use std::f32::consts::TAU;

use crate::effect::ChannelNotes;

/// Where the target notes come from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MidiTarget {
    /// The scale.
    #[default]
    Off,
    /// The pitch classes held on the channel are the scale (the scale itself
    /// when nothing is held).
    Notes,
    /// The last note held is the target, in the octave nearest the voice;
    /// nothing held, nothing corrected.
    Melody,
}

/// Settings of the corrector, from the effect's parameters.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    /// Time to reach 90 % of the way to the target note, in seconds (0: at once).
    pub retune: f32,
    pub humanize: f32,
    pub flex: f32,
    /// Factor on the singer's own vibrato (1: unchanged).
    pub natural_vibrato: f32,
    /// Glide between two target notes, in seconds.
    pub transition: f32,
    /// Allowed target notes (bit 0: C … bit 11: B) and the ones left alone.
    pub allowed: u16,
    pub bypass: u16,
    pub midi: MidiTarget,
    pub transpose: f32,
    pub reference_hz: f32,
    /// Created vibrato: depth (semitones), rate (Hz), delay and onset (s).
    pub vib_depth: f32,
    pub vib_rate: f32,
    pub vib_delay: f32,
    pub vib_onset: f32,
    /// Auto-Tune 5 era curve: a first-order glide.
    pub classic: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            retune: 0.02,
            humanize: 0.0,
            flex: 0.0,
            natural_vibrato: 1.0,
            transition: 0.0,
            allowed: 0xFFF,
            bypass: 0,
            midi: MidiTarget::Off,
            transpose: 0.0,
            reference_hz: 440.0,
            vib_depth: 0.0,
            vib_rate: 5.5,
            vib_delay: 0.15,
            vib_onset: 0.3,
            classic: false,
        }
    }
}

/// What the corrector decided for one frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Corrected {
    /// Pitch of the voice (MIDI, fractional; NaN when unvoiced).
    pub input: f32,
    /// Pitch sent out (NaN when unvoiced).
    pub output: f32,
    /// Semitones the shifter moves the voice by.
    pub shift: f32,
    /// Note aimed at.
    pub note: Option<u8>,
    /// The note is one left alone (bypass, or no target).
    pub bypassed: bool,
}

/// Fractional MIDI note of a frequency.
pub fn hz_to_midi(hz: f32, reference_hz: f32) -> f32 {
    69.0 + 12.0 * (hz / reference_hz).log2()
}

/// Nearest note of `mask` (bit 0: C … bit 11: B) to `pitch`, the lower on a tie.
pub fn nearest(pitch: f32, mask: u16) -> Option<i32> {
    let mask = mask & 0xFFF;
    if mask == 0 || !pitch.is_finite() {
        return None;
    }
    let base = pitch.floor() as i32;
    let mut best: Option<(f32, i32)> = None;
    for n in base - 12..=base + 12 {
        if mask & (1 << n.rem_euclid(12)) == 0 {
            continue;
        }
        let d = (pitch - n as f32).abs();
        if best.is_none_or(|(bd, _)| d < bd - 1e-4) {
            best = Some((d, n));
        }
    }
    best.map(|b| b.1)
}

/// Pitch-domain lowpass (two poles, about 3 Hz): what is left is the vibrato.
#[derive(Clone, Copy, Debug, Default)]
struct Slow {
    a: f32,
    b: f32,
}

pub struct Corrector {
    frame_secs: f32,
    note: Option<i32>,
    /// Target pitch gliding by the transition time.
    target: f32,
    /// Correction smoothed by the retune speed (two critically damped poles).
    c1: f32,
    c2: f32,
    slow: Slow,
    unvoiced_secs: f32,
    /// Time on the current note, for humanize and the created vibrato.
    held: f32,
    vib_phase: f32,
    last_shift: f32,
    active: bool,
    glide: f32,
}

/// Time of the first-order slow pitch follower that separates the vibrato.
const SLOW_SECS: f32 = 0.06;
/// A note changes only when the voice is that much nearer another (semitones).
const HYSTERESIS: f32 = 0.12;
/// A gap shorter than this keeps the note and the correction (consonants).
const GAP_SECS: f32 = 0.06;

impl Corrector {
    pub fn new(frame_secs: f32) -> Self {
        Self {
            frame_secs,
            note: None,
            target: 0.0,
            c1: 0.0,
            c2: 0.0,
            slow: Slow::default(),
            unvoiced_secs: 1.0,
            held: 0.0,
            vib_phase: 0.0,
            last_shift: 0.0,
            active: false,
            glide: 0.0,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new(self.frame_secs);
    }

    pub fn frame_secs(&self) -> f32 {
        self.frame_secs
    }

    /// Next frame: `hz` the detected frequency, 0 or unvoiced frames keep the
    /// last shift. `held`: the notes of the MIDI source channel.
    pub fn process(&mut self, s: &Settings, hz: f32, voiced: bool, held: &ChannelNotes) -> Corrected {
        let dt = self.frame_secs;
        if !voiced || hz <= 0.0 {
            self.unvoiced_secs += dt;
            if self.unvoiced_secs > GAP_SECS {
                self.active = false;
                self.note = None;
                self.c1 = 0.0;
                self.c2 = 0.0;
                self.held = 0.0;
            }
            return Corrected { input: f32::NAN, output: f32::NAN, shift: self.last_shift, note: self.note.map(|n| n.clamp(0, 127) as u8), bypassed: false };
        }
        let p = hz_to_midi(hz, s.reference_hz);
        let onset = !self.active;
        self.active = true;
        self.unvoiced_secs = 0.0;
        // Vibrato: what moves faster than the slow follower.
        let k = 1.0 - (-dt / SLOW_SECS).exp();
        if onset || (p - self.slow.b).abs() > 1.0 {
            self.slow = Slow { a: p, b: p };
        } else {
            self.slow.a += k * (p - self.slow.a);
            self.slow.b += k * (self.slow.a - self.slow.b);
        }
        let vibrato = p - self.slow.b;
        let pv = p + (s.natural_vibrato - 1.0) * vibrato;

        // The note aimed at.
        let (mask, melody) = match s.midi {
            MidiTarget::Notes if held.any() => (held.pitch_classes(), None),
            MidiTarget::Melody => (0, Some(held.last)),
            _ => (s.allowed | s.bypass, None),
        };
        let candidate = match melody {
            Some(Some(n)) => {
                let pc = i32::from(n % 12);
                Some(pc + 12 * ((pv - pc as f32) / 12.0).round() as i32)
            }
            Some(None) => None,
            None => nearest(pv, mask),
        };
        let keep = match (self.note, candidate) {
            (Some(cur), Some(c)) if c != cur && melody.is_none() => {
                let still = mask & (1 << cur.rem_euclid(12)) != 0;
                still && (pv - cur as f32).abs() - (pv - c as f32).abs() < 2.0 * HYSTERESIS
            }
            _ => false,
        };
        let note = if keep { self.note } else { candidate };
        if note != self.note {
            self.held = 0.0;
            if let (Some(n), true) = (note, self.note.is_none() || s.transition <= 0.0) {
                self.target = n as f32;
            }
            self.note = note;
        } else {
            self.held += dt;
        }
        let from_held = melody.is_some() || (s.midi == MidiTarget::Notes && held.any());
        let bypassed = match note {
            None => true,
            Some(n) => !from_held && s.bypass & (1 << n.rem_euclid(12)) != 0,
        };
        let mut error = 0.0;
        if let Some(n) = note.filter(|_| !bypassed) {
            let goal = n as f32;
            if s.transition > 0.0 && !onset {
                // Constant-time glide: the speed is set when the note changes.
                if self.held == 0.0 {
                    self.glide = (goal - self.target).abs() * dt / s.transition;
                }
                let d = goal - self.target;
                self.target += d.clamp(-self.glide, self.glide);
            } else {
                self.target = goal;
            }
            error = self.target - pv;
            // Flex-Tune: only near the target note does the correction act.
            if s.flex > 0.0 {
                let off = (goal - pv).abs() * 100.0;
                let window = 50.0 * (1.0 - s.flex) + 4.0;
                let w = 1.0 - ((off - window) / 20.0).clamp(0.0, 1.0);
                error *= w * w * (3.0 - 2.0 * w);
            }
        }
        // Retune speed, slower on long notes with humanize.
        let long = ((self.held - 0.12) / 0.3).clamp(0.0, 1.0) * s.humanize;
        let retune = s.retune * (1.0 + 3.0 * long) + 0.12 * long;
        if retune <= dt * 0.25 {
            self.c1 = error;
            self.c2 = error;
        } else if s.classic {
            let a = 1.0 - (-dt * 2.303 / retune).exp();
            self.c2 += a * (error - self.c2);
            self.c1 = self.c2;
        } else {
            let a = 1.0 - (-dt * 3.89 / retune).exp();
            self.c1 += a * (error - self.c1);
            self.c2 += a * (self.c1 - self.c2);
        }
        let mut out = pv + self.c2;
        // Created vibrato.
        if s.vib_depth > 0.0 && note.is_some() {
            let ramp = if s.vib_onset > 0.0 { ((self.held - s.vib_delay) / s.vib_onset).clamp(0.0, 1.0) } else { f32::from(self.held >= s.vib_delay) };
            self.vib_phase = (self.vib_phase + s.vib_rate * dt).fract();
            out += s.vib_depth * ramp * (TAU * self.vib_phase).sin();
        } else {
            self.vib_phase = 0.0;
        }
        out += s.transpose;
        let shift = (out - p).clamp(-24.0, 24.0);
        self.last_shift = shift;
        Corrected { input: p, output: p + shift, shift, note: note.map(|n| n.clamp(0, 127) as u8), bypassed }
    }
}
