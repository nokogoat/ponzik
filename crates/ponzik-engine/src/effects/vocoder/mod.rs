//! Vocoder, like FL Studio's Vocodex or Ableton's Vocoder: the spectrum of a
//! modulator (a voice, from the key track of the sidechain input) shapes a
//! carrier (the track's own audio, or an internal chord of saw or pulse
//! oscillators playing a channel's notes, or noise).
//!
//! Both go through matching banks of 8 to 40 log-spaced band-pass filters
//! (two cascaded biquads each). An envelope follower on every modulator band
//! sets the level of the carrier band it is mapped to; the formant shift
//! moves the carrier bands against the modulator ones. Each carrier band is
//! flattened by its own peak follower, so the output takes the modulator's
//! spectrum whatever the carrier's tilt, at the carrier's overall level.
//! When the modulator sounds like a consonant (most of its energy high up),
//! noise is mixed into the carrier so the "s" and "t" stay intelligible.
//!
//! Without a key track the vocoder listens to its own track as the modulator.
//! Everything is allocated at construction; `process` never allocates.

mod carrier;
pub mod shared;

#[cfg(test)]
mod tests;

use std::any::Any;
use std::sync::Arc;

use ponzik_dsp::biquad::{Biquad, BiquadCoefs};
use ponzik_dsp::smooth::Smoother;
use ponzik_dsp::util::{db_to_gain, decay_coef};

use super::{DEFAULT_SR, KeyInput, Values};
use crate::effect::{ChannelNotes, Effect};
use crate::param::{ParamInfo, Params, Unit};

pub use carrier::{Carrier, Wave};
pub use shared::VocoderShared;

/// Most bands of the filter bank.
pub const MAX_BANDS: usize = 40;
/// Band counts offered (choice index → count).
pub const BAND_COUNTS: [usize; 11] = [8, 10, 12, 14, 16, 20, 24, 28, 32, 36, 40];
const BAND_NAMES: &[&str] = &["8", "10", "12", "14", "16", "20", "24", "28", "32", "36", "40"];
/// Carriers (choice index): the track's audio, internal chords, noise.
pub const CARRIERS: &[&str] = &["Track audio", "Saw chord", "Pulse chord", "Noise"];
pub const CARRIER_TRACK: usize = 0;
pub const CARRIER_NOISE: usize = 3;
/// Note of the drone at 0 st (C3), played by an internal carrier with no MIDI channel.
pub const DRONE_ROOT: f32 = 48.0;

pub const P_BANDS: u32 = 0;
pub const P_LOW: u32 = 1;
pub const P_HIGH: u32 = 2;
pub const P_WIDTH: u32 = 3;
pub const P_FORMANT: u32 = 4;
pub const P_ATTACK: u32 = 5;
pub const P_RELEASE: u32 = 6;
pub const P_UNVOICED: u32 = 7;
pub const P_EMPHASIS: u32 = 8;
pub const P_CARRIER: u32 = 9;
pub const P_MIDI_CHANNEL: u32 = 10;
pub const P_DRONE: u32 = 11;
pub const P_DETUNE: u32 = 12;
pub const P_MOD_LEVEL: u32 = 13;
pub const P_CAR_LEVEL: u32 = 14;
pub const P_SPREAD: u32 = 15;
pub const P_OUTPUT: u32 = 16;
pub const P_MIX: u32 = 17;
pub const PARAM_COUNT: usize = 18;

const G_BANDS: &str = "Bands";
const G_ENV: &str = "Envelope";
const G_CARRIER: &str = "Carrier";
const G_OUT: &str = "Output";

static PARAMS: [ParamInfo; PARAM_COUNT] = [
    ParamInfo::choice(P_BANDS, "Bands", BAND_NAMES, 5).group(G_BANDS),
    ParamInfo::new(P_LOW, "Low", 40.0, 1_000.0, 110.0, Unit::Hz).log().group(G_BANDS),
    ParamInfo::new(P_HIGH, "High", 1_500.0, 16_000.0, 8_000.0, Unit::Hz).log().group(G_BANDS),
    ParamInfo::new(P_WIDTH, "Width", 0.25, 4.0, 1.0, Unit::Percent).log().group(G_BANDS),
    ParamInfo::new(P_FORMANT, "Formant", -12.0, 12.0, 0.0, Unit::Semitones).group(G_BANDS),
    ParamInfo::new(P_ATTACK, "Attack", 0.000_5, 0.2, 0.004, Unit::Seconds).log().group(G_ENV),
    ParamInfo::new(P_RELEASE, "Release", 0.005, 2.0, 0.07, Unit::Seconds).log().group(G_ENV),
    ParamInfo::new(P_UNVOICED, "Unvoiced", 0.0, 1.0, 0.5, Unit::Percent).group(G_ENV),
    ParamInfo::new(P_EMPHASIS, "Emphasis", -6.0, 6.0, 1.5, Unit::Db).group(G_ENV),
    ParamInfo::choice(P_CARRIER, "Carrier", CARRIERS, 0).group(G_CARRIER),
    ParamInfo::new(P_MIDI_CHANNEL, "MIDI channel", 0.0, 128.0, 0.0, Unit::None).group(G_CARRIER),
    ParamInfo::new(P_DRONE, "Drone", -24.0, 24.0, 0.0, Unit::Semitones).group(G_CARRIER),
    ParamInfo::new(P_DETUNE, "Detune", 0.0, 50.0, 8.0, Unit::Cents).group(G_CARRIER),
    ParamInfo::new(P_MOD_LEVEL, "Modulator", 0.0, 1.0, 0.0, Unit::Percent).group(G_OUT),
    ParamInfo::new(P_CAR_LEVEL, "Carrier level", 0.0, 1.0, 0.0, Unit::Percent).group(G_OUT),
    ParamInfo::new(P_SPREAD, "Spread", 0.0, 1.0, 0.3, Unit::Percent).group(G_OUT),
    ParamInfo::new(P_OUTPUT, "Output", -24.0, 24.0, 0.0, Unit::Db).group(G_OUT),
    ParamInfo::new(P_MIX, "Mix", 0.0, 1.0, 1.0, Unit::Percent).group(G_OUT),
];

pub fn params() -> &'static [ParamInfo; PARAM_COUNT] {
    &PARAMS
}

/// Carrier level the output is calibrated on: a carrier peaking there gives
/// the modulator's own level back.
const REFERENCE: f32 = 0.25;
/// Rectified sine → amplitude (the mean of |sin| is 2/π).
const RECTIFIED: f32 = std::f32::consts::FRAC_PI_2;
/// Above this frequency the modulator counts as unvoiced energy.
const UNVOICED_HZ: f32 = 4_500.0;
/// Level of the consonant noise, against the reference.
const NOISE_LEVEL: f32 = 0.3;

/// Where the bands sit: centers of the modulator bands, of the carrier
/// bands they drive (formant shift), and the Q of each filter stage.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub count: usize,
    pub modulator: [f32; MAX_BANDS],
    pub carrier: [f32; MAX_BANDS],
    pub q: f32,
}

impl Layout {
    pub fn new(count: usize, low: f32, high: f32, width: f32, formant: f32, sample_rate: f32) -> Self {
        let count = count.clamp(2, MAX_BANDS);
        let nyquist = 0.45 * sample_rate;
        let low = low.clamp(20.0, nyquist * 0.5);
        let high = high.clamp(low * 2.0, nyquist.max(low * 2.0));
        let shift = 2f32.powf(formant.clamp(-24.0, 24.0) / 12.0);
        let span = (high / low).log2();
        let mut modulator = [0.0; MAX_BANDS];
        let mut carrier = [0.0; MAX_BANDS];
        for i in 0..count {
            let hz = low * 2f32.powf(span * i as f32 / (count - 1) as f32);
            modulator[i] = hz;
            carrier[i] = (hz * shift).clamp(20.0, nyquist);
        }
        // Bandwidth in octaves: the spacing of the bands times the width,
        // widened for the two cascaded stages (which narrow it again).
        let octaves = (span / (count - 1) as f32 * width.clamp(0.1, 8.0) * 1.55).clamp(0.02, 4.0);
        let q = 1.0 / (2.0 * (std::f32::consts::LN_2 * 0.5 * octaves).sinh());
        Self { count, modulator, carrier, q }
    }
}

#[derive(Clone, Debug, Default)]
struct Band {
    modulator: [Biquad; 2],
    left: [Biquad; 2],
    right: [Biquad; 2],
    env: f32,
    carrier_env: f32,
    carrier_release: f32,
    gain: f32,
    pan: (f32, f32),
    /// Levels shown by the interface (peak, slow release).
    shown_mod: f32,
    shown_out: f32,
}

impl Band {
    #[inline]
    fn cascade(f: &mut [Biquad; 2], x: f32) -> f32 {
        let [a, b] = f;
        b.process(a.process(x))
    }
}

pub struct Vocoder {
    values: Values<PARAM_COUNT>,
    key: KeyInput,
    sample_rate: f32,
    bands: Vec<Band>,
    count: usize,
    carrier: Carrier,
    wave: Option<Wave>,
    notes: ChannelNotes,
    noise: ponzik_dsp::noise::WhiteNoise,
    attack: f32,
    release: f32,
    /// Unvoiced detector: high-passed modulator and envelopes.
    hf: [Biquad; 2],
    hf_env: f32,
    all_env: f32,
    unvoiced: f32,
    detector: (f32, f32),
    /// Peak follower of the whole carrier.
    carrier_env: f32,
    carrier_release: f32,
    mix: Smoother,
    output: Smoother,
    mod_level: Smoother,
    car_level: Smoother,
    shared: Arc<VocoderShared>,
}

impl Default for Vocoder {
    fn default() -> Self {
        let mut v = Self {
            values: Values::new(&PARAMS),
            key: KeyInput::new(),
            sample_rate: DEFAULT_SR,
            bands: vec![Band::default(); MAX_BANDS],
            count: 0,
            carrier: Carrier::new(DEFAULT_SR),
            wave: None,
            notes: ChannelNotes::EMPTY,
            noise: ponzik_dsp::noise::WhiteNoise::new(0xC0FF_EE11),
            attack: 0.0,
            release: 0.0,
            hf: Default::default(),
            hf_env: 0.0,
            all_env: 0.0,
            unvoiced: 0.0,
            detector: (0.0, 0.0),
            carrier_env: 0.0,
            carrier_release: 0.0,
            mix: Smoother::new(1.0),
            output: Smoother::new(1.0),
            mod_level: Smoother::new(0.0),
            car_level: Smoother::new(0.0),
            shared: Arc::new(VocoderShared::default()),
        };
        v.set_sample_rate(DEFAULT_SR);
        v
    }
}

impl Vocoder {
    /// Band layout of the current settings.
    pub fn layout(&self) -> Layout {
        layout_of(&|id| self.values.get(id), self.sample_rate)
    }

    fn update(&mut self) {
        let sr = self.sample_rate;
        let v = &self.values;
        let layout = layout_of(&|id| v.get(id), sr);
        self.count = layout.count;
        let emphasis = v.get(P_EMPHASIS);
        let spread = v.get(P_SPREAD);
        for (i, b) in self.bands.iter_mut().enumerate().take(layout.count) {
            let m = BiquadCoefs::bandpass(layout.modulator[i], layout.q, sr);
            let c = BiquadCoefs::bandpass(layout.carrier[i], layout.q, sr);
            b.modulator.iter_mut().for_each(|f| f.coefs = m);
            b.left.iter_mut().chain(&mut b.right).for_each(|f| f.coefs = c);
            // A few periods of the band: the flattening follows the band's own waves.
            b.carrier_release = decay_coef((4.0 / layout.carrier[i]).max(0.015), sr);
            b.gain = db_to_gain(emphasis * (layout.modulator[i] / 1_000.0).log2()) * RECTIFIED / REFERENCE;
            // Bands alternate left and right; equal power at the center.
            let side = if i % 2 == 0 { -1.0 } else { 1.0 };
            let angle = (1.0 + side * spread) * std::f32::consts::FRAC_PI_4;
            b.pan = (angle.cos() * std::f32::consts::SQRT_2, angle.sin() * std::f32::consts::SQRT_2);
        }
        self.attack = decay_coef(v.get(P_ATTACK), sr);
        self.release = decay_coef(v.get(P_RELEASE), sr);
        self.wave = match v.index(P_CARRIER) {
            1 => Some(Wave::Saw),
            2 => Some(Wave::Pulse),
            3 => Some(Wave::Noise),
            _ => None,
        };
        self.carrier.set_detune(v.get(P_DETUNE));
        self.mix.set_target(v.get(P_MIX));
        self.output.set_target(db_to_gain(v.get(P_OUTPUT)));
        self.mod_level.set_target(v.get(P_MOD_LEVEL));
        self.car_level.set_target(v.get(P_CAR_LEVEL));
        self.follow_notes();
    }

    /// The internal chord plays the source channel's notes, or the drone without a channel.
    fn follow_notes(&mut self) {
        if self.values.get(P_MIDI_CHANNEL).round() as usize == 0 {
            let mut drone = ChannelNotes::EMPTY;
            drone.note_on((DRONE_ROOT + self.values.get(P_DRONE).round()).clamp(0.0, 127.0) as u8);
            self.carrier.follow(&drone);
        } else {
            self.carrier.follow(&self.notes);
        }
    }

    /// The interface's view of the vocoder.
    pub fn shared_state(&self) -> Arc<VocoderShared> {
        self.shared.clone()
    }
}

/// Layout from the values of the parameters (`get`), at `sample_rate`.
pub fn layout_of(get: &dyn Fn(u32) -> f32, sample_rate: f32) -> Layout {
    let index = (get(P_BANDS).round().max(0.0) as usize).min(BAND_COUNTS.len() - 1);
    Layout::new(BAND_COUNTS[index], get(P_LOW), get(P_HIGH), get(P_WIDTH), get(P_FORMANT), sample_rate)
}

impl Params for Vocoder {
    fn params(&self) -> &'static [ParamInfo] {
        &PARAMS
    }

    fn get_param(&self, id: u32) -> f32 {
        self.values.get(id)
    }

    fn set_param(&mut self, id: u32, value: f32) {
        if self.values.set(&PARAMS, id, value) {
            self.update();
        }
    }
}

impl Effect for Vocoder {
    fn kind(&self) -> &'static str {
        "vocoder"
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate.max(1_000.0);
        let sr = self.sample_rate;
        self.carrier.set_sample_rate(sr);
        let hp = BiquadCoefs::highpass(UNVOICED_HZ, std::f32::consts::FRAC_1_SQRT_2, sr);
        self.hf.iter_mut().for_each(|f| f.coefs = hp);
        self.detector = (decay_coef(0.002, sr), decay_coef(0.03, sr));
        self.carrier_release = decay_coef(0.1, sr);
        for s in [&mut self.mix, &mut self.output, &mut self.mod_level, &mut self.car_level] {
            s.set_time(0.02, sr);
        }
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        for b in &mut self.bands {
            b.modulator.iter_mut().chain(&mut b.left).chain(&mut b.right).for_each(Biquad::reset);
            (b.env, b.carrier_env, b.shown_mod, b.shown_out) = (0.0, 0.0, 0.0, 0.0);
        }
        self.hf.iter_mut().for_each(Biquad::reset);
        (self.hf_env, self.all_env, self.unvoiced, self.carrier_env) = (0.0, 0.0, 0.0, 0.0);
        self.carrier.reset();
        self.follow_notes();
        for s in [&mut self.mix, &mut self.output, &mut self.mod_level, &mut self.car_level] {
            s.snap();
        }
    }

    fn sidechain(&mut self, left: &[f32], right: &[f32]) {
        self.key.store(left, right);
    }

    fn channel_notes(&mut self, notes: &[ChannelNotes]) {
        let ch = self.values.get(P_MIDI_CHANNEL).round() as usize;
        let held = if ch == 0 { ChannelNotes::EMPTY } else { notes.get(ch - 1).copied().unwrap_or_default() };
        if held != self.notes {
            self.notes = held;
            if ch > 0 {
                self.carrier.follow(&held);
            }
        }
    }

    fn shared(&self) -> Option<Arc<dyn Any + Send + Sync>> {
        Some(self.shared.clone())
    }

    fn tail(&self) -> f32 {
        self.values.get(P_RELEASE)
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let keyed = self.key.take();
        let n = self.count;
        let (attack, release) = (self.attack, self.release);
        let (det_attack, det_release) = self.detector;
        let unvoiced_amount = self.values.get(P_UNVOICED);
        let wave = self.wave;
        let bands = &mut self.bands[..n];
        for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
            let (xl, xr) = (if l.is_finite() { *l } else { 0.0 }, if r.is_finite() { *r } else { 0.0 });
            let m = if keyed {
                let (a, b) = self.key.at(i);
                let m = 0.5 * (a + b);
                if m.is_finite() { m } else { 0.0 }
            } else {
                0.5 * (xl + xr)
            };

            // Consonant detector: share of the modulator's level above UNVOICED_HZ.
            let h = Band::cascade(&mut self.hf, m).abs();
            let a = m.abs();
            self.hf_env = h + (self.hf_env - h) * if h > self.hf_env { det_attack } else { det_release };
            self.all_env = a + (self.all_env - a) * if a > self.all_env { det_attack } else { det_release };
            let ratio = self.hf_env / (self.all_env + 1e-6);
            let want = ((ratio - 0.3) / 0.35).clamp(0.0, 1.0) * (self.all_env > 1e-4) as u8 as f32;
            self.unvoiced += (want - self.unvoiced) * (1.0 - det_release);

            let (mut cl, mut cr) = match wave {
                None => (xl, xr),
                Some(w) => {
                    let c = self.carrier.next(w);
                    (c, c)
                }
            };
            if unvoiced_amount > 0.0 {
                let noise = self.noise.next_sample() * NOISE_LEVEL * REFERENCE * 4.0 * unvoiced_amount * self.unvoiced;
                cl += noise;
                cr += noise;
            }
            let stereo = wave.is_none();
            let peak = cl.abs().max(cr.abs());
            self.carrier_env = if peak > self.carrier_env { peak } else { self.carrier_env * self.carrier_release };
            let level = self.carrier_env;
            let floor = 1e-5 + 0.02 * level;

            let (mut ol, mut or) = (0.0, 0.0);
            for b in bands.iter_mut() {
                let mb = Band::cascade(&mut b.modulator, m).abs();
                b.env = mb + (b.env - mb) * if mb > b.env { attack } else { release };
                let yl = Band::cascade(&mut b.left, cl);
                let yr = if stereo { Band::cascade(&mut b.right, cr) } else { yl };
                let p = yl.abs().max(yr.abs());
                b.carrier_env = if p > b.carrier_env { p } else { b.carrier_env * b.carrier_release };
                let g = b.env * b.gain * level / (b.carrier_env + floor);
                let (zl, zr) = (yl * g * b.pan.0, yr * g * b.pan.1);
                ol += zl;
                or += zr;
                b.shown_mod = b.shown_mod.max(b.env * RECTIFIED);
                b.shown_out = b.shown_out.max(zl.abs().max(zr.abs()));
            }

            let (ml, cv) = (self.mod_level.next_value(), self.car_level.next_value());
            let wl = ol + ml * m + cv * cl;
            let wr = or + ml * m + cv * cr;
            let (mix, gain) = (self.mix.next_value(), self.output.next_value());
            *l = (xl * (1.0 - mix) + wl * mix) * gain;
            *r = (xr * (1.0 - mix) + wr * mix) * gain;
        }
        // Levels for the interface: peaks of the block, falling slowly.
        let fall = decay_coef(0.12, self.sample_rate).powi(left.len() as i32);
        for (k, b) in bands.iter_mut().enumerate() {
            self.shared.publish(k, b.shown_mod, b.shown_out);
            b.shown_mod *= fall;
            b.shown_out *= fall;
            b.env = ponzik_dsp::util::flush_denormal(b.env);
            b.carrier_env = ponzik_dsp::util::flush_denormal(b.carrier_env);
        }
        self.shared.publish_state(n, self.unvoiced, keyed);
    }
}
