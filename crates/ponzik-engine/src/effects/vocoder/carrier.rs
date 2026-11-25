//! Internal carrier of the vocoder: a chord of band-limited saw or pulse
//! oscillators playing the notes held on a channel of the rack (or one drone
//! note), or white noise. Fixed voices, nothing allocated after construction.

use ponzik_dsp::noise::WhiteNoise;
use ponzik_dsp::util::midi_to_hz;

use crate::effect::ChannelNotes;

/// Notes the chord plays at once.
pub const VOICES: usize = 8;
/// Width of the pulse wave (a narrow, buzzy pulse, the robot sound).
const PULSE_WIDTH: f32 = 0.3;
/// Level of the chord, shared by its voices (about −10 dBFS whatever the chord).
const LEVEL: f32 = 0.32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wave {
    Saw,
    Pulse,
    Noise,
}

#[derive(Clone, Copy, Debug, Default)]
struct Voice {
    note: u8,
    /// 1 while the note is held, 0 once released.
    target: f32,
    amp: f32,
    phase: [f32; 2],
    inc: [f32; 2],
}

/// PolyBLEP correction of a step at phase 0 (`dt`: phase increment).
#[inline]
fn blep(t: f32, dt: f32) -> f32 {
    if t < dt {
        let t = t / dt;
        t + t - t * t - 1.0
    } else if t > 1.0 - dt {
        let t = (t - 1.0) / dt;
        t * t + t + t + 1.0
    } else {
        0.0
    }
}

#[inline]
fn saw(t: f32, dt: f32) -> f32 {
    2.0 * t - 1.0 - blep(t, dt)
}

#[inline]
fn pulse(t: f32, dt: f32) -> f32 {
    let w = PULSE_WIDTH;
    let naive = if t < w { 1.0 } else { -1.0 };
    let down = (t - w + 1.0).fract();
    // Without its DC offset (2w − 1).
    naive + blep(t, dt) - blep(down, dt) - (2.0 * w - 1.0)
}

pub struct Carrier {
    voices: [Voice; VOICES],
    noise: WhiteNoise,
    sample_rate: f32,
    /// Detune between the two oscillators of a voice, as a frequency ratio.
    spread: f32,
    attack: f32,
    release: f32,
    /// Gain of the chord, smoothed toward `LEVEL / sqrt(voices held)`.
    gain: f32,
    gain_target: f32,
}

impl Carrier {
    pub fn new(sample_rate: f32) -> Self {
        let mut c = Self {
            voices: [Voice::default(); VOICES],
            noise: WhiteNoise::new(0x5EED_C0DE),
            sample_rate,
            spread: 1.0,
            attack: 0.0,
            release: 0.0,
            gain: LEVEL,
            gain_target: LEVEL,
        };
        c.set_sample_rate(sample_rate);
        c
    }

    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate.max(1.0);
        // 4 ms in, 60 ms out: no clicks, and a chord change stays tight.
        self.attack = 1.0 - (-1.0 / (0.004 * self.sample_rate)).exp();
        self.release = 1.0 - (-1.0 / (0.06 * self.sample_rate)).exp();
        self.retune();
    }

    pub fn set_detune(&mut self, cents: f32) {
        self.spread = 2f32.powf(cents.clamp(0.0, 100.0) / 2_400.0);
        self.retune();
    }

    pub fn reset(&mut self) {
        for v in &mut self.voices {
            *v = Voice::default();
        }
        self.gain = self.gain_target;
    }

    fn retune(&mut self) {
        let (sr, spread) = (self.sample_rate, self.spread);
        for v in &mut self.voices {
            let hz = midi_to_hz(f32::from(v.note));
            v.inc = [(hz / spread / sr).min(0.45), (hz * spread / sr).min(0.45)];
        }
    }

    /// Holds exactly `notes` (the lowest [`VOICES`] of them), releasing the others.
    pub fn follow(&mut self, notes: &ChannelNotes) {
        let mut wanted = [0u8; VOICES];
        let mut count = 0;
        for n in (0..128u8).filter(|&n| notes.is_held(n)) {
            if count == VOICES {
                break;
            }
            wanted[count] = n;
            count += 1;
        }
        let wanted = &wanted[..count];
        for v in &mut self.voices {
            if v.target > 0.0 && !wanted.contains(&v.note) {
                v.target = 0.0;
            }
        }
        for &n in wanted {
            if self.voices.iter().any(|v| v.target > 0.0 && v.note == n) {
                continue;
            }
            // The same note fading out, else a silent voice, else the quietest.
            let pick = self
                .voices
                .iter()
                .position(|v| v.note == n && v.amp > 0.0)
                .or_else(|| self.voices.iter().position(|v| v.target == 0.0 && v.amp < 1e-4))
                .unwrap_or_else(|| {
                    let mut best = 0;
                    for (i, v) in self.voices.iter().enumerate() {
                        if v.target == 0.0 && (self.voices[best].target > 0.0 || v.amp < self.voices[best].amp) {
                            best = i;
                        }
                    }
                    best
                });
            let v = &mut self.voices[pick];
            if v.note != n || v.amp < 1e-4 {
                // Spread the starting phases so a chord does not start on one spike.
                v.phase = [(f32::from(n) * 0.618_034).fract(), (f32::from(n) * 0.414_214).fract()];
            }
            v.note = n;
            v.target = 1.0;
        }
        self.gain_target = LEVEL / (count.max(1) as f32).sqrt();
        self.retune();
    }

    /// True while a voice still sounds.
    pub fn sounding(&self) -> bool {
        self.voices.iter().any(|v| v.target > 0.0 || v.amp > 1e-5)
    }

    /// Next sample of the carrier.
    #[inline]
    pub fn next(&mut self, wave: Wave) -> f32 {
        if wave == Wave::Noise {
            return 0.35 * self.noise.next_sample();
        }
        self.gain += (self.gain_target - self.gain) * 0.001;
        let (attack, release) = (self.attack, self.release);
        let mut out = 0.0;
        for v in &mut self.voices {
            if v.target == 0.0 && v.amp < 1e-6 {
                v.amp = 0.0;
                continue;
            }
            v.amp += (v.target - v.amp) * if v.target > v.amp { attack } else { release };
            let mut s = 0.0;
            for k in 0..2 {
                let (t, dt) = (v.phase[k], v.inc[k]);
                s += if wave == Wave::Saw { saw(t, dt) } else { pulse(t, dt) };
                v.phase[k] = (t + dt).fract();
            }
            out += 0.5 * s * v.amp;
        }
        out * self.gain
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plays_the_held_notes_and_releases_them() {
        let mut c = Carrier::new(48_000.0);
        let mut notes = ChannelNotes::EMPTY;
        notes.note_on(57);
        c.follow(&notes);
        let x: Vec<f32> = (0..48_000).map(|_| c.next(Wave::Saw)).collect();
        let mut tuner = ponzik_dsp::tuner::Tuner::new(48_000.0, 2_048);
        tuner.push(&x[24_000..]);
        let hz = tuner.detect().expect("pitch").hz;
        assert!((hz - 220.0).abs() < 1.5, "{hz}");
        c.follow(&ChannelNotes::EMPTY);
        let tail: Vec<f32> = (0..48_000).map(|_| c.next(Wave::Pulse)).collect();
        assert!(tail[40_000..].iter().all(|s| s.abs() < 1e-4));
        assert!(!c.sounding());
    }

    #[test]
    fn a_big_chord_keeps_its_level_and_stays_bounded() {
        let mut c = Carrier::new(48_000.0);
        let mut notes = ChannelNotes::EMPTY;
        (40..60).for_each(|n| notes.note_on(n));
        c.set_detune(50.0);
        c.follow(&notes);
        let peak = (0..48_000).map(|_| c.next(Wave::Pulse)).fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.1 && peak < 2.0, "{peak}");
    }
}
