//! A synthetic singer, to test and show the pitch correction: glottal pulses
//! (harmonics falling 12 dB per octave) shaped by three vowel formants, with
//! vibrato, a slow drift off pitch, glides between notes and sibilant noise
//! where asked. Additive, so the true pitch and formants are known exactly.
//! Allocates its output: not for the audio thread.

use std::f64::consts::TAU;

use ponzik_dsp::noise::WhiteNoise;

/// Formants (Hz) of five vowels: a, e, i, o, u.
pub const VOWELS: [[f32; 3]; 5] =
    [[730.0, 1_090.0, 2_440.0], [530.0, 1_840.0, 2_480.0], [270.0, 2_290.0, 3_010.0], [570.0, 840.0, 2_410.0], [300.0, 870.0, 2_240.0]];
const BANDWIDTHS: [f32; 3] = [90.0, 110.0, 150.0];

/// One sung note, or a consonant (`midi` NaN: noise only).
#[derive(Clone, Copy, Debug)]
pub struct Sung {
    pub start: f32,
    pub dur: f32,
    pub midi: f32,
    pub vowel: usize,
}

/// How the singer sings.
#[derive(Clone, Copy, Debug)]
pub struct Style {
    pub vibrato_cents: f32,
    pub vibrato_hz: f32,
    /// Peak of the slow drift off pitch.
    pub drift_cents: f32,
    /// Glide into each note from the previous one.
    pub glide: f32,
    pub level: f32,
}

impl Default for Style {
    fn default() -> Self {
        Self { vibrato_cents: 30.0, vibrato_hz: 5.5, drift_cents: 20.0, glide: 0.05, level: 0.3 }
    }
}

/// Spectral envelope (linear) of a vowel at `hz`: formants times the glottal tilt.
pub fn envelope(hz: f32, vowel: usize) -> f32 {
    let f = VOWELS[vowel % VOWELS.len()];
    let mut g = 1.0 / (1.0 + (hz / 120.0).powi(2));
    for (fi, bi) in f.iter().zip(BANDWIDTHS) {
        let r = hz / fi;
        let q = fi / bi;
        g *= 1.0 / ((1.0 - r * r).powi(2) + (r / q).powi(2)).sqrt();
    }
    g
}

/// The singer's true pitch at time `t` (MIDI, NaN outside notes).
pub fn true_pitch(notes: &[Sung], style: &Style, t: f32) -> f32 {
    let Some(i) = notes.iter().position(|n| n.midi.is_finite() && t >= n.start && t < n.start + n.dur) else { return f32::NAN };
    let n = notes[i];
    let mut m = n.midi;
    if let Some(prev) = notes[..i].iter().rev().find(|p| p.midi.is_finite() && n.start - (p.start + p.dur) < 0.03) {
        let k = ((t - n.start) / style.glide.max(1e-3)).clamp(0.0, 1.0);
        let k = k * k * (3.0 - 2.0 * k);
        m = prev.midi + (n.midi - prev.midi) * k;
    }
    let drift = style.drift_cents / 100.0 * ((0.37 * t).sin() * 0.7 + (1.13 * t + 1.0).sin() * 0.3);
    // Vibrato grows in after the first tenth of a second of the note.
    let grow = ((t - n.start - 0.1) / 0.3).clamp(0.0, 1.0);
    m + drift + grow * style.vibrato_cents / 100.0 * (std::f32::consts::TAU * style.vibrato_hz * t).sin()
}

/// Renders `secs` seconds at `sample_rate`.
pub fn sing(sample_rate: f32, notes: &[Sung], style: &Style, secs: f32) -> Vec<f32> {
    let n = (secs * sample_rate) as usize;
    let mut out = vec![0.0f32; n];
    let mut phase = 0.0f64;
    let mut noise = WhiteNoise::new(11);
    let (mut b1, mut b2) = (0.0f32, 0.0f32);
    let mut amps = [0.0f32; 160];
    let mut harmonics = 0;
    for (i, y) in out.iter_mut().enumerate() {
        let t = i as f32 / sample_rate;
        let pitch = true_pitch(notes, style, t);
        let note = notes.iter().find(|n| t >= n.start && t < n.start + n.dur);
        let Some(note) = note else {
            phase = 0.0;
            continue;
        };
        let env = ((t - note.start) / 0.03).clamp(0.0, 1.0) * ((note.start + note.dur - t) / 0.05).clamp(0.0, 1.0);
        if pitch.is_finite() {
            let f0 = ponzik_dsp::util::midi_to_hz(pitch);
            if i % 32 == 0 {
                harmonics = ((0.45 * sample_rate).min(8_000.0) / f0) as usize;
                harmonics = harmonics.min(amps.len());
                for (k, a) in amps[..harmonics].iter_mut().enumerate() {
                    *a = envelope(f0 * (k + 1) as f32, note.vowel);
                }
                // A singer keeps the level up as the pitch rises: RMS of 0.5.
                let power = amps[..harmonics].iter().map(|a| a * a * 0.5).sum::<f32>().sqrt().max(1e-9);
                amps[..harmonics].iter_mut().for_each(|a| *a *= 0.5 / power);
            }
            phase = (phase + f64::from(f0) / f64::from(sample_rate)).fract();
            let mut s = 0.0f32;
            for (k, a) in amps[..harmonics].iter().enumerate() {
                s += a * (TAU * (k + 1) as f64 * phase).sin() as f32;
            }
            *y = s * env * style.level;
        } else {
            // Sibilant: high band noise (two one-pole sections).
            let w = noise.next_sample();
            b1 += 0.75 * (w - b1);
            b2 += 0.75 * (b1 - b2);
            *y = (w - b2) * env * style.level * 0.5;
        }
    }
    out
}
